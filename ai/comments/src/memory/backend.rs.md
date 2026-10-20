# backend.rs

The pluggable model access the user asked for: `Backend::open(system)` starts a conversation, `Conversation::say(blocks)` sends one user message and returns the reply in the same context (size retries need that). A stateless HTTP API implements `Conversation` by keeping its own message history. Only implementation: `claude.rs`.

`Backend::stop` ends active calls when the owning compactor is dropped; fake/stateless backends may keep the default no-op.

`Backend::agent` and `Conversation::session` are provenance: with the model they become the agent, model and session of every node the compactor writes. A backend without session ids returns an empty string (recorded `-`).
