use crate::outgoing_message::ConnectionId;
use codex_app_server_protocol::ThreadSetupFailureCode;
use codex_app_server_protocol::ThreadSetupStatus;
use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::PoisonError;
use std::time::Duration;
use std::time::Instant;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

const DEFAULT_PENDING_RETENTION: Duration = Duration::from_secs(10 * 60);
const DEFAULT_TERMINAL_RETENTION: Duration = Duration::from_secs(5 * 60);
const MAX_CLIENT_THREAD_ID_LENGTH: usize = 128;
const MAX_RESOLVED_ID_LENGTH: usize = 256;

/// Errors returned by the host-side setup handle.
///
/// Request handlers intentionally collapse all lookup and authorization errors
/// to one generic JSON-RPC error so callers cannot use the status API to probe
/// another connection's handles.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ThreadSetupStatusError {
    InvalidHandle,
    UnknownHandle,
    AccessDenied,
    AlreadyCompleted,
    InvalidValue,
    TimedOut,
    Cancelled,
}

impl fmt::Display for ThreadSetupStatusError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::InvalidHandle => "invalid thread setup handle",
            Self::UnknownHandle => "unknown thread setup handle",
            Self::AccessDenied => "thread setup handle access denied",
            Self::AlreadyCompleted => "thread setup already completed",
            Self::InvalidValue => "invalid thread setup value",
            Self::TimedOut => "thread setup wait timed out",
            Self::Cancelled => "thread setup wait cancelled",
        };
        f.write_str(message)
    }
}

impl std::error::Error for ThreadSetupStatusError {}

#[derive(Clone)]
pub(crate) struct ThreadSetupStatusRegistry {
    state: Arc<Mutex<RegistryState>>,
    pending_retention: Duration,
    terminal_retention: Duration,
}

struct RegistryState {
    entries: HashMap<String, Entry>,
}

struct Entry {
    authority: ConnectionId,
    status: ThreadSetupStatus,
    expires_at: Instant,
    status_tx: watch::Sender<ThreadSetupStatus>,
}

/// Connection-scoped host API for asynchronous thread setup state.
///
/// A handle is bound to the app-server connection that created it. Hosts use
/// it to register the opaque client handle before setup begins and to publish
/// only the terminal identities or a sanitized failure code. The same registry
/// is read through `thread/setupStatus/read`.
#[derive(Clone)]
pub struct ThreadSetupStatusHandle {
    registry: ThreadSetupStatusRegistry,
    authority: ConnectionId,
}

impl ThreadSetupStatusRegistry {
    pub(crate) fn new() -> Self {
        Self::new_with_durations(DEFAULT_PENDING_RETENTION, DEFAULT_TERMINAL_RETENTION)
    }

    pub(crate) fn new_with_durations(
        pending_retention: Duration,
        terminal_retention: Duration,
    ) -> Self {
        Self {
            state: Arc::new(Mutex::new(RegistryState {
                entries: HashMap::new(),
            })),
            pending_retention,
            terminal_retention,
        }
    }

    pub(crate) fn handle_for(&self, authority: ConnectionId) -> ThreadSetupStatusHandle {
        ThreadSetupStatusHandle {
            registry: self.clone(),
            authority,
        }
    }

    pub(crate) fn connection_closed(&self, authority: ConnectionId) {
        let mut state = self.lock_state();
        state
            .entries
            .retain(|_, entry| entry.authority != authority);
    }

    fn purge_expired(&self, authority: Option<ConnectionId>) -> usize {
        let now = Instant::now();
        let mut state = self.lock_state();
        purge_expired_locked(&mut state, now, authority)
    }

