//! Optional, metadata-only bridge from unified exec to Mac Control.
//!
//! This module deliberately lives at the per-command boundary. It classifies
//! the parsed command locally, sends only bounded safe context to `macctl`,
//! and never forwards command text, arguments, environment values, output,
//! prompts, or credentials. Mac Control is advisory and fail-open: a missing
//! daemon or bridge failure never changes command execution.

use std::time::Duration;

use serde::Deserialize;
use tokio::process::Command;
use tokio::time::timeout;

use crate::unified_exec::ExecCommandRequest;
use crate::unified_exec::UnifiedExecContext;
use codex_utils_path_uri::PathUri;

const MACCTL_TIMEOUT: Duration = Duration::from_millis(400);
const MACCTL_BIN_ENV: &str = "MACCTL_BIN";
const MAX_LABEL_LEN: usize = 96;

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct MacControlAuthorizationBridge;

/// Owns one prepared notice and closes it when the unified exec call ends.
///
/// A dropped future is treated as cancellation on a best-effort basis. The
/// daemon still expires notices on its own when the process or bridge cannot
/// report completion.
pub(crate) struct MacControlAuthorizationLifecycle {
    bridge: MacControlAuthorizationBridge,
    request_id: Option<String>,
}

impl MacControlAuthorizationLifecycle {
    pub(crate) async fn prepare(
        request: &ExecCommandRequest,
        context: &UnifiedExecContext,
    ) -> Self {
        let bridge = Self::bridge();
        let request_id = bridge.prepare(request, context).await;
        Self { bridge, request_id }
    }

    pub(crate) async fn bind(&self, process_id: i32) {
        if let Some(request_id) = self.request_id.as_deref() {
            self.bridge.bind(request_id, process_id).await;
        }
    }

    pub(crate) async fn resolve_for_result(&mut self, succeeded: bool) {
        let outcome = if succeeded { "completed" } else { "failed" };
        self.resolve(outcome).await;
    }

    async fn resolve(&mut self, outcome: &'static str) {
        let Some(request_id) = self.request_id.take() else {
            return;
        };
        self.bridge.resolve(&request_id, outcome).await;
    }

    fn bridge() -> MacControlAuthorizationBridge {
        MacControlAuthorizationBridge
    }
}

impl Drop for MacControlAuthorizationLifecycle {
    fn drop(&mut self) {
        let Some(request_id) = self.request_id.take() else {
            return;
        };
        let bridge = self.bridge;
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            return;
        };
        handle.spawn(async move {
            bridge.resolve(&request_id, "cancelled").await;
        });
    }
}

impl MacControlAuthorizationBridge {
    async fn prepare(
        &self,
        request: &ExecCommandRequest,
        context: &UnifiedExecContext,
    ) -> Option<String> {
        let classification = classify_sensitive_command(&request.command)?;
        let (project, repository) = project_labels(&request.cwd);
        let requesting_executable = command_executable(&request.command)
            .unwrap_or_else(|| "unknown-executable".to_string());
        let thread_id = context.session.thread_id().to_string();
        let thread_title = context.session.thread_name().await;
        let task_id = safe_label(&context.call_id);

        let mut args = vec![
            "--json".to_string(),
            "control".to_string(),
            "authorization".to_string(),
            "prepare".to_string(),
            "--kind".to_string(),
            classification.kind.to_string(),
            "--project".to_string(),
            project,
            "--action".to_string(),
            classification.action.to_string(),
            "--summary".to_string(),
            classification.summary.to_string(),
            "--thread-id".to_string(),
            thread_id.clone(),
            "--source-reference".to_string(),
            format!("codex://thread/{thread_id}"),
            "--requesting-executable".to_string(),
            requesting_executable,
            "--requesting-helper".to_string(),
            "codex-unified-exec".to_string(),
            "--expires-in".to_string(),
            "30".to_string(),
        ];
        if let Some(repository) = repository {
            args.extend(["--repository".to_string(), repository]);
        }
        if let Some(task_id) = task_id {
            args.extend([
                "--task-id".to_string(),
                task_id,
                "--task-title".to_string(),
                "Codex unified_exec command".to_string(),
            ]);
        }
        if let Some(thread_title) = thread_title.and_then(|title| safe_label(&title)) {
            args.extend(["--thread-title".to_string(), thread_title]);
        }
        if let Some(target_service) = classification.target_service {
            args.extend(["--target-service".to_string(), target_service.to_string()]);
        }

        let output = self.invoke(args).await?;
        if !output.status.success() {
            return None;
        }
        parse_request_id(&output.stdout)
    }

