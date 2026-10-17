# chat/store.rs

Design: `ai/research/20261014T002322---chat-design.md` (Threads and sessions, Data model, Roll-up keys, Storage). Git plumbing: `src/git.rs` (git.rs.md), shared with `ai memory`.

Where
- One ref per thread, `refs/ai/chat/<name>`, in the repository found from a directory (`gix::discover`). Refs other than `HEAD` live in the common git dir, so every worktree of a repository sees the same threads; nothing pushes them unless asked. The committer is the user's git identity, else `ai chat <ai@localhost>`.
- Names: letters, digits, `-`, `_`, starting with a letter or digit: a safe ref component and lock file name. Harness, session and agent ids become tree path names: letters, digits, `.`, `_`, `-`, not starting with `.` (record.rs, `check_id`).
- Compactor lock per thread: `<common git dir>/ai/chat/<name>.lock` (`file::try_lock`), outside `refs/` so git never mistakes it for a ref lock. The compactor's log beside it, `<name>.log` (`log_path`; both make the directory).

Layout of a thread's tree (format 2)
- `format`: `2`, written by the first commit (`Ref::mutate`). Newer formats are refused before writes. The CLI detects v1 threads and requests terminal confirmation before importing them.
- `index/<fan s>`: JSON array of `Head` records for items `[256s, 256s+256)`. Positions and embedded sequence numbers agree; parents name earlier items.
- `items/<fan i>`: a serde JSON tagged item kind and its content.
- `raw/<fan i>`: a recognised item's source line after text decoding, without its line ending. Several items from one entry share a blob.
- `sources/<harness>/<session>/<relative path>`: complete original main transcript and companion session files, with their original bytes. Changed files replace their current blobs; missing files retain their last captured blobs. Git parents retain earlier versions.
- `sessions/<harness>/<session>`: a binding (record.rs).
- `agents/<harness>/<agent>/`: a subagent: `agent` (type, description, `call`: the sequence number of the Agent tool call that started it) and its own `index/`, `items/`, `raw/`, numbered from 0. Its roots have no parent; `call` links it to the main tree.
- `lines/<level>/<fan last>`: a summary line keyed by level and the last item it covers (record.rs `Line`).
- `<fan n>` = `{n>>8:04x}/{n&255:02x}` (git.rs `fan_path`).

Why this split
- Sync needs source identity and parents for deduplication; the index supplies both without fetching variable-size content. An appended item never rewrites an older one.
- `heads` reads structure; `items`/`tree` read content for the roll-up and view. Sequence lengths are checked against the index before content is returned.
- Subagents get their own numbering so the main tree's positions stay its sequence numbers: `Snapshot::tree` is `model::Tree` over the main items, and a subagent's items never show as a branch or tip of the conversation.
- Lines are one blob each: written once, never rewritten, few per turn.

Items
- Append-only. An item's sequence number is its position; its parent is an earlier number, so the tree has no cycles and `model::Tree` rebuilt from `items` gives back the stored links (each stored item's `parent` is the source id of its stored parent).
- Dedupe: by exact harness and entry id, not session: Claude Code copies entries under a new session id when it forks, so a fork attached to the thread adds only its new entries, hung on the copies already there.
- `ingest`: every item of the transcript the sequence lacks, in transcript order; its parent is its source parent's number, and a root (or an unknown parent) hangs on the binding's `graft`. The adapter gives a longer file the same items plus new ones (claude.rs.md), so re-reading the whole file and keeping the unknown entries is the whole sync. `tip` moves to the item of the transcript's last entry.
- Subagents: each `Subagent` from the adapter goes to its own sequence, deduped the same way; `call` is found by the Agent tool call id among the session's items. A previously resolved call survives observations that cannot resolve it, including a missing main transcript.

Lines
- `put_line`: first write wins and returns the stored line; a key past the last item is refused. Kind (`chunk`, `turn`, `node`) and who wrote it (`memory::Who`: agent, model, session) are stored with the text; the level numbering is the roll-up's (rollup.rs.md).
- `put_lines`: the same for many lines in one commit (`line <key>` for one, `<n> lines` for more): the compactor writes each round of free lines at once, so a first summariser over a long history does not make a commit per short turn.
- `all_lines`: every line by key, one walk per level directory; the compactor reads it once per pass.

Queries
- `Chat::list`: per thread its newest item (harness, session, time) and tips from the index; its turns (turns on the newest branch, from the tree) and its sessions, each with the time of its newest item (else of its attaching), newest first. `Chat::holds(dir)`: `dir` is in this repository (gix discover from it reaches the same canonical common git dir). `Thread::drop_lines`: removes the `lines` tree in one commit (`Changes::remove`), for `summarise --rebuild`. `Snapshot::tree` and `model::Tree::path`/`branch` give the item tree and the path to a tip; `Snapshot::tip(harness, session)` the newest tip of a session; `Snapshot::line`/`lines` lines by key and by level.
- `Thread::snapshot_at`: the thread as of one of its commits (`Snapshot::as_ref().commit()`), so the parts of a paged view all read one state.
- A `Snapshot` borrows its `Thread`: bind the thread first (`let t = chat.thread(name)?; let s = t.snapshot()?;`).

Source capture reads complete session files on each sync; no source checkpoint or parser state is kept.
