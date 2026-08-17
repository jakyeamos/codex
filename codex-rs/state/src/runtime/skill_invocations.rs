use super::StateRuntime;
use codex_protocol::ThreadId;
use codex_protocol::protocol::SkillScope;
use sqlx::Row;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkillInvocationType {
    Explicit,
    Implicit,
}

impl SkillInvocationType {
    fn as_str(self) -> &'static str {
        match self {
            Self::Explicit => "explicit",
            Self::Implicit => "implicit",
        }
    }

    fn from_str(value: &str) -> anyhow::Result<Self> {
        match value {
            "explicit" => Ok(Self::Explicit),
            "implicit" => Ok(Self::Implicit),
            _ => Err(anyhow::anyhow!("unknown skill invocation type {value}")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkillInvocationStatus {
    Ok,
    Error,
}

impl SkillInvocationStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Error => "error",
        }
    }

    fn from_str(value: &str) -> anyhow::Result<Self> {
        match value {
            "ok" => Ok(Self::Ok),
            "error" => Ok(Self::Error),
            _ => Err(anyhow::anyhow!("unknown skill invocation status {value}")),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillInvocationRecord {
    pub thread_id: ThreadId,
    pub turn_id: String,
    pub skill_name: String,
    pub skill_path: PathBuf,
    pub skill_scope: SkillScope,
    pub invocation_type: SkillInvocationType,
    pub status: SkillInvocationStatus,
    pub occurred_at_ms: i64,
}

impl StateRuntime {
    pub async fn record_skill_invocations(
        &self,
        records: &[SkillInvocationRecord],
    ) -> anyhow::Result<()> {
        if records.is_empty() {
            return Ok(());
        }

        let mut transaction = self.pool.begin().await?;
        for record in records {
            sqlx::query(
                r#"
INSERT INTO skill_invocations (
    thread_id,
    turn_id,
    skill_name,
    skill_path,
    skill_scope,
    invocation_type,
    status,
    occurred_at_ms
) VALUES (?, ?, ?, ?, ?, ?, ?, ?)
ON CONFLICT(thread_id, turn_id, skill_path, invocation_type) DO UPDATE SET
    skill_name = excluded.skill_name,
    skill_scope = excluded.skill_scope,
    status = CASE
        WHEN skill_invocations.status = 'ok' OR excluded.status = 'ok' THEN 'ok'
        ELSE 'error'
    END,
    occurred_at_ms = MIN(skill_invocations.occurred_at_ms, excluded.occurred_at_ms)
"#,
            )
            .bind(record.thread_id.to_string())
            .bind(record.turn_id.as_str())
            .bind(record.skill_name.as_str())
            .bind(record.skill_path.to_string_lossy().as_ref())
            .bind(skill_scope_as_str(record.skill_scope))
            .bind(record.invocation_type.as_str())
            .bind(record.status.as_str())
            .bind(record.occurred_at_ms)
            .execute(&mut *transaction)
            .await?;
        }
        transaction.commit().await?;
        Ok(())
    }

    pub async fn skill_invocation_records(&self) -> anyhow::Result<Vec<SkillInvocationRecord>> {
        let rows = sqlx::query(
            r#"
SELECT
    thread_id,
    turn_id,
    skill_name,
    skill_path,
    skill_scope,
    invocation_type,
    status,
    occurred_at_ms
FROM skill_invocations
ORDER BY occurred_at_ms ASC, thread_id ASC, turn_id ASC, skill_path ASC
"#,
        )
        .fetch_all(self.pool.as_ref())
        .await?;

        rows.into_iter()
            .map(|row| {
                let thread_id: String = row.try_get("thread_id")?;
                let skill_scope: String = row.try_get("skill_scope")?;
                let invocation_type: String = row.try_get("invocation_type")?;
                let status: String = row.try_get("status")?;
                Ok(SkillInvocationRecord {
                    thread_id: ThreadId::from_string(&thread_id)?,
                    turn_id: row.try_get("turn_id")?,
                    skill_name: row.try_get("skill_name")?,
                    skill_path: PathBuf::from(row.try_get::<String, _>("skill_path")?),
                    skill_scope: skill_scope_from_str(&skill_scope)?,
                    invocation_type: SkillInvocationType::from_str(&invocation_type)?,
                    status: SkillInvocationStatus::from_str(&status)?,
                    occurred_at_ms: row.try_get("occurred_at_ms")?,
                })
            })
            .collect()
    }
}

fn skill_scope_as_str(scope: SkillScope) -> &'static str {
    match scope {
        SkillScope::User => "user",
        SkillScope::Repo => "repo",
        SkillScope::System => "system",
        SkillScope::Admin => "admin",
    }
}

fn skill_scope_from_str(value: &str) -> anyhow::Result<SkillScope> {
    match value {
        "user" => Ok(SkillScope::User),
        "repo" => Ok(SkillScope::Repo),
        "system" => Ok(SkillScope::System),
        "admin" => Ok(SkillScope::Admin),
        _ => Err(anyhow::anyhow!("unknown skill scope {value}")),
    }
}

#[cfg(test)]
#[path = "skill_invocations_tests.rs"]
mod tests;
