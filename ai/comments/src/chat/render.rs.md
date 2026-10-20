# chat/render.rs

One item as a line, for the view (view.rs) and the compactor's input (rollup.rs): the agent and the compactor read items the same way, and an item's id cited in a summary (`[12.3]`) is the id the view prints and `ai chat zoom` takes.

- `item_line`: `t.i|user: …`, `t.i|agent <sender>: …`, `t.i|ai: …`, `t.i|tool <name>: key=value …`, `t.i|output: …` / `t.i|error: …`, markers as `t.i|[…]`. The kinds are plain words an agent reads without a legend.
- Text continues on lines indented by two spaces, trailing whitespace trimmed: a tool output holding `ai memory` lines (`0+16|…`) or Markdown headings (`## …`) then never reads as a view line or section. Summary lines are one line already.
- Tool input: `key=value` per field (serde_json's sorted keys), strings verbatim, other values as JSON: readable without escapes, and no per-tool table to maintain. A cut input is flattened to one line, so a listing shows one line per call.
- `Cut`: bytes kept of an item's words, of a tool input, and of a tool output; `WHOLE` keeps all. `output: None` prints only the output's size (`(40 lines, 3211 bytes)`): a zoom shows tool output only when asked (`--tool-output`). Callers: the view (fold fallbacks 512, recent turns' message and final reply whole, the newest turn as zoom lists it), zoom (messages whole, calls 400 on one line, output hidden or whole), the compactor (words 24 KB, tools 4 KB).
- `clip`: head and tail around `[N bytes cut]`, at char boundaries. Every printed item starts with its id, so the note needs no pointer of its own.
