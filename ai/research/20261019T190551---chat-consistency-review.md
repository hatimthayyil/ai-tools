# Review: codex/chat-consistency

Branch `codex/chat-consistency` at 1eb908f, six commits over main ade17de, written by Codex (GPT-6) and audited by DeepSeek. This review checks each DeepSeek claim against the code, adds its own findings, and reads all of it against the decision to drop custom storage formats in favour of serde JSON/JSONL.

Method: read `src/chat/{sync,store,record,source,v1,cli}.rs`, `src/git.rs`, the tests and `ai/comments`. Probe tests were written in a throwaway worktree (`tests/review_probe.rs`, not committed). Ran the full suite twice. Timed a release build against a copy of a real session (9.5 MB transcript plus a 31 MB companion directory) in a temporary repository with the summariser disabled. Live refs were only read.

## Verdicts on the DeepSeek items

| # | Claim | Verdict | Survives rewrite | Evidence |
|---|---|---|---|---|
| 1 | Double-attached session is wedged | CONFIRMED | partly | `sync.rs:88` bails, and detach (`sync.rs:219`), attach and sync all go through it. Probe: with two refs holding one attached binding, detach, sync and attach all fail; `list` works. New code cannot create the state, because the per-session lock serialises attach and detach. Only old versions or an import can. |
| 2 | Bad binding tip panics | CONFIRMED | yes | Probe: `tip: 9999` panics at `store.rs:212:68` (index out of bounds) in `list`. `model.rs:140` (`branch`) indexes the same way for view, zoom and grep. `v1.rs` `binding()` publishes `tip` and `graft` unchecked. `graft` is unchecked on read too: a bad graft is written as a parent, and the next read fails with "Item N is corrupt". |
| 3 | Version not bumped | CONFIRMED | yes | `Cargo.toml` is 0.9.0 on both. |
| 4 | Import aborts thread by thread, unnamed | CONFIRMED | no (importer) | `cli.rs` `thread.upgrade()?` has no thread context. Threads converted earlier stay converted (each by its own CAS). The preflight runs on every command, so one bad v1 thread blocks every command in the repository. |
| 5 | Archive path dir→file breaks sync | CONFIRMED | yes | Probe: after `s-1/x/` becomes the file `s-1/x`, every sync fails with "Object … was supposed to be of kind blob, but was kind tree." The failure comes from `ingest`'s `git.read` at `store.rs:636`, before any write. The old tree stays. |
| 6 | Detach drops the tail | CONFIRMED, pre-existing | yes | `cli.rs` `detach` matches main byte for byte, so this is not a branch regression. The bytes are only lost from the archive: the transcript stays on disk, and a later reattach ingests it. |
| L1 | Whitespace in v1 ids duplicates | PLAUSIBLE, unreachable | no | v1 `Seg::values` splits on spaces. Claude entry ids are UUID-based, and `check_id` forbids whitespace in session ids. |
| L2 | Missing `format` blob reads as v1 | CONFIRMED | no | `git.rs:119`. Chat threads always had the blob, so only corruption reaches this path. |
| L3 | Missing `Option` fields decode as absent | CONFIRMED | yes | Probe: a binding without `detached` (or `tip`, `graft`) decodes as attached. `record.rs.md` claims missing fields fail, which is wrong for options. |
| M1 | Archive growth | PLAUSIBLE (order confirmed) | no | Each changed sync writes the whole transcript as a new loose blob: 9.5 MB becomes about 1.9–2.3 MB compressed. One attach of the real session wrote 9,997 loose objects and 66 MB, which crosses git's `gc.auto` threshold (6,700). |
| M2 | Read cost; `list` builds the whole tree | CONFIRMED, pre-existing | yes | `store.rs:205`, the same as main. On the real session `list` took 0.05 s. DeepSeek's 20k-item figures were not rerun. |
| — | "No flakes across repeated runs" | REFUTED | — | `tests/cli.rs:995` `a_note_from_stdin_is_summarised_in_the_background` failed in 1 of 2 full runs. This is the known memory-side flake. Otherwise 201 pass and 1 is ignored. |
| — | "Your store is clean" | REFUTED | — | The live ai-tools threads `ai-chat` and `test` were converted to format 2 at 2026-10-17 22:56 (`8d677fa`, `35b5b38`). The installed `ai` 0.8.0 refuses them ("format 2; this ai reads up to 1"), so this session's Stop-hook sync has failed since then. `ai-chat` has no `sources/` (it came from v1), 48 lines and one binding; `test` has no lines. |
| — | Doc diff: `Head` "within a thread" | CONFIRMED (minor) | yes | Subagent heads number within their own sequence. The rest of the 27-line restore is accurate. |

## Own findings

