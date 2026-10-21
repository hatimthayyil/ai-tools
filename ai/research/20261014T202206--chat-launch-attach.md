# ai chat: launching, attaching, continuity, reset

How `ai chat <harness> <thread>` (launch) and `ai chat resume <thread>` (attach from inside) bind a harness session to a thread, keep it bound across `/clear`/`/new`, and reset context with our view. Builds on `20261014T001824--claude-code-sessions-hooks.md` and `20261014T001805--pi-codex-sessions.md`; their findings are not repeated.

Versions: Claude Code 2.1.300 (binary), pi 1.1.0 (installed; local source 0.86.1 at `19451accd`), codex-cli 0.162.1 (local source `a6c5f3a9fa`, 2026-10-05).

Labels: **[doc]** official docs (code.claude.com fetched 2026-10-14; pi 1.1.0 `docs/` in the nix store, below `$PI`; learn.chatgpt.com/docs/hooks); **[src]** source; **[exp]** run here; **[inf]** inference.

Experiments: scratch dir `/tmp/claude-1000/.../scratchpad/cc/` (hook logger `hook.sh`, mod `mod/`). Claude runs used `--model haiku`, `--setting-sources project|local` (user hooks excluded), one interactive run in a private tmux server in the already-trusted throwaway dir `~/.cache/cc-blackhole-feasibility/hooks/proj`. Six haiku calls in total. pi and Codex were not run (see Limits).

## Claude Code

### 1. Launch

