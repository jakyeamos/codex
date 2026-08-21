use super::SkillReadMetrics;
use super::SkillReadProvenance;
use super::SkillReadProviderRequest;
use super::SkillReadTelemetry;
use super::SkillReadTelemetryOutcome;
use codex_api::ProviderRequestAttribution;
use codex_api::ProviderRequestAttributionError;
use codex_api::ProviderTerminalAttribution;
use codex_api::ProviderTerminalAttributionReceipt;
use codex_api::ProviderTerminalInputItem;
use codex_api::ProviderTerminalResponse;
use codex_api::provider_model_fingerprint;
use codex_protocol::ResponseItemId;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;
use pretty_assertions::assert_eq;

fn skill_item(id_suffix: &str, contents: &str) -> ResponseItem {
    let mut item = ResponseItem::Message {
        id: None,
        role: "user".to_string(),
        content: vec![ContentItem::InputText {
            text: contents.to_string(),
        }],
        phase: None,
        internal_chat_message_metadata_passthrough: None,
    };
    item.set_id(Some(ResponseItemId::with_suffix("msg", id_suffix)));
    item
}

fn provenance(
    session_id: &str,
    turn_id: &str,
    path: &str,
    digest: &str,
    item: ResponseItem,
) -> SkillReadProvenance {
    SkillReadProvenance::new_for_test(session_id, turn_id, path, digest, item)
}

fn record_exact(
    telemetry: &SkillReadTelemetry,
    session_id: &str,
    turn_id: &str,
    input: Vec<ResponseItem>,
    input_tokens: Vec<u64>,
) {
    let attribution = exact_attribution(&input, &input_tokens);
    telemetry
        .record_successful_provider_request(
            session_id,
            turn_id,
            SkillReadProviderRequest { input, attribution },
        )
        .expect("exact provider attribution should be accepted");
}

fn exact_attribution(input: &[ResponseItem], input_tokens: &[u64]) -> ProviderRequestAttribution {
    let authoritative_input_tokens = input_tokens.iter().sum();
    let model = "test-model".to_string();
    let response_id = "test-response".to_string();
    let request_fingerprint = "test-request-fingerprint".to_string();
    ProviderRequestAttribution::ExactTerminalReceipt(ProviderTerminalAttribution {
        receipt: ProviderTerminalAttributionReceipt {
            session_id: "session".to_string(),
            turn_id: "turn".to_string(),
            response_id: response_id.clone(),
            request_fingerprint: request_fingerprint.clone(),
            requested_model: model.clone(),
            resolved_model: model.clone(),
            requested_model_fingerprint: provider_model_fingerprint(&model),
            resolved_model_fingerprint: provider_model_fingerprint(&model),
            provider_transformation_version: "test-provider-v1".to_string(),
            tokenizer_accounting_version: "test-tokenizer-v1".to_string(),
            input_items: input
                .iter()
                .zip(input_tokens.iter().copied())
                .map(|(item, input_tokens)| ProviderTerminalInputItem {
                    item_id: item.id().expect("test input item ID").to_string(),
                    input_tokens,
                })
                .collect(),
            authoritative_input_tokens,
        },
        terminal: ProviderTerminalResponse {
            session_id: Some("session".to_string()),
            turn_id: Some("turn".to_string()),
            response_id,
            requested_model: model.clone(),
            resolved_model: Some(model),
            authoritative_input_tokens: Some(authoritative_input_tokens),
            successful: true,
        },
        final_request_fingerprint: request_fingerprint,
    })
}

#[test]
fn zero_skills_produce_zero_metrics() {
    let telemetry = SkillReadTelemetry::default();

    assert_eq!(telemetry.outcome(), SkillReadTelemetryOutcome::CompleteZero);
    assert_eq!(telemetry.metrics(), SkillReadMetrics::default());
    assert_eq!(
        telemetry.host_observation_metrics(),
        Some(SkillReadMetrics::default())
    );
}

#[test]
fn one_complete_skill_uses_exact_captured_item_tokens() {
    let telemetry = SkillReadTelemetry::default();
    let item = skill_item("one", "complete skill body");
    telemetry.register(provenance(
        "session",
        "turn",
        "/skills/one/SKILL.md",
        "sha1:one",
        item.clone(),
    ));

    record_exact(&telemetry, "session", "turn", vec![item], vec![37]);

    assert_eq!(
        telemetry.metrics(),
        SkillReadMetrics {
            skill_read_calls: 1,
            skill_read_input_tokens: 37,
        }
    );
    assert_eq!(
        telemetry.outcome(),
        SkillReadTelemetryOutcome::CompleteExact
    );
    assert_eq!(
        telemetry.host_observation_metrics(),
        Some(SkillReadMetrics {
            skill_read_calls: 1,
            skill_read_input_tokens: 37,
        })
    );
}

