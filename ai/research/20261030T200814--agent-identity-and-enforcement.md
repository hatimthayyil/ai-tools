# Agent identity and enforcement

Author: Claude Code (Claude Opus 5.5)

How `ai` can know which agent is acting, and enforce what it may do, when every agent runs as the same OS user. It serves `20261030T142550--agent-team.md` ("the team note") §3, §4 and §10, and replaces that note's recommendation that the session binding alone identifies a sender. Where everything is stored is in `20261021T224734--storage.md` ("the storage note"). Of this design only `reports-to` and its read grants are built. **Agreed** marks the user's decisions; the rest is recommended.

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
- Threads are ordinary files of the user, `.ai/local/agents/threads/<name>/`, each a git repository of its own (src/agent/store.rs, `Store::create`). Any process of the user reads every thread; one command commits every thread's repository (`Store::commit`), and session locks sit at the store root, shared by every agent.
- Claude Code writes every session of a repo into one folder, `~/.claude/projects/<dir>/`, and the thread store keeps verbatim transcript copies in `sources/` (src/agent/store.rs:37).

**Writes go around the gate.**
- herdr's socket (`HERDR_SOCKET_PATH`, in `~/.config/herdr`) offers `pane read`, `agent read`, `agent prompt`, `send-text` and `send-keys` to any process in a pane: read another agent's screen, or type into it. Read from `herdr pane --help` and `herdr agent --help`; not exercised.
- Claude Code's peer sockets sit in `/run/user/1000/cc-socks` (note 971). **Checked here** (location).
- The session D-Bus (`/run/user/1000/bus`) and systemd's user manager start processes outside any sandbox (`systemd-run --user`). Not exercised.

**Policy is writable.** `ai.toml` at user, project and private level (src/config.rs:17-47) are ordinary files in the user's home.

**What agents cannot change.** `kernel.yama.ptrace_scope` is 1 and `sudo` asks for a password. **Checked here.** So the kernel, root-owned files and the Nix store are out of an agent's reach; everything the user owns is within it.

## Agreed

1. Mail is stored in per-sender outboxes, and the desk writes approved mail into the recipient's inbox (recommendation 2, team note §10).
2. One Claude credentials file is bound read-write into every sandbox (recommendation 4). It cannot keep one login as stated ("How Claude writes linked files" below); the replacement awaits the user.
3. A session not started through `ai resume` counts as the user's own ("The gap that remains").
4. The setting is `reports-to = [...]` (recommendation 5).
5. Threads live in `~/ai`, one git repository the user keeps, and agents never touch git: a sandbox gets its own thread's files read-write and its subordinates' read-only, never `~/ai/.git`; only `ai` outside every sandbox commits (recommendation 3). One repository per thread is undone.
6. Every agent has full network for now.
7. `root`, the user-level main, runs sandboxed like every other agent.
8. No sandbox gets the ssh agent by default; a setting in the agent's `[agents.<name>]` table grants it (recommendation 1). The key's name is the implementation's.

## Recommendation

**1. `ai resume` starts every agent inside its own sandbox, around the whole harness.** `ai resume` is already the one place an agent starts: it commits the store and execs `claude` (src/agent/cli.rs:578-586). It execs bubblewrap instead, with the agent's grants:

| Access | What |
| :-- | :-- |
| read-write | the checkouts it works in; its own thread's files in `~/ai`; its own outbox; its own harness state, in `~/.ai/harness/`; a private `/tmp`; `ai memory` (shared by design) |
| read-only | the threads of every agent below it; its own inbox; the `ai.toml` files; the user's Claude `settings.json`, `CLAUDE.md`, skills, commands and agents; the system and `/nix/store` |
| absent | `~/ai/.git`; every other thread and outbox; other agents' harness state; herdr's socket; Claude's peer sockets; the session D-Bus and systemd's user socket; the gpg agent; the ssh agent, unless granted; other processes (own PID namespace) |
| allowed | network; the Nix daemon socket, for builds; the ssh agent, for an agent whose settings grant it |

