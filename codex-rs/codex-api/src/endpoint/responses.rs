use crate::auth::SharedAuthProvider;
use crate::common::ResponseEvent;
use crate::common::ResponseStream;
use crate::common::ResponsesApiRequest;
use crate::endpoint::session::EndpointSession;
use crate::error::ApiError;
use crate::provider::Provider;
use crate::provider_attribution::ProviderRequestAttribution;
use crate::provider_attribution::ProviderRequestTokenAttributor;
use crate::provider_attribution::ProviderTerminalAttributionHandle;
use crate::provider_attribution::ProviderTerminalResponse;
use crate::requests::Compression;
use crate::requests::headers::build_session_headers;
use crate::requests::headers::insert_header;
use crate::requests::headers::subagent_header;
use crate::sse::spawn_response_stream;
use crate::telemetry::SseTelemetry;
use codex_client::EncodedJsonBody;
use codex_client::HttpTransport;
use codex_client::RequestCompression;
use codex_client::RequestTelemetry;
use codex_protocol::protocol::SessionSource;
use futures::StreamExt;
use http::HeaderMap;
use http::HeaderValue;
use http::Method;
use serde_json::Value;
use std::sync::Arc;
use std::sync::OnceLock;
use tokio::sync::mpsc;
use tracing::instrument;

pub struct ResponsesClient<T: HttpTransport> {
    session: EndpointSession<T>,
    sse_telemetry: Option<Arc<dyn SseTelemetry>>,
    provider_request_token_attributor: Option<Arc<dyn ProviderRequestTokenAttributor>>,
}

#[derive(Default)]
pub struct ResponsesOptions {
    pub session_id: Option<String>,
    pub thread_id: Option<String>,
    pub turn_id: Option<String>,
    pub session_source: Option<SessionSource>,
    pub extra_headers: HeaderMap,
    pub compression: Compression,
    pub turn_state: Option<Arc<OnceLock<String>>>,
}

impl<T: HttpTransport> ResponsesClient<T> {
    pub fn new(transport: T, provider: Provider, auth: SharedAuthProvider) -> Self {
        Self {
            session: EndpointSession::new(transport, provider, auth),
            sse_telemetry: None,
            provider_request_token_attributor: None,
        }
    }

    pub fn with_telemetry(
        self,
        request: Option<Arc<dyn RequestTelemetry>>,
        sse: Option<Arc<dyn SseTelemetry>>,
    ) -> Self {
        Self {
            session: self.session.with_request_telemetry(request),
            sse_telemetry: sse,
            provider_request_token_attributor: self.provider_request_token_attributor,
        }
    }

    /// Installs the provider-owned terminal receipt producer for final request bodies.
    ///
    /// This hook is intentionally opt-in because the current public provider surface may expose
    /// only aggregate usage. Providers that cannot expose every required terminal receipt field
    /// leave this unset or return a typed unavailable error. The request still proceeds, but its
    /// attribution must not be replaced with a local estimate.
    pub fn with_provider_request_token_attributor(
        mut self,
        attributor: Arc<dyn ProviderRequestTokenAttributor>,
    ) -> Self {
        self.provider_request_token_attributor = Some(attributor);
        self
    }

