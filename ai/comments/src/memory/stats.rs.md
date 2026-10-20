# stats.rs

- `ai memory stats`: the name follows `atuin stats` (usage counts of a shell history) and `hledger stats`.
- Sources: notes from the store (every `Kind::Note` message, with its stored provenance; covers the time before the usage log existed), view/zoom/grep/show from `usage.jsonl`. Log `note` lines only feed the `Failed:` line. Other commands (summarise, config, import, stats, prompt-hook) are logged but not counted: `hook` is not recall.
- Hook line: `N prompts hooked, M reminded (k periodic).` after the session lines, when the window has successful `prompt-hook` calls (M: those logged with `reminded`; k: those also logged with `periodic`, so long-prompt reminders are M - k). Not per row or session; relating reminders to zooms is left to `jq` over `usage.jsonl` until a need shows.
- Counts successful calls only; failed ones are summed on `Failed:` (a refused note, a bad zoom id). `view` counts every call, later parts (`view <id>`) included.
- Default window: the last 14 UTC days, today included. `--since` as in grep.
- Rows: one per UTC day, or `--by agent|repo|session`, in order of first call; days without calls are left out. `all` totals the window.
- Sessions: `-` (no session id: a person at the terminal, an unknown harness) is left out. A session woke if it ran `view` once in the window. Means (`Per woken session`) and shares (`Of these`, rounded half up) are over woken sessions only, counting their own calls. A session cut by the window start counts only its calls inside it.
- Not added: `--by model`, medians, JSON output. `usage.jsonl` is JSON Lines for anything else (`jq`).
