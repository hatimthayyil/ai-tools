# Agent identity and enforcement

Author: Claude Code (Claude Opus 5.5)

How `ai` can know which agent is acting, and enforce what it may do, when every agent runs as the same OS user. It serves `20261030T142550--agent-team.md` ("the team note") §3, §4 and §10, and replaces that note's recommendation that the session binding alone identifies a sender. Nothing here is built. **Agreed** marks the user's decisions; the rest is recommended.

Sources are cited by path and line. Local repos: ai-tools at the working tree, Codex at `/hatimthayyil/code/codex` (a6c5f3a9fa), devenv at `/hatimthayyil/code/devenv` (34a93eec), pi, deepseek-harness, nixpkgs and llm-agents.nix under `/hatimthayyil/code`. Cloned for this note: nono (f9f58b7), sandbox-runtime (98a4016), claudebox (2a937ed), jai (ed1f58e), container-use (2e43e62), fence (737751a). Claude Code claims cite its docs in `~/.claude-code-docs/docs`, version 2.1.314. **Checked here** marks what was run on this machine (NixOS, kernel 6.18, user hatim); everything else was read.

## What must hold

- An agent cannot pose as another agent or as the user.
- Only the user approves, edits or rejects mail.
- An agent reads only its own thread and the threads of the agents below it, through any number of superiors.
- An agent cannot change what decides the above: the hierarchy, the gate's decisions, keys.

## What breaks it today

**Identity is a claim.**
- `ai` takes the session from `CLAUDE_CODE_SESSION_ID` (src/agent/claude.rs:19-23) or from the hidden `--session` and `--harness` flags (src/agent/cli.rs:201-216). Memory notes record their session the same way (src/memory/prov.rs:72-86). pi exports `PI_SESSION_ID` to its shell likewise (pi/packages/coding-agent/src/core/tools/bash.ts:194-201).
- Every live Claude session's id is public to the user's processes: `~/.claude/sessions/<pid>.json` holds `sessionId` and `messagingSocketPath`. **Checked here** (keys only).

**Reads go around `ai`.**
- One git repository holds every thread of a project, `.ai/local` (src/project.rs:149), committed as a whole (src/repo.rs:55-58). Whatever can commit there can read every thread through its objects.
- Claude Code writes every session of a repo into one folder, `~/.claude/projects/<dir>/`, and the thread store keeps verbatim transcript copies in `sources/` (src/agent/store.rs:37).

**Writes go around the gate.**
- herdr's socket (`HERDR_SOCKET_PATH`, in `~/.config/herdr`) offers `pane read`, `agent read`, `agent prompt`, `send-text` and `send-keys` to any process in a pane: read another agent's screen, or type into it. Read from `herdr pane --help` and `herdr agent --help`; not exercised.
- Claude Code's peer sockets sit in `/run/user/1000/cc-socks` (note 971). **Checked here** (location).
- The session D-Bus (`/run/user/1000/bus`) and systemd's user manager start processes outside any sandbox (`systemd-run --user`). Not exercised.

**Policy is writable.** `ai.toml` at user, project and private level (src/config.rs:17-47) are ordinary files in the user's home.

**What agents cannot change.** `kernel.yama.ptrace_scope` is 1 and `sudo` asks for a password. **Checked here.** So the kernel, root-owned files and the Nix store are out of an agent's reach; everything the user owns is within it.

## Agreed

1. Mail is stored in per-sender outboxes, and the desk writes approved mail into the recipient's inbox (recommendation 2, team note §10).
2. One Claude credentials file is bound read-write into every sandbox (recommendation 4).
3. A session not started through `ai resume` counts as the user's own ("The gap that remains").
4. The setting is `reports-to = [...]` (recommendation 5).
5. One git repository per thread (recommendation 3).
6. Every agent has full network for now.
7. `main` runs sandboxed like every other agent.
8. No sandbox gets the ssh agent by default; a setting in the agent's `[agents.<name>]` table grants it (recommendation 1). The key's name is the implementation's.

## Recommendation

