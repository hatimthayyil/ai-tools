# ai under agent-shell

Author: Claude Code (Claude Opus 5.5)

How `ai` works when Claude runs inside Emacs, in its two patterns: Claude Code's own terminal UI (a herdr pane, or an eat buffer from claude-code-ide or claude-code.el), and Claude over the Agent Client Protocol (ACP), driven by agent-shell. `ai` must support both.

Sources: emx at `/hatimthayyil/code/emx` (`packages.el:132-133`, `modules/ai.el:177-211`, `nix/emacs/default.nix:71-74`); agent-shell 0.91.1, acp.el 0.18.2, claude-code-ide 0.3.0 and claude-agent-acp 0.93.0 from the Nix store; Claude Code 2.1.319. **Checked here** marks what probes showed. A node script spoke ACP to claude-agent-acp the way agent-shell does. A batch Emacs (`emacs -Q`, scratch XDG dirs and init directory) ran agent-shell itself. Both ran in scratch projects, with an `ai` shim that logged each call and answered the agent plumbing as "not attached", so no store was written. No session or Emacs of the user's was touched.

## Answer

Mostly yes, today. Under agent-shell, Claude Code is the real CLI with the user's settings, so the `ai` plugin loads. Its hooks sync and end sessions. The memory reminder reaches the model. `/fold` folds. Transcripts land where `ai` reads them, so `ai reply` quotes them. Sessions take names and reach each other by Claude Code's messaging, and permission prompts, a hook's `ask` included, are Emacs buttons. Three things are missing:

- `ai agent resume` can't start an agent-shell session: it execs `claude`.
- An agent's session can be attached only after it starts, so it misses the thread's view at start.
- `/reply` can't fill a prompt box agent-shell doesn't have.

## Findings

### 1. The adapter

- agent-shell runs `claude-agent-acp` for Claude (`agent-shell-anthropic-claude-acp-command`, `agent-shell-anthropic.el:111`). emx puts nixpkgs' claude-agent-acp 0.93.0 on Emacs's PATH (`@agentclientprotocol/claude-agent-acp`, node 24), overridden so its `claude` is the one on PATH (`nix/emacs/default.nix:72-74`).
- The adapter drives Claude through the Agent SDK 0.3.300 (`acp-agent.js:8447-8506`). The SDK ships its own CLI, but the Nix wrapper sets `CLAUDE_CODE_EXECUTABLE`, so the user's installed CLI runs. **Checked here:** the session registered as version 2.1.319, `entrypoint: "sdk-ts"`, `kind: "interactive"`.
- Logged in with the machine's Claude login: agent-shell's default `:login t` clears `ANTHROPIC_API_KEY` (`agent-shell-anthropic.el:61, 182-187`).

### 2. What loads

- The adapter passes `settingSources: ["user", "project", "local"]` (`acp-agent.js:8449`), and managed settings load as well. **Checked here:**
  - `ai@ai`, enabled only in `/etc/claude-code/managed-settings.json`, loaded. `fold`, `ai`, `recall` and `reply` appear in ACP's `available_commands_update`, and agent-shell lists them in its welcome text.
  - User skills are advertised (`but`, `deslop`, …), and `~/.claude/CLAUDE.md` reaches the model.
  - Hooks fire:
    - SessionStart: the mod's `classic.SessionStart` called `ai agent compact-session` at startup, and stored the transcript path on resume.
    - Stop: `ai agent sync-session` ran after each turn.
    - Session end: `ai agent end-session` ran when the adapter closed.
    - UserPromptSubmit: the user's `ai memory prompt-hook` ran on every prompt. Its reminder was in the transcript as `hook_additional_context` when the prompt was long.
    - PreToolUse: a project hook ran.