    async fn bind(&self, request_id: &str, process_id: i32) {
        let args = vec![
            "--json".to_string(),
            "control".to_string(),
            "authorization".to_string(),
            "bind".to_string(),
            request_id.to_string(),
            "--process-id".to_string(),
            process_id.to_string(),
        ];
        let _ = self.invoke(args).await;
    }

    async fn resolve(&self, request_id: &str, outcome: &str) {
        let args = [
            "--json",
            "control",
            "authorization",
            "resolve",
            request_id,
            "--outcome",
            outcome,
        ]
        .into_iter()
        .map(String::from)
        .collect();
        let _ = self.invoke(args).await;
    }

    async fn invoke(&self, args: Vec<String>) -> Option<std::process::Output> {
        let executable = std::env::var_os(MACCTL_BIN_ENV).unwrap_or_else(|| "macctl".into());
        let mut command = Command::new(executable);
        command.args(args).env_clear().kill_on_drop(true);
        if let Some(path) = std::env::var_os("PATH") {
            command.env("PATH", path);
        }
        if let Some(home) = std::env::var_os("HOME") {
            command.env("HOME", home);
        }
        command.env("LC_ALL", "C");

        match timeout(MACCTL_TIMEOUT, command.output()).await {
            Ok(Ok(output)) => Some(output),
            Ok(Err(_)) | Err(_) => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SensitiveCommand {
    kind: &'static str,
    action: &'static str,
    summary: &'static str,
    target_service: Option<&'static str>,
}

fn classify_sensitive_command(command: &[String]) -> Option<SensitiveCommand> {
    let executable = command
        .first()?
        .rsplit(['/', '\\'])
        .next()?
        .to_ascii_lowercase();
    let args = &command[1..];
    let first_argument = first_command_argument(args);
    let target_service = known_target_service(command);

    if executable.starts_with("git-credential-")
        || (executable == "git" && first_argument == Some("credential"))
    {
        return Some(SensitiveCommand {
            kind: "credential",
            action: "read credential",
            summary: "Git credential helper access",
            target_service,
        });
    }

    if executable == "git"
        && matches!(
            first_argument,
            Some("clone" | "fetch" | "ls-remote" | "pull" | "push" | "submodule")
        )
    {
        return Some(SensitiveCommand {
            kind: "credential",
            action: "use repository credential",
            summary: "Credential-capable Git operation",
            target_service,
        });
    }

    if executable == "gh" && matches!(first_argument, Some("api" | "auth" | "release" | "repo")) {
        return Some(SensitiveCommand {
            kind: "credential",
            action: "read credential",
            summary: "GitHub CLI credential access",
            target_service: Some("github.com"),
        });
    }

    if executable == "security"
        && args.iter().any(|argument| {
            matches!(
                argument.to_ascii_lowercase().as_str(),
                "find-generic-password"
                    | "find-internet-password"
                    | "get-generic-password"
                    | "add-generic-password"
                    | "add-internet-password"
            )
        })
    {
        return Some(SensitiveCommand {
            kind: "keychain",
            action: "read Keychain credential",
            summary: "macOS Keychain access",
            target_service,
        });
    }

    if matches!(executable.as_str(), "ssh" | "scp" | "sftp") {
        return Some(SensitiveCommand {
            kind: "credential",
            action: "use SSH credential",
            summary: "SSH credential-capable operation",
            target_service,
        });
    }

    if executable == "codesign" {
        return Some(SensitiveCommand {
            kind: "permission",
            action: "use signing identity",
            summary: "Code-signing identity access",
            target_service: None,
        });
    }

    if executable == "xcrun" && args.iter().any(|argument| argument == "notarytool") {
        return Some(SensitiveCommand {
            kind: "credential",
            action: "use notarization credential",
            summary: "Apple notarization credential access",
            target_service: Some("apple.com"),
        });
    }

    if executable == "docker" && first_argument == Some("login") {
        return Some(SensitiveCommand {
            kind: "credential",
            action: "read registry credential",
            summary: "Container registry credential access",
            target_service,
        });
    }

    None
}

fn first_command_argument(args: &[String]) -> Option<&str> {
    let mut skip_next = false;
    for argument in args {
        if skip_next {
            skip_next = false;
            continue;
        }
        if matches!(argument.as_str(), "-C" | "--git-dir" | "--work-tree") {
            skip_next = true;
            continue;
        }
        if argument.starts_with('-') {
            continue;
        }
        return Some(argument.as_str());
    }
    None
}

fn known_target_service(command: &[String]) -> Option<&'static str> {
    const SERVICES: &[(&str, &str)] = &[
        ("github.com", "github.com"),
        ("gitlab.com", "gitlab.com"),
        ("bitbucket.org", "bitbucket.org"),
        ("registry.npmjs.org", "registry.npmjs.org"),
        ("npmjs.com", "npmjs.com"),
        ("docker.io", "docker.io"),
        ("apple.com", "apple.com"),
    ];
    command.iter().find_map(|argument| {
        let lower = argument.to_ascii_lowercase();
        SERVICES
            .iter()
            .find(|(needle, _)| lower.contains(needle))
            .map(|(_, service)| *service)
    })
}

fn command_executable(command: &[String]) -> Option<String> {
    let executable = command.first()?.rsplit(['/', '\\']).next()?;
    safe_label(executable)
}

fn project_labels(cwd: &PathUri) -> (String, Option<String>) {
    let components = cwd
        .to_abs_path()
        .ok()
        .map(|path| {
            path.as_path()
                .components()
                .filter_map(|component| component.as_os_str().to_str())
                .filter_map(safe_label)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    let repository = ["projects", "repos", "repositories"]
        .iter()
        .find_map(|marker| {
            components
                .iter()
                .position(|component| component.eq_ignore_ascii_case(marker))
                .and_then(|index| components.get(index + 1).cloned())
        })
        .or_else(|| {
            components
                .iter()
                .rposition(|component| component.eq_ignore_ascii_case("worktrees"))
                .and_then(|index| components.get(index + 2).cloned())
        })
        .or_else(|| components.last().cloned());
    let project = repository
        .clone()
        .unwrap_or_else(|| "unknown-project".to_string());
    (project, repository)
}

fn safe_label(value: &str) -> Option<String> {
    let label: String = value
        .chars()
        .take(MAX_LABEL_LEN)
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, ' ' | '.' | '_' | '-') {
                character
            } else {
                '_'
            }
        })
        .collect();
    let label = label.trim();
    (!label.is_empty()).then(|| label.to_string())
}

#[derive(Debug, Deserialize)]
struct MacCtlResponse {
    result: Option<MacCtlResult>,
}

#[derive(Debug, Deserialize)]
struct MacCtlResult {
    notice: Option<MacCtlNotice>,
}

#[derive(Debug, Deserialize)]
struct MacCtlNotice {
    request_id: Option<String>,
}

fn parse_request_id(output: &[u8]) -> Option<String> {
    let response = serde_json::from_slice::<MacCtlResponse>(output).ok()?;
    let request_id = response.result?.notice?.request_id?;
    safe_label(&request_id)
}

#[cfg(test)]
#[path = "authorization_bridge_tests.rs"]
mod tests;
