# upgrade/mod.rs

Design: `ai/research/20261013T224252--record-versioning.md`; crate and prior-art survey: `20261013T224300--format-versioning-crates.md`.

- `FORMAT`: the one store format this ai reads and writes. No version enum: live code knows only `FORMAT`; older formats exist only as indices into `UPGRADES` and as decoders inside their upgrader modules.
- `UPGRADES: [Upgrade; FORMAT - 1]` (rusqlite_migration's model): raising `FORMAT` without adding its upgrader fails to compile. `UPGRADES[v - 1]` takes format `v` to `v + 1`; `step` adds the `format` blob.
- Where the number lives: the `format` blob at the tree root, committed with the data it describes. Absent is format 1 (every store from ai ≤ 0.7.0). `Ref::parse_format` (git.rs, shared with `ai chat`) refuses 0 and non-numbers as corrupt.
- `Ref::readable`: a newer store is refused before any write: "This memory is format N; this ai reads up to M. Upgrade ai."
- Upgraders are pure: a tree in, changes out, no network, model, clock or machine state, so the result is deterministic and the upgrade runs unasked. Machine-dependent fills (`root` on old notes) are a separate command (`ai memory update-links`, not yet written).
- Format bumps: removing a field, changing a field's meaning or encoding, moving paths. An optional field or node column added: no bump.
- Pruning: an upgrader stays while stores of its source format may exist; dropping one names the minimum release to upgrade through.
- Format 2 (0.8.0): named message headers and node segments with a column row (record.rs.md); `root` on new notes.

## Old binaries on a format-2 store

ai ≤ 0.7.0 checks no format. Paths are unchanged (messages stay at `log/`), so the barrier is the encoding: the old `Message::decode` rejects a named header, and every old command that writes reads a message first. Observed with the 0.7.0 release binary on the upgraded fixture (`tests/fixtures/memory-v1.bundle`), ref unchanged after each:

| Command | Output | Exit |
|---|---|---|
| `note hello` | `Message 15` (decoding the previous message for the time-order check) | 1 |
| `import` | `Message 15` | 1 |
| `wake` | `Message 0` | 1 |
| `nap` (fake `claude` on `PATH`) | `Message 0`; nap.log: `failed error="Message 0: A message is corrupt."` | 1 |
| `zoom 5`, `show 5` | `Message 5` | 1 |
| `grep widget` | `A message is corrupt.` | 1 |
| `zoom 0+16` | `No line 0+16.` | 1 |
| `show 8+8` | prints node `0+8`'s fields: the old reader takes the column row for node 0, shifting every node by one | 0 |
| `config` | prints the knobs (config only, no data) | 0 |

Old node reads are misaligned (row 0 is the column row, decoded as an unbuilt node), so only read-only output is wrong; nothing old can write a node:
- An old nap's first due node is always leaf 0: node `(l, 0)` reads as unbuilt at every level, so the view starts with a placeholder and rule 3 dues nothing else. Building it reads message 0, which fails.
- An old compactor already running when the upgrade starts holds the compactor lock; the upgrade waits for it (store.rs.md, Why the lock).