- Plugin commands, **checked here:**
  - `/fold` works: "ai: Folding.", then a `manual` compact boundary whose summary is `ai view --whole`.
  - `/reply` answers "ai: Not quoted: this session has no prompt box." `$.prompt.fill` refuses `no_composer` (`quote` in `plugins/claude/hooks/register.ts` on `feat/reply`).
  - `/rename` works.
  - `/ai new|attach|detach` and `/recall` weren't run. They use the same `command.run` path (`/recall` adds `$.agent.spawn`).

### 3. Transcripts and resuming

- **Checked here:** the transcript is `~/.claude/projects/<cwd slug>/<session id>.jsonl`, in the same format as the terminal's (`custom-title`, `attachment`, `user` with `origin.kind: "human"`, `assistant`).
  - `ai reply -1`, `-2`, `0`, `2` and `3` quoted it, or named the turn, as on a terminal session.
  - A peer message became turn 3, a report "unknown".
- The session id is known only once `session/new` answers. The adapter picks it (`randomUUID`, `acp-agent.js:8245-8263`), and a client can't choose it. agent-shell keeps it in `agent-shell--state` `(:session :id)` and shows it in the header (`agent-shell--session-id-indicator`). The transcript path follows from it and the cwd (`claude::project_dir`).
- Resume, **checked here:** `session/resume` with the id carried on: same id and file, the earlier "pong" still remembered, the name restored. agent-shell resumes by id with `agent-shell-resume-session` or `:session-id` (`agent-shell.el:1773, 5189`).
- What `ai agent resume` passes and ACP's equivalent:

  | `ai agent resume` (`launch`, `resume`, `src/agent/cli.rs:649-728`) | ACP |
  | :-- | :-- |
  | cwd = the agent's worktree | `session/new` `cwd` |
  | `--session-id <uuid>` chosen before start | no equivalent: the id comes back from `session/new` |
  | `--resume <id>` | `session/resume` `sessionId` |
  | `--append-system-prompt <instructions>` | `_meta.systemPrompt.append` (`acp-agent.js:8316-8333`) |

### 4. Names and messaging

- `_meta.claudeCode.options.extraArgs.name` passes `--name` to the CLI. **Checked here:** the session registered `name: "probe.acp"`, `nameSource: "user"`. `/rename probe.renamed` renamed it. agent-shell sends a fixed `:session-meta` (`agent-shell-anthropic.el:157`), so emx would supply its own per session.
- An ACP session registers in `~/.claude/sessions/<pid>.json` with a messaging socket, like a terminal one. **Checked here:**
  - One ACP session sent to another by bare name `probe.peer`, and the reply came back.
  - A raw post to an ACP session's socket started a turn.
  - Not run: ACP to an interactive terminal session. It is the same registry and socket, and the earlier note checked terminal to terminal (`20261108T104659--claude-code-cross-session-messaging.md`).
- Display, **checked here:**
  - In agent-shell, a peer's turn shows as an "Activity" block and Claude's reply. Neither the incoming text nor the sender is shown.
  - The adapter sends no `user_message_chunk` for it; the origin rides only on `usage_update._meta["_claude/origin"]`. It also hides peer messages from replay (`acp-agent.js:926`).
  - Before its first prompt, an ACP session answers a peer message in the CLI, but the client gets the output only when the first prompt is sent, glued to that prompt's answer ("cold-okwarm-ok").

### 5. Permissions

- **Checked here:** a `PreToolUse` hook returning `ask` reached the client as `session/request_permission` with options Yes and No.
  - agent-shell drew it in the buffer: "⚠ Tool Permission ⚠", the command, `[ Yes (y) ] [ No (C-c C-c) ]`.
  - The hook's `permissionDecisionReason` was not in the request, so the user doesn't see why.
- `agent-shell-permission-responder-function` lets Elisp answer requests itself (`agent-shell.el:214, 3929`).

### 6. Compaction

- `/compact` is passed through. **Checked here:** right after a fold, it said "Not enough messages to compact."
- `/fold` works as on a terminal (§2). Claude refuses to compact an empty session, so `/fold` can't seed a fresh one.
- Not run: the adapter's auto-compaction (`context-compaction.js`).

