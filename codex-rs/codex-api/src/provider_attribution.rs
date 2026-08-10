use crate::common::ResponsesApiRequest;
use sha2::Digest;
use sha2::Sha256;
use std::sync::Arc;

/// One final input item and its provider-reported token count.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderTerminalInputItem {
    pub item_id: String,
    pub input_tokens: u64,
}

/// The provider-owned terminal receipt required for exact per-skill attribution.
///
/// A provider implementation must obtain these values from the same successful terminal response
/// that produced `authoritative_input_tokens`. It must not derive token counts from request bytes,
/// local tokenizers, transcript reconstruction, or aggregate-only usage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderTerminalAttributionReceipt {
    pub session_id: String,
    pub turn_id: String,
    pub response_id: String,
    pub request_fingerprint: String,
    pub requested_model: String,
    pub resolved_model: String,
    pub requested_model_fingerprint: String,
    pub resolved_model_fingerprint: String,
    pub provider_transformation_version: String,
    pub tokenizer_accounting_version: String,
    pub input_items: Vec<ProviderTerminalInputItem>,
    pub authoritative_input_tokens: u64,
}

/// The terminal response context to which a provider receipt must belong.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderTerminalResponse {
    pub session_id: Option<String>,
    pub turn_id: Option<String>,
    pub response_id: String,
    pub requested_model: String,
    pub resolved_model: Option<String>,
    pub authoritative_input_tokens: Option<u64>,
    pub successful: bool,
}

/// A validated provider receipt plus the Codex bindings for the exact request and terminal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderTerminalAttribution {
    pub receipt: ProviderTerminalAttributionReceipt,
    pub terminal: ProviderTerminalResponse,
    pub final_request_fingerprint: String,
}

/// A provider-owned terminal attribution result for a final request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderRequestAttribution {
    Pending(ProviderTerminalAttributionHandle),
    ExactTerminalReceipt(ProviderTerminalAttribution),
    Unavailable(ProviderRequestAttributionError),
}

/// A typed reason why exact per-item provider token attribution is unavailable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderRequestAttributionError {
    ProviderDoesNotExposeExactPerItemTokens { provider: String },
    ProviderReportsAggregateOnly { provider: String, input_tokens: u64 },
    EncoderRejected { provider: String, reason: String },
    MissingTerminalResponse,
    TerminalResponseNotSuccessful,
    MissingSessionId,
    MissingTurnId,
    MissingResponseId,
    MissingAggregateInputTokens,
    MissingRequestFingerprint,
    MissingRequestedModel,
    MissingResolvedModel,
    MissingRequestedModelFingerprint,
    MissingResolvedModelFingerprint,
    MissingProviderTransformationVersion,
    MissingTokenizerAccountingVersion,
    RequestFingerprintMismatch,
    RequestedModelMismatch,
    RequestedModelFingerprintMismatch,
    ResolvedModelMismatch,
    ResolvedModelFingerprintMismatch,
    SessionIdMismatch,
    TurnIdMismatch,
    ResponseIdMismatch,
    MissingFinalInputItemId { index: usize },
    DuplicateFinalInputItemId { item_id: String },
    MissingReceiptInputItemId { item_id: String },
    DuplicateReceiptInputItemId { item_id: String },
    ExtraReceiptInputItemId { item_id: String },
    AggregateInputTokensMismatch { expected: u64, actual: u64 },
    InputTokenSumOverflow,
}

/// Provider-owned terminal attribution hook.
///
/// Implementations receive the exact uncompressed JSON bytes produced for the provider request,
/// the request model, and the successful terminal response context. They must return a receipt with
/// exact provider token counts for every final `input` item. A byte count, compact-JSON lexical
/// count, local tokenizer, aggregate usage value, or independent approximation is not a valid
/// implementation. An unavailable public provider surface must return a typed error instead of
/// synthesizing a receipt.
pub trait ProviderRequestTokenAttributor: Send + Sync {
    fn terminal_attribution(
        &self,
        request: &ResponsesApiRequest,
        final_body: &[u8],
        terminal: &ProviderTerminalResponse,
    ) -> Result<ProviderTerminalAttributionReceipt, ProviderRequestAttributionError>;
}

/// Fingerprints the exact serialized request body captured immediately before transport.
pub fn provider_request_fingerprint(final_body: &[u8]) -> String {
    format!("sha256:request-v1:{:x}", Sha256::digest(final_body))
}

/// Fingerprints a model name for receipt-to-request binding.
pub fn provider_model_fingerprint(model: &str) -> String {
    format!("sha256:model-v1:{:x}", Sha256::digest(model.as_bytes()))
}

#[derive(Clone)]
pub struct ProviderTerminalAttributionHandle {
    state: Arc<std::sync::Mutex<ProviderTerminalAttributionState>>,
}

