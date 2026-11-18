# Changelog

## v0.18.4 - 2026-11-18

### Bug fixes

- **error:** tell a gix error tree on one line
- **repo:** write ~/ai's index without git's index.lock
- **agent:** don't warn when Claude Code rewrites a subagent's meta file

## v0.18.3 - 2026-11-18

### Bug fixes

- **agent:** make hooks' commits in a committer outside their processes

### Features

- **agent:** count a plugin's prompt sent as the user's words as theirs
- **plugin:** /fold has Claude write a handoff before it folds
- **plugin:** `/fold <direction>` aims the handoff and starts the new session on it

## v0.18.2 - 2026-11-17

### Bug fixes

- **cli:** print the whole error chain
- **repo:** remove git's lock files when a signal ends ai

## v0.18.1 - 2026-11-17

### Bug fixes

- **backup:** say whether ai backup watch is running
- **nix:** link the system aws-lc and sqlite, as nixpkgs' cargo hook now asks

## v0.18.0 - 2026-11-12

### Upgrading

From 0.17, once on each machine, before agents run on this release. Close every agent session first, so no hook runs `ai` halfway through.

1. **Back up the harnesses' transcripts.** Threads keep no copy of a session's files: the harness's own files and the backup hold those bytes. Enable `services.ai-tools.backup` in Home Manager (or run `ai backup run` yourself), set `[backup]` in your own `~/.ai/ai.toml` if its defaults don't suit, and `[backup.remote]` for a copy off this machine. Run `ai backup run` once and check `ai backup status` before converting.

2. **Let `~/ai` be made, or make it.** `ai` makes `~/ai` a git repository on its first write. If `~/ai` already exists and holds files, make it a repository first (`git init ~/ai`); a `~/ai` that holds files but is no repository is refused. Give it a remote of your own if it should leave this machine.

3. **Convert each project.** `ai convert --dry-run ~/code/*/` tells what it would do for every project there; run it again without `--dry-run`. With no directory, it converts the current directory's project. For each project whose `.ai/local/` holds agents or private settings, it moves them to `~/ai/projects/<project>/`, relinks `ai.local.toml` and commits `~/ai`, naming any session file it had to write back into the harness's files. Until a project is converted, the commands that open its agents, hooks and `/fold` included, and `ai init` refuse it, naming `ai convert`.

4. **Keep `.ai/local/`.** It is the project's private working data and stays ignored by the project's git; `ai init` adds `/.ai/local/` to `.gitignore` where it is missing. `ai convert` never changes or removes it; the agents in it are no longer read once converted, and once you have checked the conversion, are yours to delete.

5. **Give each API provider its key** if the summariser runs on `backend = "api"`: `[summariser.keys]` in your own `~/.ai/ai.toml` maps each provider to a command that prints its key, such as `deepseek = "pass show api/deepseek"`. Without it, the provider's usual environment variable is read.

6. **Fold with `/fold`.** It replaces the conversation as bare `/ai` did, and takes no arguments. Bare `/ai` now prints its usage.

7. **Drop any permission rule that names `mcp__ai__compress`.** The model no longer has that tool, so asking it in words no longer folds.

### Breaking changes

- **agent:** keep threads in ~/ai, pointing at the harness files

> agent stores are format 6, in `~/ai/projects/<project>/agents/`, and a project's private settings are `~/ai/projects/<project>/ai.toml`. A store ai 0.17 kept in .ai/local is refused until moved; see the upgrade notes.

- **plugin:** fold with /fold, and drop the model's compress tool

> bare /ai no longer folds; type /fold. The model no longer has the compress tool (mcp__ai__compress).

### Bug fixes

- **repo:** write the index before HEAD moves in a commit
- **memory:** keep system prompt files in the cache, not a folder per process
- **config:** fail only the commands that read a broken setting
- **config:** read a project's private settings by its id, and check its name before minting one
- **plugin:** show what an ai agent command that went through warned of
- **repo:** write the index before HEAD moves in commit_paths
- **summariser:** say why no summariser starts, where it would be started
- **summariser:** make an agent build's backend only once it holds the thread's lock
- **agent:** close the gaps a review found in attaching, renaming, removing and restoring
- **plugin:** report a cancelled fold as cancelled, not as a TypeError
- **cli:** wrap help text at word boundaries

### Features

