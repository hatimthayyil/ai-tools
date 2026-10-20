# view.rs

`ai view`: what an agent reads to start or after `/ai`. `# Memory`, the memory's `view --whole` (`memory::Cli::view_whole`, run through `run_with`, so it is logged as `view` with `--whole` and counts in `stats` as a view), then, for a session attached to a thread here, `chat::session_view`: the session synced, then its branch's view. Outside an agent session, outside a repository with commits, or unattached, the memory alone.

- No paging: `view` pages only for the 30,000-character cap on a Bash tool's output. `/ai` hands the text to Claude Code as the compaction's one message, which has no such cap (register.ts.md, Seed size), so the agent reads everything in one place, with no `view <id>` round trips.
- Read-only: it writes no summary line (spec: reading never calls the model). `/ai` first runs `ai chat compact-session`, which writes the lines the thread's view needs and fails visibly; then `ai view`, whose thread part now has them.
- The memory and chat modules stay unaware of each other: this module composes them.
