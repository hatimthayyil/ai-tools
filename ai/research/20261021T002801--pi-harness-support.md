# pi as a second harness

Survey of the work to run `ai chat` and `/ai` on pi, including its forks, subagents and tree
sessions. Read-only; nothing implemented. Sources: `src/`, pi 1.3.0 docs installed at
`/nix/store/p343pbfrzda5a37m9dhmg17pwc47bkls-pi-1.3.0/libexec/pi/docs`, 82 real pi session
files, pi-subagents docs.

## Verdict

Four seams and one model decision. The store, record, line and view layers are already
harness-neutral (`Binding`/`Track`/`Agent`/`Head` carry `harness: String`,
`src/chat/record.rs:78-160`; `store.rs` locks and ingest take strings; `view`, `rollup`,
`render`, `page` never mention a harness). All Claude coupling sits in four adapters:
`src/chat/claude.rs`, `src/chat/cli.rs` defaults, `src/memory/claude.rs`, `plugins/claude`.
The missing capability, agent reports reaching a thread, is missing for Claude too
(`ai/research/20261020T231858--fork-transcripts-in-threads.md:24`), so it belongs in the
model, not in the pi adapter.

## What pi provides

| Concern | pi |
|---|---|
| Session file | `~/.pi/agent/sessions/--<cwd, non-alnum→->--/<ts>_<uuid>.jsonl`; `--session-dir` > `PI_CODING_AGENT_SESSION_DIR` > `sessionDir` |
| Format | JSONL, header `{type:"session", version:3, id, cwd, parentSession?}`; tree by `id`/`parentId` |
| Current session | extension: `ctx.sessionManager.getSessionId()/getSessionFile()`; bash tool: `$PI_SESSION_ID`, `$PI_SESSION_FILE`; other children: nothing |
| `/tree` | branches in one file, no persisted leaf (last entry is leaf on reload) |
| `/fork`, `/clone` | new file, header `parentSession` = source path, **entries copied with their original ids** |
| pi-subagents | child session at `<sessionDir>/<parent-stem>/<runId>/run-0/session.jsonl`; artifacts at `<sessionDir>/subagent-artifacts/<runId>_<agent>_{input.md,transcript.jsonl,output.md,meta.json}` |
| Report hand-back | `custom_message`, `customType: "subagent-notify"`, in the parent's context |
| One-shot | `pi -p --mode json --no-session --no-tools --no-extensions ... --system-prompt <text> --model provider/id`; per-message `usage.*.cost.total` |
| Extension | `registerCommand`, `registerTool`, events `turn_end`, `before_agent_start`, `session_before_compact`, `session_shutdown`; `ctx.compact()` |

## Mapping pitfalls

`custom`/`custom_message` with `subagent-notify` is the pi equivalent of Claude's
`task-notification`, both currently dropped. `compaction` carries both the boundary and its
summary in one entry; `branch_summary` is a summary on the live path; `context_edit` rewrites
an earlier entry in 15 real entries. Omitting non-message entries as parents orphans 434
messages in the corpus: parents must be threaded through dropped entries.

`ends_turn` is absent from all 5,651 messages; derive it from `stopReason` (`stop`, `length`,
`error`, `aborted` end; `toolUse` continues). Entry ids are 8 hex, unique in the corpus but
scope every item by session; pi may fall back to full UUIDs.

## Work in layers

### Layer 0 — prep, no behaviour change

1. `src/memory/proc.rs`: move `Live`, `Record`, `Proc`, `Prompts` out of `memory/claude.rs`.
   They hold no Claude flags, only process lifecycle, retries, PDEATHSIG and session records.
2. `src/chat/harness.rs`: `enum Harness { Claude, Pi }` with `id`, `parse`, `detect`,
   `session_id`, `transcript`, `session_dir`, `parse_bytes`, `subagents`, `program`,
   `new_argv`, `resume_argv`. `claude.rs` and a new `pi.rs` become private parsers.