Mask by construction: a tmpfs over `~/ai`, `~/.ai`, `~/.claude` and the runtime directories, then bind back the grants. A path not bound in does not exist inside. **Checked here:** with `bwrap --ro-bind / / --tmpfs <stores> --bind <own> --ro-bind <subordinate>`, the agent wrote its own thread, read but could not write the subordinate's, could not see a third (`No such file or directory`), could not see `~/.claude`, and saw 4 processes.

A sandboxed agent that runs `ai resume` for another agent gets a nested sandbox that can only narrow what it already has.

**2. Identity is location.** Inside its sandbox an agent can write only its own outbox, so a message's sender is the owner of the outbox it sits in. The desk runs outside every sandbox, as the user: it reads outboxes, and on approval writes the message, as approved or edited, into the recipient's inbox, which the recipient can read but not write. No environment variable, flag or token decides who sent a message or who approved it. The session binding stays, to find an agent's thread, never as authority.

**3. History stays outside every sandbox.** Threads live in `~/ai`, one repository (Agreed 5). A sandbox binds thread files, never `~/ai/.git`, so an agent can neither read another thread through git objects nor rewrite history: what it can damage is what changed since the last commit. `ai` outside the sandbox commits: the user's own commands, the desk, and the `ai resume` process that launched the agent, which commits what its agent wrote (storage note, "Writing to `~/ai`"). Today's code still gives each thread a repository of its own (src/agent/store.rs, `Store::create`); that is undone with the move to `~/ai`.

**4. Each agent gets its own harness state.** Claude Code reads and writes `~/.claude/projects/<dir>/` for its own session, and agents in one repo share that folder. A `CLAUDE_CONFIG_DIR` per agent separates them; `ai` already honours that variable (src/agent/claude.rs:26-31). pi and DSH take a per-agent home the same way (`$DSH_HOME`, deepseek-harness apps/cli/reference/README.md:137). Agents share the user's subscription through a long-lived token `ai resume` hands each one, with no credentials file inside any sandbox ("How Claude writes linked files" below; it replaces Agreed decision 2, pending the user).

**5. Rank stays the setting; `ai` compiles it into grants at launch.** An agent's read grants are the threads of every agent below it. Several superiors make the hierarchy a graph, not a tree; `ai` refuses a cycle. The setting is a list in the agent's own table:

```toml
[agents.developer]
reports-to = ["main", "engineer@~"]
```

`reports-to` names the relation, reads as plain English, and takes several names (Agreed). Each name resolves in the scope of the file it is written in: `main` is this project's main, `engineer@~` the user-level `engineer`, and a tracked `ai.toml` holds only names of its own project (Agreed, team note §8). Explicit per-thread grants would be a second setting that nothing needs yet.

Built (src/config.rs): `ai` reads `reports-to` from every `ai.toml`, a later file's list replacing an earlier one's, and `Settings::hierarchy` refuses an agent reporting to itself, to a name with no `[agents.<name>]` table, or in a cycle, naming the cycle; only the commands that read the hierarchy fail on it, never `ai memory` or another agent's commands. `Hierarchy::below` gives the agents below an agent through any number of levels: the threads the launcher grants read-only. Nothing enforces the grants until the launcher exists. Not built: resolution by file scope. Today's check looks names up among the tables of every file merged, which the scoped rule replaces.

**6. bubblewrap, invoked by `ai`, from a path fixed at build time by Nix.** Codex looks `bwrap` up only outside the working directory, so a repo cannot plant one (codex-rs/linux-sandbox/README.md). Nix can do better: the store path is compiled in.

### What this changes in agreed design

- **Mail store.** A sender appending into the recipient's thread would need write access into every recipient's thread. Each sender writes only its own outbox, and the desk delivers approved mail into the recipient's inbox (Agreed, team note §10): every file keeps a single writer, and the path proves the sender.
- **Order.** The sandboxed launch comes before mail: mail's sender identity and the gate rest on it.
- **The CLI's own checks** become friendly errors. The kernel enforces; `ai` explains.

### The gap that remains

A session not started through `ai resume`, such as `claude` typed in a terminal, runs unsandboxed and can write anything the user can, including the desk's decisions. Such a session counts as the user's own terminal (Agreed): a sandboxed agent has no herdr socket, session bus or systemd user manager through which to start a process outside its sandbox, so only the user starts one. Signing each approval with the user's hardware key, which an agent cannot use without the user's touch, would close the gap further.

