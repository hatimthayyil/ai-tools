# agent/id.rs

Design: `ai/research/20261014T002322--chat-design.md`, Ids.

- Turns, not items, carry the numbers: the normalised `user:ai` history. `t+n` is memory's `id+n` over turns, so the two tools zoom alike, and an agent can write any id itself instead of copying ranges from the view. Item numbers restart in each turn (`t.i`), so an item's id says where it sits.
- `Turns { first, n }` prints `t` or `t+n`; `Items { turn, first, n }` prints `t.i` or `t.i+n`. `+1` reads back as the bare form. A node is an aligned power of two (`Coord::at`); other ranges are valid ids too (zoom tiles them).
- A chunk of one item prints `t.i`, like the item; zooming it shows the item, which is all it holds.
- Ids are positions on a branch. Items get theirs from `Tree::place` (model.rs.md), which depends only on the path from the root, so the compactor can render an item once for all branches. Past a fork, `5` names a different turn on each branch; zoom reads ids on the session's branch.
- The store keeps keying lines by `level@last` (record.rs): ids are presentation.