    fn lock_state(&self) -> std::sync::MutexGuard<'_, RegistryState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl ThreadSetupStatusHandle {
    /// Registers a pending handle. Re-registering the same handle on the same
    /// connection is idempotent; another connection cannot claim it.
    pub fn register(
        &self,
        client_thread_id: impl Into<String>,
    ) -> Result<(), ThreadSetupStatusError> {
        let client_thread_id = client_thread_id.into();
        if !is_valid_client_thread_id(&client_thread_id) {
            return Err(ThreadSetupStatusError::InvalidHandle);
        }

        let now = Instant::now();
        let mut state = self.registry.lock_state();
        purge_expired_locked(&mut state, now, None);
        match state.entries.get(&client_thread_id) {
            Some(entry) if entry.authority != self.authority => {
                Err(ThreadSetupStatusError::AccessDenied)
            }
            Some(_) => Ok(()),
            None => {
                let (status_tx, _status_rx) = watch::channel(ThreadSetupStatus::Pending);
                state.entries.insert(
                    client_thread_id,
                    Entry {
                        authority: self.authority,
                        status: ThreadSetupStatus::Pending,
                        expires_at: now + self.registry.pending_retention,
                        status_tx,
                    },
                );
                Ok(())
            }
        }
    }

    /// Publishes the canonical thread and host identities for a completed setup.
    pub fn complete_ready(
        &self,
        client_thread_id: &str,
        thread_id: impl Into<String>,
        host_id: impl Into<String>,
    ) -> Result<(), ThreadSetupStatusError> {
        let thread_id = thread_id.into();
        let host_id = host_id.into();
        if !is_valid_resolved_id(&thread_id) || !is_valid_resolved_id(&host_id) {
            return Err(ThreadSetupStatusError::InvalidValue);
        }
        self.complete(
            client_thread_id,
            ThreadSetupStatus::Ready { thread_id, host_id },
        )
    }

    /// Publishes a sanitized terminal failure category.
    pub fn complete_failed(
        &self,
        client_thread_id: &str,
        code: ThreadSetupFailureCode,
    ) -> Result<(), ThreadSetupStatusError> {
        self.complete(client_thread_id, ThreadSetupStatus::Failed { code })
    }

    /// Reads a setup status without consulting thread history or list state.
    pub fn get_thread_setup_status(
        &self,
        client_thread_id: &str,
    ) -> Result<ThreadSetupStatus, ThreadSetupStatusError> {
        if !is_valid_client_thread_id(client_thread_id) {
            return Err(ThreadSetupStatusError::InvalidHandle);
        }

        let now = Instant::now();
        let mut state = self.registry.lock_state();
        purge_expired_locked(&mut state, now, None);
        let entry = state
            .entries
            .get(client_thread_id)
            .ok_or(ThreadSetupStatusError::UnknownHandle)?;
        if entry.authority != self.authority {
            return Err(ThreadSetupStatusError::AccessDenied);
        }
        Ok(entry.status.clone())
    }

    /// Waits for a terminal state using the same registry as the non-blocking
    /// status read. The timeout is an overall bound and cancellation is explicit.
    pub async fn wait_until_terminal(
        &self,
        client_thread_id: &str,
        timeout_duration: Duration,
        cancellation: &CancellationToken,
    ) -> Result<ThreadSetupStatus, ThreadSetupStatusError> {
        if cancellation.is_cancelled() {
            return Err(ThreadSetupStatusError::Cancelled);
        }

        let (mut status_rx, expires_at) = self.subscribe(client_thread_id)?;
        if is_terminal(&status_rx.borrow()) {
            return Ok(status_rx.borrow().clone());
        }

        let wait = async {
            loop {
                let status = status_rx.borrow().clone();
                if is_terminal(&status) {
                    return Ok(status);
                }

                let expiry = tokio::time::sleep_until(tokio::time::Instant::from_std(expires_at));
                tokio::pin!(expiry);
                tokio::select! {
                    changed = status_rx.changed() => {
                        if changed.is_err() {
                            return Err(ThreadSetupStatusError::UnknownHandle);
                        }
                    }
                    _ = &mut expiry => {
                        return match self.get_thread_setup_status(client_thread_id) {
                            Ok(status) if is_terminal(&status) => Ok(status),
                            Err(error) => Err(error),
                            Ok(_) => {
                                self.registry.purge_expired(Some(self.authority));
                                Err(ThreadSetupStatusError::UnknownHandle)
                            }
                        };
                    }
                    _ = cancellation.cancelled() => {
                        return match self.get_thread_setup_status(client_thread_id) {
                            Ok(status) if is_terminal(&status) => Ok(status),
                            Err(error) => Err(error),
                            Ok(_) => Err(ThreadSetupStatusError::Cancelled),
                        };
                    }
                }
            }
        };

        match tokio::time::timeout(timeout_duration, wait).await {
            Ok(result) => result,
            Err(_) => match self.get_thread_setup_status(client_thread_id) {
                Ok(status) if is_terminal(&status) => Ok(status),
                Err(error) => Err(error),
                Ok(_) => Err(ThreadSetupStatusError::TimedOut),
            },
        }
    }