### Per-agent Claude config folder

Probed with Claude Code 2.1.316 (one empty folder, one with the credentials link alone, two seeded; `claude auth status`, `claude -p` on Haiku with `--debug-file`, one interactive start under tmux). **Checked here.**

**What a fresh folder gives, with nothing seeded but the credentials link:**
- **Login works.** `claude auth status` reports `loggedIn: true`; without the link, `loggedIn: false`.
- **The `ai` plugin loads.** Managed settings (`/etc/claude-code/managed-settings.json`: `appendPlugins`, `enabledPlugins`, `extraKnownMarketplaces`) apply whatever the config folder. The debug log shows the plugin read in place from the declared marketplace path although `known_marketplaces.json` does not exist yet, its hooks module loaded at tier `append`, and `classic.SessionStart`, `classic.Stop`, `session.start` and `session.end` run. The `ai` MCP server is listed. `claude plugin list` shows no plugins in a fresh folder; the session loads it anyway.
- **Everything from the user tier is missing:** `settings.json` (hooks, including `ai memory prompt-hook`; `cleanupPeriodDays`; `autoMemoryEnabled: false`; `syncClaudeAiSkills: false`; `disableClaudeAiConnectors`; permission mode), user skills, `CLAUDE.md`, `commands/`. Consequences: transcripts fall under the default 30-day cleanup; auto-memory writes `projects/<dir>/memory/`; claude.ai skills sync into `skills/synced/` and the claude.ai connector appears.
- **The interactive start stops on onboarding** (theme picker), then on the workspace trust prompt. `claude -p` skips both. The user has no MCP servers in `~/.claude.json`, so nothing there needs carrying.

**How Claude writes linked files.** Settings **checked here** (Claude Code 2.1.316, interactive sessions under tmux in scratch folders, a dummy `ANTHROPIC_API_KEY` so no login was used, `/effort` to save). Credentials read in the 2.1.316 bundle, not triggered: a refresh from a copied file would revoke the user's login.
- **A settings save writes through the link.** Claude resolves the link, writes `settings.json.tmp.<pid>.<hex>` beside the target and renames it over the target. The link survived; the target changed and got a new inode.
- **Bound read-only, the save fails visibly.** `Failed to set effort level: … EROFS: read-only file system, open '…/settings.json.tmp.<pid>.<hex>'`. The effort stayed as it was, and the session carried on.
- **Bound as a file, read-write, the save writes in place.** The rename onto the mount point fails, and Claude writes the file itself: the save succeeded and the inode stayed the same.
- **A token refresh replaces the link.** The credentials writer (`gre`, in the chunk that also exports `Un`) writes `.credentials.json.tmp.<8 hex>` beside the slot, not beside the link's target, and renames it onto the slot; `rename(2)` replaces the link itself. Only a rename failing with `EXDEV`, `EPERM`, `EEXIST` or `EBUSY` makes it write in place, opening the slot with `O_NOFOLLOW`: a link there fails with `ELOOP`, a bind-mounted file is written through. clauth works around the same behaviour (clauth src/oauth.rs:2945-2947, "renames a temp sibling over the live slot — `rename(2)` acts on the link"), and its log records five adoptions of a rotated login from a live session between 2026-10-26 and 2026-10-31 (`~/.clauth/clauth.log`).
- **A second credentials reader in the bundle refuses links.** It opens the file with `O_NOFOLLOW` and reports a link as `refused-symlink`. It is not the reader in use, since login works through a link here; a release that switches to it would refuse a linked slot outright.

**Decision 2 cannot hold as stated.** Neither form of one shared file keeps one chain of refresh tokens:
- **The slot links to the shared file.** The agent's first refresh replaces its link with a file of its own. The shared file keeps a refresh token the refresh has revoked, so the user's sessions and every other agent fail at their next refresh. clauth repairs only `~/.claude` (clauth src/profile.rs:1858-1860) and its own `clauth start` trees.
- **The shared file is bind-mounted at the slot.** Claude's refresh writes through, but clauth replaces the shared file by rename on every rotation, adoption and login (`atomic_write_600`, clauth src/profile.rs:2466-2491), and an account switch points `~/.claude/.credentials.json` at another profile's file. The bind keeps the old inode, and the agent refreshes a chain clauth has moved past.

