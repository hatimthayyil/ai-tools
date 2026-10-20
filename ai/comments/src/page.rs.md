# page.rs

One paging rule for `ai view`, `ai memory view` and `ai chat view`, and one part size for every paged output (views, both zooms, both greps).

- `PART` = 20,000 bytes: under Claude Code's 30,000-character cap on a command's output, with room for a footer. A constant, not a knob: an agent harness's cap does not vary per machine.
- `pick`: an asked-for part wins; else whole on a terminal (`IsTerminal` on stdout, checked only in each `exec`, so tests and library callers default to parts); else part 1. A person reads the whole view in the terminal; an agent's Bash tool output is never a terminal.
- `print`: whole, or one part ending `Part k of n. Next: <command>` while parts remain and `Part n of n.` on the last of several. A single part prints no footer. The caller supplies the command, which names the state the parts come from (`--at`), so every part is cut from the same text.
- `pages` cuts after the last line end in a part when that keeps at least half of it, else at a character boundary; `page` numbers parts from 1.