3. Move `Subagent` to `chat/model.rs` (`claude.rs:104`); move `uuid()` to `chat/id.rs`
   (`memory/claude.rs:32`, imported by `chat/cli.rs:29`).
4. Resolve the harness once. `--harness` loses its clap `default_value`
   (`chat/cli.rs:198`), `Session::current()` detects `CLAUDE_CODE_SESSION_ID` /
   `PI_SESSION_ID` / `AI_AGENT`, and `sync::read` takes a `Harness` instead of matching a
   string and bailing (`sync.rs:34-60`).
5. Bind by identity, discover by path. Delete `predict_transcript` and the precomputed
   transcript path in `chat start` (`cli.rs:515-544`); the first sync supplies
   `--transcript`, and discovery is the fallback. Pi cannot predict the timestamp prefix,
   Claude's prediction was only an optimisation, and both plugins already pass the real path.

### Layer 1 — read-only pi

`src/chat/pi.rs`:

- resolve the session dir (env, setting, default), find a session by header `id` (basename
  suffix first, then scan), read `cwd` from the header;
- parse v3 entries to `Vec<Item>`, threading `parentId` through dropped entries, splitting
  content blocks into `{id}.{k}`;
- `ends_turn` from `stopReason`; truncate at the last `\n`;
- enumerate child sessions and their artifacts for `Subagent`.

Exit: `ai chat attach --harness pi --session <id> --transcript <path>` then `view`/`zoom`/
`grep`/`build` work on a real pi session with no launch and no extension. Roughly 350 lines
plus fixtures.

### Layer 2 — launch and `/ai`

- `chat start`/`resume`: `program()` = `pi`, `new_argv` = `--session-id <uuid>`, `resume_argv`
  = `--session <id>`.
- `plugins/pi/`: `registerCommand("ai")` with attach/detach/plain; `turn_end` fires a
  fire-and-forget `ai chat sync-session`; `session_shutdown` awaits a final sync;
  `session_before_compact` returns the stored view as
  `{compaction:{summary: view, firstKeptEntryId: null}}`; `before_agent_start` seeds a
  launched session with `ai chat compact-session`; `registerTool("compress")` sets the flag.
  Plain `/ai` is: build lines, `ai view --whole`, store, `ctx.compact()`; any failed step
  compacts nothing, matching Claude.
- `flake.nix` copies `plugins/pi` beside `plugins/claude`.

### Layer 3 — forks, tree, subagents

- `/tree` already works: branches are items with parents and `tips()` returns every branch.
- `/fork` and `/clone`: read `parentSession`, resolve it to an existing binding, and graft the
  fork at that binding's **tip** instead of the thread's newest item (`sync.rs:207`,
  `store.rs:578`). Fall back to the newest item when the parent is not bound.
- pi-subagents: each child run is a child session with `parentSession`; store its items as an
  `Agent` (identity: child session, not agent name), with the parent's tool-call id from the
  run metadata.
- Reports: `custom_message`/`subagent-notify` (pi) and `task-notification` (Claude) become
  `Kind::Work { from }`, opening a turn. Shared model change, both harnesses.

### Layer 4 — parity extras

- `ai memory prompt-hook` is Claude JSON only (`memory/hook.rs:31-57`). Split the rule from
  the framing and add a plain output the pi extension injects from `before_agent_start`;
  config wires it at `home/hatim/claude/settings.json:63`.
- `memory/prov.rs:108` knows `CLAUDECODE`, Codex, `AI_AGENT`; add `PI_SESSION_ID` for
  session attribution. Model attribution from pi is an unknown.
- README, glossary (`backend`, `launcher`, `seed`), config keys.

## Cross-cutting simplifications

