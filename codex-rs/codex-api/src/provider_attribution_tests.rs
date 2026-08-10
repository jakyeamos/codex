use super::*;
use codex_protocol::ResponseItemId;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;
use pretty_assertions::assert_eq;

fn input_item(id: Option<&str>) -> ResponseItem {
    let mut item = ResponseItem::Message {
        id: None,
        role: "user".to_string(),
        content: vec![ContentItem::InputText {
            text: "skill body".to_string(),
        }],
        phase: None,
        internal_chat_message_metadata_passthrough: None,
    };
    if let Some(id) = id {
        item.set_id(Some(ResponseItemId::with_suffix("msg", id)));
    }
    item
}

fn request(input: Vec<ResponseItem>) -> ResponsesApiRequest {
    ResponsesApiRequest {
        model: "test-model".to_string(),
        instructions: String::new(),
        input,
        tools: None,
        tool_choice: "auto".to_string(),
        parallel_tool_calls: false,
        reasoning: None,
        store: false,
        stream: true,
        stream_options: None,
        include: Vec::new(),
        service_tier: None,
        prompt_cache_key: None,
        text: None,
        client_metadata: None,
    }
}

fn receipt(request: &ResponsesApiRequest, input_tokens: u64) -> ProviderTerminalAttributionReceipt {
    let requested_model = request.model.clone();
    ProviderTerminalAttributionReceipt {
        session_id: "session".to_string(),
        turn_id: "turn".to_string(),
        response_id: "response".to_string(),
        request_fingerprint: provider_request_fingerprint(
            &serde_json::to_vec(request).expect("request should serialize"),
        ),
        requested_model: requested_model.clone(),
        resolved_model: requested_model.clone(),
        requested_model_fingerprint: provider_model_fingerprint(&requested_model),
        resolved_model_fingerprint: provider_model_fingerprint(&requested_model),
        provider_transformation_version: "provider-transform-v1".to_string(),
        tokenizer_accounting_version: "tokenizer-accounting-v1".to_string(),
        input_items: request
            .input
            .iter()
            .filter_map(|item| {
                item.id().map(|item_id| ProviderTerminalInputItem {
                    item_id: item_id.to_string(),
                    input_tokens,
                })
            })
            .collect(),
        authoritative_input_tokens: input_tokens
            * request
                .input
                .iter()
                .filter(|item| item.id().is_some())
                .count() as u64,
    }
}

fn terminal(authoritative_input_tokens: u64) -> ProviderTerminalResponse {
    ProviderTerminalResponse {
        session_id: Some("session".to_string()),
        turn_id: Some("turn".to_string()),
        response_id: "response".to_string(),
        requested_model: "test-model".to_string(),
        resolved_model: Some("test-model".to_string()),
        authoritative_input_tokens: Some(authoritative_input_tokens),
        successful: true,
    }
}

#[test]
fn terminal_receipt_accepts_exact_bijection() {
    let request = request(vec![input_item(Some("one"))]);
    let receipt = receipt(&request, 7);
    let body = serde_json::to_vec(&request).expect("request should serialize");

    assert!(
        validate_terminal_receipt(
            &request,
            &provider_request_fingerprint(&body),
            &terminal(7),
            receipt,
        )
        .is_ok()
    );
}

#[test]
fn terminal_receipt_rejects_missing_final_item_id() {
    let request = request(vec![input_item(None)]);
    let receipt = receipt(&request, 0);
    let body = serde_json::to_vec(&request).expect("request should serialize");

    assert_eq!(
        validate_terminal_receipt(
            &request,
            &provider_request_fingerprint(&body),
            &terminal(0),
            receipt,
        )
        .expect_err("final input items must have stable IDs"),
        ProviderRequestAttributionError::MissingFinalInputItemId { index: 0 }
    );
}

#[test]
fn terminal_receipt_rejects_duplicate_final_item_id() {
    let item = input_item(Some("duplicate"));
    let request = request(vec![item.clone(), item]);
    let receipt = receipt(&request, 7);
    let body = serde_json::to_vec(&request).expect("request should serialize");

    assert_eq!(
        validate_terminal_receipt(
            &request,
            &provider_request_fingerprint(&body),
            &terminal(14),
            receipt,
        )
        .expect_err("final input item IDs must be unique"),
        ProviderRequestAttributionError::DuplicateFinalInputItemId {
            item_id: "msg_duplicate".to_string(),
        }
    );
}
