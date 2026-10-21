# Record versioning and store upgrades

Author: Claude Code (Claude Opus 5.5). Status: proposal. Crate survey: `20261013T224300---format-versioning-crates.md`.

## Ground

- Store: bare git repo, all data under `refs/ai/memory` (`src/memory/store.rs`). One commit per mutation, CAS on the ref.
- Message: one blob at `log/{i>>8:04x}/{i&255:02x}`. Header line of nine positional fields `ts kind origin repo head branch agent model session` (`-` empty, whitespace `_`), `\n`, text verbatim. `Message::decode` rejects any other field count.
- Node: one line `ts origin agent model session text`, 256 per segment blob at `tree/<l>/...`; text last, parsed with `splitn(6)`.
- Config (`ai.memory.*`) lives in the repo's git config: per machine, outside the data.
- No format version exists. Published binaries 0.6.3 and 0.7.0 cannot tell a newer store from their own.

## Requirements

1. A binary reads and writes only stores of its own format.
2. An older store upgrades on first open, with no user action, no model calls and no network.
3. A newer store is refused before any write.
4. Upgrades are atomic and reversible; concurrent sessions see the old format or the new, never a mix.
5. Adding an optional field needs no upgrade.
6. Message ids and summary texts survive every upgrade unchanged.

## Design

### Format version in the data

A `format` blob at the tree root holds one integer. It changes in the same commit as the data it describes, and travels with the store if sync returns. Every open reads it first:

| Stored | Action |
|---|---|
| absent | format 1 (every store written before this design) |
| equal to the binary's | proceed |
| older | upgrade, then proceed |
| newer | refuse: "This memory is format 3; this ai reads up to 2. Upgrade ai." |

In Rust: `const FORMAT: u32` and `const UPGRADERS: [fn(&Tree) -> Result<Tree>; FORMAT as usize - 1]`, so a bump without its upgrader fails to compile (rusqlite_migration's model). No version enum: the live code reads and writes `FORMAT` only, and per-version record structs live inside their upgrader modules. `Store::find` reads the `format` blob once; `Snapshot` construction asserts it.

### Named fields

Message header becomes one `key value` line per field, then a blank line, then the text (git commit object layout). Hand-written, about 40 lines; header crates (mailparse, deb822, toml) bring more than this needs:

```
ts 20261013T062003Z
kind note
origin GTAQG6
repo hatimthayyil/ai-tools
root 3f1c...
head 0350ec8693df
branch main
agent claude-code_2-1-296_agent
model claude-opus-5-5
session aa7056f5-...

text
```

- Required keys: `ts`, `kind`, `origin`. Missing: corrupt.
- Optional keys: absent means empty; `-` placeholders go.
- Unknown keys: kept on read, ignored by the current code. An older binary of the same format reading a newer optional key loses nothing, since records are immutable.
- Node segments: first line names the columns (`ts origin agent model session text`); each node line follows them. A new optional column is appended.

Format bumps: removing a field, changing a field's meaning or encoding, moving paths. Additive optional fields: no bump.

### Upgrades

- `upgrade::step_n(tree_n) -> tree_n+1`: pure, deterministic, chained `1 -> 2 -> ... -> FORMAT`. Each step owns a frozen decoder for the format it replaces; the live reader decodes `FORMAT` only.
- Commit: the new tree as a commit whose parent is the old head, message `upgrade format N -> M`, CAS on the ref. Rollback: move the ref to the parent.
- Lock: the upgrade takes the compactor's lock first, so a `nap` running an older binary cannot commit an old-format node over the new format.
- Concurrency: a session losing the CAS rereads, finds the new format, proceeds.
- Report: one stderr line, `Upgraded memory to format 2; previous state is commit <id>.` Stdout (wake output) stays clean.
- Ids and texts: positions and summary texts carry over byte for byte; only encodings change.
- Pruning: an upgrader stays while stores of its source format may exist; dropping one names the minimum release to upgrade through.

### Published binaries fail closed

0.6.3 and 0.7.0 check no format; both pin gix 0.88.0. Old binaries may break on a format-2 store; they must not write to it.

- The barrier is the format-2 encoding. The old decoder rejects named headers, and every old write decodes a message first: `note` and `import` the previous message, `nap` message 0 (an old reader takes the segment's column row for node 0, so node `(l, 0)` looks unbuilt at every level and its first job is leaf 0). Verified with 0.7.0 per command; the table is in `ai/comments/src/memory/upgrade/mod.rs.md`.
- An old compactor running at upgrade time holds the compactor's lock; the upgrade waits for it.
- Messages stay at `log/`: the name describes the structure (append-only, position is id) and pairs with `tree/`.
- Git's repository format is no lever. gix 0.88 refuses `repositoryformatversion` 2 before any open option applies, locking out the new binary and plain `git` alike. Version 1 with an unknown `extensions.*` key is ignored by gix and refused by plain `git`.

### `root` and `ai memory link`

- New notes record `root`: the root commit of the default branch (oldest when several), cached per clone in `.git/config`. Survives moves, renames and rebases; a history rewrite (`filter-repo`) changes it.
- `repo` keeps the name the repo had when the note was taken.
- Old notes: `ai memory link`, run in a repo, sets `root` on notes from that repo lacking one (recorded `head` in the repo, or `repo` equal to its current name). Fills empty fields only, never changes one. Same commit-on-parent mechanism as upgrades. Machine-dependent, hence separate from the pure upgrade.
- Line markers in `wake` match on `root`.

### Tests

- Fixtures: a store written by each published release, in `tests/fixtures/`.
- Each fixture upgrades to `FORMAT`; `wake`, `zoom`, `show` output matches a golden file.
- An upgraded store re-opened is unchanged (idempotent); a store with `format` > `FORMAT` is refused with no new commit.
- CI installs the previous release and checks it refuses an upgraded store.

### Release

A format bump ships as a minor version (0.8.0), with the upgrade and rollback in the release notes.

## Scope of the first release

Format blob and check, named headers, upgrader 1 -> 2, fixtures, a test running 0.7.0 against an upgraded store, `root` on new notes. Next: `ai memory link`, line markers, the "last note in this repo" line.

## Open

- Whether chat stores (repo-local `ai chat`) and `ai notes` share this machinery from day one.
