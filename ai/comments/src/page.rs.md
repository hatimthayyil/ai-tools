# page.rs

One paging rule and one part size for every paged output: `ai view`, and the views, zooms and greps of `ai memory` and `ai chat`.

- `PART` = 20,000 bytes: under Claude Code's 30,000-character cap on a command's output, with room for a footer. A constant, not a knob: an agent harness's cap does not vary per machine.
- `pick`: an asked-for part wins; else whole on a terminal (`IsTerminal` on stdout, checked only in each `exec`, so tests and library callers default to parts); else part 1. A person reads the whole view in the terminal; an agent's Bash tool output is never a terminal.
- `print`: whole, or one part ending `Part k of n. Next: <command>` while parts remain and `Part n of n.` on the last of several. A single part prints no footer. The caller supplies the command, which names the state the parts come from (`--at`), so every part is cut from the same text.
- `pages` cuts after the last line end in a part when that keeps at least half of it, else at a character boundary; `page` numbers parts from 1.
- Searches page by whole matches, not bytes of text, and newest first: the newest matches are what an agent most often wants, so part 1 holds them (`newest_first`: each part the newest left that fit in `PART`, at least one). Each part still prints its matches oldest first. `search` gives the matches to print and the footer: `n matches.` (or `Newest k of n matches.` when `-m` kept only k) when whole or in one part, else `Part k of n, newest first: j of n matches. Next: <command>`, no `Next` on the last. Same `--part`/`--at` continuation as the views, so a note or summary landing between parts cannot shift them.
