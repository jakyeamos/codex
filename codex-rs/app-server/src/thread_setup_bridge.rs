use crate::thread_setup_status::ThreadSetupStatusError;
use crate::thread_setup_status::ThreadSetupStatusHandle;
use codex_app_server_protocol::ThreadSetupFailureCode;
use codex_app_server_protocol::ThreadSetupStatus;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

/// Host-facing adapter for asynchronous thread setup.
///
/// Desktop or other embedders should depend on this adapter instead of reaching
/// into the registry implementation. The adapter keeps the lifecycle explicit:
/// register a pending client handle, publish either a ready identity pair or a
/// sanitized failure code, and read the same connection-scoped state exposed by
/// `thread/setupStatus/read`.
#[derive(Clone)]
pub struct ThreadSetupBridge {
    handle: ThreadSetupStatusHandle,
}

impl ThreadSetupBridge {
    /// Registers a client-owned setup handle in the pending state.
    pub fn register_pending(
        &self,
        client_thread_id: impl Into<String>,
    ) -> Result<(), ThreadSetupStatusError> {
        self.handle.register(client_thread_id)
    }

    /// Publishes the canonical thread and host identities for a successful setup.
    pub fn mark_ready(
        &self,
        client_thread_id: &str,
        thread_id: impl Into<String>,
        host_id: impl Into<String>,
    ) -> Result<(), ThreadSetupStatusError> {
        self.handle
            .complete_ready(client_thread_id, thread_id, host_id)
    }

    /// Publishes a sanitized terminal failure category.
    pub fn mark_failed(
        &self,
        client_thread_id: &str,
        code: ThreadSetupFailureCode,
    ) -> Result<(), ThreadSetupStatusError> {
        self.handle.complete_failed(client_thread_id, code)
    }

    /// Reads the status that the app-server exposes through
    /// `thread/setupStatus/read`.
    pub fn read(
        &self,
        client_thread_id: &str,
    ) -> Result<ThreadSetupStatus, ThreadSetupStatusError> {
        self.handle.get_thread_setup_status(client_thread_id)
    }

    /// Waits for a terminal status using the same bounded, connection-scoped
    /// state as [`Self::read`].
    pub async fn wait_until_terminal(
        &self,
        client_thread_id: &str,
        timeout_duration: Duration,
        cancellation: &CancellationToken,
    ) -> Result<ThreadSetupStatus, ThreadSetupStatusError> {
        self.handle
            .wait_until_terminal(client_thread_id, timeout_duration, cancellation)
            .await
    }

    /// Removes expired handles owned by this bridge's connection.
    pub fn cleanup_expired(&self) -> usize {
        self.handle.cleanup_expired()
    }
}

impl From<ThreadSetupStatusHandle> for ThreadSetupBridge {
    fn from(handle: ThreadSetupStatusHandle) -> Self {
        Self { handle }
    }
}
