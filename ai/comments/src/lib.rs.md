# lib.rs

The lib target exposes `memory`, `notes`, `chat` and `files`, so the binary and the integration tests in `tests/` use their public API. Shared by them: `file` (paths, locks, store formats), `git` (a store under one ref) and `record` (named headers, column segments).
