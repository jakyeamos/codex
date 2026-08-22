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
    let attribution = exact_attribution(&input, &input_tokens, session_id, turn_id);
    telemetry
        .record_successful_provider_request(
            session_id,
            turn_id,
            SkillReadProviderRequest { input, attribution },
        )
        .expect("exact provider attribution should be accepted");
}

fn exact_attribution(
    input: &[ResponseItem],
    input_tokens: &[u64],
    session_id: &str,
    turn_id: &str,
) -> ProviderRequestAttribution {
    let authoritative_input_tokens = input_tokens.iter().sum();
    let requested_model = "test-model".to_string();
    let receipt = ProviderTerminalAttributionReceipt {
        session_id: session_id.to_string(),
        turn_id: turn_id.to_string(),
        response_id: "response-1".to_string(),
        request_fingerprint: "sha256:request-v1:test".to_string(),
        requested_model: requested_model.clone(),
        resolved_model: requested_model.clone(),
        requested_model_fingerprint: provider_model_fingerprint(&requested_model),
        resolved_model_fingerprint: provider_model_fingerprint(&requested_model),
        provider_transformation_version: "provider-transform-v1".to_string(),
        tokenizer_accounting_version: "tokenizer-accounting-v1".to_string(),
        input_items: input
            .iter()
            .zip(input_tokens)
            .map(|(item, input_tokens)| ProviderTerminalInputItem {
                item_id: item
                    .id()
                    .expect("exact test input should have an item id")
                    .to_string(),
                input_tokens: *input_tokens,
            })
            .collect(),
        authoritative_input_tokens,
    };
    ProviderRequestAttribution::ExactTerminalReceipt(ProviderTerminalAttribution {
        receipt,
        terminal: ProviderTerminalResponse {
            session_id: Some(session_id.to_string()),
            turn_id: Some(turn_id.to_string()),
            response_id: "response-1".to_string(),
            requested_model,
            resolved_model: Some("test-model".to_string()),
            authoritative_input_tokens: Some(authoritative_input_tokens),
            successful: true,
        },
        final_request_fingerprint: "sha256:request-v1:test".to_string(),
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
    let mut retry = match exact_attribution(&[first.clone()], &[11], "session", "turn") {
        ProviderRequestAttribution::ExactTerminalReceipt(attribution) => attribution,
        _ => panic!("test helper must create an exact terminal attribution"),
    };
    retry.receipt.response_id = "response-retry".to_string();
    retry.receipt.request_fingerprint = "sha256:request-v1:retry".to_string();
    retry.terminal.response_id = "response-retry".to_string();
    retry.final_request_fingerprint = "sha256:request-v1:retry".to_string();
    telemetry
        .record_successful_provider_request(
            "session",
            "turn",
            SkillReadProviderRequest {
                input: vec![first],
                attribution: ProviderRequestAttribution::ExactTerminalReceipt(retry),
            },
        )
        .expect("a successful retry should remain exact");

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
fn failed_truncated_filtered_and_extension_suppressed_items_withhold_observation() {
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
    assert_eq!(telemetry.outcome(), SkillReadTelemetryOutcome::Unavailable);
    assert_eq!(telemetry.host_observation_metrics(), None);
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
fn aggregate_only_provider_attribution_fails_closed() {
    let telemetry = SkillReadTelemetry::default();
    let item = skill_item("aggregate-only", "provider aggregate only");
    telemetry.register(provenance(
        "session",
        "turn",
        "/skills/aggregate-only/SKILL.md",
        "sha1:aggregate-only",
        item.clone(),
    ));

    let error = telemetry
        .record_successful_provider_request(
            "session",
            "turn",
            SkillReadProviderRequest {
                input: vec![item],
                attribution: ProviderRequestAttribution::Unavailable(
                    ProviderRequestAttributionError::ProviderReportsAggregateOnly {
                        provider: "test-provider".to_string(),
                        input_tokens: 23,
                    },
                ),
            },
        )
        .expect_err("aggregate-only attribution must fail closed");

    assert_eq!(
        error,
        ProviderRequestAttributionError::ProviderReportsAggregateOnly {
            provider: "test-provider".to_string(),
            input_tokens: 23,
        }
    );
    assert_eq!(telemetry.metrics(), SkillReadMetrics::default());
    assert_eq!(telemetry.outcome(), SkillReadTelemetryOutcome::Unavailable);
    assert_eq!(telemetry.host_observation_metrics(), None);
}

#[test]
fn missing_receipt_item_id_with_a_surviving_skill_withholds_observation() {
    let telemetry = SkillReadTelemetry::default();
    let item = skill_item("wrong-count", "body");
    telemetry.register(provenance(
        "session",
        "turn",
        "/skills/wrong-count/SKILL.md",
        "sha1:wrong-count",
        item.clone(),
    ));

    let mut attribution = match exact_attribution(&[item.clone()], &[7], "session", "turn") {
        ProviderRequestAttribution::ExactTerminalReceipt(attribution) => attribution,
        _ => panic!("test helper must create an exact terminal attribution"),
    };
    attribution.receipt.input_items.clear();
    let error = telemetry
        .record_successful_provider_request(
            "session",
            "turn",
            SkillReadProviderRequest {
                input: vec![item],
                attribution: ProviderRequestAttribution::ExactTerminalReceipt(attribution),
            },
        )
        .expect_err("missing receipt item id must fail closed");

    assert_eq!(
        error,
        ProviderRequestAttributionError::MissingReceiptInputItemId {
            item_id: "msg_wrong-count".to_string(),
        }
    );
    assert_eq!(telemetry.outcome(), SkillReadTelemetryOutcome::Unavailable);
    assert_eq!(telemetry.host_observation_metrics(), None);
}

fn assert_receipt_rejected<F>(suffix: &str, mutate: F)
where
    F: FnOnce(&mut ProviderTerminalAttribution),
{
    let telemetry = SkillReadTelemetry::default();
    let item = skill_item(suffix, "body");
    telemetry.register(provenance(
        "session",
        "turn",
        "/skills/rejected/SKILL.md",
        "sha1:rejected",
        item.clone(),
    ));
    let mut attribution = match exact_attribution(&[item.clone()], &[7], "session", "turn") {
        ProviderRequestAttribution::ExactTerminalReceipt(attribution) => attribution,
        _ => panic!("test helper must create an exact terminal attribution"),
    };
    mutate(&mut attribution);

    assert!(
        telemetry
            .record_successful_provider_request(
                "session",
                "turn",
                SkillReadProviderRequest {
                    input: vec![item],
                    attribution: ProviderRequestAttribution::ExactTerminalReceipt(attribution),
                },
            )
            .is_err()
    );
    assert_eq!(telemetry.outcome(), SkillReadTelemetryOutcome::Unavailable);
    assert_eq!(telemetry.host_observation_metrics(), None);
}

#[test]
fn duplicate_and_extra_receipt_item_ids_withhold_observation() {
    assert_receipt_rejected("duplicate-receipt-id", |attribution| {
        let item = attribution.receipt.input_items[0].clone();
        attribution.receipt.input_items.push(item);
    });
    assert_receipt_rejected("extra-receipt-id", |attribution| {
        attribution
            .receipt
            .input_items
            .push(ProviderTerminalInputItem {
                item_id: "extra-item".to_string(),
                input_tokens: 0,
            });
    });
}

#[test]
fn request_model_session_and_turn_mismatches_withhold_observation() {
    assert_receipt_rejected("request-fingerprint-mismatch", |attribution| {
        attribution.receipt.request_fingerprint = "sha256:request-v1:other".to_string();
    });
    assert_receipt_rejected("model-mismatch", |attribution| {
        attribution.receipt.requested_model = "other-model".to_string();
    });
    assert_receipt_rejected("session-mismatch", |attribution| {
        attribution.receipt.session_id = "other-session".to_string();
    });
    assert_receipt_rejected("turn-mismatch", |attribution| {
        attribution.receipt.turn_id = "other-turn".to_string();
    });
}

#[test]
fn missing_receipt_versions_and_aggregate_mismatch_withhold_observation() {
    assert_receipt_rejected("missing-transformation-version", |attribution| {
        attribution.receipt.provider_transformation_version.clear();
    });
    assert_receipt_rejected("missing-tokenizer-version", |attribution| {
        attribution.receipt.tokenizer_accounting_version.clear();
    });
    assert_receipt_rejected("aggregate-mismatch", |attribution| {
        attribution.receipt.input_items[0].input_tokens = 6;
    });
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