1. **One adapter, one resolution point.** Replace the `HARNESS` constants, the clap default,
   the `match harness` in `sync.rs` and the per-command `default_value` with `Harness`
   resolved at the CLI boundary. Glue then passes nothing: `ai chat sync-session` detects pi
   from `PI_SESSION_ID`. A trait buys nothing for a closed set of two; use an enum. Keep the
   `Backend` trait where processes already justify it.
2. **Identity first, path later.** Remove path prediction from `chat start`; make
   `--transcript` the primary contract for both harnesses. This deletes the pre-binding
   plumbing and the only asynchronous assumption in attach.
3. **One child-session concept.** Fork, clone and subagent run are all "session file with
   `parentSession`". Model lineage once (parent binding → graft) and carry the child kind as
   metadata (`subagent`, `fork`). This is what makes the subagent and fork requirements one
   feature instead of two.
4. **One process layer.** `Live`/`Record`/`Proc`/`Prompts` are generic; extracting them makes
   the pi backend ~200 lines and leaves `memory/claude.rs` as flags and event parsing.
5. **One TypeScript glue.** Both plugins are TypeScript against the same CLI protocol:
   argument building, exit code 3, timeouts, build-then-print, state keys. Share it in
   `plugins/shared/`; only events and injection differ.
6. **Fix agent reports once.** Do it in `chat::model` for both harnesses, otherwise the pi
   adapter will copy the Claude gap.
7. **Fixtures per harness.** `tests/fixtures/pi/` with trimmed real sessions, one mapping
   test table per harness. The current suite is Claude-shaped; parallel fixtures keep the
   adapters honest without a second test harness.

## Decisions

1. **Fork binding: automatic or explicit?** Auto-attach a fork to its parent's thread keeps
   the view continuous at the moment of branching; explicit attach keeps the one-session-
   one-team rule visible. Recommendation: automatic when the parent is bound.
2. **Dedupe of copied entries.** The key is `(harness, entry)` and excludes the session
   (`store.rs:78`), so a fork's copied prefix merges into the parent's items. That is the
   desired semantics; the alternative, session-scoped items, duplicates the prefix. Keep the
   merge and record lineage. 8-hex ids make a cross-session collision possible; the corpus has
   none, and the lineage rule can distinguish inherited from unrelated.
3. **Subagent children: `Agent` or branch?** Recommendation: `Agent` side transcripts; a
   subagent's conversation is not the thread's conversation. Its report opens a `Work` turn,
   which renumbers turns after that point.
4. **`context_edit`:** ignore. The store is append-only and the glossary promises nothing is
   removed; the view records what was said, not the model's final context.
5. **Summariser backend.** `Backend` is neutral; add `memory::pi` and choose per config
   (`ai.chat.backend`, `ai.memory.backend`) with a model valid for that backend. The verified
   invocation is in `pi-summariser`'s report. `~/.pi/agent/APPEND_SYSTEM.md` contaminates
   `--system-prompt`; isolating it needs a minimal `PI_CODING_AGENT_DIR` with auth, untested.
   `--mode json` exits 0 on a failed response, so read `stopReason`.
6. **Archived evidence.** Claude archives `<transcript>` + `<stem>/`. Pi's child sessions and
   artifacts are siblings (`<sessionDir>/<parent-stem>/`, `<sessionDir>/subagent-artifacts/`).
   Decide what is archived byte-for-byte: child session files and `meta.json` are evidence;
   `input.md`/`output.md` are derived and duplicate content.

## Unknowns

- pi's subagent layout is documented by pi-subagents, not pi; treat it as a moving dependency
  and pin behaviour with fixtures.
- The extension surface for `turn_end` drafts and a null `firstKeptEntryId` is documented but
  has no shipped 1.3.0 example; prototype before designing around it.
- `/hatimthayyil/code/pi` is 0.86.1 while the installed pi is 1.3.0. Implement against the
  installed docs and sessions.
- Whether pi's compaction summary re-enters the view as content (the `/ai` view supplied as
  the summary) needs checking; Claude's compaction marker has the same question today.
