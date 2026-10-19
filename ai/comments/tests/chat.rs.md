# tests/chat.rs

The thread store and sync against real git repositories in temporary directories (`gix::init`; the `git` CLI only to make a worktree), with the Claude Code fixtures of `tests/fixtures/claude` as transcripts. `Repo::state` is every file of a thread, to check that nothing was written.

- `relabel`: fixtures reuse entry ids (`u1`, `a1`) across files; Claude Code uuids are unique, and the store dedupes by harness and entry id, so a test putting two fixtures on one thread prefixes one's ids.
- Attach then sync on a growing file: items, sources and kinds equal the adapter's; no write when nothing is new; the `format` file, one attachment event, one row per item.
- Not attached: outside a repository, no thread, another session, after detach (no write); a bad session id is refused.
- Graft: a second session's root hangs on the first's newest item; tips, session tips, paths and `list`.
- Fork: the same entries under another session id add nothing; new entries hang on the copies.
- Detach and re-attach: the graft and transcript path are kept; re-attach catches up.
- Reattachment elsewhere requires explicit detach and a transcript source. Both threads retain their recorded history. Same-thread reattachment with unchanged sources writes nothing.
- An occupied session is rejected before source parsing and leaves both threads unchanged.
- Original sources: complete main JSONL bytes, companion subagent JSONL and metadata, nested binary artifacts, hidden files and empty files belong to the session archive. Unrelated files beside the session stay outside it.
- Archive updates: unknown events and incomplete tails change the archive without adding projected items. Unchanged files write nothing; missing live files retain their last captured bytes; a path changing between file and directory replaces the archived entry, both ways.
- Subagents: kept under `agents/`, out of the main tree, linked to their Agent call; a second sync adds nothing.
- Lines: first write wins; lookups by key and level; a key past the last item is refused.
- Locks per thread in the common git dir; worktrees share the store and locks. The compactor lock is retaken with a deadline: a child process another test spawns holds a copy of a lock's descriptor until it execs.
- A store newer than format 3 is refused by `Chat::find` and sync, and left unchanged.
- Corrupt records (a tip or graft beyond the items, a record missing a field, an item numbered out of place, an agent call beyond the items) are errors naming the thread and record, from the snapshot and from `list`. A corrupt thread leaves the sessions of other threads syncing, detaching and attaching.
- `two_processes_syncing_one_thread_lose_nothing`: two sessions on one thread grow line by line; at each step four child processes (two per session) sync at once. Children are this test binary re-run with `--exact sync_child` and `AI_CHAT_TEST_SYNC=<repo> <session>`; `sync_child` does nothing without the variable. Every entry lands once with its links, and only syncs that added something write a track.
