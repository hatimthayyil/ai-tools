# chat/view.rs

Design: `ai/research/20261014T002322---chat-design.md`, View and Roll-up. Fold: `/hatimthayyil/code/optchat/optchat.md` §3.2 and `20261008T074205---optchat-cache-revision-review.md` (the corrected due rule).

Branch
- `Branch`: the path from the root to a session's tip, its turns and their chunks as the roll-up cuts them (`rollup::branch`), and every line of the thread (`Snapshot::all_lines`), loaded once into a map. A session with no item yet uses its graft as tip.
- A thread with more than one branch: the view says which it follows (the session's, else the newest), the item it ends at, and the session and time that wrote that item, the time as `ai chat list` prints it. Outside a session the newest is all a command reads, so the line tells the user which of the branches `list` counts they see.
- Fold coordinates are memory's `Coord` over turn positions along the branch: `(l, i)` covers turns `[i·2^l, (i+1)·2^l)`, its line is key `(1 + l, last item of its last turn)`, the contract with the roll-up. The fold covers the turns before the recent ones.

Levels
- One list, oldest first, every turn once (spec.md), in four levels of detail, least first: oldest (merged lines `8+4`), older (one turn line), recent (message and final reply word for word, work lines between), newest (every item, as `zoom` lists a turn's items, tool output as its size). Each level holds what the one before it does, so detail never decreases toward the present; the test `every_turn_shows_once_in_order_and_detail_never_decreases` checks both on varied branches. `levels` decides each turn's; `view`, `needed` and the tests read it.
- `recent`: back from the turn before the newest, whole turns while their `most` sum fits `RECENT` (16,000 bytes); the first that does not fit ends them, so the levels stay in order. `most` is what a recent turn prints at worst, from the items alone: message and final reply as printed, each work line at its `rollup::work_limit` plus its id. Building lines never moves a turn between levels, so the lines `needed` asks for are the lines the view shows once built, and `ready` (cli.rs) finds none missing after the build.
- The newest turn is the branch's last, closed or open: `/ai` runs once the turn has ended, and the reply the user answers next must be in the view whole. It needs no line, so `/ai` never summarises the turn that just ended. Its items print whole, built work lines or not: the newest level shows every reply, which a work line would hide.
- No section per level and no "in progress" heading: a seeded session's last turn is the previous session's, and a heading that called it current was taken for the session's own request.

Fold
- Computed fresh at every call: start from one line per turn, merge the most due aligned pair, due = (T − last)/2^l with `last` the pair's last turn, oldest first on ties, until the lines fit. No high/low-water batches and no persisted view: the view is sent once per compaction, so its prefix never has to stay stable from one call to the next; only the merge order matters.
- The fold of the turns before the recent ones into at most `FOLD_LINES` (64) lines, as `ai memory view` caps its view by lines: the fold the view wants if everything were built. `needed` is its unbuilt lines and the recent turns' unbuilt work lines; `compact` asks the roll-up to build them before rendering. 64 lines of 512 to 2,048 bytes (50 to 200 words, rollup.rs.md) is 33 to 131 KB.
- `render`: each planned line if built; else what is built under it (a node's halves, recursively; a turn's message cut to `LINE`, its chunk lines, a placeholder for the rest); else one placeholder naming its id, a turn's carrying its message cut to `LINE` (`12|(not summarised yet) user: …`). Fallbacks can outgrow the plan, so while over `FOLD_LINES` the oldest multi-line fallbacks collapse back to one placeholder each. A full fold has no room for multi-line fallbacks, but a turn's placeholder is one line, so an unsummarised turn shows its message at any size: `view` writes no line, so it shows unsummarised turns often.
- Zoom keeps bare placeholders: it opens what is built, and a turn zooms into its items.

Ids
- Lines and items print by id (id.rs.md): a node `t+n` from its fold coordinate, a turn line `t`, a chunk or `not shown` span `t.i+n` from the place of its first item, an item `t.i`. Ids are read on the session's branch, so zoom loads the session's branch, not the branch to an item.

Chunks
- A turn's chunks are the roll-up's own cut (`Part::chunks`, every chunk, the open last one included): the view shows the built chunk lines consecutive from the first, each with its exact range, so the compactor may write them in any order. A chunk line after an unbuilt one is left out until the gap is built.

Budgets
- The fold at most `FOLD_LINES` lines, the recent turns at most `RECENT` bytes; the newest turn whole, however long. Constants until the config system.
- Item lines: `render::item_line`, the one renderer the compactor's input uses too (render.rs.md). Fold fallbacks cut words to 512 bytes (`SHORT`); recent turns print message and reply whole; the newest turn and zoom share `LISTING`.
- The instruction says a summary cites the item holding a detail as `[12.3]`, as the compactor's prompt asks.

Zoom (`Branch::zoom`)
- A node opens two levels down, as `ai memory zoom` does: four lines of `n/4` turns; `t+2` its two turn lines (below a turn line are its items, not lines). A turn opens into its message, chunk lines and items; items `t.i+n` into the chunk lines strictly inside them and their other items; one item prints whole. A line not built shows what is built under it, as in the fold.
- Any range zooms: `5+3` is no node, so it prints the fewest aligned nodes that tile it (`tile`: `5`, `6+2`).
- Listings print messages and replies whole, each tool call on one line cut to 400 bytes, and tool output as its size only; `--tool-output` prints calls and output whole. Normal use never pulls raw output into the context by accident; it stays one flag away.

Grep (`Branch::grep`)
- Searches the session's branch: message and reply words, tool calls (`key=value` input), tool output only with `--tool-output`, and every line built over the branch (chunk lines, turn lines of closed turns, nodes). Markers never. One line per match, `id|kind: excerpt` or `id|excerpt`, memory's 400-byte window around the match (`memory::excerpt`).
- Matches sort by their last position, then by span, so an item comes before the lines that end with it, as `ai memory grep` orders by last member then size. The CLI pages them (page.rs.md, searches).
