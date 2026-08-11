use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::Mutex;
use std::sync::PoisonError;

use codex_api::ProviderRequestAttribution;
use codex_api::ProviderRequestAttributionError;
use codex_core_skills::injection::SkillInjection;
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
        skill: &SkillInjection,
        response_item: ResponseItem,
    ) -> Self {
        let response_item_id = response_item
            .id()
            .cloned()
            .expect("skill telemetry items must have a response item ID");
        Self {
            session_id: session_id.to_string(),
            turn_id: turn_id.to_string(),
            canonical_path: skill.path.clone(),
            content_digest: content_digest(&skill.contents),
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

impl SkillReadTelemetry {
    pub(crate) fn register(&self, provenance: SkillReadProvenance) {
        let mut state = self.state();
        state
            .candidates
            .insert(provenance.response_item_id.clone(), provenance);
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
        state.metrics = SkillReadMetrics::default();
        state.counted.clear();
        state.successful_request_recorded = true;

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

        let input_tokens = match attribution {
            ProviderRequestAttribution::ExactInputItemTokens(input_tokens)
                if input_tokens.len() == input.len() =>
            {
                input_tokens
            }
            ProviderRequestAttribution::ExactInputItemTokens(input_tokens) => {
                state.outcome = if has_surviving_candidate {
                    SkillReadTelemetryOutcome::Unavailable
                } else {
                    SkillReadTelemetryOutcome::CompleteZero
                };
                return Err(ProviderRequestAttributionError::WrongInputItemCount {
                    expected: input.len(),
                    actual: input_tokens.len(),
                });
            }
            ProviderRequestAttribution::Unavailable(error) => {
                state.outcome = if has_surviving_candidate {
                    SkillReadTelemetryOutcome::Unavailable
                } else {
                    SkillReadTelemetryOutcome::CompleteZero
                };
                return Err(error);
            }
        };

        if has_boundary_mismatch {
            state.outcome = SkillReadTelemetryOutcome::Unavailable;
            return Ok(());
        }
        if !has_surviving_candidate {
            state.outcome = SkillReadTelemetryOutcome::CompleteZero;
            return Ok(());
        }

        for (item, input_tokens) in input.iter().zip(input_tokens) {
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
            state.metrics.skill_read_input_tokens += input_tokens;
        }
        state.outcome = SkillReadTelemetryOutcome::CompleteExact;
        Ok(())
    }

    /// Marks a successful provider request whose authoritative attribution record was missing.
    pub(crate) fn record_missing_successful_provider_request(&self) {
        let mut state = self.state();
        state.metrics = SkillReadMetrics::default();
        state.counted.clear();
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

pub(crate) fn content_digest(contents: &str) -> String {
    let mut hasher = Sha1::new();
    hasher.update(contents.as_bytes());
    format!("sha1:{:x}", hasher.finalize())
}

#[cfg(test)]
#[path = "skill_telemetry_tests.rs"]
mod tests;
