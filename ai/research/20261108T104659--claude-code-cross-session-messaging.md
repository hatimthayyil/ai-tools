# Claude Code cross-session messaging

Author: Claude Code (Claude Opus 5.5)

What Claude Code's own messaging between sessions offers, set against the mail designed in `20261030T142550--agent-team.md` ("the team note"), `20261030T202150--claude-code-mail-delivery.md` ("the delivery note") and `20261030T200814--agent-identity-and-enforcement.md` ("the identity note"), and whether `ai` mail can wait.

Sources: Claude Code 2.1.319; its docs at `code.claude.com/docs/en/` (`cross-session-messaging`, `hooks`, `sessions`, `agent-teams`, `remote-control`, `env-vars`, `settings-reference`), fetched 2026-11-08; the `SendMessage`, `ListAgents` and `SendToProject` tool descriptions; strings of the binary. **Checked here** marks what throwaway sessions showed: `claude -p --input-format stream-json` sessions named `xs-recv` and `xs-send` in the scratchpad, logging hooks passed with `--settings`, and a node script posting to a session's socket. No session of the user's was listed, read or messaged.

## Answer

Yes, mail can wait. Between live Claude Code sessions on one machine, Claude Code already delivers messages to idle and busy sessions, carries replies, records each message in both transcripts in a form `ai agent` sync already turns into thread items, and lets a hook stop, rewrite or ask about every send. It does not hold mail for a stopped agent, prove a sender, reach other harnesses, or survive the identity note's sandbox. Those gaps need the sandbox (team note §11, layer 2) before `ai` mail could close them.

## Findings

### 1. What it is

- `ListAgents` and `SendMessage`, on with nothing to enable since 2.1.224 (docs). The same `SendMessage` reaches subagents, agent-team teammates, other sessions on this machine, and, only while this session is connected to Remote Control, the account's sessions on other machines and in the cloud.
- Same machine: each session binds a Unix socket, `/run/user/<uid>/cc-socks/<pid>.sock`, and registers in `<config dir>/sessions/<pid>.json` with its `sessionId`, `name`, `status` (busy, idle) and `messagingSocketPath`. Delivery never passes Anthropic servers (docs). **Checked here.**
- Other machines and cloud: through Anthropic servers over Remote Control, which needs a claude.ai login; on Team and Enterprise an Owner must turn Remote Control on (remote-control docs). Cloud sessions can't reply.
- Agent teams are a different feature: experimental (`CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS=1`), one lead session's team with file mailboxes at `~/.claude/teams/<team>/inboxes/<agent>.json`, removed when the session ends, not restored on resume, panes through tmux or iTerm2 only (agent-teams docs). They don't fit long-lived agents.
- `SendToProject` and `SendToProjectContext` are the last step of `/send-to-project`: one message, as the user, into a claude.ai cloud Project. They work only in the turn the user ran the command. They are not messaging between agents.
- Settings: `crossSessionInbound` (`accept`, `hold`, `refuse`), `isolatePeerMachines`, `dialogExpiry` (five minutes by default), and deny rules on `SendMessage` and `ListAgents`. Neither `/etc/claude-code/managed-settings.json` nor `~/.claude/settings.json` sets any of them, and `~/.claude/policy-limits.json` restricts nothing about messaging; it sets `remote_control_at_startup: false`. **Checked here.** Whether the work organisation allows Remote Control is unchecked.

### 2. Addressing

- A session's address is its name, from `-n`/`--name` or `/rename`, else one Claude Code makes up (`dotsend-a9` for a session in folder `dotsend`). A short `[ref]` tells apart rows that share a name. `uds:<socket path>` also works as `to`; replies use it. **Checked here.**
- `to` refuses `@`: sending to a session named `main@xs-probe` failed with "to must be a bare teammate name — there is only one team per session". `main.xs-probe` worked. **Checked here.** So `main@ai-tools` can be a session's name but never its address.
- A name is not an identity. A session started or resumed with a name a live session holds gets a variant of it instead (docs). Otherwise the name survives resume.
- Discovery reads one config folder's `sessions/`. A session started with its own `CLAUDE_CONFIG_DIR` registered only there, and a default session sending to it by name got "No agent named 'xs-cfg' is reachable". **Checked here.** Per-agent config folders (identity note) would hide agents from each other by name; the socket path still reaches across (`reply_across_default_dirs`, binary).

