# agent/record.rs

The records of the chat store (store.rs.md), one JSON object per line.

- `Row`: an item's `Head` and its `Kind`, flattened into one object: `seq`, `parent`, `harness`, `session`, `entry`, `ts`, then `kind` naming the snake-case variant and `content` its text or fields. Tool input stays a JSON value. Source ids keep their exact whitespace and text.
- `Tag`: a row read for its `kind` tag only, head and content skipped: whether the item opens a turn. Only the opening variants are named (`#[serde(other)]` for the rest); `rows_are_flat_with_tagged_kinds` checks it agrees with `Kind::opens` for every kind.
- `Track`: a session's branch in one thread. `Event`: an attachment change, `state` `attached` (with its `thread`) or `detached`. `Binding` is not stored: store.rs joins a track with its session's events.
- `Agent`, `Line`, `LineOp` (`put`, `drop`, `clear`). `Line`'s `who` is `memory::Who`'s serde derive (`agent`, `model`, `session`), shared with `files::store`; memory's own records stay hand-encoded.
- Every field is required, except `Row::ends_turn`: a flag, written only when true; absent is false. `Option` fields are written as `null` when empty and read with `present`, so a missing field fails decoding where serde would read it as absent. Unknown variants fail too.
- jiff's serde feature keeps timestamps exact; JSON escapes newlines, so multiline text stays on one line.
- `Key` prints `level@last`. `check_id` validates ids used as path components.