- **config:** read reports-to and compile it into read grants
- **agent:** rename an agent with ai agent rename
- **agent:** name each agent's Claude Code sessions by its address
- **backup:** back up every harness's transcripts with restic, and read a thread's bytes back from it
- **agent:** remove an agent with ai agent remove
- **summariser:** read each provider's API key from its command in your own ai.toml
- **agent:** carry a thread into the session /fold starts
- **convert:** convert what ai 0.17 kept with ai convert, outside ai's library
- **plugin:** `/recall <message>` briefs you on what you want to work on
- **plugin:** /fold goes on in a new session
- **reply:** ai reply quotes the assistant's answers, ready to reply inline
- **plugin:** /reply quotes an answer into the prompt box
- **plugin:** name a session attached in place by its agent's address

## v0.17.0 - 2026-10-28

### Breaking changes

- **agent:** make build --dry-run print the plan only; --preview writes without storing
- **memory:** rebuild into a copy of the current tree, switch once built

> the memory store moves to format 3, which installed ai releases cannot read.
New `tree` commands: list, rebuild, reroll, use, drop, diff.

### Bug fixes

- **memory:** switch trees without copying, rebuild long messages, finish an interrupted copy

### Features

- **summariser:** write summaries through a provider API with backend = "api"
- **summariser:** set thinking effort and parallel calls in [summariser]
- **summariser:** check every summary with a witness model in [summariser.witness]
- **build:** show the plan, a bar per level and each summary as it is written
- **build:** show [summariser] jobs in ai agent build's plan

## v0.16.0 - 2026-10-26

### Breaking changes

- **agent:** size the view by counts in [agent.view]

> [agent.view] lines is removed; use total.

- drop ai/comments

### Bug fixes

- **agent:** keep hashes and work minutiae out of thread summaries
- **memory:** keep hashes and ids out of memory summaries
- **agent:** reword the thread summariser prompt around its future reader
- **memory:** reword the compaction prompt around its future reader
- **memory:** remind agents to recall before acting, not to note

### Features

- **agent:** number summariser input within its turn
- **view:** reword the memory and thread headers, kept as text files
- **memory:** print the tightened memory instructions from ai init

### Performance

- **jsonl:** split lines with memchr

## v0.15.0 - 2026-10-24

### Upgrading

From 0.14, do these once, before any `ai` command of this release runs. Close agent sessions first, so no hook runs `ai` halfway through. Where `$AI_CHAT_DIR` or `$AI_FILES_DIR` is set, use it in place of `~/.ai/chat` or `~/.ai/files`; neither is read any more.

1. **Move the summariser's model to `[summariser]`.** In `~/.ai/ai.toml`, and in any project's `ai.toml` that sets `model` under `[chat]`:

   ```toml
   [summariser]
   model = "sonnet"
   ```

   A `model` left under `[chat]` fails every command as an unknown key. `$AI_MEMORY_MODEL` is no longer read; set `[summariser] model` instead.

2. **Move `view-lines` to `[memory.view] lines`.** In the same files, replace

   ```toml
   [memory]
   view-lines = 96
   ```

   with

   ```toml
   [memory.view]
   lines = 96
   ```

   A file that still sets `view-lines` fails to load.

3. **Move each project's agents into the project.** Threads move from `~/.ai/chat/<project id>/` to `.ai/local/agents/` in the project's main checkout. In the main checkout of every project that has threads (not in a linked worktree):

   ```sh
   id=$(ai init)
   mv ~/.ai/chat/"$id" .ai/local/agents
   ```

   `ai init` prints the project's id, creates `.ai/local/ai.toml`, links `ai.local.toml` to it, and adds `/.ai/local/` and `/ai.local.toml` to `.gitignore`. Commit the `.gitignore` change. Once every project's threads are moved, remove `~/.ai/chat`.

4. **Convert each project's agents to format 4.** Each growing file of an agent's thread is now a directory of 1 MiB segments, and a format-3 store is refused, by hooks and `/ai` too, until it is converted. In each project with agents, run any `ai agent` command on a terminal, such as `ai agent list`, and answer `y` when it asks to convert. It converts the store, checking every byte, and commits `.ai/local` once as `ai agent: convert to format 4`. Answering `n` leaves the store unchanged.

5. **Delete the file summaries.** `ai files` is gone, so nothing reads them:

   ```sh
   rm -rf ~/.ai/files
   ```

6. **Use `ai agent` in place of `ai chat`** in scripts and shell aliases, with `-a <name>` in place of `-t <name>`, and `ai agent resume <name>` in place of `start`.

### Breaking changes

- **config:** `[summariser]` chooses the backend and model for every summary
- **agent:** `ai agent` replaces `ai chat`, and only `ai agent new` creates an agent

