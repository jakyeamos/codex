use super::ThreadSetupStatusError;
use super::ThreadSetupStatusRegistry;
use crate::outgoing_message::ConnectionId;
use crate::thread_setup_bridge::ThreadSetupBridge;
use codex_app_server_protocol::ThreadSetupFailureCode;
use codex_app_server_protocol::ThreadSetupStatus;
use pretty_assertions::assert_eq;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Barrier;
use tokio_util::sync::CancellationToken;

const OWNER: ConnectionId = ConnectionId(1);
const OTHER_OWNER: ConnectionId = ConnectionId(2);

#[test]
fn bridge_exposes_pending_ready_failed_lifecycle_and_preserves_connection_scope() {
    let registry = ThreadSetupStatusRegistry::new();
    let owner = ThreadSetupBridge::from(registry.handle_for(OWNER));
    let other_owner = ThreadSetupBridge::from(registry.handle_for(OTHER_OWNER));

    owner.register_pending("client-thread-bridge").unwrap();
    assert_eq!(
        owner.read("client-thread-bridge"),
        Ok(ThreadSetupStatus::Pending)
    );
    assert_eq!(
        other_owner.read("client-thread-bridge"),
        Err(ThreadSetupStatusError::AccessDenied)
    );

    owner
        .mark_ready("client-thread-bridge", "thread-bridge", "host-bridge")
        .unwrap();
    assert_eq!(
        owner.read("client-thread-bridge"),
        Ok(ThreadSetupStatus::Ready {
            thread_id: "thread-bridge".to_string(),
            host_id: "host-bridge".to_string(),
        })
    );

    owner.register_pending("client-thread-failed").unwrap();
    owner
        .mark_failed("client-thread-failed", ThreadSetupFailureCode::SetupFailed)
        .unwrap();
    assert_eq!(
        owner.read("client-thread-failed"),
        Ok(ThreadSetupStatus::Failed {
            code: ThreadSetupFailureCode::SetupFailed,
        })
    );
}

#[test]
fn immediate_ready_status_is_resolvable() {
    let registry = ThreadSetupStatusRegistry::new();
    let handle = registry.handle_for(OWNER);

    handle.register("client-thread-1").unwrap();
    handle
        .complete_ready("client-thread-1", "thread-1", "host-1")
        .unwrap();

    assert_eq!(
        handle.get_thread_setup_status("client-thread-1"),
        Ok(ThreadSetupStatus::Ready {
            thread_id: "thread-1".to_string(),
            host_id: "host-1".to_string(),
        })
    );
}

#[tokio::test]
async fn pending_then_ready_is_observed_without_polling_thread_list() {
    let registry = ThreadSetupStatusRegistry::new();
    let handle = registry.handle_for(OWNER);
    let cancellation = CancellationToken::new();
    handle.register("client-thread-2").unwrap();

    let waiter_handle = handle.clone();
    let waiter = tokio::spawn(async move {
        waiter_handle
            .wait_until_terminal("client-thread-2", Duration::from_secs(1), &cancellation)
            .await
    });
    tokio::task::yield_now().await;
    handle
        .complete_ready("client-thread-2", "thread-2", "host-2")
        .unwrap();

    assert_eq!(
        waiter.await.unwrap().unwrap(),
        ThreadSetupStatus::Ready {
            thread_id: "thread-2".to_string(),
            host_id: "host-2".to_string(),
        }
    );
}

#[tokio::test]
async fn pending_then_failed_returns_only_sanitized_failure_code() {
    let registry = ThreadSetupStatusRegistry::new();
    let handle = registry.handle_for(OWNER);
    let cancellation = CancellationToken::new();
    handle.register("client-thread-3").unwrap();

    let waiter_handle = handle.clone();
    let waiter = tokio::spawn(async move {
        waiter_handle
            .wait_until_terminal("client-thread-3", Duration::from_secs(1), &cancellation)
            .await
    });
    handle
        .complete_failed("client-thread-3", ThreadSetupFailureCode::SetupFailed)
        .unwrap();

    assert_eq!(
        waiter.await.unwrap().unwrap(),
        ThreadSetupStatus::Failed {
            code: ThreadSetupFailureCode::SetupFailed,
        }
    );
}

#[test]
fn malformed_unknown_and_cross_owner_handles_do_not_resolve() {
    let registry = ThreadSetupStatusRegistry::new();
    let owner = registry.handle_for(OWNER);
    let other_owner = registry.handle_for(OTHER_OWNER);
    owner.register("client-thread-4").unwrap();

    assert_eq!(
        owner.get_thread_setup_status("not a valid handle"),
        Err(ThreadSetupStatusError::InvalidHandle)
    );
    assert_eq!(
        owner.get_thread_setup_status("client-thread-unknown"),
        Err(ThreadSetupStatusError::UnknownHandle)
    );
    assert_eq!(
        other_owner.get_thread_setup_status("client-thread-4"),
        Err(ThreadSetupStatusError::AccessDenied)
    );
    assert_eq!(
        other_owner.complete_failed("client-thread-4", ThreadSetupFailureCode::Cancelled),
        Err(ThreadSetupStatusError::AccessDenied)
    );
}