### 3. Delivery

- Idle: the message starts a new turn. **Checked here** for a `-p` session; docs for interactive ones.
- Busy: it waits and is read between tool calls, at the next tool round; a running tool is never interrupted (docs). Not checked.
- Not running: refused at the sender ("No agent named 'xs-gone' is reachable"). **Checked here.** No mailbox keeps it. Only a Remote Control session shown `offline` gets it later, on reconnect (docs).
- Replies: the receiver answers with `SendMessage` to the message's `from`. **Checked here**, both ways.
- "Sent" means it reached the inbox, not that the receiver read it. A message the receiver's hook blocked was still reported sent. **Checked here.**
- Limits (docs): about a million characters; bursts refused at the sender; per-sender rate limit; 50 queued; 100 held, held ones expiring after `dialogExpiry`.

### 4. Record

- Receiver: one `user` entry with `isMeta: true` and `origin: {kind: "peer", from: "uds:…sock", name, fromMode, msg_id, body, verifiedPeerPid, verifiedPeerProcStart, hopChain}`. The content is the body framed: "Another Claude session sent a message:", a `<cross-session-message from=… from-name=… from-mode=…>` block, then a paragraph telling Claude a peer can't grant permissions. **Checked here.** `ai agent` sync already reads such an entry as a `Work` item from `origin.name` with `origin.body` (`src/agent/claude.rs:378`), before it drops `isMeta` entries (`src/agent/claude.rs:331`).
- Sender: the `SendMessage` tool call (`to`, `message`, `summary`) and its result, `{"success":true,"message":"… in that session's inbox, not yet read …","msg_id":…}`. **Checked here.** Sync records both as a tool call.
- A hook's rewrite never reaches the sender's transcript: its tool call keeps the original text. Only `PostToolUse` and the receiver see the edited one. **Checked here.**
- A message a `UserPromptSubmit` hook blocks leaves no entry in the receiver, only a system notice. **Checked here.**
- `queue-operation` entries (enqueue, dequeue) carry no text. **Checked here.**

### 5. Control

