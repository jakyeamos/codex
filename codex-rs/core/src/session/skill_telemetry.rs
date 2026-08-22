use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::Mutex;
use std::sync::PoisonError;

use codex_api::ProviderRequestAttribution;
use codex_api::ProviderRequestAttributionError;
use codex_api::ProviderTerminalAttribution;
use codex_api::provider_model_fingerprint;
use codex_protocol::ResponseItemId;
use codex_protocol::models::ResponseItem;
use sha1::Digest;
use sha1::Sha1;

pub(crate) const CODEX_TMCP_HOST_OBSERVATION_SCHEMA: &str = "codex-tmcp-host-observation-v0.1";

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct SkillReadMetrics {
    pub(crate) skill_read_calls: u64,
    pub(crate) skill_read_input_tokens: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum SkillReadTelemetryOutcome {
    #[default]
    CompleteZero,
    CompleteExact,
    Unavailable,
}

#[derive(Debug, Clone)]
pub(crate) struct SkillReadProvenance {
    session_id: String,
    turn_id: String,
    canonical_path: String,
    content_digest: String,
    response_item_id: ResponseItemId,
    expected_item: ResponseItem,
}

#[derive(Debug)]
pub(crate) struct SkillReadProviderRequest {
    pub(crate) input: Vec<ResponseItem>,
    pub(crate) attribution: ProviderRequestAttribution,
}

impl SkillReadProvenance {
    pub(crate) fn for_skill(
        session_id: &str,
        turn_id: &str,
        canonical_path: &str,
        contents: &str,
        response_item: ResponseItem,
    ) -> Self {
        let response_item_id = response_item
            .id()
            .cloned()
            .expect("skill telemetry items must have a response item ID");
        Self {
            session_id: session_id.to_string(),
            turn_id: turn_id.to_string(),
            canonical_path: canonical_path.to_string(),
            content_digest: content_digest(contents),
            response_item_id,
            expected_item: response_item,
        }
    }

    #[cfg(test)]
    fn new_for_test(
        session_id: &str,
        turn_id: &str,
        canonical_path: &str,
        content_digest: &str,
        response_item: ResponseItem,
    ) -> Self {
        let response_item_id = response_item
            .id()
            .cloned()
            .expect("test telemetry items must have a response item ID");
        Self {
            session_id: session_id.to_string(),
            turn_id: turn_id.to_string(),
            canonical_path: canonical_path.to_string(),
            content_digest: content_digest.to_string(),
            response_item_id,
            expected_item: response_item,
        }
    }
}

#[derive(Debug, Default)]
pub(crate) struct SkillReadTelemetry {
    state: Mutex<SkillReadTelemetryState>,
}

#[derive(Debug, Default)]
struct SkillReadTelemetryState {
    candidates: HashMap<ResponseItemId, SkillReadProvenance>,
    counted: HashSet<SkillReadKey>,
    accepted_receipt_key: Option<SkillReadReceiptKey>,
    metrics: SkillReadMetrics,
    outcome: SkillReadTelemetryOutcome,
    successful_request_recorded: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct SkillReadKey {
    session_id: String,
    turn_id: String,
    canonical_path: String,
    content_digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SkillReadReceiptKey {
    session_id: String,
    turn_id: String,
    response_id: String,
    request_fingerprint: String,
}

impl SkillReadTelemetry {
    pub(crate) fn register(&self, provenance: SkillReadProvenance) {
        let mut state = self.state();
        state
            .candidates
            .insert(provenance.response_item_id.clone(), provenance);
        state.accepted_receipt_key = None;
        state.successful_request_recorded = false;
    }

    /// Records only typed, complete skill items that survive into a successful final provider
    /// request with exact per-item attribution.
    ///
    /// The request is captured after prompt transformations and provider request encoding. An
    /// unavailable provider attribution result is returned as an error and marks the turn
    /// unavailable; no local token estimate is substituted.
    pub(crate) fn record_successful_provider_request(
        &self,
        session_id: &str,
        turn_id: &str,
        request: SkillReadProviderRequest,
    ) -> Result<(), ProviderRequestAttributionError> {
        let SkillReadProviderRequest { input, attribution } = request;
        let mut state = self.state();
        state.successful_request_recorded = true;
        let has_registered_candidate = !state.candidates.is_empty();

        let has_surviving_candidate = input.iter().any(|item| {
            item.id()
                .and_then(|response_item_id| state.candidates.get(response_item_id))
                .is_some_and(|candidate| candidate.expected_item == *item)
        });
        let has_boundary_mismatch = input.iter().any(|item| {
            item.id()
                .and_then(|response_item_id| state.candidates.get(response_item_id))
                .is_some_and(|candidate| {
                    candidate.expected_item == *item
                        && (candidate.session_id != session_id || candidate.turn_id != turn_id)
                })
        });

        let attribution = match attribution.resolve() {
            Ok(attribution) => attribution,
            Err(error) => {
                state.metrics = SkillReadMetrics::default();
                state.counted.clear();
                state.accepted_receipt_key = None;
                state.outcome = if has_registered_candidate {
                    SkillReadTelemetryOutcome::Unavailable
                } else {
                    SkillReadTelemetryOutcome::CompleteZero
                };
                return Err(error);
            }
        };

        if has_boundary_mismatch {
            state.metrics = SkillReadMetrics::default();
            state.counted.clear();
            state.accepted_receipt_key = None;
            state.outcome = SkillReadTelemetryOutcome::Unavailable;
            return Ok(());
        }
        if !has_surviving_candidate {
            state.metrics = SkillReadMetrics::default();
            state.counted.clear();
            state.accepted_receipt_key = None;
            state.outcome = if has_registered_candidate {
                SkillReadTelemetryOutcome::Unavailable
            } else {
                SkillReadTelemetryOutcome::CompleteZero
            };
            return Ok(());
        }

        let receipt_key =
            match validate_terminal_attribution(&input, session_id, turn_id, &attribution) {
                Ok(receipt_key) => receipt_key,
                Err(error) => {
                    state.metrics = SkillReadMetrics::default();
                    state.counted.clear();
                    state.accepted_receipt_key = None;
                    state.outcome = SkillReadTelemetryOutcome::Unavailable;
                    return Err(error);
                }
            };
        if state.accepted_receipt_key.as_ref() == Some(&receipt_key) {
            state.outcome = SkillReadTelemetryOutcome::CompleteExact;
            return Ok(());
        }

        let input_tokens_by_id: HashMap<_, _> = attribution
            .receipt
            .input_items
            .iter()
            .map(|item| (item.item_id.as_str(), item.input_tokens))
            .collect();
        state.accepted_receipt_key = Some(receipt_key);

        for item in &input {
            let Some(response_item_id) = item.id() else {
                continue;
            };
            let Some(candidate) = state.candidates.get(response_item_id) else {
                continue;
            };
            if candidate.session_id != session_id
                || candidate.turn_id != turn_id
                || candidate.expected_item != *item
            {
                continue;
            }
            let key = SkillReadKey {
                session_id: candidate.session_id.clone(),
                turn_id: candidate.turn_id.clone(),
                canonical_path: candidate.canonical_path.clone(),
                content_digest: candidate.content_digest.clone(),
            };
            if !state.counted.insert(key) {
                continue;
            }
            state.metrics.skill_read_calls += 1;
            let Some(input_tokens) = input_tokens_by_id.get(response_item_id.as_str()) else {
                state.metrics = SkillReadMetrics::default();
                state.counted.clear();
                state.accepted_receipt_key = None;
                state.outcome = SkillReadTelemetryOutcome::Unavailable;
                return Err(ProviderRequestAttributionError::MissingReceiptInputItemId {
                    item_id: response_item_id.to_string(),
                });
            };
            let Some(input_tokens) = state
                .metrics
                .skill_read_input_tokens
                .checked_add(*input_tokens)
            else {
                state.metrics = SkillReadMetrics::default();
                state.counted.clear();
                state.accepted_receipt_key = None;
                state.outcome = SkillReadTelemetryOutcome::Unavailable;
                return Err(ProviderRequestAttributionError::InputTokenSumOverflow);
            };
            state.metrics.skill_read_input_tokens = input_tokens;
        }
        state.outcome = SkillReadTelemetryOutcome::CompleteExact;
        Ok(())
    }

    /// Marks a successful provider request whose authoritative attribution record was missing.
    pub(crate) fn record_missing_successful_provider_request(&self) {
        let mut state = self.state();
        state.metrics = SkillReadMetrics::default();
        state.counted.clear();
        state.accepted_receipt_key = None;
        state.successful_request_recorded = true;
        state.outcome = if state.candidates.is_empty() {
            SkillReadTelemetryOutcome::CompleteZero
        } else {
            SkillReadTelemetryOutcome::Unavailable
        };
    }

    #[cfg(test)]
    pub(crate) fn outcome(&self) -> SkillReadTelemetryOutcome {
        self.state().outcome
    }

    pub(crate) fn host_observation_metrics(&self) -> Option<SkillReadMetrics> {
        let state = self.state();
        match state.outcome {
            SkillReadTelemetryOutcome::CompleteZero
                if state.candidates.is_empty() || state.successful_request_recorded =>
            {
                Some(state.metrics.clone())
            }
            SkillReadTelemetryOutcome::CompleteExact => Some(state.metrics.clone()),
            SkillReadTelemetryOutcome::CompleteZero | SkillReadTelemetryOutcome::Unavailable => {
                None
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn metrics(&self) -> SkillReadMetrics {
        self.state().metrics.clone()
    }

    fn state(&self) -> std::sync::MutexGuard<'_, SkillReadTelemetryState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

fn validate_terminal_attribution(
    input: &[ResponseItem],
    session_id: &str,
    turn_id: &str,
    attribution: &ProviderTerminalAttribution,
) -> Result<SkillReadReceiptKey, ProviderRequestAttributionError> {
    let receipt = &attribution.receipt;
    let terminal = &attribution.terminal;
    if !terminal.successful {
        return Err(ProviderRequestAttributionError::TerminalResponseNotSuccessful);
    }
    if attribution.final_request_fingerprint.is_empty() || receipt.request_fingerprint.is_empty() {
        return Err(ProviderRequestAttributionError::MissingRequestFingerprint);
    }
    if receipt.request_fingerprint != attribution.final_request_fingerprint {
        return Err(ProviderRequestAttributionError::RequestFingerprintMismatch);
    }
    let terminal_session_id = terminal
        .session_id
        .as_deref()
        .filter(|value| !value.is_empty())
        .ok_or(ProviderRequestAttributionError::MissingSessionId)?;
    let terminal_turn_id = terminal
        .turn_id
        .as_deref()
        .filter(|value| !value.is_empty())
        .ok_or(ProviderRequestAttributionError::MissingTurnId)?;
    if terminal_session_id != session_id || receipt.session_id != session_id {
        return Err(ProviderRequestAttributionError::SessionIdMismatch);
    }
    if terminal_turn_id != turn_id || receipt.turn_id != turn_id {
        return Err(ProviderRequestAttributionError::TurnIdMismatch);
    }
    if terminal.response_id.is_empty() {
        return Err(ProviderRequestAttributionError::MissingResponseId);
    }
    if receipt.response_id.is_empty() {
        return Err(ProviderRequestAttributionError::MissingResponseId);
    }
    if receipt.response_id != terminal.response_id {
        return Err(ProviderRequestAttributionError::ResponseIdMismatch);
    }
    if receipt.requested_model.is_empty() {
        return Err(ProviderRequestAttributionError::MissingRequestedModel);
    }
    if receipt.requested_model != terminal.requested_model {
        return Err(ProviderRequestAttributionError::RequestedModelMismatch);
    }
    if receipt.requested_model_fingerprint.is_empty() {
        return Err(ProviderRequestAttributionError::MissingRequestedModelFingerprint);
    }
    if receipt.requested_model_fingerprint != provider_model_fingerprint(&receipt.requested_model) {
        return Err(ProviderRequestAttributionError::RequestedModelFingerprintMismatch);
    }
    if receipt.resolved_model.is_empty() {
        return Err(ProviderRequestAttributionError::MissingResolvedModel);
    }
    if terminal
        .resolved_model
        .as_deref()
        .is_some_and(|resolved_model| resolved_model != receipt.resolved_model)
    {
        return Err(ProviderRequestAttributionError::ResolvedModelMismatch);
    }
    if receipt.resolved_model_fingerprint.is_empty() {
        return Err(ProviderRequestAttributionError::MissingResolvedModelFingerprint);
    }
    if receipt.resolved_model_fingerprint != provider_model_fingerprint(&receipt.resolved_model) {
        return Err(ProviderRequestAttributionError::ResolvedModelFingerprintMismatch);
    }
    if receipt.provider_transformation_version.is_empty() {
        return Err(ProviderRequestAttributionError::MissingProviderTransformationVersion);
    }
    if receipt.tokenizer_accounting_version.is_empty() {
        return Err(ProviderRequestAttributionError::MissingTokenizerAccountingVersion);
    }
    let authoritative_input_tokens = terminal
        .authoritative_input_tokens
        .ok_or(ProviderRequestAttributionError::MissingAggregateInputTokens)?;
    if receipt.authoritative_input_tokens != authoritative_input_tokens {
        return Err(
            ProviderRequestAttributionError::AggregateInputTokensMismatch {
                expected: authoritative_input_tokens,
                actual: receipt.authoritative_input_tokens,
            },
        );
    }

    let mut final_item_ids = HashSet::new();
    for (index, item) in input.iter().enumerate() {
        let Some(item_id) = item.id() else {
            return Err(ProviderRequestAttributionError::MissingFinalInputItemId { index });
        };
        let item_id = item_id.to_string();
        if !final_item_ids.insert(item_id.clone()) {
            return Err(ProviderRequestAttributionError::DuplicateFinalInputItemId { item_id });
        }
    }
    let mut receipt_item_ids = HashSet::new();
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
    Ok(SkillReadReceiptKey {
        session_id: session_id.to_string(),
        turn_id: turn_id.to_string(),
        response_id: receipt.response_id.clone(),
        request_fingerprint: receipt.request_fingerprint.clone(),
    })
}

pub(crate) fn content_digest(contents: &str) -> String {
    let mut hasher = Sha1::new();
    hasher.update(contents.as_bytes());
    format!("sha1:{:x}", hasher.finalize())
}

#[cfg(test)]
#[path = "skill_telemetry_tests.rs"]
mod tests;
