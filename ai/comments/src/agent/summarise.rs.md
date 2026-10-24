# agent/summarise.rs

The summariser: builds the lines `rollup.rs` plans. Design: `ai/research/20261014T002322--chat-design.md`, Roll-up (background compactor) and Contract.

Config (`Store::config`)
- `[chat]` in `ai.toml` (config.rs.md), the user's under the project's under its private settings: `background` (bool, default true). `Store::settings` reads the files from the directory the store was found from, each time it is asked, so a command that never needs them never reads them.
- `backend(model)`: the `[summariser]` backend (summariser.rs.md) with `model` (`build --model`) over the configured one.

Runs (`Run`)
- One pass reads the latest snapshot: items (`Rollup::new`) and every built line (`all_lines`). Pending lines are ordered by (last item, level): oldest first, a line before the node above it that ends at the same item.
- Each round first writes every ready free line in one commit (`put_lines`), repeating while new ones become ready, then starts the ready calls up to `memory::JOBS` (8). Each call runs `memory::write_line` under `PROMPT` on its own thread (same dispatcher and span as memory's pump; a panic becomes a failure so a pass never waits forever) and reports on a channel; each finished call is one commit (`put_line`, first write wins).
- A failed call leaves its line unbuilt and is not retried in that run; lines above it wait. A reply still over its `limit` after `memory::TRIES` tries is a failure too, never stored: the limit is the view's budget, so it holds. Memory keeps the shortest try instead (compact.rs.md).
- One failure rule, by who waits (`OnFail`). A request someone runs (`build`, `rebuild`, `rewrite`, `dry_run`, and `summarise` from the command line) stops: after the first failure no call starts, the calls running finish and their lines are kept, and the caller reports every failed line. Its retry is running the command again, which picks up from the lines written. The background summariser (`summarise --background`, spawned by `sync-session`) continues past a failure and only logs; after `JOBS` failures in a row no call starts, so a dead backend costs at most about two waves of calls per run (test: 8–15). The next summariser tries again; a line it could not write is built, or reported, by the next request that needs it. Memory's per-node backoff is not needed: a summariser ends instead of waiting, and the next sync starts another.
- Dropping a run with calls in flight calls `Backend::stop`.

`build(thread, backend, keys)` (for `compact-session`, `ai agent resume` starting a new session and `ai agent build <id>`)
- The keys and the unbuilt lines they are made from; a key the roll-up does not have and that is not built is an error (`<key> is no line of thread <name>.`). Returns once they are built, or once the first failure's running calls end, with what is built. No time limit: a caller someone waits on is stopped by killing it (`/ai` cancelled), and its `claude -p` calls die with it (memory/claude.rs.md, `die_with_parent`); the lines written so far stay, and the next build goes on from them.
- Under the thread's lock. While a summariser holds it, waits for it however long (`Thread::lock`, a blocking `flock`; logs `busy: waiting`), then builds only what the summariser left unbuilt: no line costs two calls. Unlocked before dropped, as a summariser.
- A sync's summariser that starts while a build holds the lock exits `busy`; lines the view did not need wait for the next sync.
- Logs to the thread's log like a summariser, under a `build` span, so a compaction's failed lines are on record.

`summarise(thread, backend)` (for `ai agent build -a <name>`)
- Logs to `Thread::log_path` (`log::init`; INFO `start`, `built` (line, chars, secs), `end`; WARN `failed`; `busy`).
- Under the thread's lock (`try_lock`; `None` at once when held): passes until one starts nothing, then releases the lock and compares the head with the last pass's commit. Moved (a sync landed meanwhile): take the lock again and go on, or exit if another summariser has it. This closes the window in which a sync's summariser finds the lock held just before this one exits.
- The lock is `unlock`ed before it is dropped: a process forked by another thread between our open and close (a `claude -p` call, a test's `git`) holds a copy of the descriptor until its exec, and the flock stays with it until then.

`rebuild(thread, backend, deadline)` (for `ai agent build --rebuild`)
- Under the thread's lock, waiting for it until the deadline (`None` if still held: nothing dropped): drops every line in one commit (`Thread::drop_lines`), then passes as `summarise` does until one starts nothing. Dropping first keeps one rule for the store (a line is written once, first write wins): a rewrite is a fresh build. It stops at the first failure; the lines it did not write stay unbuilt until the next summariser.

`rewrite(thread, backend, key, deadline)` (for `ai agent build <id> --rebuild`)
- Under the lock as `rebuild`: drops the line and every line made from it up to the top (`Rollup::above`), in one commit (`Thread::drop_keys`), then passes on that line alone until one starts nothing: the lines under it stay, so only it is written again. The lines above wait for the next summariser, so a rewrite costs one call, and the user sees the new line before paying for the rest. Returns how many lines it dropped and how many of those were above it.

`dry_run(thread, backend, rebuild, line)` (for `--dry-run`)
- One pass with `Run::dry` set: free lines and model replies go to the pass's in-memory map, so lines above them are built from them, and to the run's list instead of the store. With `rebuild` the pass reads no stored line (`Anew::All`), so it builds every line anew; with `line` it builds that line and the unbuilt ones under it, and with both it reads that one line as unbuilt (`Anew::Line`), as `rewrite` would write it. No lock: it writes nothing. Each line comes back with its first item (for its printed id) and the stored line it would replace. Logged under a `dry_run` span.

`Store::spawn_summariser(thread)` (for `ai agent sync-session`)
- Unless `chat.background` is false: this executable as `agent build -a <name>` from the project's directory, detached by `memory::log::spawn` (stdin/stdout null, stderr to the thread's log, own process group). No readiness check: a summariser with nothing to do exits after one pass, and sync stays fast.

Smoke (2026-10-15, haiku, throwaway repo, synthetic 4-turn thread): 3 calls, 1 free turn line, 1 node; the spawning process's piped stdout closed at once (spawn 0.3 ms); a second summariser exited `busy`.

The run owns the progress bar and clears it when dropped, so the report the CLI prints next never sits under a stale bar. Callers that show nothing pass `ProgressBar::hidden()`; the total is the lines written plus those still planned, so it grows when `summarise` reads a newer snapshot.
