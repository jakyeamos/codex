CREATE TABLE skill_invocations (
    thread_id TEXT NOT NULL,
    turn_id TEXT NOT NULL,
    skill_name TEXT NOT NULL,
    skill_path TEXT NOT NULL,
    skill_scope TEXT NOT NULL CHECK (skill_scope IN ('user', 'repo', 'system', 'admin')),
    invocation_type TEXT NOT NULL CHECK (invocation_type IN ('explicit', 'implicit')),
    status TEXT NOT NULL CHECK (status IN ('ok', 'error')),
    occurred_at_ms INTEGER NOT NULL,
    PRIMARY KEY (thread_id, turn_id, skill_path, invocation_type)
) WITHOUT ROWID;

CREATE INDEX skill_invocations_name_time_idx
ON skill_invocations(skill_name, occurred_at_ms);