> `ai chat` is removed; run `ai agent` wherever you ran
`ai chat`, in scripts and shell aliases too. Existing threads are agents
already; create a new one with `ai agent new <name>` before
`/ai attach <name>` or `ai agent resume <name>`.

- **agent:** `-a/--agent` names the agent, and `grep` takes several

> use `-a <name>` where you used `-t <name>`.

- **agent:** an agent's thread lives in its project, shared by its worktrees

> threads are no longer read from `~/.ai/chat`; the upgrade
steps move them into each project. `$AI_CHAT_DIR` is no longer read.

- **agent:** `ai agent resume` starts a new session when there is none to continue

> `start` is gone; `ai agent resume <name>` takes its place.

- **memory:** `ai memory view`'s size is `[memory.view] lines`
- remove `ai files`
- **agent:** an agent's growing files are kept in 1 MiB segments
- **page:** paged output continues with `ai page <view>:<part>`

### Bug fixes

- **plugin:** /ai refuses where Claude Code withholds its session events

### Features

- **plugin:** the package ships a Claude Code marketplace for its plugin
- **config:** `ai.local.toml` overrides a project's settings for one user
- **config:** `[agents.<name>]` names an agent's instructions and worktree
- **config:** an agent's instructions resolve from the file that names them
- **agent:** an agent's sessions run in its worktree with its instructions
- **plugin:** `/ai new <name>` creates an agent and attaches the session to it
- **plugin:** `/ai` compacts an unattached session into the memory alone
- **config:** an agent's instructions add up across settings files
- **config:** a linked worktree reads the main checkout's `ai.local.toml`
- `ai resume <name>` is `ai agent resume <name>`
- **agent:** `[agent.view] lines` sets how many lines the view folds older turns into
- a project's private store is a git repository
- **agent:** a project's private store commits on events, such as `/ai` or a session's end
- **project:** a project's private data lives in `.ai/local`, and `ai.local.toml` links there
- **memory:** a note may run 40 characters past `entry-chars`
- text limits count characters, not bytes
- **agent:** `/ai` waits however long its lines take, and Esc stops it

## v0.14.0 - 2026-10-22

### Upgrading

From 0.13, do these once, before any `ai` command of this release runs. Close agent sessions first, so no hook runs `ai` halfway through. Where `$AI_MEMORY_DIR` or `$AI_CHAT_DIR` is set, use it in place of `~/.ai/memory` or `~/.ai/chat`.

1. **Copy memory's origin.** The memory store's id moves from the store's git config to a plain file:

   ```sh
   git --git-dir ~/.ai/memory config ai.memory.origin > ~/.ai/memory/origin
   ```

   Every memory note records this id. Without the file, the store mints a new id when it next opens, and every later note looks as if it came from another store.

2. **Move settings to `ai.toml`.** `ai` no longer reads git config. Yours go in `~/.ai/ai.toml`; a project's `ai.toml` overrides them key by key.

   | Old git config key | Where it was set | `ai.toml` |
   |---|---|---|
   | `ai.chat.model` | the repo | `model` in `[chat]` |
   | `ai.chat.background` | the repo | `background` in `[chat]` |
   | `ai.memory.viewLines` | the memory store | `view-lines` in `[memory]` |
   | `ai.memory.entryChars` | the memory store | `entry-chars` in `[memory]` |

   For example:

   ```toml
   [chat]
   background = false

   [memory]
   view-lines = 96
   ```

   `ai memory config` only prints the sizes now; edit `ai.toml` to change them.

3. **Make each project.** Chat and `ai files` now need a project: the directory of the nearest `ai.toml`, holding an id that `ai init` mints. In each repository where you used `ai chat`, at its top directory:

   ```sh
   old=$(git config ai.root)
   new=$(ai init)
   mv ~/.ai/chat/"$old" ~/.ai/chat/"$new"
   ```

   Commit the `ai.toml` that `ai init` writes, so every clone shares the id. On another machine, pull it first; the same three lines then move that machine's store. Where `git config ai.root` prints nothing, that repository has no chat to move, and `ai init` alone is enough.

4. **Remove the old keys.** Nothing reads them now:

   ```sh
   git config --remove-section ai.chat
   git config --unset ai.root
   git --git-dir ~/.ai/memory config --remove-section ai.memory
   ```

   Run the first two in each repository; each fails harmlessly where nothing is set.

### Breaking changes

- **notes:** name notes `<id>--<slug>.md`, as Denote does
- read settings from ai.toml instead of git config

> ai.chat.model, ai.chat.background,
ai.memory.viewLines, ai.memory.entryChars and ai.memory.origin in git
config are no longer read; `ai memory config NAME=VALUE` is gone.

