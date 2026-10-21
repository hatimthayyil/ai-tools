# Memory storage, provenance, merge, zoom, grep

Research for `ai memory` (Rust port of OptMem). No code changed. Date: 2026-10-03.

## Summary

Recommendation, in one paragraph: store the memory **only in git objects under a custom ref `refs/ai/memory`**, with no checkout and no files in any worktree. Write blobs/trees/commits and CAS the ref in-process with `gix`; shell out to `git` for fetch/push (gix has no push; jj made the same split). One commit per mutation (`note`, `nap`, `forget`, `merge`). Records stay fixed-width (512 B) inside fixed-count segment blobs, so lookups stay O(1) after one inflate. Records carry provenance inline and **never embed their position**. The log is kept **strictly sorted by key `(ts, origin)`**, so merging two clones is a sorted set union: commutative, associative, idempotent. Tree summaries carry a fingerprint of the memory keys they cover. After a merge, any summary whose fingerprint still matches is reused, from either side and at any offset. Everything else falls back to the existing "pending → nap" flow. `wake` runs a best-effort sync. Zoom defaults to depth 3 (8 nodes) and jumps straight to raw memories for blocks of 16 or fewer. `recall` becomes `grep`: rg-style regex with smart-case, log by default, `--tree` to include summaries. Use a linear scan; tantivy is not warranted.

| Req | Decision |
|---|---|
| 1 Not in trunk | Custom ref `refs/ai/memory`, plumbing only (gix + `git` for transport). No worktree, no orphan *branch*. Shared by all linked worktrees for free. |
| 2 Versioned + provenance | Commit per mutation. 512 B record: `ts origin head branch agent model session text`. Key `(ts, origin)` is unique and stable. Position is display/navigation only. |
| 3 Merge | `ai memory sync` (also run by `wake`): fetch, sorted union of logs, reuse summaries by fingerprint (last writer wins per block), merge commit, CAS, push, retry. |
| 4 Zoom | `zoom <id> [--depth N]`, default 3. Shows the frontier only. Blocks ≤16 open to raw memories. Cap 64 lines. |
| 5 Grep | `grep <pattern> [-F] [-i/-s] [--tree] [-C N] [--before <pos>]`. Newest page first, positions printed. Streaming scan; no index. |

## Current state (baseline)

- Store: `<worktree root>/.ai/memory/` (gitignored). Each linked worktree gets its own store, because `repo::root` stops at the nearest `.git` entry.
- `LOG.txt`: 320 B records `#<id> <date> <text>`, padded. The position is embedded in the record text.
- `TREE/<size>`: 288 B summaries, a dense prefix per level, built smallest-first.
- Concurrency: `flock` on `.lock`. `config`: per-store knobs file.
- No provenance beyond the date. No sync.

Embedding `#id` in the record is the first thing that has to go. Any merge that inserts upstream of a record would otherwise rewrite every later record.

---

## R1. Where the memory lives

### Options

| Option | Trunk conflicts | Agent search pollution | Shared across worktrees | Versioned / syncable | Complexity | Verdict |
|---|---|---|---|---|---|---|
| A. Files in trunk (`.ai/memory/` committed) | yes, on every merge | yes | via git only | yes | low | rejected (req 1) |
| B. Files, gitignored (today) | none | rg: no (ignored). GNU `grep -r`: **yes** | **no**: one per worktree | no | low | rejected |
| C. Orphan **branch** checked out as a worktree at `.ai/memory/` | none | rg/ugrep: no if gitignored. GNU `grep -r`: yes. IDE indexers, watchers: yes | one checkout; other worktrees must locate it | yes, but needs index/add/commit per note, and merge needs a worktree merge plus a driver | medium-high | rejected |
| D. Orphan branch, hidden worktree under `.git/` (old beads `sync.branch`) | none | rg `--hidden`: **yes**. GNU `grep -r`: yes | yes | yes | high | beads **removed** this |
| E. Working files in `$(git rev-parse --git-common-dir)/ai/memory` + snapshot commits to a ref on sync | none | rg default: no. rg `--hidden`: **yes**. GNU `grep -r .`: **yes** | yes | yes, coarse (per sync) | medium: two sources of truth | fallback |
| F. **Custom ref, objects only** (`refs/ai/memory`, gix/plumbing) | none | **none**: objects are zlib-compressed and packed | yes: refs outside `refs/{bisect,worktree,rewritten}` are shared | yes, per mutation, atomic CAS | medium | **recommended** |
| G. git notes (`refs/notes/ai`) | none | none | yes | yes; `cat_sort_uniq` union merge | notes attach to *objects*; memory is not about commits; no order control | rejected |
| H. Orphan branch `refs/heads/ai-memory`, plumbing only | none | none | yes | yes | same as F | inferior to F (see below) |