    /// Removes expired handles owned by this connection and returns the count.
    pub fn cleanup_expired(&self) -> usize {
        self.registry.purge_expired(Some(self.authority))
    }

    fn complete(
        &self,
        client_thread_id: &str,
        status: ThreadSetupStatus,
    ) -> Result<(), ThreadSetupStatusError> {
        if !is_valid_client_thread_id(client_thread_id) {
            return Err(ThreadSetupStatusError::InvalidHandle);
        }

        let now = Instant::now();
        let mut state = self.registry.lock_state();
        purge_expired_locked(&mut state, now, None);
        let entry = state
            .entries
            .get_mut(client_thread_id)
            .ok_or(ThreadSetupStatusError::UnknownHandle)?;
        if entry.authority != self.authority {
            return Err(ThreadSetupStatusError::AccessDenied);
        }
        match &entry.status {
            ThreadSetupStatus::Pending => {
                entry.status = status.clone();
                entry.expires_at = now + self.registry.terminal_retention;
                entry.status_tx.send_replace(status);
                Ok(())
            }
            current if current == &status => Ok(()),
            _ => Err(ThreadSetupStatusError::AlreadyCompleted),
        }
    }

    fn subscribe(
        &self,
        client_thread_id: &str,
    ) -> Result<(watch::Receiver<ThreadSetupStatus>, Instant), ThreadSetupStatusError> {
        if !is_valid_client_thread_id(client_thread_id) {
            return Err(ThreadSetupStatusError::InvalidHandle);
        }

        let now = Instant::now();
        let mut state = self.registry.lock_state();
        purge_expired_locked(&mut state, now, None);
        let entry = state
            .entries
            .get(client_thread_id)
            .ok_or(ThreadSetupStatusError::UnknownHandle)?;
        if entry.authority != self.authority {
            return Err(ThreadSetupStatusError::AccessDenied);
        }
        Ok((entry.status_tx.subscribe(), entry.expires_at))
    }
}

fn purge_expired_locked(
    state: &mut RegistryState,
    now: Instant,
    authority: Option<ConnectionId>,
) -> usize {
    let mut removed = 0;
    state.entries.retain(|_, entry| {
        let owned_by_authority = authority.is_none_or(|authority| entry.authority == authority);
        let expired = entry.expires_at <= now && owned_by_authority;
        if expired {
            removed += 1;
        }
        !expired
    });
    removed
}

fn is_terminal(status: &ThreadSetupStatus) -> bool {
    !matches!(status, ThreadSetupStatus::Pending)
}

fn is_valid_client_thread_id(value: &str) -> bool {
    is_valid_identifier(value, MAX_CLIENT_THREAD_ID_LENGTH)
}

fn is_valid_resolved_id(value: &str) -> bool {
    is_valid_identifier(value, MAX_RESOLVED_ID_LENGTH)
}

fn is_valid_identifier(value: &str, max_length: usize) -> bool {
    !value.is_empty()
        && value.len() <= max_length
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~' | b':')
        })
}

#[cfg(test)]
#[path = "thread_setup_status_tests.rs"]
mod tests;
