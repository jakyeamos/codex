use crate::config::Config;
use crate::session::session::Session;
use crate::session::turn_context::TurnContext;
use chrono::Utc;
use codex_analytics::InvocationType;
use codex_analytics::SkillInvocation;
use codex_analytics::TrackEventsContext;
use codex_analytics::build_track_events_context;
use codex_extension_api::SkillInvocationInput;
use codex_extension_api::SkillInvocationKind;
use codex_otel::sanitize_metric_tag_value;
use codex_protocol::protocol::SkillScope;
use codex_rollout::state_db::SkillInvocationRecord;
use codex_rollout::state_db::SkillInvocationStatus;
use codex_rollout::state_db::SkillInvocationType as PersistedInvocationType;
use codex_utils_absolute_path::AbsolutePathBuf;
use codex_utils_plugins::PluginSkillRoot;
use std::collections::HashSet;
use tokio::sync::Mutex;
use tracing::warn;

pub use codex_skills::SkillError;
pub use codex_skills::SkillMetadata;
pub use codex_skills::SkillPolicy;
pub use codex_skills::build_skill_name_counts;
pub use codex_skills::collect_explicit_skill_mentions;
pub use codex_skills::detect_implicit_skill_invocation_for_command;
pub use codex_skills_extension::HostSkillsLoadInput;
pub use codex_skills_extension::HostSkillsService;
pub use codex_skills_extension::SkillLoadOutcome;
pub use codex_skills_extension::bundled_skills_enabled_from_stack;

#[derive(Debug, Default)]
struct ImplicitSkillInvocations(Mutex<HashSet<String>>);

pub(crate) fn skills_load_input_from_config(
    config: &Config,
    effective_skill_roots: Vec<PluginSkillRoot>,
) -> HostSkillsLoadInput {
    HostSkillsLoadInput::new(
        config.cwd.clone(),
        effective_skill_roots,
        config.config_layer_stack.clone(),
        config.bundled_skills_enabled(),
    )
}

pub(crate) async fn emit_explicit_skill_invocations(
    sess: &Session,
    turn_context: &TurnContext,
    mentioned_skills: &[SkillMetadata],
    injected_skills: &[SkillMetadata],
    tracking: TrackEventsContext,
) {
    let injected_skill_paths = injected_skills
        .iter()
        .map(|skill| &skill.path_to_skills_md)
        .collect::<HashSet<_>>();
    let occurred_at_ms = Utc::now().timestamp_millis();
    let mut persisted_invocations = Vec::with_capacity(mentioned_skills.len());
    for skill in mentioned_skills {
        let skill_name_tag = sanitize_metric_tag_value(skill.name.as_str());
        let status = if injected_skill_paths.contains(&skill.path_to_skills_md) {
            "ok"
        } else {
            "error"
        };
        turn_context.session_telemetry.counter(
            "codex.skill.injected",
            /*inc*/ 1,
            &[
                ("status", status),
                ("skill", skill_name_tag.as_str()),
                ("invoke_type", "explicit"),
            ],
        );
        persisted_invocations.push(SkillInvocationRecord {
            thread_id: sess.thread_id,
            turn_id: turn_context.sub_id.clone(),
            skill_name: skill.name.clone(),
            skill_path: skill.path_to_skills_md.to_path_buf(),
            skill_scope: skill.scope,
            invocation_type: PersistedInvocationType::Explicit,
            status: if status == "ok" {
                SkillInvocationStatus::Ok
            } else {
                SkillInvocationStatus::Error
            },
            occurred_at_ms,
        });
    }
    if let Some(state_db) = sess.state_db()
        && let Err(err) = state_db
            .record_skill_invocations(&persisted_invocations)
            .await
    {
        warn!("failed to persist explicit skill invocation events: {err}");
    }

    for skill in injected_skills {
        for contributor in sess.services.extensions.skill_invocation_contributors() {
            contributor
                .on_skill_invocation(SkillInvocationInput {
                    session_store: &sess.services.session_extension_data,
                    thread_store: &sess.services.thread_extension_data,
                    turn_store: turn_context.extension_data.as_ref(),
                    turn_id: turn_context.sub_id.as_str(),
                    skill_resource: skill.path_to_skills_md.to_string_lossy().as_ref(),
                    kind: SkillInvocationKind::Explicit,
                })
                .await;
        }
    }

    let invocations = injected_skills
        .iter()
        .map(|skill| SkillInvocation {
            skill_name: skill.name.clone(),
            skill_scope: skill.scope,
            skill_path: skill.path_to_skills_md.to_path_buf(),
            plugin_id: skill.plugin_id.clone(),
            remote_plugin_id: skill.remote_plugin_id.clone(),
            invocation_type: InvocationType::Explicit,
        })
        .collect();
    sess.services
        .analytics_events_client
        .track_skill_invocations(tracking, invocations);
}