Checked locally (git 2.55, rg 15.2, this machine's `grep` is Claude Code's ugrep shim with `--exclude-dir=.git --ignore-files`):

- `rg needle` sees neither `.git/ai/memory/LOG.txt` nor gitignored `.ai/memory/LOG.txt`.
- `rg --hidden` **does** search inside `.git/` and hits `.git/ai/memory/LOG.txt`. Only `.gitignore` saves `.ai/memory`.
- `command grep -r needle .` (GNU) hits both.
- Packed or loose git objects are compressed, so no text search tool matches them.

So only F/G/H are fully immune to pollution. E/D leak under `rg --hidden` and GNU grep.

### Branch vs custom ref (F vs H)

- A branch is fetched by every `git fetch` (default refspec `refs/heads/*`). It appears in branch lists, PR "compare" banners, and GitHub branch UI.
- A branch push **triggers CI** for workflows with `on: push` and no branch filter.
- A branch may hit branch-protection rules.
- Custom refs avoid all of that. Precedent: git-bug `refs/bugs/<id>`, Dolt/beads `refs/dolt/data`, Gerrit `refs/meta/config`, git-appraise `refs/notes/devtools/*`.
- A custom ref costs one explicit refspec. `init` adds a **fetch** refspec `+refs/ai/memory:refs/ai/remotes/origin/memory` to `remote.origin.fetch`. Plain `git fetch` then also brings the memory, which is harmless.
- **Never** add a `remote.origin.push` refspec. Once one is set, a bare `git push` pushes only the listed refspecs and stops pushing the current branch.
- Verified: `git clone` does not copy `refs/ai/*`; an explicit fetch does.

### Why F over E

- One source of truth: no dirty-vs-snapshot state, no crash window between files and ref.
- Atomic CAS on the ref replaces `flock`: build the commit, then update the ref only if it still points at the commit you read. Losers retry. Every command reads one commit, which gives snapshot isolation (`wake` parts read the same state).
- History per mutation, for free. Provenance is also inline, so history is a bonus, not a dependency.
- Global memory (backlog) becomes the same thing in a bare repo, e.g. `~/.ai/memory.git`, with the same ref. One code path.
- Costs: write amplification and `git gc` (see Risks); an inflate per segment read; the gix dependency (pre-1.0, API churn).

### Physical layout inside the ref

```
refs/ai/memory -> commit -> tree
  log/<hi>/<lo>              segment n = records [256n, 256n+256), path = {n>>8:04x}/{n&255:02x}
  tree/<size>/<hi>/<lo>      same segmentation per level (size = 2,4,8,...)
```

- Segment = 256 records × 512 B = 128 KiB raw; padding compresses ~10×.
- Two-level fanout keeps each tree object ≤256 entries (~9 KB), so a note rewrites one tail blob plus three small trees plus one commit.
- Reads: record i = inflate `log/seg(i)`, slice `(i%256)*512`. `wake` touches ≤96 records in a handful of segments, about a millisecond.
- No `config` in the ref. Knobs move to git config (`git config ai.memory.wakeLines 96`), which is per clone, unversioned, and needs no merge. Knobs are reading budgets tied to the harness/model on that machine, so per-clone is right.
- Per-clone state lives in git config too: `ai.memory.origin` (clone id), `ai.memory.remote` (default `origin`).