#[test]
fn repeated_reads_are_idempotent_and_repeated_completion_is_safe() {
    let registry = ThreadSetupStatusRegistry::new();
    let handle = registry.handle_for(OWNER);
    handle.register("client-thread-5").unwrap();
    handle
        .complete_ready("client-thread-5", "thread-5", "host-5")
        .unwrap();

    let first = handle.get_thread_setup_status("client-thread-5").unwrap();
    let second = handle.get_thread_setup_status("client-thread-5").unwrap();
    assert_eq!(first, second);
    assert_eq!(
        handle.complete_ready("client-thread-5", "thread-5", "host-5"),
        Ok(())
    );
    assert_eq!(
        handle.complete_failed("client-thread-5", ThreadSetupFailureCode::Cancelled),
        Err(ThreadSetupStatusError::AlreadyCompleted)
    );
}

#[tokio::test]
async fn wait_has_bounded_timeout_and_explicit_cancellation() {
    let registry = ThreadSetupStatusRegistry::new();
    let handle = registry.handle_for(OWNER);
    handle.register("client-thread-timeout").unwrap();
    let cancellation = CancellationToken::new();
    assert_eq!(
        handle
            .wait_until_terminal(
                "client-thread-timeout",
                Duration::from_millis(20),
                &cancellation,
            )
            .await,
        Err(ThreadSetupStatusError::TimedOut)
    );

    handle.register("client-thread-cancel").unwrap();
    let cancel_token = CancellationToken::new();
    let waiter_handle = handle.clone();
    let waiter_token = cancel_token.clone();
    let waiter = tokio::spawn(async move {
        waiter_handle
            .wait_until_terminal(
                "client-thread-cancel",
                Duration::from_secs(1),
                &waiter_token,
            )
            .await
    });
    tokio::time::sleep(Duration::from_millis(5)).await;
    cancel_token.cancel();
    assert_eq!(
        waiter.await.unwrap(),
        Err(ThreadSetupStatusError::Cancelled)
    );
}

#[tokio::test]
async fn terminal_status_is_retained_then_explicitly_cleaned() {
    let registry = ThreadSetupStatusRegistry::new_with_durations(
        Duration::from_millis(50),
        Duration::from_millis(30),
    );
    let handle = registry.handle_for(OWNER);
    handle.register("client-thread-retained").unwrap();
    handle
        .complete_ready("client-thread-retained", "thread-retained", "host-retained")
        .unwrap();

    tokio::time::sleep(Duration::from_millis(5)).await;
    assert!(
        handle
            .get_thread_setup_status("client-thread-retained")
            .is_ok()
    );
    tokio::time::sleep(Duration::from_millis(40)).await;
    assert_eq!(handle.cleanup_expired(), 1);
    assert_eq!(
        handle.get_thread_setup_status("client-thread-retained"),
        Err(ThreadSetupStatusError::UnknownHandle)
    );
}

#[tokio::test]
async fn concurrent_status_reads_and_completion_are_race_safe() {
    let registry = ThreadSetupStatusRegistry::new();
    let handle = registry.handle_for(OWNER);
    handle.register("client-thread-race").unwrap();
    let barrier = Arc::new(Barrier::new(33));
    let mut readers = Vec::new();
    for _ in 0..32 {
        let reader_handle = handle.clone();
        let reader_barrier = Arc::clone(&barrier);
        readers.push(tokio::spawn(async move {
            reader_barrier.wait().await;
            reader_handle.get_thread_setup_status("client-thread-race")
        }));
    }
    let completion_handle = handle.clone();
    let completion_barrier = Arc::clone(&barrier);
    let completion = tokio::spawn(async move {
        completion_barrier.wait().await;
        completion_handle.complete_ready("client-thread-race", "thread-race", "host-race")
    });

    for reader in readers {
        match reader.await.unwrap().unwrap() {
            ThreadSetupStatus::Pending | ThreadSetupStatus::Ready { .. } => {}
            ThreadSetupStatus::Failed { .. } => panic!("unexpected failed status"),
        }
    }
    completion.await.unwrap().unwrap();
    assert_eq!(
        handle.get_thread_setup_status("client-thread-race"),
        Ok(ThreadSetupStatus::Ready {
            thread_id: "thread-race".to_string(),
            host_id: "host-race".to_string(),
        })
    );
}

#[tokio::test]
async fn closing_an_authority_cleans_all_of_its_handles() {
    let registry = ThreadSetupStatusRegistry::new();
    let handle = registry.handle_for(OWNER);
    handle.register("client-thread-closed").unwrap();
    registry.connection_closed(OWNER);

    assert_eq!(
        handle.get_thread_setup_status("client-thread-closed"),
        Err(ThreadSetupStatusError::UnknownHandle)
    );
}