struct ProviderTerminalAttributionState {
    request: ResponsesApiRequest,
    final_body: Vec<u8>,
    final_request_fingerprint: String,
    provider: String,
    session_id: Option<String>,
    turn_id: Option<String>,
    attributor: Option<Arc<dyn ProviderRequestTokenAttributor>>,
    result: Option<Result<ProviderTerminalAttribution, ProviderRequestAttributionError>>,
}

impl std::fmt::Debug for ProviderTerminalAttributionHandle {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProviderTerminalAttributionHandle")
            .finish()
    }
}

impl PartialEq for ProviderTerminalAttributionHandle {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.state, &other.state)
    }
}

impl Eq for ProviderTerminalAttributionHandle {}

impl ProviderTerminalAttributionHandle {
    pub(crate) fn new(
        request: ResponsesApiRequest,
        final_body: Vec<u8>,
        provider: String,
        session_id: Option<String>,
        turn_id: Option<String>,
        attributor: Option<Arc<dyn ProviderRequestTokenAttributor>>,
    ) -> Self {
        let final_request_fingerprint = provider_request_fingerprint(&final_body);
        Self {
            state: Arc::new(std::sync::Mutex::new(ProviderTerminalAttributionState {
                request,
                final_body,
                final_request_fingerprint,
                provider,
                session_id,
                turn_id,
                attributor,
                result: None,
            })),
        }
    }

    pub(crate) fn complete(&self, terminal: ProviderTerminalResponse) {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        if state.result.is_some() {
            return;
        }
        let result = if !terminal.successful {
            Err(ProviderRequestAttributionError::TerminalResponseNotSuccessful)
        } else if state.session_id != terminal.session_id {
            Err(ProviderRequestAttributionError::SessionIdMismatch)
        } else if state.turn_id != terminal.turn_id {
            Err(ProviderRequestAttributionError::TurnIdMismatch)
        } else if terminal.requested_model != state.request.model {
            Err(ProviderRequestAttributionError::RequestedModelMismatch)
        } else if let Some(attributor) = state.attributor.as_ref() {
            match attributor.terminal_attribution(&state.request, &state.final_body, &terminal) {
                Ok(receipt) => validate_terminal_receipt(
                    &state.request,
                    &state.final_request_fingerprint,
                    &terminal,
                    receipt,
                )
                .map(|receipt| ProviderTerminalAttribution {
                    receipt,
                    terminal,
                    final_request_fingerprint: state.final_request_fingerprint.clone(),
                }),
                Err(error) => Err(error),
            }
        } else {
            Err(
                ProviderRequestAttributionError::ProviderDoesNotExposeExactPerItemTokens {
                    provider: state.provider.clone(),
                },
            )
        };
        state.result = Some(result);
    }

    pub(crate) fn finish_without_terminal(&self) {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        if state.result.is_none() {
            state.result = Some(Err(
                ProviderRequestAttributionError::MissingTerminalResponse,
            ));
        }
    }

    fn resolve(&self) -> Result<ProviderTerminalAttribution, ProviderRequestAttributionError> {
        let state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        state.result.clone().unwrap_or(Err(
            ProviderRequestAttributionError::MissingTerminalResponse,
        ))
    }
}

impl ProviderRequestAttribution {
    /// Resolves terminal attribution after the stream has delivered its successful completion.
    pub fn resolve(self) -> Result<ProviderTerminalAttribution, ProviderRequestAttributionError> {
        match self {
            Self::Pending(handle) => handle.resolve(),
            Self::ExactTerminalReceipt(attribution) => Ok(attribution),
            Self::Unavailable(error) => Err(error),
        }
    }
}