### Library split

- `gix` handles in-process local work: `write_blob`, `edit_tree`, `new_commit`/`commit_as`, `edit_reference` with `PreviousValue::MustExistAndMatch` (the CAS), `merge_base`, and `common_dir`.
- The `git` CLI handles `fetch`/`push`, so the user's ssh config, credential helpers, and `insteadOf` rules just work.
- Precedent: jj moved fetch/push to a `git` subprocess (default since 0.26/0.29; libgit2 path removed in 0.30) because library transports lagged git's auth/ssh behaviour.
- Rejected alternatives:
  - All-CLI plumbing (`hash-object`, `mktree`, `commit-tree`, `update-ref`): about 6 subprocesses per note, plus text parsing. Viable, but brittle.
  - git2/libgit2: C dependency, and jj abandoned it for transport.

---

## R2. Versioning and provenance

### Identity

| Scheme | Unique across clones | Sortable | Human-readable | Merge-friendly | Notes |
|---|---|---|---|---|---|
| Position (today) | no | yes | yes | no: renumbers | keep for **display/navigation** only |
| Content hash | yes (dup text → dup id) | no | no | dedup is free, order is not | git-bug op ids; no order |
| ULID | yes (80 random bits) | yes (ms) | no | yes | 26 chars, opaque to the agent |
| **`YYYYMMDDThhmmssZ` + origin** | yes, with the bump rule | yes | yes | yes | **recommended** |
| Lamport clock + origin | yes | causal, not wall | no | yes | git-bug uses Lamport; wall time matters to the agent |

Rules:

- `ts` is UTC, ISO 8601 basic format with `Z` (`20261003T142233Z`), 16 bytes. UTC because clones span timezones; `Z` so nobody guesses.
- `origin` is 6 Crockford base32 chars (30 bits), random per clone, created on first write, stored in `ai.memory.origin`.
  - Not the hostname: two clones on one host must differ.
  - Not the git email: one human runs many clones.
- Invariant: **LOG is strictly sorted by `(ts, origin)`.** Append rule, inside the CAS:
  - `ts = max(now, tail.ts)`.
  - If `(ts, origin) ≤ tail.key`, then `ts = tail.ts + 1s`.
  - This is a hybrid-logical-clock in miniature. Keys are unique. Bursts drift by seconds. A clone whose clock runs ahead drags later appends forward by at most its skew.
  - Warn if `tail.ts > now + 1 day`.
- Consequence: identical keys mean an identical memory, so dedup is exact. Merge is a linear sorted union (R3).

### Which provenance fields

| Field | Source | Keep? |
|---|---|---|
| ts, origin | tool | yes (key) |
| head | main repo `HEAD`, abbreviated 12 hex | yes: ties a memory to code state; cheap |
| branch | main repo branch, truncated 32 | yes: the user asked for it; `-` if detached |
| agent | `$AI_AGENT` (set by Claude Code here: `claude-code_2-1-288_agent`), else detected (`CLAUDECODE`, Codex env), else `-` | yes |
| model | `$AI_MODEL` or `note --model`; **not exposed by Claude Code env** | yes, often `-` |
| session | `$CLAUDE_CODE_SESSION_ID` (present here), other harness equivalents, `$AI_SESSION` | yes |
| human author | `git config user.email` | no: already in the commit author, and personal data |

Inline beats commit-trailer-only. `grep` and `show` need provenance without walking history, and history may be squashed one day. Commit message and trailers still record the command (`note`, `nap 16-31`, `merge <remote>`).

### Record format: LOG (512 B, fixed)

