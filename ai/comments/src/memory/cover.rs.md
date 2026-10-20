# cover.rs

OptMem's `cover` (`../OptMem/memo` `_cover`/`cover`), ported to `Coord`. Tiles `[0, t)` with aligned blocks; a block stays whole iff it fits in the log and `n ≤ alpha·(t − id)`. Alpha is bisected (60 steps) to the largest tiling within the budget; the lines left over split the newest blocks. Recomputed on every `view`: lines shift between reads, which OptChat rejects for cache reasons, but `view` is read once per session, so stability buys nothing.

`from(cs, id)`: the blocks from message `id` on, for `view <id>`. Blocks ending at or before `id` drop; the one straddling `id` splits into children, recursively, down to blocks starting at or after `id` (single messages at worst), so the result tiles `[id, t)`.