clauth met this as "N live sessions + clauth all rotating the same chains through one live slot", the cause of serial `refresh token revoked` failures, and fixed it with a token that never rotates (clauth src/claude.rs:27-35).

**Recommended: a long-lived token, and no credentials file in any sandbox.** `claude setup-token` mints a one-year subscription token (authentication.md:248-262). `ai resume` sets it as `CLAUDE_CODE_OAUTH_TOKEN` in the agent's environment, read outside the sandbox from pass, where each account's token is kept at `ai/claude-token/<profile>`, apart from clauth's profiles (below). The variable ranks above the saved login (authentication.md:207), and a token with no refresh token gives Claude nothing to refresh or write, so nothing inside a sandbox writes a login, and no agent holds the renewable refresh chain. Costs:
- one token per account, renewed yearly;
- the token "can only make model requests": no Remote Control and no claude.ai connectors, neither used here;
- an agent stays on the account its token names until relaunched, so clauth's switching and fallback reach it at its next launch.

**A session on such a token**, probed with Claude Code 2.1.316 for both clauth profiles, `personal` (Pro) and `not-that-silent-valley` (Team), each minted by `clauth login <profile> --setup-token`. The token came from the profile's `session-token.json` into `CLAUDE_CODE_OAUTH_TOKEN` alone, in a folder seeded as in the table below without `.credentials.json`: `claude auth status`, `claude -p` on Haiku and Opus with `--debug-file`, one interactive start each under tmux. **Checked here.**

| | `personal` (Pro) | `not-that-silent-valley` (Team) |
| :-- | :-- | :-- |
| Login | `loggedIn: true`, `authMethod: oauth_token`; no login or onboarding prompt | same; the Team organisation accepts the token |
| Models | Haiku 5.5 and Opus 5.5 answer through `--model` | same |
| `ai` plugin | loaded at tier `append` from managed `appendPlugins`; `session.start`, `classic.SessionStart`, `classic.Stop`, `session.end` run; `ai` MCP server connected | same |
| User hooks | `ai memory prompt-hook` runs (from the linked `settings.json`) | same |
| Managed settings | file policy applies; `policy-limits.json` fetched; server-managed settings return 404, as the user's own `~/.claude/remote-settings.json` is empty too | same; `policy-limits.json` identical to the user's |
| `.credentials.json` written | never | never |

The token carries two scopes, `user:inference` and `user:sessions:claude_code`, with no refresh token and no tier stamp (clauth: "untiered"), which changes what a session offers:
- **Default model is Sonnet 5.5,** and the header reads "Claude API". The user's sessions default to Opus 5.5. `ai resume` passes the agent's model from its table, as `--model opus` works.
- **Effort shows "Medium (default)"** although the linked `settings.json` sets `effortLevel: "xhigh"`; likely a plan-gated level falling back to its untiered default. Not traced further.
- **`/usage`** shows the session's cost, durations and usage attribution, without the plan's limit bars; the Team session still showed the weekly-limit notice on start.
- **No profile scope:** the start-up bootstrap is skipped (`403 for OAuth token without profile scope`) and Claude in Chrome is off.

**The user's own sessions changed too.** With a `session-token.json` present, clauth points the live `~/.claude/.credentials.json` at that file, so the user's sessions on the active profile run on the same untiered token (`clauth which`: `not-that-silent-valley`; link target `~/.clauth/profiles/not-that-silent-valley/session-token.json`). `clauth static-token --clear <profile>` returns the profile to its rotating login and removes the token with it, so tokens for agents only, with the user's sessions on their full login, need the token kept apart from the profile. The user keeps both tokens in pass, `ai/claude-token/personal` and `ai/claude-token/not-that-silent-valley`, and clauth's profiles hold none. The installed clauth 0.17.0 has no `__oauth-token` command, so `ai resume` reads pass, never clauth.