1. **Exclusivity is scanned, so one bad thread blocks every session.** `attached_unlocked` loads every thread's snapshot and binding on each sync, attach and detach. One corrupt, v1 or newer thread therefore fails the Stop hook for every session in the repository, and duplicates are possible because nothing structural forbids them. A single repo-level map from session to thread, written under the session lock, makes "at most one attachment" structural. Survives: yes.
2. **Per-turn sync is O(session size).** A release build takes 0.53 s with nothing changed and 0.54–0.66 s per appended entry on the real 41 MB session. Each sync re-reads every source file, re-parses the transcript, and decompresses each archived blob to compare it byte for byte (`store.rs:636`). Comparing git blob hashes, or size plus mtime, removes the decompression. Appending only the bytes past the stored length removes most of the copy. Survives: yes for the capture and parse; the compare is Git-specific.
3. **`raw/` is dead storage.** Nothing outside tests reads `Item::raw`. The complete source archive makes it a third copy of each recognised line, and it accounts for half the per-item blobs. Drop it. Survives: yes.
4. **Binding state allows impossible combinations.** `detached: Option<Timestamp>` decides the attachment, and serde fills a missing field with `None`, meaning "attached". A tagged state (`attached` / `detached {at}`) makes absence a decode error. Also validate `tip`, `graft` and `call` against sequence lengths on read. Survives: yes.
5. **The importer and its confirmation are out of proportion to the data.** About 460 lines (`v1.rs` 209; `run_with_confirm`, `confirm_import`, `foreground_terminal` with `unsafe libc` and 7 tests in `cli.rs`) exist to convert two threads. Both transcripts are still on disk. Re-attaching rebuilds the items and the binding; only 48 summary lines need an LLM to regenerate. The live store is already format 2, so the importer has no remaining user. Survives: no.
6. **The spec is not on the branch.** `ai/spec.md` exists only untracked in the Codex worktree. It also states "Normalised conversation history is an ordered sequence of user–AI pairs", which nothing enforces: interrupted turns stay open and nothing rejects an unanswered exchange. Survives: yes.
7. **Codex's handover.** Codex ended by telling the user to put the branch binary first on `PATH` for manual checks. The live conversion happened 62 minutes after memory note 464 ("No live stores converted"). The cause is unproven, since the committer is the user's identity, but the outcome is a live store that the installed binary cannot read.
8. **Lock files accumulate.** There is one `ai/chat/sessions/<harness>/<session>.lock` per session ever attached, and nothing removes them. Survives: yes (trivial).
9. **Tests are good where they exist.** The stale-sync, concurrent-attach and capture/projection tests are deterministic: they use channels and barriers, not sleeps. Nothing covers a corrupt binding range, a dir↔file source change, detach-vs-sync ordering or required fields. The probes above are ready-made regressions.

## Under a JSONL store

What a plain-file store gives up compared with the Git tree, and whether it matters:

- **Atomic multi-file publish.** A per-thread flock already serialises writers. Write in order: source copies, then item appends, then the binding (temp file plus rename). A crash between steps leaves sources ahead of items, and the next sync repairs that because ingest is idempotent (dedupe by harness and entry id). Reading inside the lock keeps "an older sync cannot undo a newer one".
- **Snapshots and `--at`.** Append-only logs make every earlier state a prefix. A paged view pins an item count instead of a commit. Bindings can be an append-only event log to keep their history.
- **Archived source versions.** Claude transcripts and subagent files are append-only, so the latest copy contains every earlier version. Only in-place files (`*.meta.json`, `custom-title.json`) lose older versions. Narrow the spec line to say so, or keep hash-named copies of replaced files. Either way the 2 MB-per-turn growth disappears.
- **Push-sharing.** It was never used, and the user ruled that local-environment details are never shared. No loss.
- **Worktree sharing.** Keep the files under `<common git dir>/ai/chat/`.

## Salvage

- **Semantics:** explicit detach before reassignment; capture inside the critical section, so archive and items come from the same bytes; dedupe by harness and entry id, not session; graft and tip; subagent sequences with `call`, kept when unresolvable.
- **Code:** `source.rs` as it stands (symlink refusal, companion directory, UTF-8 paths); the serde types in `record.rs` (`Head`, `Binding`, `Agent`, `Line`, tagged `Kind`, the `WhoRecord` remote derive); `check_id`; `session_lock`; and the `sync_with`/`attach_with` reader injection.
- **Tests:** stale sync, concurrent attach, capture/projection, byte-exact archive (invalid UTF-8, nested binaries, empty files), fork dedupe, subagent linkage, and the four probes.
- **Spec:** lines 1–4, 6 and 10, plus "structured records use JSON/JSONL". Drop the import lines (8–9) and hold the pairing line until it is designed.
- **Drop:** `v1.rs`, the import preflight and confirmation, `Thread::upgrade`, `raw/`, fan paths and index segments, and the `format`-blob rules.

## Recommendations

1. Do not merge the branch's storage layer. Merging it means merging format 2 and its importer only to replace both.
2. Build the JSONL store in a fork, carrying over the salvage list. Add no importer: re-attach the two live sessions, and let the summariser rebuild the 48 lines.
3. With it: a repo-level session map, range-checked bindings with a tagged state, detach that syncs first, and change detection that skips unchanged sources.
4. Bump to 0.10.0 with the store change. Commit `ai/spec.md` with the pairing line marked open.
5. Until then, the installed 0.8.0 cannot sync this session's thread. Either install the branch build or accept the gap; the transcript keeps everything.
