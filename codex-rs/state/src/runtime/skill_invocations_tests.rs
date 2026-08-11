use super::*;
use crate::runtime::test_support::unique_temp_dir;
use codex_utils_absolute_path::test_support::PathExt;
use pretty_assertions::assert_eq;

fn record(
    thread_id: ThreadId,
    status: SkillInvocationStatus,
    occurred_at_ms: i64,
) -> SkillInvocationRecord {
    SkillInvocationRecord {
        thread_id,
        turn_id: "turn-1".to_string(),
        skill_name: "example".to_string(),
        skill_path: PathBuf::from("/skills/example/SKILL.md"),
        skill_scope: SkillScope::User,
        invocation_type: SkillInvocationType::Explicit,
        status,
        occurred_at_ms,
    }
}

#[tokio::test]
async fn records_skill_invocations_idempotently_and_upgrades_success() -> anyhow::Result<()> {
    let runtime = StateRuntime::init(
        crate::SqliteConfig::new_for_testing(unique_temp_dir().as_path().abs()),
        "test-provider".to_string(),
    )
    .await?;
    let thread_id = ThreadId::from_string("00000000-0000-0000-0000-000000000048")?;

    runtime
        .record_skill_invocations(&[record(thread_id, SkillInvocationStatus::Error, 2_000)])
        .await?;
    runtime
        .record_skill_invocations(&[record(thread_id, SkillInvocationStatus::Ok, 3_000)])
        .await?;

    assert_eq!(
        runtime.skill_invocation_records().await?,
        vec![record(thread_id, SkillInvocationStatus::Ok, 2_000)]
    );
    Ok(())
}

#[tokio::test]
async fn keeps_explicit_and_implicit_invocations_distinct() -> anyhow::Result<()> {
    let runtime = StateRuntime::init(
        crate::SqliteConfig::new_for_testing(unique_temp_dir().as_path().abs()),
        "test-provider".to_string(),
    )
    .await?;
    let thread_id = ThreadId::from_string("00000000-0000-0000-0000-000000000049")?;
    let explicit = record(thread_id, SkillInvocationStatus::Ok, 1_000);
    let implicit = SkillInvocationRecord {
        invocation_type: SkillInvocationType::Implicit,
        occurred_at_ms: 2_000,
        ..explicit.clone()
    };

    runtime
        .record_skill_invocations(&[explicit.clone(), implicit.clone()])
        .await?;

    assert_eq!(
        runtime.skill_invocation_records().await?,
        vec![explicit, implicit]
    );
    Ok(())
}
