# lib.rs

The lib target exposes `memory`, `notes` and `agent`, so the binary and the integration tests in `tests/` use their public API. Shared by them: `file` (paths, locks, store formats), `git` (a store under one ref), `repo` (a git repository whose work tree is a store, committed whole: notes and a project's `.ai`), `record` (named headers, column segments), `project` (the project at a directory, for agents) and `progress` (the stderr bar of agent builds).