```
off  len  field     content
  0   16  ts        YYYYMMDDThhmmssZ
 16    1  ' '
 17    6  origin    Crockford base32
 23    1  ' '
 24   12  head      hex, '-' if none
 36    1  ' '
 37   32  branch    ASCII, no spaces, truncated, '-' if none
 69    1  ' '
 70   32  agent     same rules
102    1  ' '
103   32  model     same rules
135    1  ' '
136   36  session   same rules (UUID fits)
172    1  ' '
173  338  text      UTF-8, ≤ ENTRY_CHARS (default 280, max 338), space padded
511    1  '\n'
```

- Meta fields are ASCII, space-padded on the right. Whitespace becomes `_`; non-ASCII becomes `?`.
- Text is last, single line, with tabs and CR normalised to spaces.
- Decode = fixed slices plus `trim_end`. Every record is still one greppable line.

### Record format: TREE (512 B, fixed)

```
off  len  field     content
  0   16  ts        when napped
 16    1  ' '
 17    6  origin
 23    1  ' '
 24   16  fp        hex, first 64 bits of SHA-256 over the block's memory keys "ts origin\n"...
 40    1  ' '
 41   32  agent
 73    1  ' '
 74   32  model
106    1  ' '
107   36  session
143    1  ' '
144  367  text      summary ≤ ENTRY_CHARS
511    1  '\n'
```

- `fp` makes every summary self-validating against any log. That one property drives the whole merge design.
- Display stays terse:
  - `wake`: `#<pos> <YYYY-MM-DD> <text>`.
  - `zoom`/`grep`: add `hh:mm` and origin.
  - New `show <pos>` prints full provenance.
- Byte budget: 512 B × 1M = 512 MB raw, ~1 GB packed with history (see Risks). LOG and TREE records now share one width, which removes the `LOG_REC`/`TREE_REC` split.

### Fixed width, still?

- Inside a decompressed segment, fixed width keeps O(1) slicing, trivial corruption detection (length % 512), and the existing code shape.
- Variable-width lines would save ~50% of *raw* bytes. Git compression erases most of that, and lookups would need a memchr pass per segment.
- Keep fixed width.

---

## R3. Merge

### Algorithm (`ai memory sync`)

```
sync(remote = ai.memory.remote or "origin"):
  loop up to 3 times:
    1. git fetch <remote> +refs/ai/memory:refs/ai/remotes/<remote>/memory
       (GIT_TERMINAL_PROMPT=0, ssh ConnectTimeout=3; offline -> warn, skip to step 6 local-only)
    2. L = local ref, R = remote-tracking ref
       R absent or R ancestor-of L        -> goto 5
       L ancestor-of R or tree(L)==tree(R) -> CAS local := R; goto 5
    3. LOG:  M = sorted_union(log(L), log(R)) by key; equal keys -> one copy
             (bytes differ -> keep min bytes, warn: corruption)
             p = first index where M != log(L)     (renumbering starts here)
    4. TREE: for size s = 2,4,8,...:
               pool_s = { fp -> record } from L and R level s (all blocks, any offset)
                        on fp clash keep max (ts, origin)   (last-writer-wins)
               for k = 0..|M|/s:
                 need fp(M[ks..ks+s]); take pool_s[fp] else stop (dense prefix)
               also clamp: count(2s) <= count(s)/2
             write merged tree; commit parents [L, R], msg "merge <remote>", trailers
             CAS local: L -> merge   (fails if a local note landed: restart at 2, no refetch)
    5. git push <remote> refs/ai/memory:refs/ai/memory     (never --force)
       rejected (non-fast-forward) -> continue loop
       ok -> break
  6. git gc --auto --quiet
  7. print: "Merged N memories from <remote>; positions from #p renumbered; K summaries to redo. Run: ai memory nap"
```

Properties:

