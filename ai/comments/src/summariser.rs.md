# summariser.rs

`[summariser]` in `ai.toml` (config.rs.md): what writes every summary, for chat, memory, notes and files. Survey: `ai/research/20261024T091219--summariser-models-and-providers.md`.

- One key, `backend`, says where a summary is made; `model` is in that backend's own spelling. No pair of keys excludes another.
- `Kind`: the backends. `claude` (`claude -p`, `ClaudeCode::compactor_at`, medium effort) is the only one; any other value fails naming the valid ones (serde's "unknown variant"). An `api` backend (genai, `model` as `<provider-slug>/<model-slug>`) joins as a new variant.
- Defaults: `claude`, `sonnet`. An empty `model` is an error.
- `backend(model, calls)`: a per-run model (`ai chat build --model`) over the configured one. `maker()`: the same as a store runtime's `MakeBackend`.
- Each tool reads it where it reads its settings: `ai memory` and `ai notes` at the command's start (`Runtime::summariser`, also the model in the build log span), `ai files build` from its cwd, `ai chat` from the directory its store was found from (`Chat::backend`).