fn validate_terminal_receipt(
    request: &ResponsesApiRequest,
    final_request_fingerprint: &str,
    terminal: &ProviderTerminalResponse,
    receipt: ProviderTerminalAttributionReceipt,
) -> Result<ProviderTerminalAttributionReceipt, ProviderRequestAttributionError> {
    let session_id = terminal
        .session_id
        .as_deref()
        .filter(|value| !value.is_empty())
        .ok_or(ProviderRequestAttributionError::MissingSessionId)?;
    let turn_id = terminal
        .turn_id
        .as_deref()
        .filter(|value| !value.is_empty())
        .ok_or(ProviderRequestAttributionError::MissingTurnId)?;
    let response_id = (!terminal.response_id.is_empty())
        .then_some(terminal.response_id.as_str())
        .ok_or(ProviderRequestAttributionError::MissingResponseId)?;
    let authoritative_input_tokens = terminal
        .authoritative_input_tokens
        .ok_or(ProviderRequestAttributionError::MissingAggregateInputTokens)?;
    if receipt.session_id.is_empty() {
        return Err(ProviderRequestAttributionError::MissingSessionId);
    }
    if receipt.turn_id.is_empty() {
        return Err(ProviderRequestAttributionError::MissingTurnId);
    }
    if receipt.response_id.is_empty() {
        return Err(ProviderRequestAttributionError::MissingResponseId);
    }
    if receipt.request_fingerprint.is_empty() {
        return Err(ProviderRequestAttributionError::MissingRequestFingerprint);
    }
    if receipt.requested_model.is_empty() {
        return Err(ProviderRequestAttributionError::MissingRequestedModel);
    }
    if receipt.resolved_model.is_empty() {
        return Err(ProviderRequestAttributionError::MissingResolvedModel);
    }
    if receipt.requested_model_fingerprint.is_empty() {
        return Err(ProviderRequestAttributionError::MissingRequestedModelFingerprint);
    }
    if receipt.resolved_model_fingerprint.is_empty() {
        return Err(ProviderRequestAttributionError::MissingResolvedModelFingerprint);
    }
    if receipt.provider_transformation_version.is_empty() {
        return Err(ProviderRequestAttributionError::MissingProviderTransformationVersion);
    }
    if receipt.tokenizer_accounting_version.is_empty() {
        return Err(ProviderRequestAttributionError::MissingTokenizerAccountingVersion);
    }
    if receipt.session_id != session_id {
        return Err(ProviderRequestAttributionError::SessionIdMismatch);
    }
    if receipt.turn_id != turn_id {
        return Err(ProviderRequestAttributionError::TurnIdMismatch);
    }
    if receipt.response_id != response_id {
        return Err(ProviderRequestAttributionError::ResponseIdMismatch);
    }
    if receipt.request_fingerprint != final_request_fingerprint {
        return Err(ProviderRequestAttributionError::RequestFingerprintMismatch);
    }
    if receipt.requested_model != terminal.requested_model {
        return Err(ProviderRequestAttributionError::RequestedModelMismatch);
    }
    if receipt.requested_model_fingerprint != provider_model_fingerprint(&receipt.requested_model) {
        return Err(ProviderRequestAttributionError::RequestedModelFingerprintMismatch);
    }
    if terminal
        .resolved_model
        .as_deref()
        .is_some_and(|resolved_model| resolved_model != receipt.resolved_model)
    {
        return Err(ProviderRequestAttributionError::ResolvedModelMismatch);
    }
    if receipt.resolved_model_fingerprint != provider_model_fingerprint(&receipt.resolved_model) {
        return Err(ProviderRequestAttributionError::ResolvedModelFingerprintMismatch);
    }
    if receipt.authoritative_input_tokens != authoritative_input_tokens {
        return Err(
            ProviderRequestAttributionError::AggregateInputTokensMismatch {
                expected: authoritative_input_tokens,
                actual: receipt.authoritative_input_tokens,
            },
        );
    }

    let mut final_item_ids = std::collections::HashSet::new();
    for (index, item) in request.input.iter().enumerate() {
        let Some(item_id) = item.id() else {
            return Err(ProviderRequestAttributionError::MissingFinalInputItemId { index });
        };
        let item_id = item_id.to_string();
        if !final_item_ids.insert(item_id.clone()) {
            return Err(ProviderRequestAttributionError::DuplicateFinalInputItemId { item_id });
        }
    }

    let mut receipt_item_ids = std::collections::HashSet::new();
    let mut input_tokens = 0_u64;
    for item in &receipt.input_items {
        if item.item_id.is_empty() {
            return Err(ProviderRequestAttributionError::MissingReceiptInputItemId {
                item_id: item.item_id.clone(),
            });
        }
        if !receipt_item_ids.insert(item.item_id.clone()) {
            return Err(
                ProviderRequestAttributionError::DuplicateReceiptInputItemId {
                    item_id: item.item_id.clone(),
                },
            );
        }
        if !final_item_ids.contains(&item.item_id) {
            return Err(ProviderRequestAttributionError::ExtraReceiptInputItemId {
                item_id: item.item_id.clone(),
            });
        }
        input_tokens = input_tokens
            .checked_add(item.input_tokens)
            .ok_or(ProviderRequestAttributionError::InputTokenSumOverflow)?;
    }
    if let Some(item_id) = final_item_ids.difference(&receipt_item_ids).next().cloned() {
        return Err(ProviderRequestAttributionError::MissingReceiptInputItemId { item_id });
    }
    if input_tokens != authoritative_input_tokens {
        return Err(
            ProviderRequestAttributionError::AggregateInputTokensMismatch {
                expected: authoritative_input_tokens,
                actual: input_tokens,
            },
        );
    }
    Ok(receipt)
}

#[cfg(test)]
#[path = "provider_attribution_tests.rs"]
mod tests;
