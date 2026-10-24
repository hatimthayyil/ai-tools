# view.rs

`ai view [THREAD]`: what an agent reads to start or after `/ai`. `# Memory`, the memory's whole view (`memory::Cli::view_whole()`, run through `run_with`, so it is logged as `view --whole` and counts in `stats` as a view), then `agent::thread_view`: the thread named, else the one the session is attached to (synced, its branch's view); outside an agent session, the chat commands' rule (cli.rs.md, `pick`). An unattached session, or a project with no threads, gets the memory alone. The thread is resolved first, so a picker or an error comes before the memory is read.

- Paging (page.rs.md): whole on a terminal or with the hidden `--whole`; otherwise in parts, because an agent reads it through the Bash tool, whose output is cut at 30,000 characters. `/ai` passes `--whole`: it hands the text to Claude Code as the compaction's one message, which has no such cap (register.ts.md, Seed size).
- The parts are saved once, so notes and syncs between parts cannot shift them; `ai page` reads the rest without touching the memory or the thread. Their header names `ai view` and the thread shown.
- Read-only: it writes no summary line (spec: reading never calls the model). `/ai` first runs `ai agent compact-session`, which writes the lines the thread's view needs and fails visibly; then `ai view --whole`, whose thread part now has them.
- The memory and chat modules stay unaware of each other: this module composes them.
- No read commits: a sync the read makes waits for the next event's commit (agent/cli.rs.md).
