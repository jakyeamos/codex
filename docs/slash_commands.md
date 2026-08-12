# Slash commands

For an overview of Codex CLI slash commands, see [this documentation](https://developers.openai.com/codex/cli/slash-commands).

## Side conversations

`/side [prompt]` opens a visible, ephemeral fork for a focused question without replacing the main
thread. The side conversation inherits the parent history as reference context, is non-mutating by
default, and can be closed with Ctrl+C.

The TUI also exposes `side_conversation.ask` to the main Codex agent. The agent can use it when an
auxiliary rubric, rating scale, critique, second perspective, or bounded exploration would help
without derailing the main task. Calls provide a stable `purpose`, a `prompt`, and an optional
`reuse` flag. A matching-purpose side conversation can be reused; a different purpose opens a fresh
side conversation. The tool waits for the side answer and returns JSON containing `thread_id`,
`purpose`, and `response`.