pub(crate) async fn maybe_emit_implicit_skill_invocation(
    sess: &Session,
    turn_context: &TurnContext,
    command: &str,
    workdir: &AbsolutePathBuf,
) {
    let Some(candidate) = detect_implicit_skill_invocation_for_command(
        turn_context.skills_snapshot().outcome(),
        command,
        workdir,
    ) else {
        return;
    };
    let invocation = SkillInvocation {
        skill_name: candidate.name,
        skill_scope: candidate.scope,
        skill_path: candidate.path_to_skills_md.to_path_buf(),
        plugin_id: candidate.plugin_id,
        remote_plugin_id: candidate.remote_plugin_id,
        invocation_type: InvocationType::Implicit,
    };
    let skill_scope = match invocation.skill_scope {
        SkillScope::User => "user",
        SkillScope::Repo => "repo",
        SkillScope::System => "system",
        SkillScope::Admin => "admin",
    };
    let skill_path = invocation.skill_path.to_string_lossy();
    let skill_name = invocation.skill_name.clone();
    let seen_key = format!("{skill_scope}:{skill_path}:{skill_name}");
    let inserted = {
        let skill_invocations = turn_context
            .extension_data
            .get_or_init(ImplicitSkillInvocations::default);
        let mut seen_skills = skill_invocations.0.lock().await;
        seen_skills.insert(seen_key)
    };
    if !inserted {
        return;
    }
    let skill_name_tag = sanitize_metric_tag_value(skill_name.as_str());

    if let Some(state_db) = sess.state_db()
        && let Err(err) = state_db
            .record_skill_invocations(&[SkillInvocationRecord {
                thread_id: sess.thread_id,
                turn_id: turn_context.sub_id.clone(),
                skill_name: skill_name.clone(),
                skill_path: invocation.skill_path.clone(),
                skill_scope: invocation.skill_scope,
                invocation_type: PersistedInvocationType::Implicit,
                status: SkillInvocationStatus::Ok,
                occurred_at_ms: Utc::now().timestamp_millis(),
            }])
            .await
    {
        warn!("failed to persist implicit skill invocation event: {err}");
    }

    for contributor in sess.services.extensions.skill_invocation_contributors() {
        contributor
            .on_skill_invocation(SkillInvocationInput {
                session_store: &sess.services.session_extension_data,
                thread_store: &sess.services.thread_extension_data,
                turn_store: turn_context.extension_data.as_ref(),
                turn_id: turn_context.sub_id.as_str(),
                skill_resource: skill_path.as_ref(),
                kind: SkillInvocationKind::Implicit,
            })
            .await;
    }

    turn_context.session_telemetry.counter(
        "codex.skill.injected",
        /*inc*/ 1,
        &[
            ("status", "ok"),
            ("skill", skill_name_tag.as_str()),
            ("invoke_type", "implicit"),
        ],
    );
    sess.services
        .analytics_events_client
        .track_skill_invocations(
            build_track_events_context(
                turn_context.model_info.slug.clone(),
                sess.thread_id.to_string(),
                turn_context.sub_id.clone(),
                turn_context.originator.clone(),
            ),
            vec![invocation],
        );
}