### 7. The terminal pattern

- claude-code-ide runs `claude-code-ide-cli-path`, by default `claude`, in an eat terminal, adding `-r`/`-c`, its own `--append-system-prompt` and an MCP config (`claude-code-ide.el:109, 1125-1160`). emx sets `CLAUDE_CODE_AUTO_CONNECT_IDE=true` for it (`modules/ai.el:204-211`).
- It is the real interactive CLI, so the plugin, `/reply`'s prompt box and everything else work as in herdr. The user's gen 6 chat test ran there (memory note 1342).
- But it starts `claude`, not `ai agent resume`, so an agent's session through claude-code-ide has no `ai` naming, instructions or attachment unless attached by `/ai attach`.

## Features against the two patterns

| `ai` feature | Terminal (herdr, eat buffer) | agent-shell (ACP) |
| :-- | :-- | :-- |
| Plugin loads, commands listed | Works | Works |
| Sync per turn, end of session | Works | Works |
| Memory reminder (UserPromptSubmit) | Works | Works |
| Thread view at start of an attached session | Works: `ai agent resume` binds the id first | Needs glue: attaching waits for `session/new`, after SessionStart's seed |
| `ai agent resume <name>` | Works in herdr or a plain eat buffer; claude-code-ide runs `claude` itself | Needs glue: `ai` must hand emx the plan (cwd, resume id or none, instructions, name), then attach the new id |
| `/fold` | Works | Works, on a session with messages |
| `/reply` | Works | Needs glue: emx inserts `ai reply`'s output with `agent-shell-insert` |
| `ai reply` on the transcript | Works | Works |
| `/ai new\|attach\|detach`, `/recall` | Works | Likely, by the same path; not run |
| Session name `<project>.<agent>` | `--name` | `_meta.claudeCode.options.extraArgs.name`, or `/rename` |
| Cross-session messaging | Works | Works; agent-shell shows the reply only, and only after the first prompt |
| Permission `ask` | Claude's dialog | Emacs buttons, without the hook's reason |
| Transcripts in `~/.claude/projects` | Works | Works; agent-shell also writes `.agent-shell/transcripts/*.md` in the project and adds `.agent-shell/` to its `.gitignore` (`agent-shell.el:4725-4760`) |

## Smallest changes for `ai`

1. **A resume plan as structured output.** For example, `ai agent resume <name> --json` prints, without exec, what it would run: cwd, the session to resume or none, the instructions, and the session name. emx turns that into `session/new` or `session/resume`, then runs `ai agent attach <name> --harness claude --session <id> --transcript <path>`. The plan stays in `ai`, so emx doesn't copy `resume`'s choices (`src/agent/cli.rs:692-728`). This belongs with the structured output already agreed (memory note 1375).
2. **The plugin seeds late attachments.** On the first turn of an attached session that SessionStart didn't seed, the plugin adds the thread's view as context. This also serves `/ai attach` in a terminal session.

Nothing in `ai` changes for transcripts, sync, the reminder or `/fold`.

emx's side, not `ai`'s:
- the per-session `_meta` (name, instructions)
- resume by id
- `/reply` through `ai reply` and `agent-shell-insert`
- moving agent-shell's `.agent-shell/` out of repositories with `agent-shell-dot-subdir-function`
- a first prompt, such as the seed, so peer messages show when they come

## Recommendation

Support both patterns as above. The terminal pattern needs nothing from `ai`. agent-shell needs the two changes, smallest first: the resume plan, then the late seed. Build the resume plan with the structured output work. Leave the rest to emx.

## Not verified

- `/ai attach`, `/recall` and the late seed under ACP.
- ACP to an interactive terminal session by name, and the reverse.
- The adapter's auto-compaction.
- `session/resume` carrying `_meta.systemPrompt`.
- An agent-shell session inside the user's emx init rather than `emacs -Q`.