- **Session id.** `--session-id <uuid>` starts a session with that id [doc cli-reference]. Hooks' `session_id`, Bash's `CLAUDE_CODE_SESSION_ID` and the transcript file name all carry it [exp]. Reusing an existing id fails: `Error: Session ID … is already in use.`; continuing is `--resume <id>` [exp].
- **Per-process hooks.** `--settings <file-or-json>` is a settings level above user/project/local [doc settings#settings-precedence]. "Hook entries merge across settings levels rather than replacing each other" [doc hooks#hook-locations]. [exp] A project SessionStart hook and a `--settings` SessionStart hook both fired, for file and inline JSON forms. `--plugin-dir <dir>` loads a plugin (settings hooks in `hooks/hooks.json`, a mod, commands) for one session [doc cli-reference, plugins/mods/reference#files].
- **Initial view.**
  - SessionStart `source:"startup"` `additionalContext` reached the model [exp: `PAPAYA-7` answered back]. Capped at 10,000 characters per field [prior note §4].
  - Interactive SessionStart hooks run in the background; the first response waits for them [doc hooks#sessionstart].
  - `--append-system-prompt[-file]` is recorded with the system prompt on the first request and reused until compaction, also on `--resume` [doc cli-reference#system-prompt-flags-in-resumed-conversations]. A `/clear` starts a new conversation that gets the same flag text again [inf]. So it cannot carry a view that changes.
  - Cost: the system prompt is the first cache layer; hook context sits in the conversation layer after project context [doc prompt-caching#how-the-cache-is-organized]. A per-thread system prompt would also stop parallel sessions in one directory from sharing the system-prompt prefix [doc prompt-caching#cache-scope; inf]. **Use SessionStart `additionalContext`**; keep `--append-system-prompt` for fixed text, if any.
  - `--append-system-prompt` has no documented size cap [doc absence]; argv strings are limited to 128 KiB each on Linux [inf], so use the `-file` form.

### 2. Attach from inside

- Bash, hook and stdio-MCP subprocesses get `CLAUDE_CODE_SESSION_ID`, "updated on `/clear`" [doc env-vars], and `CLAUDE_PID`, Claude Code's own pid [doc env-vars]. [exp] Bash saw the new id after `/clear` (`SID=651c9be7…`), and a `CLAUDE_ENV_FILE` export from SessionStart(clear) (`PROBE=from-envfile-651c9be7…`).
- **Subagents.** A subagent's Bash gets the **parent's** session id and `CLAUDE_PID` [obs: this research subagent's env holds `799603ff…`, the parent's id]. Nothing in the environment marks a subagent [prior note §3]. So `ai chat resume` run from a subagent binds the parent session and prints the view into the subagent's context [inf]. Hooks do see `agent_id` for subagent tool calls [doc hooks#common-input-fields]; a PreToolUse guard could refuse `ai chat resume` there [inf].
- The view printed by `ai chat resume` is capped by the Bash output limit: 30,000 characters by default, `BASH_MAX_OUTPUT_LENGTH` up to 150,000 [doc env-vars].

### 3. Continuity across `/clear`

- [exp, interactive] `/clear` fired SessionEnd(`reason:"clear"`, old id), then SessionStart(`source:"clear"`, new id) 37–49 ms later. Both had the same `CLAUDE_PID` (= the hook's PPID). Twice: `412f1f7b → 651c9be7 → 6844c95b`, pid 1296220.
- The hook input carries no previous-session field [doc hooks#sessionstart-input]. The link is the process: `CLAUDE_PID` is set in hooks [doc env-vars; exp].
- **Do not carry the binding on `resume`.** In-app `/resume` and `/branch` also end and start sessions in the same process: mods report `/branch` as `reason:"resume"` [doc plugins/mods/reference#session]. Carrying the binding by pid there would bind an unrelated conversation [inf]. Carry only on SessionEnd(clear) → SessionStart(clear); resolve `resume` by session id.
- **Robustness** [inf]:
  - Two sessions in one multiplexer are two processes, so two pids: no ambiguity.
  - A restart with `--resume <id>` keeps the id, so the session-id binding still holds.
  - Pid reuse matters only within the ~50 ms handoff. Key the handoff by pid plus process start time (`/proc/<pid>/stat`), or expire it after a few seconds.
- The user's herdr multiplexer already tracks sessions per pane the same way: a SessionStart hook keyed by `HERDR_PANE_ID` (`~/.claude/hooks/herdr-agent-state.sh`) [obs].

### 4. A user command that resets with our view

**Settings-level routes: none.**
- `/compact` and `/clear` are not reachable through the Skill tool, and hooks cannot run slash commands [prior note §2].
- Keybinding actions include `chat:submit` and `chat:clearInput` (a redraw), but no action that runs a slash command [doc keybindings].

**Mods can** (plugins with a JS hooks module; terminal ≥2.1.287; in `claude -p` too; "EARLY ACCESS: this surface may change between releases without notice" [src claude-code.d.ts header; doc plugins/mods/overview]):

- **Replace the compaction result.** A `session.compact` hook can "answer `{ messages }` of your own" [src d.ts `session.compact`]. [exp, `-p`] A native `/compact` (trigger `manual`) answered by the mod gave:
  - `compact_boundary` with `durationMs:9`, `postTokens:60` and no summarizer call;
  - then our view as a plain `user` entry (no `isCompactSummary` flag);
  - then the SessionStart(compact) settings hook still ran.
  - The settings PreCompact hook did not run, because the mod answered without calling `next`.
  - Next turn: the model listed only `MANGO-42` (our view) and `PAPAYA-7` (the hook), not the pre-compaction `APPLE-1`.
  - [exp, interactive] Same through a mod command: `$.command.run({command:'compact', args})` scheduled from `$.clock.after`, with the same mod answering. 80 ms, no model call.
  - The same event fires for `auto`, `plugin` and `precompute` triggers [src d.ts `SessionCompactTrigger`], so auto-compaction can be replaced too.
- **Trigger `/clear` or `/compact`.** `$.command.run` "runs a slash command as if the person typed" it [src d.ts]. [exp]
  - From a mod command handler, `/clear` worked when deferred with `$.clock.after(200, …)`. SessionEnd(clear) and SessionStart(clear) fired as usual.
  - **Agent-triggered:** a mod tool `chat_reset` that the model called queued `/clear` the same way, and it ran once the turn ended [exp: session `651c9be7 → 6844c95b` after the tool call].
- **Constraints** [exp]:
  - Called directly inside a `command.run` hook, both fail: `session.compact: called from a command.run hook, it would compact under the turn this hook is holding; call it from a later event (turn.complete)`, and the matching `command.run` error. Defer with `$.clock.after`.
  - `$.session.compact()` "runs through every hook but the calling one" [src d.ts]. Called from our own mod, our `session.compact` answer is skipped and the native summarizer runs [exp: a native summary appeared and PreCompact fired]. **Use `$.command.run({command:'compact'})`, or let the user type `/compact`.**
- **Validation note.** `claude plugin validate`: "hooks on session.compact do not run in a plugin a person installs where an organization manages Claude Code" [exp]. Not an issue on this personal machine.
- **Global install.** A mod loads per session with `--plugin-dir`, for every session through `CLAUDE_CODE_PLUGIN_DIRS` (env or settings `env`), or from a marketplace [doc plugins/mods/reference#settings-and-environment-variables, env-vars]. A plugin's `hooks/hooks.json` can hold both `modules` and settings `hooks` [doc plugins/mods/reference#files].

### 5. Global hooks, cost

- [exp, `--debug-file`] A command hook running `ai --version` took 93 ms on the first SessionStart (cold), 20 ms on UserPromptSubmit, and 11 ms on Stop. `ai --version` alone is ~2 ms, `bash -c true` ~4 ms [exp].
- `"async": true` keeps Stop off the critical path; no timeout is enforced on async hooks [doc hooks#run-hooks-in-the-background].
- Default timeouts [doc hooks#common-fields]:
  - 600 s for `command`;
  - 30 s for UserPromptSubmit;
  - SessionEnd shares a 1.5 s budget, raisable per hook up to 60 s.
- Mods [doc plugins/mods/reference#limits]:
  - 10 s of a hook's own execution, excluding API calls;
  - `$.process.run` 30 s by default, 10 min at most;
  - `session.end` within the SessionEnd budget.

## pi (1.1.0)

pi was not run here (see Limits). Claims are [doc]/[src].

### 1. Launch

- `--session-id <id>` "Opens the exact project session ID or creates it if absent"; it cannot be combined with `--session`, `--continue` or `--resume` [doc `$PI/docs/cli.md:92-107`]. `--session <path|id>` opens an existing one.
- `-e/--extension <path>` loads an extension for one run and is repeatable [doc cli.md:194]. `pi.registerFlag()` adds CLI flags [doc extensions.md; src types.ts:1354-1370].
- Initial view, in-process with no cap documented:
  - `session_start` (`reason:"startup"`) + `pi.sendMessage({customType, content}, {deliverAs:"nextTurn"})`: "Queued for next user prompt. Does not interrupt or trigger anything" [doc 0.86.1 extensions.md:1475];
  - or `before_agent_start` returning `{message}` [src types.ts:1165-1169].
- Prefer a message to a system-prompt edit: prompt changes are appended as transcript deltas and can force a full checkpoint for some providers [doc 1.1.0 extensions.md "Activate tools dynamically"].

### 2. Attach from inside

- The bash tool sets `PI_SESSION_ID` and `PI_SESSION_FILE` from `ctx.sessionManager` at each spawn [src tools/bash.ts:177-192; doc environment-variables.md:26-27]. After `/new` they follow the new session, since they are read per spawn [inf].
- Output truncation is 50 KB / 2000 lines [src tools/truncate.ts:11-12].
- pi core has no subagents. Extension subagents run their own pi processes, so the variables describe the child [inf].

### 3. Continuity across `/new`

- `session_start` carries `reason:"new"|"resume"|"fork"` and `previousSessionFile` [src types.ts:563-569].
- `newSession()` emits `session_shutdown(reason:"new", targetSessionFile)`, then `session_start(reason:"new", previousSessionFile)` [src agent-session-runtime.ts:226-260].
- So the extension links old → new with no pid bookkeeping. It is in-process, so multiplexers and parallel sessions cannot confuse it [inf]. Carry the binding on `new` and `fork`; resolve `resume` by file.

### 4. A user command that resets with our view

- `ctx.compact({customInstructions})` exists on every `ExtensionContext`, tool handlers included [src types.ts:346-347].
  - A `session_before_compact` handler returns `{compaction:{summary, firstKeptEntryId, tokensBefore}}` [prior note §3].
  - So `/chat-compact` (and an agent tool) can compact with our view as the summary.
  - It competes with pi-blackhole, which the user runs and which owns `session_before_compact` [prior note §3].
- `ExtensionCommandContext.newSession({parentSession, setup, withSession})` [src types.ts:356-393]:
  - `setup(sessionManager)` can append our view (`appendMessage` of a custom message, src session-manager.ts:1113) before the session runs;
  - `parentSession` links the files;
  - it is command-only ("calling them from lifecycle handlers can deadlock") [doc 1.1.0 extensions.md "Context and session changes"].
  - `/chat-new` = clear + our view, with no compaction owner conflict.

### 5. Cost

Extensions run in-process [doc]. An untracked session costs a map lookup [inf]. Spawn `ai chat` only when bound.

## Codex (0.162.1)

### 1. Launch

- **No session-id flag** [`codex --help`]. `codex resume <id>` takes an existing id only.
- **SessionStart fires lazily.** It is queued as pending and runs at the start of the first turn [src core/src/session/session.rs:1941-1966, core/src/session/turn.rs:323]. So the thread id is first known inside the first turn's hooks.
- **Shared daemon.** The TUI uses a shared app-server daemon by default (`DaemonAutoStart` stable, default on [src features/src/lib.rs:979-980]). Core and hooks run in the daemon. Hooks are spawned as `$SHELL -lc <cmd>` in a new session, with a "session snapshot" environment [src hooks/src/engine/command_runner.rs:384-437]. So launcher-set environment and PPID do not reach hooks [inf]. Thread start requests carry no environment [src app-server-protocol v2/thread.rs `ThreadStartParams`].
- **Hooks via `-c` are possible but costly** [src]:
  - `-c` hooks land in the `SessionFlags` layer [src hooks/src/engine/discovery.rs:833].
  - Non-managed hooks run only when trusted. Trust means a matching `hooks.state.<key>.trusted_hash`, honoured from the User and SessionFlags layers, or `--dangerously-bypass-hook-trust` [src discovery.rs:679-737, hooks/src/config_rules.rs:15-27; doc]. The hash is Codex's internal TOML identity hash [src discovery.rs:769-794].
  - The bypass flag trusts every hook, project hooks included.
  - Any `-c` other than a few feature flags, or the bypass flag, forces embedded mode (no daemon) [src tui/src/daemon_startup.rs:90-125].
- **Simplest launch** [inf]: global hooks in `~/.codex/hooks.json`, trusted once through `/hooks` [doc]. The user already runs `ai memory hook` there.
  - Launch as `codex [args] "<first prompt>"`: the positional prompt starts the session [`codex --help`].
  - Either the prompt asks the agent to run `ai chat resume <thread>` (attach path), or it carries a marker that the UserPromptSubmit hook binds by `session_id` and answers with `additionalContext`.
- **Injection cap.** About 2,500 tokens per hook message by default; `additionalContextLimit` raises it, and `0` removes it [doc hooks].

### 2. Attach from inside

- Shell commands get `CODEX_THREAD_ID` (this thread) and `CODEX_SESSION_ID` (root session) [src core/src/exec_env.rs:30-50].
- Hook `session_id`: "Subagent hooks use the parent session id" [doc hooks]. So a subagent's shell is detectable by `CODEX_THREAD_ID != CODEX_SESSION_ID` [inf]; `ai chat resume` should refuse there.
- Shell output is bounded by `tool_output_token_limit` [src core/src/config/mod.rs:933].

### 3. Continuity across `/clear` and `/new`

- `/clear` starts a thread with `ThreadStartSource::Clear`, giving SessionStart `source:"clear"` [src tui/src/chatwidget/slash_dispatch.rs:303-305, tui/src/app/event_dispatch.rs:411-425, core/src/session/session.rs:1951].
- `/new` opens the checkout picker, then starts a thread with no source [src slash_dispatch.rs:234-236, event_dispatch.rs:166-170]. That gives `startup` [inf].
- The old thread is detached, not shut down [src tui/src/app/session_lifecycle.rs:1032]. SessionEnd has a fixed reason and fires at thread shutdown [src core/src/hook_runtime.rs:471-499, core/src/session/handlers.rs:325], so it does not mark a `/clear`.
- No field links the new thread to the old one [src `ThreadStartParams`], and the hooks share the daemon process. **No reliable old → new link** [inf].
- `/compact` keeps the thread id, and SessionStart(compact) injects "before the next model request" [doc hooks].
- `codex resume <id>` keeps the id [prior note §4].

### 4. A user command that resets with our view

- No user command runs code [inf from the TUI slash-command set; custom prompts expand text only].
- PreCompact/PostCompact can only stop [prior note §4].
- Route: `/compact` + SessionStart(compact) `additionalContext` (raise `additionalContextLimit`). The native summary stays and is opaque.

### 5. Cost

- Default hook timeout is 600 s; SessionEnd/Interrupt 1 s, at most 3 s [doc hooks; src discovery.rs:741-765].
- A login shell spawn is ~5 ms here [exp: `bash -lc true`].

## Limits of this note

- **pi was not exercised.** pi 1.1.0 startup (`--mode rpc`, `--list-models`) hung with no output. It ignored SIGTERM, and each start spawned `gpg2 -d … ~/.password-store/*.gpg` (pass-backed API keys), which raised a `pinentry` dialog on the display. The probe extension (`scratchpad/pi/probe.ts`) never ran.
- **Codex was not run.**
- The Claude mod findings come from `-p` and one interactive session on 2.1.300. The mods API is early access.

## Implications for ai chat

One binding store, keyed by harness session id (Claude session uuid, pi session id/file, Codex thread id). Hooks resolve the thread from it and no-op when absent. The launcher is "bind, then exec"; attach is "bind from env, then print the view".

**Claude Code: the simplest working route.**
1. `ai chat install` adds global settings hooks: SessionStart (all sources), Stop (`async`), SessionEnd, each running `ai chat hook` (~10–20 ms when unbound).
2. `ai chat claude <thread> [-- args]` generates a uuid, binds it, and execs `claude --session-id <uuid> args…`. SessionStart(startup) injects the view's first ≤10k characters, with "page on with `ai chat resume <thread> 2..`".
3. `ai chat resume <thread>` from Bash binds `CLAUDE_CODE_SESSION_ID` and prints the view (≤30k by default). It cannot tell a subagent from the main agent.
4. Reset is the user's `/clear`. SessionEnd(clear) records a handoff under `CLAUDE_PID` (+ start time); SessionStart(clear) consumes it, rebinds the new id and injects the view. On `resume` or `fork`, look up by session id only.
5. Next layer: a small mod (same plugin, loaded through `CLAUDE_CODE_PLUGIN_DIRS`) whose `session.compact` hook answers manual and auto compactions with `ai chat` output (`$.process.run`). That replaces the native summary, costs no summarizer call, and keeps the session id. Optionally add `/chat-compact` and an agent tool, both via `$.command.run({command:'compact'})` from `$.clock.after`. Early access: keep `/clear` as the fallback.

**pi: the simplest working route.**
1. A global extension, `~/.pi/agent/extensions/ai-chat.ts`, in-process and free when unbound.
2. `ai chat pi <thread>` binds a fresh id and execs `pi --session-id <id> args…`. `session_start(startup)` sends the view with `deliverAs:"nextTurn"`.
3. Attach uses `PI_SESSION_ID`/`PI_SESSION_FILE`.
4. Continuity: `session_start(new|fork, previousSessionFile)`.
5. Reset is `/chat-new`: `ctx.newSession({parentSession, setup})` appends the view to the new session, with no compaction-owner conflict with pi-blackhole. `/chat-compact` through `session_before_compact` stays opt-in, for users without another compactor.

**Codex: the simplest working route.**
1. Global hooks in `~/.codex/hooks.json`, trusted once in `/hooks`: SessionStart(`compact|resume`), UserPromptSubmit, Stop. Set `additionalContextLimit` on the view hook. Avoid `-c` hooks: they need a trust bypass and drop the daemon.
2. `ai chat codex <thread>` execs `codex args… "Run \`ai chat resume <thread>\` and continue."`. That is the attach path, with no session-id flag needed.
3. Attach uses `CODEX_THREAD_ID`; refuse when it differs from `CODEX_SESSION_ID`.
4. Reset is `/compact` (same thread) + SessionStart(compact) view. `/clear` and `/new` start an unlinked thread; tell the user to re-run `ai chat resume <thread>` there.

**Uniform fallback.** "Exec the harness with the first prompt `Run ai chat resume <thread>`" works in all three with zero harness flags. It costs one model turn, and the view is bounded by tool-output limits (Claude 30k chars, pi 50 KB, Codex `tool_output_token_limit`).