**1. `ai resume` starts every agent inside its own sandbox, around the whole harness.** `ai resume` is already the one place an agent starts: it commits the store and execs `claude` (src/agent/cli.rs:578-586). It execs bubblewrap instead, with the agent's grants:

| Access | What |
| :-- | :-- |
| read-write | the checkouts it works in; its own thread; its own outbox; its own harness state; a private `/tmp`; `ai memory` (shared by design) |
| read-only | the threads of every agent below it; its own inbox; the `ai.toml` files; the system and `/nix/store` |
| absent | every other thread and outbox; other agents' harness state; herdr's socket; Claude's peer sockets; the session D-Bus and systemd's user socket; the gpg agent; the ssh agent, unless granted; other processes (own PID namespace) |
| allowed | network; the Nix daemon socket, for builds; the ssh agent, for an agent whose settings grant it |

Mask by construction: a tmpfs over `~/.ai`, `~/.claude`, the project's `.ai/local` and the runtime directories, then bind back the grants. A path not bound in does not exist inside. **Checked here:** with `bwrap --ro-bind / / --tmpfs <stores> --bind <own> --ro-bind <subordinate>`, the agent wrote its own thread, read but could not write the subordinate's, could not see a third (`No such file or directory`), could not see `~/.claude`, and saw 4 processes.

A sandboxed agent that runs `ai resume` for another agent gets a nested sandbox that can only narrow what it already has.

**2. Identity is location.** Inside its sandbox an agent can write only its own outbox, so a message's sender is the owner of the outbox it sits in. The desk runs outside every sandbox, as the user: it reads outboxes, and on approval writes the message, as approved or edited, into the recipient's inbox, which the recipient can read but not write. No environment variable, flag or token decides who sent a message or who approved it. The session binding stays, to find an agent's thread, never as authority.

**3. One git repository per thread.** A thread's repository is then exactly what its grants cover: its own read-write, a subordinate's read-only.

**4. Each agent gets its own harness state.** Claude Code reads and writes `~/.claude/projects/<dir>/` for its own session, and agents in one repo share that folder. A `CLAUDE_CONFIG_DIR` per agent separates them; `ai` already honours that variable (src/agent/claude.rs:26-31). pi and DSH take a per-agent home the same way (`$DSH_HOME`, deepseek-harness apps/cli/reference/README.md:137). One Claude credentials file is bound read-write into every sandbox, so agents share the user's login (Agreed).

**5. Rank stays the setting; `ai` compiles it into grants at launch.** An agent's read grants are the threads of every agent below it. Several superiors make the hierarchy a graph, not a tree; `ai` refuses a cycle. The setting is a list in the agent's own table:

```toml
[agents.developer]
reports-to = ["lead", "engineer"]
```

`reports-to` names the relation, reads as plain English, and takes several names (Agreed). Explicit per-thread grants would be a second setting that nothing needs yet.

**6. bubblewrap, invoked by `ai`, from a path fixed at build time by Nix.** Codex looks `bwrap` up only outside the working directory, so a repo cannot plant one (codex-rs/linux-sandbox/README.md). Nix can do better: the store path is compiled in.

### What this changes in agreed design

- **Mail store.** A sender appending into the recipient's thread would need write access into every recipient's thread. Each sender writes only its own outbox, and the desk delivers approved mail into the recipient's inbox (Agreed, team note §10): every file keeps a single writer, and the path proves the sender.
- **Order.** The sandboxed launch comes before mail: mail's sender identity and the gate rest on it.
- **The CLI's own checks** become friendly errors. The kernel enforces; `ai` explains.

### The gap that remains

A session not started through `ai resume`, such as `claude` typed in a terminal, runs unsandboxed and can write anything the user can, including the desk's decisions. Such a session counts as the user's own terminal (Agreed): a sandboxed agent has no herdr socket, session bus or systemd user manager through which to start a process outside its sandbox, so only the user starts one. Signing each approval with the user's hardware key, which an agent cannot use without the user's touch, would close the gap further.

## Capabilities

**Bearer tokens don't help on one host.** Macaroons (Birgisson et al., NDSS 2014) and Biscuit (Eclipse Biscuit specification) are bearer credentials: holding one is having the right. Their strength is attenuation and offline verification across services. On one host the token is a file, as reachable as the threads it would protect, so it reduces to the same question: what can this process open? They earn a place when mail crosses hosts.