- **Convergent.** The log merge is a G-Set under a total order. The tree merge is a per-fingerprint LWW register. Any sync order yields identical trees on every clone, so racing syncs create one extra no-op merge, or a fast-forward via the `tree(L)==tree(R)` shortcut.
- **No merge base needed.** Two clones that each ran `init` (unrelated histories) merge the same way.
- **Truncation is automatic.** Blocks entirely before `p` keep their fp and survive. Blocks entirely within one side's run survive when the other side's items all sort after them. In the common "laptop in the morning, desktop at night" case, only the later side's run and the spine are redone.
- **Re-nap triggering needs no new mechanism.** `pending` lists the missing blocks, `wake` refuses when its cover needs one, and `note` hands out the next nap. Two clones that re-nap the same block resolve by LWW on the next sync.
- **`forget` + nap beats the old summary** under max-ts LWW. `forget` alone (no re-nap) is undone by a sync that re-imports the remote copy. That is acceptable: forget exists to be followed by nap.
- **Concurrency with local sessions.** Everything is a CAS on one ref, so notes during a sync just make sync retry step 4. No lock file.
- **Stale nap prompts.** A prompt printed before a merge may name a block whose members changed. Fix: the prompt prints `Run: ai memory nap 16-31@3fa9 "<line>"`, where `@3fa9` is the first 4 hex of fp. `nap` refuses on mismatch ("block changed by a sync; Run: ai memory nap"). This costs 5 characters and buys correctness.
- **Stale positions** in an agent's context after a mid-session sync: harmless but confusing. Sync runs only in `wake`/`sync` and reports `p`.

### Why not a merge driver

- `merge=union` (gitattributes): "lines from both versions... in random order". No sort, no dedup, no tree logic.
- A custom `merge.<x>.driver` needs `git merge` on a checked-out branch. Verified: `git merge-tree --write-tree` honours `.git/info/attributes` drivers, but only per file, and our merge is cross-file (log, then tree via fp).
- git-bug and Dolt both merge custom refs in-tool. Same pattern here.

### When to sync

| Trigger | Pro | Con | Recommend |
|---|---|---|---|
| explicit `ai memory sync` | predictable | agents forget | yes |
| `wake` (start of every session) | catches up before reading; pushes the last session's notes | network latency at start; must not fail offline | **yes, best-effort, ~3 s timeout** |
| `note` | freshest | latency on the hot path | no |
| `git fetch` via refspec | free download | no merge, no push | yes (refspec only); `wake` merges the tracking ref locally even when offline |
| harness hook (Claude Code `SessionEnd`/`Stop` → `ai memory sync`) | pushes promptly | harness-specific | optional, documented |

OptMem's rule "nothing runs in the background" holds: there are no daemons.

### Cost of re-summarising

- A complete tree over n memories has n−1 internal nodes. Invalidating `[p, T)` costs about `(T−p) + log2 T` naps.
- Examples:
  - 2 clones × 50 divergent notes that interleave in time: ~100 + 20 = ~120 naps.
  - Non-interleaved: ~50 + 20.
- A nap is one short LLM turn (~1–3 s, ~400 tokens of input since ≤16 lines), so ~120 naps ≈ 3–5 min of agent time.
- It is spread across sessions anyway: `wake` demands only cover blocks, and the rest trickle in via `note`.
- Not worth optimising further now. Reuse by fp already catches shifted-but-identical blocks.
- If it hurts later: sync more often (smaller divergence); `pending` could prioritise blocks the cover needs.

---

## R4. Zoom depth

Facts:

- One line ≈ 280 B ≈ 75 tokens.
- Depth d shows the 2^d frontier nodes: d=1 → 2 lines (~150 tok), d=2 → 4 (~300), d=3 → 8 (~600), d=4 → 16 (~1.2k).
- A tool turn costs ~200–500 tokens of call and reasoning overhead plus seconds of latency.
- From a 2^20 block to a raw memory (raw shortcut at ≤16):

