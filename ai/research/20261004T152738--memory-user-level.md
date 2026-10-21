# Memory at user level

Addendum to `20261003T091838--memory-git-ref.md`. Date: 2026-10-04. Supersedes it where they differ; everything not mentioned here stands as written there.

## Change

The default memory belongs to the user, as in OptMem (`~/.optmem/memory`): one memory per human per machine, shared by every repo and every session. Not one per repository.

## Effect on the git-ref design

| Topic | Before (per repo) | Now (per user) |
|---|---|---|
| Store | `refs/ai/memory` in the project repo | `refs/ai/memory` in a dedicated **bare** repo at `~/.ai/memory`; `$AI_MEMORY_DIR` overrides the path |
| Repo discovery, `.gitignore` entry | needed | removed |
| Remote | project `origin`, fetch refspec added to it | the memory repo's own remote: `ai.memory.remote` in its git config, default `origin` if present; none → local only, no error |
| Push privacy (Q1) | open | settled: the remote is the user's own; sync pushes |
| Team repos (Q2) | open | moot: one memory per user |
| Knobs (Q5) | project git config | memory repo git config (`ai.memory.wakeLines`, …); per machine, unsynced |
| `head`, `branch` | the repo holding the memory | the repo at the **cwd** when the note is taken; `-` outside a repo |
| New field `repo` | — | cwd repo identity: `<owner>/<name>` from its `origin` URL, else the root dir name, else `-` |
| Subagent line in template | "in this repository" | "on this machine" |

Record layout (LOG, 512 B): insert `repo` (32 B + space) after `origin`, before `head`; text shrinks to 305 B max. `ENTRY_CHARS` stays 280. TREE layout unchanged.

Custom ref vs branch: inside a dedicated repo the ref's advantages over a branch (no CI, no branch UI) mostly vanish. Keep `refs/ai/memory` anyway: one code path if per-repo memory returns, and the repo stays free for anything else.

## Open questions, decided

- Q3 network at start: `wake` runs a best-effort sync (~3 s timeout) only when a remote is configured.
- Q4: one commit per mutation.
- Q6: model is `-` unless `$AI_MODEL` is set.
- Q7: `wake` shows the date only; `zoom`/`grep` add `hh:mm`, origin and repo; `show <pos>` prints full provenance.
- Q8: `recall` becomes `grep`, no alias; log only by default, `--tree` opt-in.
- Q10: `ENTRY_CHARS` default 280.
- Old per-repo `.ai/memory` stores: dropped, no migration. `import` stays for bootstrap (e.g. from OptMem).

## Stages

1. Store: bare repo at user level, gix objects under `refs/ai/memory`, 512 B records with provenance, CAS per mutation, knobs in git config. All current commands on it.
2. Sync: `ai memory sync`, sorted-union merge with fingerprint reuse, best-effort in `wake`, `nap <id>@<fp>` guard.
3. Navigation: `zoom --depth`, `grep` (replaces `recall`), `show`.
