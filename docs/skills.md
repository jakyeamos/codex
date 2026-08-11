# Skills

For information about skills, refer to [this documentation](https://developers.openai.com/codex/skills).

## Local invocation evidence

This fork records structured explicit and implicit skill invocation events in
the `skill_invocations` table of the local SQLite state database
(`$CODEX_SQLITE_HOME/state_5.sqlite`, or `~/.codex/state_5.sqlite` by default).
Successful events can be aggregated as usage; failed explicit loads are stored
with `status = 'error'` for diagnostics and must not be counted as use. Events
are idempotent by thread, turn, skill path, and invocation type.

The record contains identifiers, skill name/path/scope, invocation type,
status, and timestamp. It does not contain prompt, response, or skill contents.
The table begins at the migration that creates it and does not reconstruct
earlier invocation history from transcripts.