| depth | turns | tokens read | tokens incl. overhead (~300/turn) |
|---|---|---|---|
| 1 | 17 | ~2.5k | ~7.6k |
| 2 | 9 | ~2.7k | ~5.4k |
| **3** | **7** | ~4.2k | **~6.3k** (≈ depth 2, half the latency of d=2, 2.5× fewer turns than d=1) |
| 4 | 5 | ~6k | ~7.5k |

Evidence:

- Chroma "Context Rot" (18 models): accuracy drops as input grows, even on trivial tasks. **Even one semantically similar distractor hurts.** LongMemEval focused prompts (~300 tok) far outperform full prompts (~113k). Siblings in a zoom are exactly "similar distractors", so keep the frontier small. 8 lines (~600 tok) is deep in the safe regime; 64 lines (~5k) is still far below where the curves bend.
- RAPTOR (ICLR 2024): **collapsed-tree retrieval** (search all levels at once, fill a token budget) **beat top-down tree traversal**. Navigation is the weaker access path, so pair zoom with a search that also covers summaries (`grep --tree`, R5), rather than deepening zoom.
- Lost in the Middle (Liu et al.): mid-context items are used worst. Small outputs avoid it.
- MemGPT/Letta: archival/recall search is **paginated** in small pages, and the agent pages on demand. Mem0, Zep and A-MEM retrieve top-k with k≈5–10. Same order of magnitude as 8.

Recommendation:

- `zoom <lo-hi> [--depth N]`, default **3**, N ∈ 1..6. Output is capped at 64 lines and PART_CHARS, refusing larger with "use --depth K".
- Print the **frontier only**. Parents were already read.
- **Raw shortcut**: any frontier node of size ≤2 is printed as its raw memories, and any zoomed block ≤16 opens fully raw (≤16 lines). A summary of 2 memories is as long as the 2 memories, so it saves nothing.
- Each printed id is a valid argument to `zoom`, so the agent descends in ≤7 turns at 1M memories.
- Adaptive depth (choose d to hit ~8 lines) adds nothing over this rule, because block sizes are powers of two.

---

## R5. Full-text search

### Semantics (`ai memory grep`)