The alternative keeps the link: `ai resume` stays the sandbox's parent and, like clauth's watchdog, restores the link and hands the new pair to clauth after each refresh. It keeps live account switching, at the cost of a supervising process per agent and a race between each refresh and its repair.

**With four links and a two-key `.claude.json`, a session matches the user's:** skills `but`, `deslop`, `find-skills`, `stop-slop` resolve; the user `CLAUDE.md` is in context; `ai memory prompt-hook` runs; onboarding is skipped. Recommended layout of `<agent home>/claude/`:

| Entry | Kind | Why |
| :-- | :-- | :-- |
| `.credentials.json` | absent | The login comes from `CLAUDE_CODE_OAUTH_TOKEN` (above). |
| `settings.json` | link to `~/.claude/settings.json`, target bound read-only | Hooks, `cleanupPeriodDays`, auto-memory and sync off: the user's settings are one file. Writable, an agent's `/effort` would rewrite the user's settings, which the config repo tracks, and a hook it added would run in the user's unsandboxed sessions. Per-agent model and effort come from the agent's table at launch. |
| `CLAUDE.md`, `skills`, `commands`, `agents` | links to the same names in `~/.claude`, targets bound read-only | User instructions, skills and commands, which reach the user's own sessions too. |
| `.claude.json` | file written by `ai` when absent: `hasCompletedOnboarding: true`, and `projects.<repo>.hasTrustDialogAccepted: true` | Per-agent state, rewritten by Claude on every start (caches, `oauthAccount`, per-project trust); sharing it would race. |
| everything else | nothing | Written by Claude at runtime. |

Plugins need no seeding: `ai` reaches every agent through managed settings, and user-scope marketplace plugins are all disabled.

**Written into the folder at runtime**, so the sandbox binds the folder read-write: `.claude.json` and `backups/`, `projects/<dir>/<session>.jsonl`, `session-env/<session>/`, `sessions/<pid>.json` and `<pid>.<hash>.key`, `plugins/` (`installed_plugins.json`, `known_marketplaces.json`, `cache/`), `policy-limits.json*`, `remote-settings.json*`, `cache/`, `.last-cleanup`; in interactive use also `history.jsonl`, `file-history/`, `shell-snapshots/`, `todos/`, `paste-cache/`.

**Consequences for the sandbox and for `ai`:**
- **Transcripts.** An agent's transcripts live in its own `projects/`, and its folder sits in `~/.ai/harness/`, so they are stored in `~/.ai` like every other transcript (storage note, rule 2). `ai` finds them through `CLAUDE_CONFIG_DIR` of the process reading them (src/agent/claude.rs:25-31), so any reader outside the agent's sandbox (the desk, a superior, the user's shell) finds them through the agent's folder or the transcript path its binding records, never through its own environment.
- **Peer sockets are outside the folder.** `sessions/<pid>.json` lands in the agent's folder, but its `messagingSocketPath` is `/run/user/1000/cc-socks/<pid>.sock`, shared by every session of the user. A sandbox that binds `/run/user/1000` hands the agent every other session's socket, which the access table lists as absent; the sandbox gives `cc-socks` a private tmpfs or leaves it out.
- **Links must resolve inside the sandbox.** `~/.claude` is itself a link into the config repo; the sandbox binds each target read-only, and the absolute hook path `~/.claude/hooks/herdr-agent-state.sh` in `settings.json`.

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
- **Across hosts they are required.** Each host's `ai`, outside the sandboxes, signs outgoing mail with a host key (Ed25519, as Codex signs agent registrations: codex-rs/agent-identity/src/lib.rs:91-95). `~/ai` is a git repository, so git over SSH is a transport that already authenticates hosts.
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

1. Long-lived token or supervised link, replacing Agreed decision 2 ("How Claude writes linked files"). A token works for both accounts; it leaves the session untiered, so `ai resume` passes each agent's model, and likely its effort, from the agent's table.
2. Which account an agent runs on: the profile active in clauth at launch, or a per-agent setting, so that a work project's agents stay on the work account.

A fresh per-agent `CLAUDE_CONFIG_DIR` loads the `ai` plugin (Per-agent Claude config folder above), which settles the delivery note's open check (`20261030T202150--claude-code-mail-delivery.md`).
