use crate::JsonSchema;
use crate::TS;
use serde::Deserialize;
use serde::Serialize;

/// Failure categories exposed by asynchronous thread setup.
///
/// These values intentionally avoid carrying provider errors, paths, prompts,
/// tool data, or other setup details across the client boundary.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase", export_to = "v2/")]
pub enum ThreadSetupFailureCode {
    SetupFailed,
    Cancelled,
    TimedOut,
}

/// A sanitized asynchronous thread setup status.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(tag = "status", rename_all = "camelCase")]
#[ts(tag = "status", rename_all = "camelCase", export_to = "v2/")]
pub enum ThreadSetupStatus {
    /// Setup has started but has not reached a terminal state.
    Pending,
    /// Setup completed and resolved the canonical thread and host identities.
    Ready {
        #[serde(rename = "threadId")]
        #[ts(rename = "threadId")]
        thread_id: String,
        #[serde(rename = "hostId")]
        #[ts(rename = "hostId")]
        host_id: String,
    },
    /// Setup stopped without exposing its internal failure details.
    Failed { code: ThreadSetupFailureCode },
}

/// Parameters for reading one connection-scoped asynchronous thread setup handle.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ThreadSetupStatusReadParams {
    pub client_thread_id: String,
}