- Rename `recall` → `grep`. Agents already know grep/rg flags, and the name says "word for word".
- Follow **ripgrep conventions**:
  - Pattern: Rust `regex` (rg's syntax). `-F` fixed string.
  - Case: **smart-case** by default (case-insensitive unless the pattern has an uppercase letter, rg `-S`). `-i` forces insensitive, `-s` sensitive.
  - Matches the **text field only** by default. Matching provenance would make `2026` or `claude` match everything. Filters `--since <date>`, `--origin`, `--agent`, `--session` cover provenance.
- Scope: log by default (ground truth). `-t/--tree` also searches summaries and prints them as `#lo-hi ...`, which can go straight to `zoom`. RAPTOR's collapsed-tree result argues for it as a deliberate second pass, not as the default, because summary hits duplicate leaf hits.
- Output: chronological lines `#<pos> <YYYY-MM-DD hh:mm> <origin> <text>`, same shape as zoom leaves.
- Paging:
  - The newest page comes first, bounded by PART_CHARS or `-m N`. Footer: `Newest 40 of 312. Older: ai memory grep '<p>' --before 1234`.
  - The cursor is a position, stable until the next sync (it can't move without one).
- `-C N` adds context lines (neighbouring memories), with `--` separators as in grep.
- `-c` prints only the count.
- Exit 0 with "No match.", like today. Agents treat nonzero exit as failure.

### Scaling

- Data size: 1M × 512 B = 512 MB raw. The scan reads the text field after inflating segments.
- Speed:
  - zlib inflate ~0.4–1 GB/s; regex with literal prefilters (memchr) runs several GB/s. So ~0.5–1.5 s at 1M, ~0.1 s at 100k.
  - Realistic growth is ~50 notes/day ≈ 18k/yr, so 1M is decades away.
- **No tantivy.** An inverted index:
  - must be rebuilt or patched on every merge-renumber;
  - does not serve arbitrary regex (rg semantics);
  - adds a large dependency and a second derived store to keep coherent.
- If 1M ever matters: a derived, disposable cache of inflated segments in `$(git rev-parse --git-common-dir)/ai/cache/<commit>`, deleted on mismatch. That is a speed knob, not a design change. Parallel segment scan (rayon) is the other cheap win.

---

## Prior art

- **git-bug**: entities as commit chains under `refs/<namespace>/<id>`. Ops are JSON blobs in a tree; each edit session adds a commit. Concurrent edits get a merge commit. Order comes from Lamport clocks, then op-pack id. Ids are hashes of the first op. Lessons: custom refs plus a tool-driven merge plus deterministic ordering. https://github.com/git-bug/git-bug/blob/master/doc/design/data-model.md
- **beads (`bd`)**: originally committed `.beads/issues.jsonl` in trunk. Then an experimental `sync.branch` committed to `beads-sync` through **hidden worktrees in `.git/beads-worktrees/`**. That workflow was **removed**; data now lives in Dolt under `refs/dolt/data`, synced by `bd dolt push/pull`, with all worktrees sharing one workspace. This is direct evidence that the worktree approach was painful and that a custom ref won. https://beads.gascity.com/reference/protected-branches , https://beads.gascity.com/reference/worktrees.md , https://beads.gascity.com/reference/git-integration.md
- **beads_rust (`br`)**: SQLite plus a JSONL export in `.beads/`. Never commits, pushes or hooks; the user commits. Id collisions across clones: the earlier `created_at` keeps the id, the later is renumbered with a warning. Same "order by creation time" rule proposed here. https://github.com/Dicklesworthstone/beads_rust
- **git notes**: blobs attached to object ids under `refs/notes/*`. Merge strategies `manual|ours|theirs|union|cat_sort_uniq`. Not fetched or pushed by default. https://git-scm.com/docs/git-notes
- **git-appraise**: reviews as one-JSON-object-per-line notes under `refs/notes/devtools/*`, merged with `cat_sort_uniq`. A proven "line-per-item + set-union merge" pattern; our sorted union is the ordered version. https://github.com/google/git-appraise
- **Gerrit `refs/meta/config`**: a parentless branch-like ref holding project config, fetched explicitly. Precedent for an out-of-band ref beside code. https://gerrit-review.googlesource.com/Documentation/config-project-config.html
- **Dolt**: versioned SQL; beads stores its Dolt data in a git remote at `refs/dolt/data`. Too heavy here, but it confirms GitHub accepts arbitrary custom refs. https://github.com/dolthub/dolt
- **jj**: uses gix/git2 for local objects and shells out to `git` for fetch/push (libgit2 path removed in 0.30). Same split recommended. https://github.com/jj-vcs/jj/releases
- **Fossil**: artifacts are immutable and content-addressed; tickets are append-only change artifacts merged by timestamp. The same "immutable records, order by time" idea. https://fossil-scm.org/home/doc/trunk/www/tickets.wiki
- **gitoxide (`gix`)**: `Repository::{write_blob, write_object, edit_tree, new_commit, commit_as, edit_reference(s), merge_base, merge_trees, common_dir}`. Fetch yes; **push not implemented** (README checklist). https://docs.rs/gix/latest/gix/struct.Repository.html , https://github.com/GitoxideLabs/gitoxide
- **gitattributes `merge=union`**: takes both sides' lines "in random order"; "do not use this if you do not understand the implications". https://git-scm.com/docs/gitattributes
- **git worktree ref sharing**: all refs are shared across worktrees except `refs/bisect`, `refs/worktree`, `refs/rewritten`, per `git-worktree(1)`. So `refs/ai/memory` is automatically one memory per clone.
- **Agent memory in git**: Engram (custom git refs, "no branch desync, no worktree pollution"); AI Docs CLI (orphan branch + worktree); repo-memory / agent-memory (committed dirs, i.e. option A). https://github.com/AVIDS2/memorix , https://glama.ai/mcp/servers/yubinkim444/repo-memory
- **Context and retrieval evidence**:
  - Chroma, Context Rot (2025): https://research.trychroma.com/context-rot
  - RAPTOR, Sarthi et al., ICLR 2024: https://arxiv.org/abs/2401.18059
  - Lost in the Middle: https://arxiv.org/abs/2307.03172
  - LongMemEval: https://arxiv.org/abs/2410.10813
  - MemGPT: https://arxiv.org/abs/2310.08560
  - A-MEM: https://arxiv.org/abs/2502.12110
  - Mem0: https://arxiv.org/abs/2504.19413
  - Zep/Graphiti: https://arxiv.org/abs/2501.13956

## Risks

- **Privacy.** Pushing the ref publishes the memory to everyone with read access to the remote. The setup template asks agents to record "anything you learn about their life". Push must be an explicit opt-in per repo, or go to a private remote.
- **Host support.** GitHub accepts custom refs (Dolt/beads rely on it). Gerrit needs an ACL for `refs/ai/*`; some forges may hide such refs or refuse them. Forks and PRs do not carry them.
- **Write amplification.** Each note writes ~25 KB of loose objects at scale (tail segment plus 3 trees plus commit). `git gc --auto` (6700 loose objects) fires about every ~1.3k notes. gix never runs gc, so `sync` must.
- **Packed growth.** ~0.6–1 KB per mutation → ~20 MB per 20k notes, ~1 GB at 1M. It stays forever because the ref keeps history reachable. Mitigation, if ever needed: an explicit `ai memory squash` that rewrites history. It breaks merge-base with other clones, but the sorted-union merge does not need a base.
- **Clock skew.** Wrong order only, never wrong data. A clone far in the future drags appends forward. Warn at >1 day.
- **Renumbering mid-session.** Positions in an agent's context go stale. Mitigated by syncing only at `wake`/`sync` and by the `@fp` nap guard.
- **Shallow/partial clones.** The merge does not need history, so this is fine. Fetching the ref may still be blocked by `--single-branch` configs; the explicit refspec handles that.
- **gix pre-1.0 churn.** Pin the version; the surface used is small.
- **Team repos.** Several humans' agents share one memory and one "identity" (see questions).

## Open questions for the user

1. **Push policy / privacy.** Should `wake` auto-push to `origin`, push only with an opt-in `git config ai.memory.push true`, or go to a separate private remote (`ai.memory.remote`)?
2. **Team repos.** One shared memory per repo (`refs/ai/memory`), or one per human (`refs/ai/memory/<user>`), possibly with a read-only view of others'?
3. **Network at session start.** Is a ~3 s best-effort fetch/push inside `wake` acceptable, or should `wake` only merge the locally fetched tracking ref and leave network to `sync` and hooks?
4. **Commit granularity.** Commit per mutation (proposed: full audit, ~1 KB packed each), or per sync/session (less growth, coarser history)?
5. **Knobs in git config.** OK to move `config` out of the store into per-clone `git config ai.memory.*`, unversioned and unsynced?
6. **Model provenance.** Claude Code does not expose the model id in env. Accept `-` unless `AI_MODEL` is set, or require `note --model`?
7. **Display.** Is `wake` with date-only and `zoom`/`grep` with time + origin right, or should wake show origin too, for multi-machine context?
8. **`recall` removal.** Rename to `grep` with no alias (per the no-compat rule)? Should the default scope stay log-only with `--tree` opt-in?
9. **Global memory (backlog).** Is a bare repo at `~/.ai/memory.git` with the same `refs/ai/memory`, synced to a private remote, the intended shape? It reuses this design unchanged.
10. **ENTRY_CHARS.** Keep 280 default (max now 338)?