#[test]
fn duplicate_injection_and_request_retry_count_once_by_identity() {
    let telemetry = SkillReadTelemetry::default();
    let first = skill_item("first", "same skill");
    let second = skill_item("second", "same skill");
    telemetry.register(provenance(
        "session",
        "turn",
        "/skills/one/SKILL.md",
        "sha1:same",
        first.clone(),
    ));
    telemetry.register(provenance(
        "session",
        "turn",
        "/skills/one/SKILL.md",
        "sha1:same",
        second.clone(),
    ));

    record_exact(
        &telemetry,
        "session",
        "turn",
        vec![first.clone(), second],
        vec![11, 13],
    );
    record_exact(&telemetry, "session", "turn", vec![first], vec![11]);

    assert_eq!(telemetry.metrics().skill_read_calls, 1);
}

#[test]
fn mismatched_session_or_turn_does_not_count_or_poison_candidate() {
    let telemetry = SkillReadTelemetry::default();
    let item = skill_item("identity", "body");
    telemetry.register(provenance(
        "session",
        "turn",
        "/skills/identity/SKILL.md",
        "sha1:identity",
        item.clone(),
    ));

    record_exact(
        &telemetry,
        "other-session",
        "turn",
        vec![item.clone()],
        vec![7],
    );
    assert_eq!(telemetry.outcome(), SkillReadTelemetryOutcome::Unavailable);
    assert_eq!(telemetry.host_observation_metrics(), None);
    record_exact(
        &telemetry,
        "session",
        "other-turn",
        vec![item.clone()],
        vec![7],
    );
    assert_eq!(telemetry.outcome(), SkillReadTelemetryOutcome::Unavailable);
    assert_eq!(telemetry.host_observation_metrics(), None);
    assert_eq!(telemetry.metrics(), SkillReadMetrics::default());

    record_exact(&telemetry, "session", "turn", vec![item], vec![7]);
    assert_eq!(telemetry.metrics().skill_read_calls, 1);
    assert_eq!(
        telemetry.outcome(),
        SkillReadTelemetryOutcome::CompleteExact
    );
}

#[test]
fn failed_truncated_filtered_and_extension_suppressed_items_count_zero() {
    let telemetry = SkillReadTelemetry::default();
    let complete = skill_item("complete", "complete body");
    let filtered = skill_item("filtered", "filtered body");
    let failed = skill_item("failed", "failed body");
    let extension_suppressed = skill_item("suppressed", "suppressed body");
    let mut truncated = complete.clone();
    if let ResponseItem::Message { content, .. } = &mut truncated {
        content[0] = ContentItem::InputText {
            text: "truncated body".to_string(),
        };
    }
    telemetry.register(provenance(
        "session",
        "turn",
        "/skills/complete/SKILL.md",
        "sha1:complete",
        complete,
    ));
    telemetry.register(provenance(
        "session",
        "turn",
        "/skills/filtered/SKILL.md",
        "sha1:filtered",
        filtered,
    ));

    // A truncated read does not match the complete typed candidate.
    record_exact(&telemetry, "session", "turn", vec![truncated], vec![23]);
    // A filtered candidate is absent from the final provider input.
    record_exact(&telemetry, "session", "turn", Vec::new(), Vec::new());
    // Failed and extension-suppressed reads never register typed candidates.
    record_exact(
        &telemetry,
        "session",
        "turn",
        vec![failed, extension_suppressed],
        vec![29, 31],
    );
    assert_eq!(telemetry.metrics(), SkillReadMetrics::default());
    assert_eq!(telemetry.outcome(), SkillReadTelemetryOutcome::CompleteZero);
    assert_eq!(
        telemetry.host_observation_metrics(),
        Some(SkillReadMetrics::default())
    );
}

#[test]
fn normal_and_substitution_turns_are_independently_attributed() {
    let normal_telemetry = SkillReadTelemetry::default();
    let substitution_telemetry = SkillReadTelemetry::default();
    let normal = skill_item("normal", "body");
    let substitution = skill_item("substitution", "body");
    normal_telemetry.register(provenance(
        "normal-session",
        "normal-turn",
        "/skills/shared/SKILL.md",
        "sha1:shared",
        normal.clone(),
    ));
    substitution_telemetry.register(provenance(
        "substitution-session",
        "substitution-turn",
        "/skills/shared/SKILL.md",
        "sha1:shared",
        substitution.clone(),
    ));

    record_exact(
        &normal_telemetry,
        "normal-session",
        "normal-turn",
        vec![normal],
        vec![17],
    );
    record_exact(
        &substitution_telemetry,
        "substitution-session",
        "substitution-turn",
        vec![substitution],
        vec![19],
    );

    assert_eq!(normal_telemetry.metrics().skill_read_calls, 1);
    assert_eq!(substitution_telemetry.metrics().skill_read_calls, 1);
}

