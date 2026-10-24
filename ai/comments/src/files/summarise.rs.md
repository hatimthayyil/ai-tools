# files/summarise.rs

Sizes as the user set them, simulated in `ai/research/20261021T234637--file-summary-simulation.md`. Pure of storage: the walk and the store call it.

- Bytes, never words: words are ambiguous in code. No ruler: a byte ruler would add a tenth of a file to its input. The task states the input's bytes and the ask, as `notes` asks in bytes.
- Ask N, accept `accept(N)` = 1.25 N, so length rarely forces a retry. A retry asks N scaled down by the overrun (N² / bytes), in the same conversation, through memory's `fit`; still over after `TRIES`, the summary fails with an error.
- File: whole, one call, `file_ask` = max(`FLOOR` 512, bytes / `RATIO` 10). Lines are numbered as the Read tool shows them, so a citation's line number is copied, never counted.
- Directory: `dir_target` T = max(512, mean child summary). Each pass asks `next_ask` = max(T, half the text before it): the first over the children's summaries, the next over the last summary alone. All passes in one conversation, so each reads the ones before from the cache. The passes end at the pass that asks T, or earlier once a summary is within `accept(T)`, since a further pass would only shorten what is already accepted.
- One system prompt (`prompts/summary.txt`) for files and directories: they share the cached prefix.
- The caller makes the backend: `summariser::Config::backend` (from `[summariser]`, summariser.rs.md) keeps every call byte for byte in `calls`; what requests took accumulates in the caller's `Spend`.