- The receiver's user sees a dim one-line preview and can expand it. Nothing lets the user edit a message.
- Built-in approval exists only where the default rule holds a message: sessions in different permission classes (one bypasses prompts, the other doesn't). The receiver then shows an Approve/Deny dialog with no edit, closed after `dialogExpiry`. `isolatePeerMachines` asks before a message leaves the machine. Between two sessions that both prompt, every message is delivered (docs).
- Hooks, **checked here**:
  - `PreToolUse` on `SendMessage` sees `tool_input.to` and `tool_input.message`. `deny` stops the send, and its reason reaches the sender's Claude as the tool error. `allow` with `updatedInput` delivers an edited text. `ask` brings up the ordinary permission prompt (hooks docs; not checked).
  - `PostToolUse` sees the edited input and the result.
  - `UserPromptSubmit` runs on every delivered message. Its `prompt` is the `<cross-session-message …>` block for a session's message but the bare text for a raw socket post, and its input has no origin, so a hook can't tell a raw post from the user's typing.
  - `SendMessage` also carries subagent and fork reports, so a hook gating only mail between sessions must tell targets apart, for example by the names in `<config dir>/sessions/`.
- Outside processes, **checked here**:
  - Any process of the user can write `{"type":"user","message":{"role":"user","content":"…"}}` to a session's socket. On Linux no auth line is needed, and the text arrives as a peer message with `from: "unknown"`.
  - A node script that bound its own `<pid>.sock` in the socket folder and sent a `<cross-session-message from="uds:<its socket>" from-name="forged-main">` block got the receiver's reply on that socket: `{"msgV":1,"msg_id":…,"type":"user","message":{…},"priority":"next","from":"uds:…"}`.
  - The receiver recorded the claimed name `forged-main` as `origin.name`. Only the pid is proven.
  - Only the `type: "user"` line is documented; the block and the `msgV` envelope are internal and may change.
- A message carries text only, never the sender's history. No feature reads another session; transcripts are plain files.

### 6. Reach

- Harnesses: Claude Code only. Pi and DSH have no inbox. A plugin there could bind a socket speaking these lines, but it would copy an internal protocol.
- bubblewrap: two sessions reach each other only when both see the same `<config dir>/sessions/` and `/run/user/<uid>/cc-socks/` (docs). The identity note's sandbox hides the peer sockets because they are a write path around the gate; binding them back reopens it.
- herdr: delivery goes through the socket, not the terminal, so the pane makes no difference. Not checked inside herdr.
- Config folders: by name only within one; by socket path across them. **Checked here.**
- Accounts: on one machine no server is involved, so two logins sharing `~/.claude` reach each other. Across machines both ends need the same claude.ai account on Remote Control (docs). Not checked.

## Against our requirements

| Requirement (source) | Status |
| :-- | :-- |
| Every message waits for the user's approval (team note §4) | Met with glue: a `PreToolUse` hook on `SendMessage` returns `ask`, or calls a desk and waits for its decision. Not enforced: an agent's shell can write to any socket, and no hook can spot the post. |
| The user edits; the edit is delivered and the sender's thread holds it (§4) | Met with glue: `updatedInput` delivers the edit; the hook's `additionalContext` or `ai` must put the edited text in the sender's thread, whose transcript keeps the original. |
| Rejection returns a reason (§4, recommended) | Met with glue: a `deny` reason reaches the sender's Claude. |
| Only the user approves (identity note) | Met while the gate is a prompt in the user's pane; not enforced, for the same reason as the first row. |
| A message proves its sender (identity note) | Missing: `from-name` is a claim; only the pid is verified. |
| Hierarchy and read grants (§3) | Not a messaging matter. Reading stays `ai`'s (`reports-to` read grants built); messaging has no routes, and a hook could refuse some. |
| Delivery to an idle or busy agent (§4) | Met, without `$.prompt.submit`, whose idle wake the delivery note left unverified. |
| A stopped agent gets its mail at `resume` (§4) | Missing: refused, no mailbox. |
| Records in `~/ai` (§9, §10) | Met through today's sync: the receiver's entry becomes a `Work` item, the sender's call a tool call. A blocked or expired message leaves nothing. |
| Harness independence (identity note) | Missing: Claude Code only. |
| Mail across machines (open question 1) | Only over Remote Control, with a claude.ai login on both ends. |
| The sandbox (identity note, recommendation 1) | Conflicts: the sandbox hides the peer sockets that native messaging needs. |

## Recommendation

Postpone `ai` mail: the store, the gate and `ai desk` (team note layers 3 and 4). For live Claude sessions, which is how agents run today, native messaging already gives delivery, replies and records. What `ai` mail would add, an enforced gate, a sender it can prove, mail held for stopped agents and other harnesses, all rests on the sandbox (layer 2). The sandbox would cut native messaging off, so mail comes back when it lands, with `ai` outside the sandbox writing approved mail into the socket of the agent it's for.

Glue worth building now, smallest first:

1. `ai resume <agent>` passes `--name`, so the agent's session answers to its address, `<project>.<agent>` (`ai-tools.main`; team note §8). The spelling can't contain `@` (§2); a dotted name works (`main.xs-probe` was reachable). The bare agent name is shorter but collides across projects, and the collision gives one session a variant name.
2. The approval of each message belongs to the desk in Emacs (team note §7): a `PreToolUse` hook on `SendMessage` to another session returns `ask`, or waits for the desk's decision. It is advisory until the sandbox.

## Not verified

- Delivery to a busy session, and an interactive receiver's idle wake.
- `ask` from a `PreToolUse` hook on `SendMessage` in an interactive session.
- Remote Control on either account, and messaging across accounts or machines.
- Sessions inside herdr panes or bubblewrap.