#[test]
fn provider_without_exact_attribution_fails_closed() {
    let telemetry = SkillReadTelemetry::default();
    let item = skill_item("unsupported", "provider-owned tokenizer unavailable");
    telemetry.register(provenance(
        "session",
        "turn",
        "/skills/unsupported/SKILL.md",
        "sha1:unsupported",
        item.clone(),
    ));

    let error = telemetry
        .record_successful_provider_request(
            "session",
            "turn",
            SkillReadProviderRequest {
                input: vec![item],
                attribution: ProviderRequestAttribution::Unavailable(
                    ProviderRequestAttributionError::ProviderDoesNotExposeExactPerItemTokens {
                        provider: "test-provider".to_string(),
                    },
                ),
            },
        )
        .expect_err("unsupported attribution must fail closed");

    assert_eq!(
        error,
        ProviderRequestAttributionError::ProviderDoesNotExposeExactPerItemTokens {
            provider: "test-provider".to_string(),
        }
    );
    assert_eq!(telemetry.metrics(), SkillReadMetrics::default());
    assert_eq!(telemetry.outcome(), SkillReadTelemetryOutcome::Unavailable);
    assert_eq!(telemetry.host_observation_metrics(), None);
}

#[test]
fn wrong_item_count_with_a_surviving_skill_withholds_observation() {
    let telemetry = SkillReadTelemetry::default();
    let item = skill_item("wrong-count", "body");
    telemetry.register(provenance(
        "session",
        "turn",
        "/skills/wrong-count/SKILL.md",
        "sha1:wrong-count",
        item.clone(),
    ));

    let error = telemetry
        .record_successful_provider_request(
            "session",
            "turn",
            SkillReadProviderRequest {
                input: vec![item.clone()],
                attribution: exact_attribution(&[item.clone()], &[]),
            },
        )
        .expect_err("wrong item count must fail closed");

    assert_eq!(
        error,
        ProviderRequestAttributionError::MissingReceiptInputItemId {
            item_id: item.id().expect("item ID").to_string(),
        }
    );
    assert_eq!(telemetry.outcome(), SkillReadTelemetryOutcome::Unavailable);
    assert_eq!(telemetry.host_observation_metrics(), None);
}

#[test]
fn attribution_error_with_a_surviving_skill_withholds_observation() {
    let telemetry = SkillReadTelemetry::default();
    let item = skill_item("encoder-error", "body");
    telemetry.register(provenance(
        "session",
        "turn",
        "/skills/encoder-error/SKILL.md",
        "sha1:encoder-error",
        item.clone(),
    ));

    let error = telemetry
        .record_successful_provider_request(
            "session",
            "turn",
            SkillReadProviderRequest {
                input: vec![item],
                attribution: ProviderRequestAttribution::Unavailable(
                    ProviderRequestAttributionError::EncoderRejected {
                        provider: "test-provider".to_string(),
                        reason: "provider error must not be serialized".to_string(),
                    },
                ),
            },
        )
        .expect_err("provider attribution error must fail closed");

    assert_eq!(
        error,
        ProviderRequestAttributionError::EncoderRejected {
            provider: "test-provider".to_string(),
            reason: "provider error must not be serialized".to_string(),
        }
    );
    assert_eq!(telemetry.outcome(), SkillReadTelemetryOutcome::Unavailable);
    assert_eq!(telemetry.host_observation_metrics(), None);
}

#[test]
fn unavailable_attempt_followed_by_exact_final_request_emits_exact_once() {
    let telemetry = SkillReadTelemetry::default();
    let item = skill_item("retry-final", "body");
    telemetry.register(provenance(
        "session",
        "turn",
        "/skills/retry-final/SKILL.md",
        "sha1:retry-final",
        item.clone(),
    ));

    telemetry
        .record_successful_provider_request(
            "session",
            "turn",
            SkillReadProviderRequest {
                input: vec![item.clone()],
                attribution: ProviderRequestAttribution::Unavailable(
                    ProviderRequestAttributionError::ProviderDoesNotExposeExactPerItemTokens {
                        provider: "test-provider".to_string(),
                    },
                ),
            },
        )
        .expect_err("unavailable attempt must fail closed");
    assert_eq!(telemetry.host_observation_metrics(), None);

    record_exact(&telemetry, "session", "turn", vec![item.clone()], vec![41]);
    record_exact(&telemetry, "session", "turn", vec![item], vec![41]);

    assert_eq!(
        telemetry.outcome(),
        SkillReadTelemetryOutcome::CompleteExact
    );
    assert_eq!(
        telemetry.host_observation_metrics(),
        Some(SkillReadMetrics {
            skill_read_calls: 1,
            skill_read_input_tokens: 41,
        })
    );
}

#[test]
fn missing_successful_provider_attribution_withholds_observation() {
    let telemetry = SkillReadTelemetry::default();
    let item = skill_item("missing", "body");
    telemetry.register(provenance(
        "session",
        "turn",
        "/skills/missing/SKILL.md",
        "sha1:missing",
        item,
    ));

    telemetry.record_missing_successful_provider_request();

    assert_eq!(telemetry.outcome(), SkillReadTelemetryOutcome::Unavailable);
    assert_eq!(telemetry.host_observation_metrics(), None);
}
