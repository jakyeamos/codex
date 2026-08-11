# Codex CLI context index

last_reviewed: 2026-08-11

Load only the packet needed for the task. Codex CLI is a local agent runtime;
keep CLI, SDK, plugin, and desktop integration surfaces distinct.

- [README.md](../../README.md) — install, runtime, and consumer overview.
- [AGENTS.md](../../AGENTS.md) — repository policy and required validation lanes.
- [Code review skill](../../.codex/skills/code-review/SKILL.md) — representative routed agent workflow.
- [Codex core](../../codex-rs/README.md) — core workspace details when that file exists in the checked-out component.

Use the repository's `just` validation recipes rather than invoking raw Cargo
tests when working on the Rust workspace. Preserve plugin, SDK, and external
consumer compatibility as separate change surfaces.
