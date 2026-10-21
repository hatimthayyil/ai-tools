# lib.rs

The lib target exposes `memory`, `notes`, `chat` and `files`, so the binary and the integration tests in `tests/` use their public API. Shared by them: `file` (paths, locks, store formats), `git` (a store under one ref), `record` (named headers, column segments), `project` (the project at a directory, for chat and files) and `progress` (the stderr bar of chat and files builds).
