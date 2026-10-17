# tests/chat.rs

The thread store and sync against real git repositories in temporary directories (`gix::init`; the `git` CLI only to make a worktree, inspect refs and forge a newer format), with the Claude Code fixtures of `tests/fixtures/claude` as transcripts.

- `relabel`: fixtures reuse entry ids (`u1`, `a1`) across files; Claude Code uuids are unique, and the store dedupes by harness and entry id, so a test putting two fixtures on one thread prefixes one's ids.
- Attach then sync on a growing file: items, sources, kinds and raw lines equal the adapter's; no commit when nothing is new; commit messages; the `format` blob.
- Not attached: outside a repository, no thread, another session, after detach (no commit); a bad session id is refused.
- Graft: a second session's root hangs on the first's newest item; tips, session tips, paths and `list`.
- Fork: the same entries under another session id add nothing; new entries hang on the copies.
- Detach and re-attach: the graft and transcript path are kept; re-attach catches up.
- Reattachment elsewhere requires explicit detach and a transcript source. Both threads retain their recorded history. Same-thread reattachment with unchanged sources adds no commit.
- An occupied session is rejected before source parsing and leaves both original and destination thread refs unchanged.
- Original sources: complete main JSONL bytes, companion subagent JSONL and metadata, nested binary artifacts, hidden files and empty files belong to the session archive. Unrelated files beside the session stay outside it.
- Archive updates: unknown events and incomplete tails change the archive without adding projected items. Unchanged files add no commit; missing live files retain their last captured bytes, with earlier versions available through pinned snapshots.
- Subagents: kept under `agents/`, out of the main tree, linked to their Agent call; a second sync adds nothing.
- Lines: first write wins; lookups by key and level; a key past the last item is refused.
- Locks per thread in the common git dir; worktrees share refs and locks.
- A thread newer than format 2 is refused by snapshot, sync and upgrade, and left unchanged.
- V1 conversion exports records, indexes, bindings, subagents and summaries as JSON while preserving raw/source object ids and the prior Git parent. Repeated conversion adds no commit. Structured ids and multiline fields round-trip exactly; invalid index sequence numbers and parents are refused.
- `two_processes_syncing_one_thread_lose_nothing`: two sessions on one thread grow line by line; at each step four child processes (two per session) sync at once. Children are this test binary re-run with `--exact sync_child` and `AI_CHAT_TEST_SYNC=<repo> <session>`; `sync_child` does nothing without the variable. Every entry lands once with its links, and only syncs that added something committed. Instrumented once: 52 lost compare-and-swaps over the 17 steps, each redone.
