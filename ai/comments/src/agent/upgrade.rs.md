# agent/upgrade.rs

Converting an agent store of format 3 (whole files) to format 4 (segmented files, segments.rs.md).

- `pending`: the store is format 3, or a conversion of it was cut short. `Store::find`/`open` then fail with `CONVERT`, so hooks, the plugin and every command refuse the store, visibly, until it is converted; none of them reads or writes it meanwhile.
- `offer`: the one way to convert, after one yes/no. The CLI calls it on a terminal only (cli.rs.md, `offer_upgrade`), before any `ai agent` command but the plumbing, and before `ai resume`. No upgrade command. A no fails, changing nothing. A yes converts, then commits the private store once as `ai agent: convert to format 4`. Tests pass the answer as a closure.
- `convert`: holds the commit lock of the private store's repository (`repo::lock`, making the repository first), so no commit captures the store half converted. It builds the new store whole in `.ai/local/.git/ai-agents-v4`, inside the git directory so no commit ever holds it, then swaps: the old store to `.ai/local/.git/ai-agents-v3`, the new one into `.ai/local/agents`, the old one removed.
- Cut short: a build is discarded and started again; after the first rename, the new store (whole by then) is renamed in; after the second, the old one is removed. Both temporary stores present beside `.ai/local/agents` is not a state a conversion leaves, and it fails naming them.
- `copy_store`: format 3 has a fixed shape, and anything else in it fails naming the path, before anything is swapped. Root: `format` (written as 4), `sessions.jsonl` (segmented), `threads/<name>/`, the lock directory `sessions/` and `*.lock` (left behind), `*.log` (copied). A thread: `items.jsonl`, `lines.jsonl`, `sessions.jsonl`, `agents.jsonl` and every file under `agents/` and `sources/` segmented; `calls/` copied as it is, one file per call, which does not grow per turn.
- `segment` reads each converted file back and compares every byte with the original before the swap.
