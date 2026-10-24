# agent/store.rs

Design: `ai/research/20261014T002322--chat-design.md` (Threads and sessions, Data model, Roll-up keys). Records: record.rs; JSON Lines files: `src/jsonl.rs`.

Where
- The store of a project is `~/.ai/chat/<project id>/` (`$AI_CHAT_DIR/<project id>` if set; `base_dir`), found from a directory by `Project::find`. Every clone and worktree holding the committed `ai.toml` sees the same threads, and they outlive the clone. Outside a project, or in one whose `ai.toml` has no id, `find` is `None` (a hook's sync then does nothing), and `open`, the CLI's, fails saying to run `ai init`.

- Thread names: letters, digits, `-`, `_`, starting with a letter or digit: a safe directory and lock file name. Harness, session and agent ids become path names: letters, digits, `.`, `_`, `-`, not starting with `.` (record.rs, `check_id`).

Layout (format 3)
- `format`: `3`, written before the first write. `Store::find` refuses another format, so a newer store is refused before any read or write.
- `sessions.jsonl`: every session's attachment changes (`Event`), appended. The last one per harness and session is the one owner lookup (`Store::owner`): a session is attached to at most one thread by construction, and finding it reads no thread.
- `threads/<name>/`: one agent's thread; it exists once `Store::create` makes it, empty.
  - `items.jsonl`: the main tree, one `Row` per item; its position is its sequence number.
  - `sessions.jsonl`: each session's branch here (`Track`: transcript, graft, tip), appended when it changes; the last per session holds.
  - `agents.jsonl`: subagent records (`Agent`), appended when they change; the last per harness and agent holds. `agents/<harness>/<agent>.jsonl`: the subagent's own rows, numbered from 0; its roots have no parent and `call` links it to the main tree.
  - `lines.jsonl`: summary line operations (`LineOp`), folded in order.
  - `sources/<harness>/<session>/<relative path>`: the session's original files, byte for byte.
  - `calls/<session>.jsonl`: the compactor's calls to the model, byte for byte (`Thread::calls_dir`, `memory/claude.rs`'s `Record`).
- Locks, beside the threads: `sessions/<harness>/<session>.lock` (one session's attach, sync and detach), `sessions.lock` (appends to `sessions.jsonl`), `<name>.write.lock` (a thread's writers), `<name>.lock` (its compactor) and its log `<name>.log`.

Writing
- Every file but the sources is append-only, so any earlier state of a thread is a prefix of its files. A `Mark` is the lengths of a thread's `items`, `lines` and `sessions` files; `snapshot_at` reads those prefixes (the view's pages, the summariser's change check).
- A crash can leave an incomplete last line: readers stop at the last newline, and the next writer truncates it under its lock before appending (`jsonl::append`). No fsync: everything but the lines is derived from the transcripts, and a lost line is built again.
- Writers hold the thread's writer lock and read the files again under it. Order within one write: sources, subagent rows, main rows, agent records, then the session's track. A record is written after the items it names, so a crash between them leaves items no record names yet, which the next sync's dedupe keeps.
- Sources: a file that extends its archived copy gets the new bytes appended (transcripts only grow); any other change rewrites it. A path that changed between file and directory replaces the entry standing in the way. Missing live files keep their last copy. Earlier versions of a rewritten file are not kept.

Reading
- A `Snapshot` holds the bytes of the thread's `items`, `lines`, `sessions` and `agents` files and the store's `sessions.jsonl` as read at one moment, and decodes on demand. Subagent item files are read live.
- Readers take no lock, so `snapshot` reads the files that name items (sessions, lines, agents) before `items`: a writer appends items before any record naming them, so every item a record names is there. Read the other way, a sync landing between the reads gave a session record with a tip past the items read (`two_processes_syncing_one_thread_lose_nothing` failed about 1 in 16 runs under parallel load).
- Every read checks what it decodes: positions equal sequence numbers and parents are earlier (`check_head`); tips, grafts, calls and line keys lie within the items. Corrupt data is an error naming the thread and the record, never a panic.
- `Binding` joins a `Track` with the session's attachment changes: attached at its last attachment to this thread, detached at the change after it. A track with no attachment to this thread yet is not a binding.

Items
- Append-only. An item's sequence number is its position; its parent is an earlier number, so the tree has no cycles and `model::Tree` rebuilt from `items` gives back the stored links (each stored item's `parent` is the source id of its stored parent).
- Dedupe: by exact harness and entry id, not session: Claude Code copies entries under a new session id when it forks, so a fork attached to the thread adds only its new entries, hung on the copies already there.
- `ingest`: every item of the transcript the sequence lacks, in transcript order; its parent is its source parent's number, and a root (or an unknown parent) hangs on the track's `graft`. The adapter gives a longer file the same items plus new ones (claude.rs.md), so re-reading the whole file and keeping the unknown entries is the whole sync. `tip` moves to the item of the transcript's last entry.
- Subagents: each `Subagent` from the adapter goes to its own sequence, deduped the same way; `call` is found by the Agent tool call id among the session's items. A previously resolved call survives observations that cannot resolve it, including a missing main transcript.

Lines
- `put_line`/`put_lines`: first write per key wins and the stored line comes back; a key past the last item is refused. Kind (`chunk`, `turn`, `node`) and who wrote it (`memory::Who`: agent, model, session) are stored with the text; the level numbering is the roll-up's (rollup.rs.md). The compactor writes each round of free lines in one append.
- `drop_lines` appends `clear`, `drop_keys` a `drop` of the keys built (`build --rebuild`, `build <id>`).

Queries
- `Store::list`: per thread its newest item (harness, session, time) and tips; its turns on the newest branch, counted from the heads and kind tags alone (`count_turns`, `record::Tag`: no item content is decoded and no tree is built; a unit test checks it against `model::turns`) and its sessions, each with the time of its newest item (else of its attaching), newest first. `Store::holds(dir)`: `dir` is in this project (its project has the same id). `Snapshot::tree` and `model::Tree::path`/`branch` give the item tree and the path to a tip; `Snapshot::tip(harness, session)` the newest tip of a session; `Snapshot::line`/`lines` lines by key and by level.
