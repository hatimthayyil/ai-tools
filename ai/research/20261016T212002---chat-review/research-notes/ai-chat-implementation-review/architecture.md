# Architecture, summary correctness, growth and storage

Reviewed commit `47a7aa9ef7827909e0ab91fa135993338c7ef575` in `/tmp/ai-chat-review-architecture`. Actual code was read before the existing design/rationale. No source fixes, commits, real model calls or real stores. Rust review followed `rust-skills`; external facts below use current official documentation. Evidence programs use the reviewed library already built in the primary worktree, write only `/tmp`, and are preserved in [evidence](evidence).

## 1. Summary correctness and growth

### Takeaway

The most substantial summary defect is a confirmed collision between chunks from different branches. The read path also loads raw transcripts unnecessarily, and fold planning is quadratic; those problems should be fixed before changing storage engines.

### Cited Findings

**Confirmed: chunk cache identity is insufficient across branches.** `Key { level, last }` does not identify a chunk's source span. Chunking keeps whole assistant steps while they fit; when one continuation makes a step exceed the budget, it cuts that step away from its preceding step. Two continuations can therefore produce different starts for a chunk ending at the same shared item. `Rollup::add` silently retains the first `Source` for that key. The view independently computes each branch's chunk ranges, then looks summaries up by the colliding key. This can duplicate earlier content on one branch, or omit earlier content on the other, depending on which source wins. The same issue affects the whole-turn summaries built from these chunks. Sources: [chunk](../../../../../src/chat/rollup.rs#L42), [Part::done](../../../../../src/chat/rollup.rs#L147), [Branch::key](../../../../../src/chat/rollup.rs#L163), [first source wins](../../../../../src/chat/rollup.rs#L287), [view lookup](../../../../../src/chat/view.rs#L369).

Reproducer: [chunk-identity.rs](evidence/chunk-identity.rs). A shared turn contains user request, assistant text A (10,000 bytes), assistant text B (20,000 bytes), then seven parallel calls. Continuation A adds a 24,000-byte assistant text then a user message. Continuation B instead adds an eighth call then eight 4,096-byte tool results. Run against the actual library:

```text
shared seq=9; A=[1..10, 10..11]; B=[1..2, 2..10]
A: key=0@9, actual source selected globally=Some(Chunk { message: Some(0), items: [1, 2, 3, 4, 5, 6, 7, 8, 9] })
B: key=0@9, actual source selected globally=Some(Chunk { message: Some(0), items: [1, 2, 3, 4, 5, 6, 7, 8, 9] })
branch A independently: key=0@9, items=[1, 2, 3, 4, 5, 6, 7, 8, 9]
branch B independently: key=0@9, items=[2, 3, 4, 5, 6, 7, 8, 9]
```

The generic tree API permits this graph. After independent reproduction, the existing rationale was found to acknowledge precisely this case and defer it because observed Claude forks occur at user messages. Thus this is a documented restriction of the generic model, a strong blocker before supporting arbitrary Codex/Pi forks, rather than evidence that ordinary existing Claude user-message forks already fail. The linear append-stability test does not test this property. Sources: [rationale, Keys paragraph](../../../../../ai/comments/src/chat/rollup.rs.md), [existing test](../../../../../src/chat/rollup.rs#L489), [Claude chain accepts older-parent assistant entries](../../../../../src/chat/claude.rs#L197).

**Confirmed: derived summaries have no recipe/input identity.** Persisted `Line` has timestamp, kind, model provenance and text, with neither exact source range/hash nor prompt/render/projector version. `Run::pass` treats every stored key as valid; `put_lines` returns an existing line without checking its input. Changing clipping, step segmentation, prompt semantics or parser projection leaves old summaries mixed with new ones until explicit rebuilding. A model override intentionally applies to newly built lines only; that policy should remain explicit rather than making a model change automatically discard useful content. Sources: [Line](../../../../../src/chat/record.rs#L97), [line serialization](../../../../../src/chat/record.rs#L324), [cache loading](../../../../../src/chat/summarise.rs#L425), [first write wins](../../../../../src/chat/store.rs#L331).

**Measured: metadata commands read whole normalized and raw histories.** `Chat::list` calls `snap.tree()` only to count turns. `Snapshot::tree` loads every `Item`, and `items_in` fetches and UTF-8 decodes both normalized and raw blobs for every item. The view loads every branch's items and every stored summary, even for a single session's branch. `fresh_view` loads once to calculate needed summaries, then again to render, even when no summaries were needed. Sources: [list](../../../../../src/chat/store.rs#L185), [items_in](../../../../../src/chat/store.rs#L409), [tree](../../../../../src/chat/store.rs#L461), [view load](../../../../../src/chat/view.rs#L154), [fresh_view](../../../../../src/chat/cli.rs#L342).

Probe [read-growth.rs](evidence/read-growth.rs) creates a temporary repository with 1,000 source items / 500 turns; 500 assistant texts are each 128 KiB, source file 65,708,278 bytes. Three warm calls against the actual library:

| Operation | Observed durations |
|---|---|
| `Snapshot::heads()` | 4.90, 5.22, 5.49 ms |
| `Snapshot::tree()` | 83.91, 78.14, 75.70 ms |
| `Chat::list()` for one thread | 83.05, 93.63, 80.26 ms |

The process high-water mark was 264,972 KiB, including fixture creation and ingestion; it is **not** an isolated measurement of list memory. Identical text makes the dataset highly compressible and deduplicable; this is an eager-read-path probe, not a representative storage benchmark or a Git/SQLite comparison. At 65 MB this is tolerable; multiplied across long-lived threads/raw sources it becomes an avoidable latency and memory tax.

**Measured: fold planning is quadratic.** `fold` starts with a vector of one coordinate per closed turn, repeatedly scans the vector to choose one merge, then splices it. Fixed 64-line output still requires roughly T merges scanning O(T) entries. `needed()` and rendering both call this planner. Source: [fold](../../../../../src/chat/view.rs#L61), [plan](../../../../../src/chat/view.rs#L397). Probe [fold-growth.rs](evidence/fold-growth.rs) copies that function unchanged and compiles it with `rustc -O`, using the actual public `Coord` and existing `memory::cover`:

| Closed turns | Exact chat fold | Existing memory cover |
|---|---:|---:|
| 1,000 | 2.81 ms | 0.426 ms |
| 10,000 | 331.56 ms | 0.398 ms |
| 20,000 | 1.312 s | 0.385 ms |

The two planners have different selection policies, so the fast cover is not a drop-in behavioral equivalence claim. Decide whether chat needs its precise urgency ordering. If so, retain it with an efficient priority queue/tree; if not, adopt the existing shared cover policy. Source: [memory cover](../../../../../src/memory/cover.rs#L26).

**Confirmed: 512 bytes is a target, not a guaranteed bound.** The shared `write_line` retries an overlong reply up to five times, then returns the shortest even if still over 512 bytes. Chat's fold counts lines as cost 1, and its advertised worst-case context size assumes 512-byte lines. A persistently verbose backend can therefore exceed that context bound. The memory rationale deliberately calls `NODE` a target; chat's public claims need to reflect that choice, or chat must validate stored line/output budgets. Sources: [write_line](../../../../../src/memory/compact.rs#L89), [fold costs](../../../../../src/chat/view.rs#L397), [memory rationale](../../../../../ai/comments/src/memory/compact.rs.md), [design size claim](../../../../../ai/research/20261014T002322---chat-design.md).

**Confirmed: input cap does not cap the long-turn reduction.** Chunk inputs are planned around 48 KiB, but `Source::Chunked` concatenates all chunk summaries plus the message into one request. There is no recursive reduction among chunks. With roughly 100 full 512-byte chunk summaries, this final request already exceeds the nominal chunk input budget; larger turns continue growing. Oversize tool-call/result pieces are also explicitly allowed. Sources: [chunked input](../../../../../src/chat/rollup.rs#L356), [budget](../../../../../src/chat/rollup.rs#L15), [oversize pieces](../../../../../src/chat/rollup.rs#L38).

**Cost behavior is eager and unbounded per successful run.** For T closed turns on a linear branch, there are T turn lines and T−popcount(T) binary merge nodes, plus any chunk lines. Short inputs can be free. Otherwise each line starts its own isolated Claude conversation, potentially with up to five length retries; background summarization covers all branches and all ready nodes. `Built.calls` counts successfully completed line jobs, not total attempts/API turns; failed jobs and length retries are not represented in that count. Source: [nodes](../../../../../src/chat/rollup.rs#L189), [all-ready planning](../../../../../src/chat/summarise.rs#L437), [per-job spawning](../../../../../src/chat/summarise.rs#L542), [counting](../../../../../src/chat/summarise.rs#L590), [backend creates process](../../../../../src/memory/claude.rs#L320).

### Inferences

- Fix chunk identity using its actual source span or content-addressed input identity, and distinguish this from the printed branch-relative `t.i+n` address. Include summary recipe version, source projection version, clipping rules and child summary hashes in invalidation. Merely adding `first` solves the demonstrated collision but not stale recipes/children. `--rebuild` cannot make a colliding key represent two different spans simultaneously.
- Store a cheap immutable graph/index containing parent sequence, kind/turn opener and provenance; load content only for the selected branch/items. Keep raw source references separate from normal `Item` reads. Count turns from graph metadata. Retain a loaded branch through `fresh_view` when its snapshot did not change.
- Ingestion should archive each complete raw source event independently of whether the current projector understands it. The current `Item.raw: String` makes every ordinary traversal pay for raw material and duplicates one event across all its projected items in memory. Git already deduplicates identical blob bytes on disk; a raw-event reference makes that sharing explicit in the model and allows later reprojection.
- Retain binary summaries and exact zoomability. Before experimenting with other summary trees, add small fidelity fixtures: corrections and superseded decisions, tool errors, missing middle-of-output findings, parallel results, agent handoffs, citations after forks. Current fake models establish scheduling and spans, not semantic recall quality. No need for a broad evaluation framework yet.
- Give callers an accurate plan before model work: missing lines, estimated input bytes, actual request/attempt totals and a run budget. The current `--dry-run` does call the model; an inexpensive planning mode is a different and useful operation. Consider summarizing only active-branch dependencies immediately and deferring unused nodes, after measuring the cost/latency tradeoff.
- A strict byte bound can reject oversized summaries and keep fallbacks rather than silently truncating important text. Alternatively keep best-effort lines but budget the rendered fold by actual byte/token costs. Pick the product contract explicitly.

### Gaps

- No real LLM fidelity/cost benchmark was run. Source-derived call counts are not billing estimates.
- No existing real transcript exhibiting the non-user-fork chunk collision was sought; the reproducer tests the supported generic graph. Existing rationale says observed Claude forks avoid it.
- The benchmark probes are local single-process timings, not distributions across machines or representative long-lived datasets.

## 2. Readability, boundaries and shared primitives

### Takeaway

The current feature already has useful pure/effect boundaries. Preserve them, extract the truly shared utilities from `memory`, and create one small harness boundary before adding Codex/Pi; avoid a generic storage framework or generic summary DAG engine.

### Cited Findings

- `model` defines the harness-neutral graph, turns and steps. `render` formats one item. `rollup` plans sources/tasks without I/O. `summarise` executes against an injected `Backend`. `view` projects a selected branch. These are coherent existing boundaries worth keeping. Sources: [model](../../../../../src/chat/model.rs), [render](../../../../../src/chat/render.rs), [rollup](../../../../../src/chat/rollup.rs), [summarise](../../../../../src/chat/summarise.rs), [view](../../../../../src/chat/view.rs).
- The adapter leaks into persistence: `store::Transcript.agents` is `Vec<claude::Subagent>`, and `ingest_agent` takes that provider type. `sync::read` dispatches only Claude. Session detection, transcript location and launching/resuming Claude are in the CLI. Sources: [Transcript](../../../../../src/chat/store.rs#L116), [ingest_agent](../../../../../src/chat/store.rs#L607), [read dispatch](../../../../../src/chat/sync.rs#L39), [session/launch orchestration](../../../../../src/chat/cli.rs#L193).
- `SourceId` is `(harness, session, entry)` but dedupe deliberately uses `(harness, entry)` to recognize Claude copied prefixes. It is a reasonable Claude identity policy, not a demonstrated present UUID collision. A generic harness will need a documented canonical event identity separate from the session in which it was observed. Sources: [SourceId](../../../../../src/chat/model.rs#L8), [dedupe](../../../../../src/chat/store.rs#L96), [Claude rationale](../../../../../ai/comments/src/chat/claude.rs.md).
- Chat imports shared backend, Claude process runner, bounded-line writer, binary coordinate, NODE, logging, flat text, pattern, shell-quoting and pagination through `memory`. Notes also imports many of the same helpers. These helpers are not conceptually owned by permanent memory. Sources: [chat summarizer imports](../../../../../src/chat/summarise.rs#L18), [chat view imports](../../../../../src/chat/view.rs#L6), [chat CLI imports](../../../../../src/chat/cli.rs#L26), [notes imports](../../../../../src/notes/cli.rs#L16), [memory reexports](../../../../../src/memory/mod.rs).
- There are three age/detail selection implementations: chat `fold`, memory `Mem::fit`, memory `cover`. They have different policies; do not mechanically replace them because their names sound similar. There is also duplicated compactor worker bookkeeping in memory/chat: channels, worker threads, tracing dispatcher, running/failed sets, completion timing and cancellation. Notes uses scoped batch threads with different lifecycle semantics. Sources: [chat fold](../../../../../src/chat/view.rs#L61), [memory view](../../../../../src/memory/view.rs#L125), [memory cover](../../../../../src/memory/cover.rs#L26), [chat Run](../../../../../src/chat/summarise.rs#L356), [memory Compactor](../../../../../src/memory/compact.rs#L181), [notes round](../../../../../src/notes/cli.rs#L485).
- Shared low-level Git CAS and text record primitives already exist: `git::Ref`, `Snap`, `Changes` and `record::{encode,decode,Seg}`. `rollup::branch` and `Rollup::branch` already share the actual cutter, so these similar entry points are not algorithm duplication needing a new abstraction. Sources: [Git primitives](../../../../../src/git.rs), [record primitives](../../../../../src/record.rs), [shared cutter](../../../../../src/chat/rollup.rs#L225).
- Notes already hashes the input for a cached summary and rechecks it immediately before writing. Chat does not do that. This is a concrete existing pattern to reuse conceptually, not grounds for replacing both with one universal summarizer. Sources: [notes input hash](../../../../../src/notes/summary.rs#L66), [notes write validation](../../../../../src/notes/cli.rs#L506).

### Inferences

Suggested small layout, mostly retaining existing files:

```text
chat/
  cli.rs                 clap/options and output, calls application operations
  harness/mod.rs         supported Harness enum; CurrentSession, Transcript, AgentRun
  harness/claude.rs       detection, discovery, reading, launch/resume plans
  model.rs               canonical IDs, item kinds, graph and turns
  sync.rs                attach/detach/sync transaction rules
  store.rs + record.rs   snapshots, persistence and typed wire records
  rollup.rs              pure summary graph and task planning
  summarise.rs           lock + model execution + checked writes
  render.rs + view.rs    item formatting and branch projection
llm/mod.rs + claude.rs   Backend, Conversation, Block, provenance, bounded-line helper
text.rs / output.rs     only helpers already used by multiple features
git.rs                  existing CAS/tree/object primitives
```

Use a closed enum plus straightforward dispatch for three known harnesses initially. There is no demonstrated need for dynamic discovery, a plugin registry, multiple selectable storage backends or a trait for every operation. Keep a transcript adapter separate from an LLM backend: importing Codex chat logs must not force summary generation to use Codex, nor should adding a summary model require implementing session hooks.

Small API sketch, not a full framework:

```rust
struct CurrentSession { harness: Harness, session: SessionId, transcript: PathBuf }
struct Transcript { events: Vec<RawEvent>, items: Vec<Item>, agents: Vec<AgentRun> }
enum Harness { Claude, Codex, Pi }
// Harness-specific functions return these values; core persistence imports no Claude type.
// Store operations receive an observed source generation/checkpoint, not just a vector of items.
```

Read the source in this order:

1. `src/chat/cli.rs` command declarations and dispatch: learn the user-facing surface; skip command bodies initially.
2. `src/chat/model.rs` and `id.rs`: source identity, parent links, turns/steps, branch-relative addresses.
3. `src/chat/claude.rs` and `sync.rs`: how source events become items, and what attaching/grafting changes.
4. `src/chat/record.rs`, `store.rs`, then `src/git.rs`: typed records, immutable snapshots, append and CAS.
5. `render.rs`, `rollup.rs`, `summarise.rs`: pure task planning followed by effects/scheduling.
6. `view.rs`, then CLI command bodies: the fold, latest user rail, current turn, zoom/search/pagination.
7. Claude plugin glue: after understanding the CLI contract, see how hooks call it (covered by CLI reviewer).

Read tests alongside each layer. The highest-value new tests are invariants across layers: two branch continuations yielding different chunk spans, source reprojected under a new version, reversed sync completion order, and competing attach operations. Avoid tests that only restate a helper's implementation.

### Gaps

- Module names are recommendations, not required abstractions. The smallest useful extraction is shared LLM/text utilities and provider-neutral transcript types; file splitting alone does not repair transaction or cache invariants.
- Worker bookkeeping looks similar but memory and chat intentionally differ on retries and lifecycle. Keep separate executors unless a small shared completion helper demonstrably removes repeated code without hiding policy.

## 3. Storage engine, format and transaction recommendation

### Takeaway

For code simplicity and correct transactional state, SQLite is the stronger default target if Git-addressable historical whole-store snapshots are not a valued product feature. Repo-local storage, worktree sharing, retained raw events and conversation forks do not require Git. Keep Git if its immutable snapshots/OID pinning and object/transport integration are important; either design still needs correct source identities, raw archival and summary invalidation.

### Cited Findings

- Current Git writes are already content-addressed blobs, trees and commits. `write_tree` calls `write_blob`; raw duplicate bytes can share the same object even though many paths reference it. A ref CAS publishes one complete snapshot, with the old commit as parent. Sources: [write_tree](../../../../../src/git.rs#L174), [new_commit](../../../../../src/git.rs#L190), [CAS](../../../../../src/git.rs#L201).
- `refs/ai/chat/*` lives in shared repo state. Git documents that ordinary `refs/*` are shared across worktrees, apart from specified special namespaces. The implementation also puts lock/log paths under the common directory. Sources: [Git worktree refs](https://git-scm.com/docs/git-worktree#_refs), [Chat::thread](../../../../../src/chat/store.rs#L157).
- Git keeps reachable history under `refs/*`; therefore removing a summary from the latest tree does not remove its old bytes from parent commits. That satisfies historical retention but means derived summaries/rewrites also accumulate permanently. Source: [Git gc notes](https://git-scm.com/docs/git-gc#_notes), [drop_lines](../../../../../src/chat/store.rs#L291).
- Per-thread CAS does not make a multi-thread attach transfer transactional. Attachment is currently inferred by scanning bindings across refs, selecting the newest timestamp, then detaching the old thread before mutating the target. The core reviewer reproduced concurrent double bindings and detached-on-failure behavior; these observations are their evidence, not a new reproduction in this subreview. Source: [attached/attach](../../../../../src/chat/sync.rs#L61).
- Git offers multi-reference transactions, and the existing pinned `gix 0.88` already exposes `Repository::edit_references`; no new library or invented Git locking layer is needed to batch known ref edits. However Git explicitly warns a concurrent reader can see a subset of multiple ref updates, so this is not a consistent repo-wide snapshot for scanning readers. Sources: [Git update-ref transactions](https://git-scm.com/docs/git-update-ref), [gix API](https://docs.rs/gix/0.88.0/gix/struct.Repository.html#method.edit_references). Local type/source was checked at `/home/hatim/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/gix-0.88.0/src/repository/reference.rs:124`.
- Ordinary configured fetch maps `refs/heads/*` to remote branches. Custom chat refs do not automatically follow normal clone/fetch/push of code branches. `git clone --mirror` maps all refs; explicit refspecs or a bundle/export command are appropriate for chat backup. Merely saying the data "moves with the repo" is incomplete if a user means ordinary remote cloning. Sources: [Git fetch configured branches](https://git-scm.com/docs/git-fetch#_configured_remote_tracking_branches), [Git clone --mirror](https://git-scm.com/docs/git-clone), [Git bundle](https://git-scm.com/docs/git-bundle).
- SQLite is designed for embedded application-local data and provides transactions and indexes. WAL permits readers alongside one writer, with same-host restrictions and checkpoint/-wal/-shm considerations. A DB under the common Git directory can be shared by local worktrees; SQLite does not require a server. Sources: [SQLite appropriate uses](https://www.sqlite.org/whentouse.html), [transactions](https://www.sqlite.org/lang_transaction.html), [WAL](https://www.sqlite.org/wal.html).
- SQLite FTS5 supplies full-text search, but it does not make arbitrary Rust regex searching behavior equivalent automatically. Its backup API provides a consistent live DB snapshot; copying only the DB file while WAL is active is not a sufficient universal backup procedure. Sources: [FTS5](https://www.sqlite.org/fts5.html), [backup API](https://www.sqlite.org/backup.html), [WAL file](https://www.sqlite.org/wal.html#the_wal_file).
- Current record encoding manually flattens newlines in headers, omits empty values, turns whitespace into underscores in IDs, and manually defaults several missing decoded fields to empty strings. The index is space-separated under a schema row, 256 records per segment. This is inspectable with Git but a custom format and parser that every future reader must understand. Sources: [record codec](../../../../../src/record.rs#L3), [chat word](../../../../../src/chat/record.rs#L107), [kind codec](../../../../../src/chat/record.rs#L190), [segments](../../../../../src/record.rs#L44), [missing field defaults](../../../../../src/chat/record.rs#L220).

### Inferences

**Recommended transaction shape:** one authoritative repo-wide transaction boundary with a single session→thread registry. An attach publishes ingestion/binding changes together. This removes the need to pick an attachment by wall-clock timestamps or to observe a half-transfer across independent refs. In SQLite, use one DB in the common Git directory and one short transaction with a unique `(harness, session)` binding. If retaining Git, one repo-state snapshot/ref containing thread manifests and the registry is the simplest corresponding design; a failed CAS reruns a short mutation. The tradeoff is brief CAS contention between different threads, likely acceptable for a few role threads. Keep model calls outside either transaction and validate the exact source/input generation on publish. A central binding registry alone can also establish authority while preserving per-thread refs, but avoid two equally authoritative attached flags.

**Recommended source/projection shape:** archive complete source events once as raw bytes, referenced by a manifest with source identity/checkpoint. Build typed normalized items as a versioned projection that references those events. Build summaries from exact source/child identities plus recipe version. Load raw bytes only for raw inspection or reprojection. Git already supplies content addressing; the improvement is clearer source ownership and lazy indexed reads, not adding another hash filesystem. Event archive and projection are distinct from backup retention: a retained partial projection cannot recover an event never archived.

**Recommended small format simplification:** if retaining Git, use serde-tagged JSON records for normalized items, bindings and summaries, and JSONL only where append/streaming actually helps. If choosing SQLite, use named typed columns for indexed identity/topology/binding fields and serde JSON for variable item payloads. Keep raw source bytes untouched in either case. For example the existing call record is:

```text
kind call
call c1
name Read

{"file_path":"src/main.rs"}
```

A typed representation is:

```json
{"kind":"tool_call","id":"c1","name":"Read","input":{"file_path":"src/main.rs"}}
```

An index row can likewise have named fields (`seq`, `parent`, `source`, `ts`, `kind`) rather than positional spaces and `word()` rewriting. Derive `Serialize`/`Deserialize`, validate IDs at the boundary, and reject missing required fields. JSON is more verbose and may need indexing/segmentation for large logs; Git compression often makes raw verbosity a secondary concern. No measured disk advantage is claimed. Keeping Git and changing the record codec are independent decisions. No compatibility reader/migration layer is required by this review; the user has authorized early refactoring.

| Option | Fit for these requirements | Main cost / reason to defer |
|---|---|---|
| Git canonical archive + typed records and cheap graph indexes | Preserves repo-local immutable history, source blobs, snapshots and branch sharing; uses existing dependency | Must design one authoritative transaction state, explicit backup/export, lazy reads and maintenance; Git schema is application work |
| SQLite authoritative store in common dir | Good local workload fit; unique session binding constraint, transactional thread moves, indexed lookup; can store raw event BLOBs and explicit append-only event history | Git snapshot/history/transport cease to be inherent; need explicit history/export/backup contract; replacing storage does not itself fix projector rules/cached spans |
| Append-only JSONL + derived index | Human readable, streaming, matches source event streams; small dependency footprint | Must implement writer serialization, crash-tail recovery, transaction grouping, snapshot checkpoints and durable index consistency; multi-record attach needs log transaction semantics; this reimplements database work |
| Git archive + rebuildable SQLite projection | Keeps durable source history while making metadata/search queries cheap; cache keyed by archive commit + projector version | Two systems and rebuild logic; worthwhile only after measured indexed-query needs justify it; never dual-write two authoritative sources |

**What would SQLite actually simplify?** Replace custom fanout paths, index segment parsing, `Changes`, ref CAS/retry, cross-ref session scans and timestamp arbitration with one schema and short transactions. Keep tables for threads, session bindings, immutable raw events, normalized items with parent links and source-event references, and summaries keyed by exact inputs/recipe. Retain original raw bytes as BLOBs. An append-only event table or versioned rows can preserve changes when that is wanted. Conversation branching remains parent pointers; worktree sharing remains one path under `common_dir`.

**What would SQLite not provide automatically?** Today's persistent `--at <Git commit>` across separate CLI invocations, Git object reachability/history, existing Git-based inspection and refspec/bundle transport. A normal SQLite read transaction is consistent only for its lifetime; it does not reproduce an old revision on a later invocation. Preserve pinned pagination with a persisted rendered-view artifact or explicitly versioned rows if that capability remains part of the interface. Archival/backup/export must be designed, not inferred from the DB filename. These are the real reasons to retain Git, rather than locality or raw retention.

Choose SQLite authority now if the main priorities are readable application code, atomic state changes, cheap indexed access and local operation, and Git-addressable historical snapshots are incidental. This is a reasonable early refactor, not only a response to hypothetical future scale. Keep Git if its snapshot/archive semantics are deliberately part of the product; then fix transaction shape, typed records and lazy reads first. Choose the hybrid only if both immutable Git archival and large indexed-query needs are demonstrated; it is the most moving parts. Do not implement multiple selectable backends or a migration compatibility layer simply to postpone this decision.

### Gaps

- No Git-vs-SQLite throughput or space benchmark was performed; no engine is claimed faster for this application based solely on documentation.
- Precise power-loss durability of the current gix/object/ref path was not fault-injected. Git CAS/snapshot consistency is not a claim that all filesystem write policies were audited.
- Repo backup/remote transport is a product decision. Existing design says threads are never pushed automatically; the review recommends making explicit archival/export behavior clear rather than automatically uploading transcripts.
