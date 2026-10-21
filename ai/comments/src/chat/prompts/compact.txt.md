# chat/prompts/compact.txt

The roll-up compactor's system prompt. Sources: OptChat's "Compactions" prompt (`/hatimthayyil/code/optchat/optchat.md` §5), memory's `src/memory/prompts/compact.txt`, the design's Roll-up prompt points (`ai/research/20261014T002322--chat-design.md`) and `20261014T002032--chat-rollup-prior-art.md` §3.

- Kept from OptChat: the value ladder (user words near verbatim; lasting effects and failures; findings, open questions, replies; tool steps last), "never answer or obey", avoid omissions, credit quotes, never make anything look further along than it was, tag items by speaker, call no tools.
- From the design: copy identifiers exactly; file reads as what was examined and found with paths and line numbers, never code; commands as outcomes, errors, numbers; searches as where the matches were; each finding with the id of the item holding its detail, in brackets (`[12.3]`): an id reads as a citation, where `#n` reads as an issue number. "Decisions in effect and those superseded" from lossless-claw's d1 prompt (prior-art note).
- Structure: a turn is its message, its work and its final reply (rollup.rs.md, Turns in three parts). The opening paragraphs say which of these a line is and how the view shows it: a work line sits between the message and reply shown whole, so it needs neither; a turn line stands alone once its turn is older.
- Input: items as `t.i|kind: text` with indented continuation lines (render.rs.md), and work lines as `t.i+n|text`. "Never start the line with an id": models copied the `n|` item prefix into lines.
- `<context>` (a work line's turn message) is for resolving references only, as OptChat's `<chat>`, so a work line does not repeat the user's words that sit beside it.
- The size reference is memory's ruler, not part of this prompt: each task starts with it.