- find the project through ai.toml and its minted id

> chat and files need `ai init`; their stores move from
`<root commit>` to `<project id>`, and memory's `root` field becomes
`project`.

### Features

- **files:** store file summaries by content hash
- **files:** walk a project's text files into a Merkle tree
- **files:** summarise files and directories at their byte sizes
- **files:** build and view a project's file summaries

## v0.13.1 - 2026-10-21

### Bug fixes

- **memory:** the Nix build no longer fails the init test

## v0.13.0 - 2026-10-21

### Bug fixes

- **memory:** the parent-death test reads the pid file only once it is whole
- **memory:** the call-record test writes its stub claude through a child process

### Features

- **chat:** a fork's report opens its own turn, from the fork's description

## v0.12.0 - 2026-10-21

### Breaking changes

- rename wake to view, summarise to build, ai.chat.summarise to ai.chat.background

### Bug fixes

- **memory:** summariser calls use Claude Code's prompt cache
- **chat:** hide plugin-only flags from help; name attach's argument

### Features

- **chat:** ai chat list shows threads newest first
- ai chat and ai memory alone print their view
- **chat:** the view names its branch when the thread has several
- **chat:** a thread is named, else attached, else the only one, else picked on a terminal
- one paging rule for ai view, ai memory view and ai chat view
- **chat:** summary line targets scale with the AI's words and tool calls
- zoom and grep print whole on a terminal, else in parts pinned by --part --at
- **chat:** a turn's work is summarised apart from its message and final reply
- **chat:** the view is one list of every turn, detail growing toward the present

## v0.11.0 - 2026-10-20

### Bug fixes

- release file locks explicitly on drop
- **chat:** view writes no line; an unsummarised turn shows its message
- **memory:** word target leaves room below the byte limit
- **memory:** word target reads about 60 words
- **chat:** a snapshot reads the records before the items they name
- **memory,chat:** summarisers see a 50-word target, never the 512-byte ceiling

### Features

- **chat:** the view header says when to zoom
- **compact:** state a word target with the byte limit
- **chat:** a turn closes where the harness ends it
- **plugin:** /ai compacts into the thread view; /compact stays Claude Code's
- **chat:** one failure rule for summary lines
- **chat:** ai chat summarise shows its progress on a terminal
- **memory:** keep every summariser call whole; log its tokens and cost
- ai view prints the whole memory, then the session's thread; /ai compacts into it

### Performance

- count chat list turns from item heads and kind tags

## v0.10.0 - 2026-10-19

### Bug fixes

- archive complete chat sources and retry fresh observations
- require explicit chat detachment and serialize session ownership
- confirm legacy chat imports through normal terminal commands
- restrict chat import confirmation to foreground jobs
- catch up before detaching a chat session

### Features

- keep chat stores in ~/.ai/chat by root commit

## v0.9.0 - 2026-10-16

### Bug fixes

- **notes:** no repeated summaries in single-child chains; claims-only gists
- **chat:** sync and compact once Claude Code has flushed its transcript
- **chat:** compact waits for a running nap instead of building its lines
- **chat:** summarise names a failed line by its printed id

### Features

- **notes:** ai notes layer 1: store, add, mv, link, summaries, grep, view, zoom, show
- **notes:** required note titles; view collapses single-child chains
- **chat:** Claude Code plugin for ai chat
- **chat:** harness-neutral item model
- **chat:** Claude Code transcript adapter
- **chat:** messages from other agents are work items
- **chat:** thread store in repo git and session sync
- **chat:** the view: fold, user rail, open exchange; zoom
- **chat:** ai chat CLI and the Claude Code launcher
- **chat:** write many lines at once, read all lines, nap log path
- **chat:** roll-up compactor
- **chat:** the roll-up behind compact, sync and nap; one item renderer
- **chat:** ai chat resume passes claude args
- **chat:** seed a launched session with its thread's view
- **chat:** porcelain and plumbing commands; summarise with --rebuild, --model, --dry-run
- **chat:** view syncs and writes the lines it needs
- **chat:** turn ids; zoom two levels down; tool output on request
- **chat:** ai chat grep
- **chat:** fold the view into 64 lines; head a seed's last turn as the previous session's
- **chat:** summarise <id> writes or rewrites one line

## v0.8.0 - 2026-10-13

### Features

- **memory:** resumable wake, date headers, bare message ids
- **memory:** format 2 with named headers and automatic upgrade

## v0.7.0 - 2026-10-11

### Features

- rename MyAI to ai-tools; crate ai-tools, npm @hatimthayyil/ai