**File descriptors are capabilities, but don't reach the agent's tools.** A Unix socket held by one process cannot be opened through `/proc/<pid>/fd` by another process of the same user: `No such device or address`. **Checked here.** But Node passes no inherited descriptor beyond 0-2 to the children it spawns. **Checked here** (Node; Claude Code's own binary not tested). A descriptor handed to the harness would never reach `ai` run from its shell.

**A sandbox's grant set is the capability.** It is fixed when the process starts, inherited by every child, and only narrowed by them. nono states the same invariant for Landlock: a sandboxed process cannot expand its own policy, only a trusted component outside it can delegate (docs/cli/internals/security-model.mdx, "Capability Expansion and Irreversibility").

## OS enforcement

| Mechanism | What it gives | Cost to this workflow |
| :-- | :-- | :-- |
| bubblewrap | Unprivileged user, mount and PID namespaces; allow-list filesystem; hides other processes. **Checked here.** | Nix daemon socket and git config bound in; one `bwrap` per agent launch. Used by Claude Code's Bash sandbox (sandboxing.md:567), sandbox-runtime (src/sandbox/linux-sandbox-utils.ts:3611, :3805-3833), Codex, DSH and claudebox. |
| Landlock | Unprivileged path rules; ABI 7 here (**Checked here**). | Cannot hide processes or isolate pathname Unix sockets: Codex rejects it for that (codex-rs/linux-sandbox/README.md). nono pairs it with a seccomp supervisor and states that `stat` and `access` still enumerate the filesystem (security-model.mdx, "Information leak surface"). |
| Separate OS users | Kernel permission checks; survive a sandbox bypass. jai's strict mode runs as a different uid (jai README). | Users declared in NixOS; a root helper to switch user; a shared checkout owned by a group (GitButler writes across it); per-user Nix profile, credentials and herdr access. The strongest, and the heaviest. |
| systemd-run sandboxing | `ProtectHome=`, `ReadOnlyPaths=`, `PrivateTmp=` (systemd.exec(5)). | An agent becomes a unit, not a pane process; the user manager it relies on is the escape route above. |
| Containers and microVMs | A perimeter: container-use (Dagger containers with a git branch each, README), Docker Sandboxes (microVMs, llm-agents.nix packages/docker-sbx), NixOS containers. | The shared checkout, GitButler workspace, Nix store and herdr all cross the perimeter; inside it, everything is flat (nono's argument, security-model.mdx). |

## Nix and devenv

- **devenv** offers no process isolation: its source has no bubblewrap, Landlock or namespace code. It builds OCI containers from an environment (docs/src/containers.md), runs processes, and resolves secrets through SecretSpec, whose keyring backend prompts before handing a secret out (docs/src/blog/posts/devenv-v2.0-a-fresh-interface-to-nix.md, "Secret isolation with SecretSpec"). Inside an agent's sandbox a repo's devenv shell needs the Nix daemon socket and devenv's cache; not tested.
- **Nix** gives what the design needs from it: an immutable `ai` and plugin in `/nix/store`, a `bwrap` path fixed at build time, and, if wanted, root-owned policy through NixOS (`environment.etc`), which no agent can change but whose every edit needs `sudo`.
- **Nix-packaged agent sandboxes:** claudebox (numtide) wraps all of Claude Code in bubblewrap with `--unshare-all` and shadows `$HOME` except `~/.claude` (src/claudebox.js:228-234), so every agent's transcripts stay visible. jai (nixpkgs `jai-jail` 0.3) gives named private homes and a strict different-uid mode. nono (llm-agents.nix, 0.78.0), sandbox-runtime (nixpkgs), landrun (nixpkgs) and fence (llm-agents.nix) are below.

## Agent sandboxes

| Sandbox | Boundary | Fit |
| :-- | :-- | :-- |
| Claude Code Bash sandbox | Bash, PowerShell and Monitor commands only. Read, Edit and Write go through permission rules (sandboxing.md:730); hooks and MCP servers run unconstrained (sandbox-environments.md:78); a failed command may retry unsandboxed unless `allowUnsandboxedCommands` is false (sandboxing.md:151-157). | `ai resume` could pass per-agent rules with `claude --settings` (cli-reference.md:127, sandboxing.md:58), but the boundary is Claude-only, and the Claude process itself still reads the shared transcript folder. |
| sandbox-runtime | The whole process, bubblewrap with PID namespace, seccomp blocking `socket(AF_UNIX)` (src/sandbox/generate-seccomp-filter.ts:142-147), network proxy. Deny-list over `--ro-bind / /`. Beta (sandbox-environments.md:84). | Usable with a generated settings file per agent; brings Node and a moving config format for a policy of a few paths. |
| Codex | Shell commands, bubblewrap read-only root with writable roots (linux-sandbox/README.md). Its message board keeps credentials in the host: "tools never see them" (agent-message-board-client/src/lib.rs:1-2). | Codex-only; the credential pattern is the one recommended here: authority stays outside the sandbox. |
| DSH | bwrap, Landlock or Seatbelt; confines writes, not reads (apps/cli/reference/README.md:131). | DSH-only; reads unconfined. |
| pi | None found in its coding agent. | Needs an outer sandbox. |
| nono | Whole process; Landlock and seccomp-notify, a trusted supervisor that brokers commands, credentials and approvals; capability profiles; signed audit (security-model.mdx; README "What nono protects"). | Closest in philosophy. Costs: generated nono profiles per agent, Landlock's limits, a large runtime for what `ai` needs. Its Claude profile grants all of `~/.claude` (docs/cli/clients/claude.mdx). |
| claudebox, jai, fence | Whole-process jails for one CLI (above; fence README). | Single-agent; no per-agent grants among agents. |
| container-use, Docker Sandboxes | Containers or microVMs per agent. | Perimeter isolation; heavy for a shared checkout. |

## Signing and Signal

- **Locally, signatures add nothing** once location proves the sender: a key an agent cannot read must live outside its sandbox, and then the sandbox already identifies it.
- **Across hosts they are required.** Each host's `ai`, outside the sandboxes, signs outgoing mail with a host key (Ed25519, as Codex signs agent registrations: codex-rs/agent-identity/src/lib.rs:91-95). `.ai/local` is a git repository, so git over SSH is a transport that already authenticates hosts.
- **Signal Protocol** (libsignal) adds end-to-end encryption and forward secrecy when mail passes through a host that should not read it. libsignal is AGPL-3.0-or-later (nixpkgs `libsignal-ffi`), and ai-tools is CC0-1.0 (Cargo.toml:7): linking it would put `ai` under the AGPL.
- **The Signal messenger** is a poor fit: accounts are phone numbers for people, Signal's servers sit in the path, and bots use the unofficial `signal-cli` (GPL-3.0).
- **The user's approvals** can be signed with the user's hardware key. That keeps the gate intact even against a session started outside `ai resume`; it costs a touch per approval or per batch.

## Harness neutrality

Wrapping the whole harness at launch is harness-neutral: `ai` neither knows nor trusts what runs inside. Each harness needs only a per-agent home for its state (Claude `CLAUDE_CONFIG_DIR`, pi's session folder, `$DSH_HOME`) and, for delivery, a plugin that reads its own inbox. Built-in harness sandboxes become optional inner layers; nono advises keeping one boundary (docs/cli/clients/claude.mdx).

## Alternatives

1. **Separate OS users per agent**, with the checkout group-shared: kernel permission checks instead of namespaces. Hold as an outer layer if bubblewrap proves leaky.
2. **nono** as the launcher: same model, its policy language and supervisor, Landlock's limits.
3. **sandbox-runtime** with a generated settings file per agent.
4. **Claude Code's own sandbox** per agent through `--settings`: rejected; Bash-only, Claude-only, and the shared transcript folder stays readable.
5. **Containers or microVMs**: rejected for now; a perimeter, not per-asset grants.

## Open questions

1. Whether a fresh per-agent `CLAUDE_CONFIG_DIR` still loads the `ai` plugin; unverified (`20261030T202150--claude-code-mail-delivery.md`).