    #[instrument(
        name = "responses.stream_request",
        level = "info",
        skip_all,
        fields(
            transport = "responses_http",
            http.method = "POST",
            api.path = "responses"
        )
    )]
    pub async fn stream_request(
        &self,
        request: ResponsesApiRequest,
        options: ResponsesOptions,
    ) -> Result<ResponseStream, ApiError> {
        let (stream, _) = self
            .stream_request_with_attribution(request, options)
            .await?;
        Ok(stream)
    }

    /// Streams a request and returns the provider-owned terminal attribution receipt.
    ///
    /// The exact encoded request body is retained until the successful terminal response. A
    /// provider without a complete terminal receipt surface returns `Unavailable` while the
    /// provider request itself remains eligible to proceed.
    pub async fn stream_request_with_attribution(
        &self,
        request: ResponsesApiRequest,
        options: ResponsesOptions,
    ) -> Result<(ResponseStream, ProviderRequestAttribution), ApiError> {
        let ResponsesOptions {
            session_id,
            thread_id,
            turn_id,
            session_source,
            extra_headers,
            compression,
            turn_state,
        } = options;

        let body = EncodedJsonBody::encode(&request)
            .map_err(|e| ApiError::Stream(format!("failed to encode responses request: {e}")))?;
        let attribution_handle = ProviderTerminalAttributionHandle::new(
            request.clone(),
            body.as_bytes().to_vec(),
            self.session.provider().name.clone(),
            session_id.clone(),
            turn_id.clone(),
            self.provider_request_token_attributor.clone(),
        );

        let mut headers = extra_headers;
        if let Some(ref thread_id) = thread_id {
            insert_header(&mut headers, "x-client-request-id", thread_id);
        }
        headers.extend(build_session_headers(session_id.clone(), thread_id));
        if let Some(subagent) = subagent_header(&session_source) {
            insert_header(&mut headers, "x-openai-subagent", &subagent);
        }

        let stream = self
            .stream_encoded(body, headers, compression, turn_state)
            .await?;
        let stream = attach_terminal_attribution(
            stream,
            attribution_handle.clone(),
            session_id,
            turn_id,
            request.model,
        );
        Ok((
            stream,
            ProviderRequestAttribution::Pending(attribution_handle),
        ))
    }

    fn path() -> &'static str {
        "responses"
    }

    #[instrument(
        name = "responses.stream",
        level = "info",
        skip_all,
        fields(
            transport = "responses_http",
            http.method = "POST",
            api.path = "responses",
            turn.has_state = turn_state.is_some()
        )
    )]
    pub async fn stream(
        &self,
        body: Value,
        extra_headers: HeaderMap,
        compression: Compression,
        turn_state: Option<Arc<OnceLock<String>>>,
    ) -> Result<ResponseStream, ApiError> {
        let body = EncodedJsonBody::encode(&body)
            .map_err(|e| ApiError::Stream(format!("failed to encode responses request: {e}")))?;
        self.stream_encoded(body, extra_headers, compression, turn_state)
            .await
    }

    async fn stream_encoded(
        &self,
        body: EncodedJsonBody,
        extra_headers: HeaderMap,
        compression: Compression,
        turn_state: Option<Arc<OnceLock<String>>>,
    ) -> Result<ResponseStream, ApiError> {
        let request_compression = match compression {
            Compression::None => RequestCompression::None,
            Compression::Zstd => RequestCompression::Zstd,
        };

        let stream_response = self
            .session
            .stream_encoded_json_with(
                Method::POST,
                Self::path(),
                extra_headers,
                Some(body),
                |req| {
                    req.headers.insert(
                        http::header::ACCEPT,
                        HeaderValue::from_static("text/event-stream"),
                    );
                    req.compression = request_compression;
                },
            )
            .await?;

        Ok(spawn_response_stream(
            stream_response,
            self.session.provider().stream_idle_timeout,
            self.sse_telemetry.clone(),
            turn_state,
        ))
    }
}

fn attach_terminal_attribution(
    mut stream: ResponseStream,
    attribution_handle: ProviderTerminalAttributionHandle,
    session_id: Option<String>,
    turn_id: Option<String>,
    requested_model: String,
) -> ResponseStream {
    let upstream_request_id = stream.upstream_request_id.clone();
    let (tx_event, rx_event) = mpsc::channel(32);
    tokio::spawn(async move {
        let mut resolved_model = None;
        let mut completed = false;
        while let Some(event) = stream.next().await {
            match &event {
                Ok(ResponseEvent::ServerModel(model)) => {
                    resolved_model = Some(model.clone());
                }
                Ok(ResponseEvent::Completed {
                    response_id,
                    token_usage,
                    ..
                }) => {
                    completed = true;
                    attribution_handle.complete(ProviderTerminalResponse {
                        session_id: session_id.clone(),
                        turn_id: turn_id.clone(),
                        response_id: response_id.clone(),
                        requested_model: requested_model.clone(),
                        resolved_model: resolved_model.clone(),
                        authoritative_input_tokens: token_usage
                            .as_ref()
                            .and_then(|usage| u64::try_from(usage.input_tokens).ok()),
                        successful: true,
                    });
                }
                Err(_) => {}
                Ok(_) => {}
            }
            if tx_event.send(event).await.is_err() {
                return;
            }
        }
        if !completed {
            attribution_handle.finish_without_terminal();
        }
    });
    ResponseStream {
        rx_event,
        upstream_request_id,
    }
}
