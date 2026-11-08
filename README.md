# ai-tools

The `ai` CLI: an all-in-one AI tool.

- `ai memory`: permanent memory for AI agents. An agent records what it learns as one-line notes. Claude Code folds older notes into summaries, and `ai memory view` prints the whole memory as one short view.
- `ai notes`: a global knowledge store for AI agents. Each note is one claim, filed in a tree of topics; Claude Code summarises every topic from its children, so an agent can browse the tree or search it.
- `ai agent`: named agents, each with one thread that spans its sessions. The thread copies its Claude Code sessions into the repository's git, summarises them into a tree you can zoom, and on `/ai` compacts a session into a view of the whole thread.
- `ai backup`: every harness's transcripts backed up with restic, into a local repository and, if you set one, a remote one that gets a copy of every snapshot.

## Install

```sh
cargo install ai-tools
```

or with Nix:

```sh
nix profile add github:hatimthayyil/ai-tools
```

### Claude Code plugin

`ai agent` needs the Claude Code plugin `ai@ai`. The Nix package ships it in a plugin marketplace, `share/ai/plugins` (`plugins/` here), whose `.claude-plugin/marketplace.json` lists it as `./claude`. Below, `<prefix>` is where the package is installed, such as `~/.nix-profile`.

Without a Team or Enterprise login, on a machine with no managed settings:

```sh
claude plugin marketplace add <prefix>/share/ai/plugins
claude plugin install ai@ai
```

Under a Team or Enterprise login, or on a machine with managed settings, Claude Code runs a built-in policy plugin ahead of every plugin a user installs, and it withholds from them the session start, turn end and compaction events `ai agent` needs. Only a plugin that managed settings install receives them. As root, add these managed settings:

```json
{
  "extraKnownMarketplaces": {
    "ai": { "source": { "source": "directory", "path": "<prefix>/share/ai/plugins" } }
  },
  "enabledPlugins": { "ai@ai": true },
  "appendPlugins": ["ai@ai"]
}
```

either to `managed-settings.json` or as a file of their own in `managed-settings.d/` beside it, in:

- Linux and WSL: `/etc/claude-code/`
- macOS: `/Library/Application Support/ClaudeCode/`
- Windows: `C:\Program Files\ClaudeCode\`

Claude Code installs the plugin at the next session start. `<prefix>` must be absolute: `/home/<you>/.nix-profile` on your own machine, or for a fleet a path every machine has and only administrators can write, such as the NixOS system profile `/run/current-system/sw` with `environment.pathsToLink = [ "/share/ai" ];`. Where the organization also delivers server-managed settings from the claude.ai console, Claude Code ignores this file unless that source sets `"managedSourcesBehavior": "merge"`.

## Use

```sh
ai memory init                  # create the memory; prints the block for your agent instructions
ai memory view                  # read the memory
ai view                         # the whole memory, then this session's thread
ai memory note "what happened"  # record one line
ai memory zoom <id>             # open a line of the view
ai memory grep <regex>          # search every message
```

Views print whole on a terminal. Piped, as an agent reads them, they come in parts of at most 20,000 bytes. The first read saves every part in `$XDG_CACHE_HOME/ai/views/` (default `~/.cache/ai/views/`), and each part ends with the command for the next, like `ai page 17:2`. A saved view is gone 10 minutes after it was last read.

`ai` stores memories in `~/.ai/memory`, or in `$AI_MEMORY_DIR` if you set it. A newer `ai` upgrades the memory on first use and prints the commit holding its previous state; older releases then fail on it without writing.

```sh
# file a note (or pipe its text on stdin)
ai notes add physics.qm "the claim, in detail" --title "Decoherence explains collapse" --confidence verified
ai notes                        # the top of the tree: each topic's scope
ai notes zoom physics.qm        # a topic's subtopics and notes
ai notes grep <regex>           # search every note and summary
ai notes show <id>              # one note, whole
ai notes mv <id|topic> <topic>  # move a note or a topic
ai notes link <id> <id>         # link one note to another
```

Notes are Markdown files in a git repository at `~/.ai/notes`, or at `$AI_NOTES_DIR` if you set it. Every change is a commit.

### Projects

```sh
ai init  # make this directory a project, once: mints its id into ai.toml, claims its folder in ~/ai, links ai.local.toml to its private settings there, ignores ai.local.toml in .gitignore, prints the id
```

`ai agent` works in a project: the directory of the nearest `ai.toml` at or above the current directory, never `~/.ai/ai.toml`. It need not be a git repository. Its id is a TypeID, `project_` and a UUIDv7; commit `ai.toml`, so every clone and worktree is the same project. `ai` writes nothing to a repository's `.git`.

A project's repository holds only what the project shares: `ai/`, the agents' visible workspace; `.ai/`, hidden and tracked; and `ai.toml`. Everything of one user's agents lives in two homes of theirs:

- `~/ai`, a git repository the user keeps, as they keep their dotfiles: each project's folder, `~/ai/projects/<project>/`, with the project's id in `id`, its private settings in `ai.toml`, and its agents' threads in `agents/threads/<name>/`. `ai init` claims the folder: a name another project's id holds is refused, with a hint to set `[project] name`. Every checkout and clone of the project finds the folder by its id. `~/ai` is made a repository when `ai` first writes to it; a `~/ai` that holds files but is no repository is refused.
- `~/.ai`, what belongs to this machine and never goes in git: the threads' locks and compactor logs in `~/.ai/agents/<project id>/`, the backup's repository in `~/.ai/backup/` with its key `backup.key`, its copies of databases in `backup-staging/` and its last run in `backup.json`.

A thread keeps its items, lines, session tracks, attachment changes and subagents, never a copy of a session's files: it records each file it read in `sources.jsonl` by the machine it read it on, where the file really sits there, and the length and SHA-256 of the bytes it read from the file's start. A file that grew is recorded again at its new length; one that no longer begins with what the thread read begins a new version, and the earlier one stays recorded. Those bytes are in the file while it begins with them, and after that in the machine's [backup](#ai-backup), in the newest snapshot whose file at that path begins with them. A sync never fails on a file gone or rewritten: it warns of a file that is gone on every sync, and of one that no longer begins with what the thread read on the sync that finds so. `ai agent list` ends each session's line with how many of its files on this machine are gone or shorter than what the thread read, and `ai backup threads` tells where the bytes are. `ai agent resume` brings a session's files that are gone back from the backup before it resumes the session.

Events commit the project's folder of `~/ai` as one commit naming the command: creating, renaming or removing an agent, attaching, detaching, resuming, `/ai` and other builds, a session's end (`ai agent attach dev`, `ai agent end-session`). A commit touches no other path of `~/ai`, and of its index only that folder's entries, so what you stage there yourself stays staged. A turn's sync, a view and the background summariser write without committing; the next event commits what they wrote. A command that fails still commits what it wrote.

ai 0.17 kept a project's agents and private settings in `.ai/local/` of the project. Until `ai convert` (below) converts them, the commands that open the project's agents and `ai init` refuse them, naming the converter, and every other command warns that the private settings there go unread; `.ai/local/` itself is the project's private working data, which the project's git ignores.

### ai convert

`ai convert [--dry-run] [DIR]...` converts what ai 0.17 kept in each project's `.ai/local/`, its agents (store format 4) and its private settings, to the project's folder of `~/ai`. Run it once per project on each machine, with every agent session of the project closed. With no `DIR` it converts the current directory's project, and given one `DIR` it says what it found there. `ai convert ~/code/*/` converts every project there, telling of each, and counts in one closing line the directories that hold no project and the projects whose `.ai/local/` holds neither. It never changes or removes `.ai/local/`: once converted, its agents are no longer read, and once you have checked the conversion, they are yours to delete.

Each thread's files become one file each, of the same bytes; each copy of a session's file the old store kept becomes a record of the harness's own file, and a copy the harness no longer holds whole is written back into the harness's files first, at its path when it is gone, else as `<session>/restored/` beside the transcript, which every sync reads. The whole folder is built in `~/ai`'s git directory and checked before it moves: each thread's files hold the old bytes, every record finds the bytes it names, and the store reads whole through `ai`'s own reader, with no session attached to two threads. It is then written through to the disk and moved into place in one step, `ai.local.toml` is relinked to it, and `~/ai` commits it. A run cut short before that step leaves nothing behind and starts again; one cut short after it is finished by the next run, which this machine's record in `~/.ai/converted/` lets it recognise. A folder of the project that `~/ai` already holds from another checkout or machine is never overwritten. `--dry-run` builds and checks in a temporary directory, changes nothing, and tells what would happen, each file it would write back included.

### ai agent

```sh
ai agent new dev                      # create agent dev, with an empty thread
ai agent rename dev reviewer          # rename it; its sessions stay attached, and each [agents.dev]
                                      # table or reports-to naming dev is printed, not changed, as
                                      # is the /rename for each session of it that runs
ai agent remove dev                   # take it out: commit its last state, then remove it, and print
                                      # the command that restores it; refused while a Claude Code
                                      # session attached to it runs; transcripts stay untouched
ai agent restore dev --commit <id>    # bring a removed agent back as that commit of ~/ai holds it;
                                      # refused while an agent has the name
ai agent resume dev [-- claude args]  # continue dev's newest session, in the directory it ran in,
                                      # else a new one, whose context is the thread's view; either
                                      # way named <project>.dev; --harness: claude is the only one yet
ai resume dev [-- claude args]        # the same, shorter
ai agent list                         # the agents here, newest first, and their sessions
ai agent view dev                     # dev's view, from the summaries written so far
ai agent zoom -a dev <id>             # open a line of it; --tool-output adds tool output
ai agent grep -a dev <regex>          # search it; repeat -a to search several agents
ai agent build -a dev                 # write its summaries now: its plan, then a bar per level (a line per summary off a terminal); --rebuild, --model; --dry-run prints the plan only; --preview writes each line and stores nothing
```

Only `new` creates an agent; `resume`, `attach` and the reading commands refuse a name it has not made. Unnamed, a command reads the session's agent, else the project's only agent, else the one you pick on a terminal; elsewhere it lists the agents. Inside a session, `/ai new <name>` creates an agent and attaches the session to it, and `/ai attach <name>` and `/ai detach` attach and detach it, so the agent reads its thread with `ai agent view` and `ai agent zoom <id>`. `/fold` goes on in a new session: it builds the thread's view, clears the conversation as `/clear` does, and starts the new session from `ai view`, the whole memory, then the thread's view, so the agent wakes with no further reads. The new session is attached where the old one was and goes on from its last turn; it keeps the old one's name, and the forks still running report to it. If `/fold` cannot build the thread's view, it says why and changes nothing; if a later step fails, it says which, and what the session holds. In a session attached to no agent, `/fold` compacts the conversation into the memory alone. `/compact`, `/clear` and auto-compaction stay Claude Code's own. Threads live in the project's folder of `~/ai`, so every checkout and clone of the project shares them.

A session is attached to one agent at a time. Detach it explicitly before attaching it to another; the previous agent's thread keeps the recorded history.

`/recall <message>` briefs you on what you want to work on: say in the message what that is, and what you remember of it. A read-only fork searches your memory, the thread and the repository, and returns one brief in four parts (decisions, state, open questions, gaps), each claim citing the note, turn or `file:line` it rests on. The session shows you the brief, and notes from it only what you approve. The fork can use only Read, Grep, Glob and one read-only `ai`, `git` or `but` command per Bash call, so it can't write notes, edit files or commit.

`/reply` puts Claude's latest answer into the prompt box, each line quoted with `> `, so you can answer it inline, there or in your editor (Ctrl+G). An answer is every final reply to one of your messages, forks' reports and Claude's replies to them included; what Claude writes between tool calls is left out. `/reply -2` quotes the answer before, `/reply #2` the second reply in the latest answer, and `/reply -2#1` the first reply in the answer before. `/reply 597` quotes the answer to your message at turn 597, and `/reply 597#2` one reply in it. Turns count each message to Claude, yours or a fork's report: in a session attached to an agent, as the thread's view and `ai agent zoom` number them, else from 0 in the session. A compaction between turns ends an answer too, and so does `/fold`. In a session attached to an agent, answers count back through the thread, into the sessions before this one, so right after `/fold`, `/reply` quotes the last answer before it and `/reply -2` the one before that; a fork that reports after the fold gets an answer of its own. A session attached to no agent counts its own answers. If the project's agents cannot be read, `/reply` and `/reply -N` count in the session's own transcript and a warning says why, while a turn's number fails. `/reply 595 -1` quotes several in order, and nothing if one fails; commas separate them too, and quotes around them are dropped, so `/reply 595,-1` and `/reply "595, -1"` do the same. While Claude works on your latest message, `/reply` quotes the answer before it. `ai reply` prints the same. To quote part of an answer, select it with the mouse, then run `/reply` with nothing after it; this needs Claude Code's fullscreen view (`/tui fullscreen`), the only one that tells plugins what you selected.

`/ai new` and `/ai attach` rename the session to the agent's address, `<project>.<agent>`, as `ai agent resume` names it, so other sessions send it messages by that name. `/ai detach` has Claude Code name the session from its conversation, so the address leaves it. A session attached with `ai agent attach` from a shell keeps its name.

Each turn's work, the agent's steps between your message and its final reply, is summarised first; the turn's line is then written from your message, that summary and the final reply. A summary line is asked for 50 to 200 words, more where the AI wrote more and ran more tools, and holds at most 512 characters per 50 words. A command you run, `/fold` included, writes the lines it needs, however long they take, and stops at the first that fails, naming each failed line with its error: `/fold` then folds nothing, and `ai agent resume` starts no new session. Press Esc while `/fold` runs to cancel it: its model calls stop, nothing is folded, the lines it wrote stay, and the next `/fold` goes on from them. The background summariser, started after each turn, builds what it can and only logs, to `<thread>.log` beside the threads.

A thread is JSON Lines files: append-only records and summaries. `ai` commits them to `~/ai` through gix, which stores each version of a file whole, so once more than `[agent] gc-after-mib` of loose objects have been written to `~/ai` since the last one, a commit starts `git gc` in the background, which packs each file's versions against each other. This needs `git` on `PATH` (the Nix package brings its own); without it, `ai` warns once per command and the commit still stands. A store of another format is refused. Re-attaching a session rebuilds its thread from the transcript.

This needs the [Claude Code plugin](#claude-code-plugin).

### ai backup

```sh
ai backup run     # back up what every harness wrote since the last run, once
ai backup watch   # run again and again, [backup] every apart; its service runs this
ai backup status  # what is backed up, where to, and how the last run went
ai backup check   # have restic check the repositories; --read-data reads every byte back
ai backup threads # where the bytes every thread read are: backed up, not yet, or lost
```

`ai backup` backs up every harness's transcripts with [restic](https://restic.net), which it runs as a program: the bytes are in a repository restic wrote, reads and checks. A run backs up each harness's folders of transcripts and session files, under the harness's default folder (Claude Code `~/.claude/projects`; Codex `~/.codex/sessions` and `archived_sessions`; pi `~/.pi/agent/sessions`; DSH `~/.dsh/sessions` and `attachments`; Gemini `~/.gemini/tmp`; opencode `~/.local/share/opencode/storage` and `opencode.db`; Hermes `~/.hermes/sessions` and `state.db`), and the folders `[backup] folders` names, each where it really sits, links resolved, as one snapshot of this machine, under its host name, in the local repository, `~/.ai/backup`. No environment variable counts: a folder that `CLAUDE_CONFIG_DIR` or another harness variable names belongs in `folders`, so the service and a command run from any shell back up the same folders. A harness folder that does not exist is skipped; a named one that does not is a problem. A database (opencode's, Hermes') is never read as the file its harness is writing: each run first copies it with SQLite's backup API, write-ahead log included, to its own path under `~/.ai/backup-staging/`, and backs up the copy; a copy the same as the last is left untouched.

A run asks restic first what a backup would do, and makes no snapshot when no file is new or changed. It never forgets or prunes, so a repository only grows, and a file a harness rewrote or deleted stays in the snapshots made before. restic stores a file as chunks, each once, so a snapshot of a transcript that grew adds the chunks at its end, not the file again.

restic encrypts every repository. The local one's password is `~/.ai/backup.key`, 32 random bytes in hex that the first run makes, readable by you alone; without it the repository can't be read, and a run refuses to make another key for a repository that exists. `[backup] password-command` names a command that prints the password instead: `sh -c` runs it, and the password is what it prints, without the blanks around it. Read the repository with restic itself:

```sh
restic -r ~/.ai/backup -p ~/.ai/backup.key snapshots
restic -r ~/.ai/backup -p ~/.ai/backup.key dump latest /home/you/.claude/projects/<project>/<session>.jsonl
```

With `[backup.remote]`, each run then copies every snapshot the remote repository lacks into it (`restic copy`); a run with nothing new to copy leaves the remote alone. The remote is any repository restic can reach, such as an S3-compatible store, named as restic names it. The first copy makes it, cutting files into chunks as the local repository does, so the copies store each chunk once too. A copied snapshot has an id of its own and names the local snapshot it copies as its `original`; both have the same `tree`. The remote's `password-command` is required and is yours to keep somewhere this machine's loss doesn't reach. `env-command` prints the variables restic reaches the remote with, such as `AWS_ACCESS_KEY_ID` and `AWS_SECRET_ACCESS_KEY`, a `NAME=value` on each line, the value as it stands but for one pair of quotes around it; `sh -c` runs it, and nothing it prints appears in an error or a log. A command that prints a password or the variables has 60 seconds: one that waits for someone to answer it is stopped, and the run fails saying so, so it can't hold up the runs after it. restic gets each password in its own environment, never as an argument. A run that can't reach the remote has made its snapshot, fails saying so, and the next run copies it.

One run goes at a time, under `~/.ai/backup.lock`: a second waits for the first. A run does all it can, then exits with failure if anything went wrong: a file restic couldn't read (the snapshot holds the rest), a database it couldn't copy, a named folder that is missing, a remote it couldn't reach. Each run leaves what it did in `~/.ai/backup.json`; `status` prints it and fails while the last run failed, so a run that had no terminal still tells you. `watch` tells a failed run on its error output and runs again `every` later, reading `[backup]` anew each time; settings it can't read end it.

`threads` tells where the bytes every thread in `~/ai` read of harness files are, each file once for each thread that read it. Bytes a file on this machine still holds are read and compared in full, and count as backed up once this machine's newest snapshot holds the file at that length; bytes a file no longer holds are read back from the local repository, then from the remote one, and compared. It prints how many files are backed up, not yet backed up, with no backup (no repository made yet) and read on another machine with no repository here holding them, and fails if any are lost, naming each on its error output, or if a thread's records can't be read. It needs restic only once a repository exists.

Once the local repository is made, each session's end (`ai agent end-session`, which the Claude Code plugin runs) starts a run in the background, unless `on-session-end = false`. Backups begin with the first `ai backup run` or the service, never with a session.

`ai backup` needs `restic` on `PATH`, and fails saying so without it; the flake's package brings its own, after any on your `PATH`. The repository and password of every restic it runs come from `[backup]` alone: `RESTIC_REPOSITORY`, `RESTIC_PASSWORD` and their like in your shell never reach it.

`[backup]` belongs in your own `~/.ai/ai.toml` alone, and `ai backup` reads no other file: a project's settings never choose where backups go or what commands print their passwords. A bad key or value there fails `ai backup` and nothing else.

With Home Manager, the flake's module runs `ai backup watch` as the user service `ai-tools-backup`, restarted on failure:

```nix
{
  imports = [ inputs.ai-tools.homeModules.default ];
  services.ai-tools.backup.enable = true;
}
```

### Settings

`ai` reads `~/.ai/ai.toml`, then the nearest `ai.toml` at or above the current directory, then the project's private settings, `ai.toml` in its folder of `~/ai`; each file's keys win over the one before. The private settings are one user's overrides of a project's settings, which no repository of the project holds; a project needs its id for them. `ai init` creates them and links `ai.local.toml` to them, so you find them beside `ai.toml`; it adds the link to the project's `.gitignore`, and in a linked git worktree puts the link in the main checkout. An `ai.local.toml` that is a file of its own, not the link, is an error that says to move it. Every checkout and clone of the project reads the same file. They take the same tables, except `[project]`. Every key is optional:

```toml
[project]          # a project's ai.toml only
id = "project_…"   # written by ai init
name = "widget"    # its name; default the directory's

[chat]
background = true  # summarise in the background after each turn

[agent]
gc-after-mib = 32  # MiB of loose objects written to ~/ai since the last git gc, past which a commit starts the next

[agent.view]       # how many entries `ai agent view` shows; a turn or a line is one
total = 64         # entries in all
latest = 1         # the newest turns, every item
recent = 3         # the turns before them, message and final reply word for word, work summarised
single = 0         # the fewest older turns kept on a line each; shared lines take the rest

[summariser]       # what writes every summary: agents, memory, notes
backend = "claude" # claude: Claude Code's claude -p; api: a provider's API
model = "sonnet"   # claude's spelling; for api, <provider>/<model>, e.g. "deepseek/deepseek-flash"
effort = "medium"  # how hard the model thinks; unset, claude runs at medium and api at the provider's default
jobs = 8           # calls a build runs at once

[summariser.keys]  # api only, your own ai.toml only: each provider's command, which prints its key
deepseek = "cat /run/secrets/deepseek-api-key"

[summariser.witness]              # a model that checks every summary; unset, none
backend = "api"                   # takes backend, model and effort, each with its default above
model = "deepseek/deepseek-flash"
effort = "high"

[memory]
entry-chars = 280  # the length a note aims for, in characters, at most 512; 40 more are accepted

[memory.view]
lines = 96         # how many lines `ai memory view` prints

[agents.stabiliser]                       # one table per agent that needs one
instructions = "ai/agents/stabiliser.md"  # its own instructions, a Markdown file
worktree = "../ai-tools-stabiliser"       # the directory it runs in
reports-to = ["lead", "engineer"]         # its superiors, each an agent with a table of its own

[backup]                      # your own ~/.ai/ai.toml only
repository = "~/.ai/backup"   # the local restic repository: an absolute path, or one under ~/
password-command = "cat /run/secrets/ai-backup"  # prints its password; unset, ~/.ai/backup.key holds it
every = "5m"                  # the time `ai backup watch` waits between runs: a second or more
on-session-end = true         # a session's end starts a run, once the repository is made
folders = ["~/.claude-work/projects"]  # folders backed up besides the harnesses' own

[backup.remote]               # a second repository, which gets a copy of every snapshot; unset, none
repository = "s3:https://s3.example.com/bucket/ai"  # as restic names it
password-command = "pass show backup/ai"            # prints its password, required
env-command = "pass show backup/ai-env"             # prints NAME=value lines, the variables restic reaches it with
```

An agent's `instructions` is relative to the folder of the file that sets it, so an agent in `~/.ai/ai.toml` keeps its instructions in `~/.ai` and works in every project; in the private settings, it is relative to the project's root, where their link `ai.local.toml` sits. Instructions add up: the agent gets the user's, then the project's, then the private settings', each file's text in turn. Its `worktree` is relative to the project's root, the directory of its `ai.toml`, in whichever file sets it; in a linked git worktree, the root in the main checkout, so `../dev` names the same directory from every checkout.

An agent's `reports-to` names the agents it reports to; it may have several, so the agents form a hierarchy that need not be a tree. A file that sets `reports-to` replaces the list from the files before it, and `reports-to = []` clears it. A command that reads the hierarchy refuses one where an agent reports to itself, to a name no `[agents.<name>]` table defines, or, through others, to an agent below it; no command reads it yet. The agents below an agent, through any number of levels, are the threads it may read besides its own; never a sibling's or a superior's. Nothing enforces that yet: every session counts as the user's.

`ai agent resume` starts a new session in the agent's `worktree`, else in the current directory, and adds its `instructions` to the harness's system prompt, for a new session and a resumed one (Claude Code's `--append-system-prompt`). `ai` creates no worktree: a missing one fails, printing the `git worktree add` command. The worktree must be a checkout of the project, holding its `ai.toml` with the project's id, so its sessions find the project's threads in `~/ai`. A session in the worktree reads the same private settings.

An agent's address is `<project>.<agent>`: the name of the project's folder in `~/ai`, which every checkout and clone shares, a dot, then the agent's name, as in `quran.thayyil.net.dev`. An agent's name holds no dot, so an address splits at its last dot. `ai agent resume` gives the session it as its name (Claude Code's `--name`; a `--name` of yours after `--` wins), so other sessions send it messages by that name. Claude Code cuts a name at 200 characters, so a longer address is refused.

With `backend = "api"`, `ai` calls the provider's API through [genai](https://github.com/jeremychone/rust-genai). `model` names the provider, one of genai's adapters (`deepseek`, `openai`, `anthropic`, `gemini`, `open_router`, `groq`, `together`, `fireworks`, …), then the model as that provider spells it. The key comes from the provider's command in `[summariser.keys]`, else from the provider's environment variable (`DEEPSEEK_API_KEY`, `OPENAI_API_KEY`, …); a command that needs the summariser fails without one. A command that only starts the summariser in the background, such as `ai memory note`, checks `[summariser]` first: if it can make no summariser, the command still does its own work, says why on stderr and starts none. Each call is kept, like a `claude -p` one, in the store's `calls/`: the request, the provider's reply as it came, and its token counts. Costs count as zero unless the provider reports them (OpenRouter does). In this repository, `secretspec set DEEPSEEK_API_KEY` stores the key once, and `secretspec run -- ai memory build` passes it.

`[summariser.keys]` names, for each provider, a shell command that prints its key, such as `cat /run/secrets/deepseek-api-key` for a file of secrets or `pass show deepseek`. `ai` runs the command of the provider that `model` names with `sh -c`, once in each summariser it starts, and takes the first line it prints, without the blanks around it, as the key: `pass` keeps a secret on the first line of its entry. A key goes to the provider it is set for and no other, whichever model a project's settings or `--model` name: a provider with no command takes its key from its environment variable. The witness's key is its own provider's, from the same table. The key reaches the summariser alone, never your shell's environment, where every program you start would see it. Only your own `~/.ai/ai.toml` may hold the table: a project's `ai.toml` or private settings that hold it fail the commands that need the summariser, so no repository you work in runs a command of its own. A command that fails, prints no key on its first line, or has not ended after 60 seconds is an error naming the command; one still running then, waiting for someone to answer it, is stopped. What it prints never appears in an error, a log or a call record, and its error output goes where the summariser's does, the terminal or its log. With `backend = "claude"`, the keys go unused: Claude Code uses its own login.

`effort` is one of genai's reasoning efforts, `zero` (or `none`), `minimal`, `low`, `medium`, `high`, `xhigh` and `max`, or a budget of thinking tokens as a number. With `backend = "api"` it goes to the provider as genai maps it: DeepSeek takes `zero` as thinking off and any other as thinking on, mapping `minimal` and `low` to its low, `medium` to `xhigh` to its high, `max` to its max, and a budget to its default; unset, nothing goes and DeepSeek thinks at its default, high. With `backend = "claude"` it is `claude --effort`, so only `low`, `medium`, `high`, `xhigh` and `max` work. Each call's thinking tokens are kept in its record and logged with the rest. `jobs` sets how many calls `ai memory build`, `ai agent build` and `ai notes build` run at once.

With `[summariser.witness]`, a second model, the witness, checks every summary the summariser writes, for memory, agent threads and notes alike. It reads the request the summariser answered, which holds the sources, and the summary. It reports what the summary says that the sources do not, what it contradicts, where it changes their meaning, and any "not", "never" or rule of the user's that it drops; what was left out to fit the length is no fault. The summariser then rewrites the summary, held to the same limit as before, or contests a finding with its reason, and the witness checks again with everything said so far. They go back and forth until the witness finds nothing wrong, with no limit on rounds; a call that fails fails the summary. The log records each round (`witness found faults`, `summariser answered`, then `witnessed`), and the witness's calls are kept in `calls/` and counted with the summariser's. Its keys are those of `[summariser]` but `jobs`, with the same defaults, not the values set there. Its prompt is `src/memory/prompts/witness.txt`.

`ai memory config` prints the memory's sizes in effect.

An unknown key or a bad value fails the commands that read its table, naming the file and the key, and no other: a bad `[summariser]` key fails `ai memory build`, never `ai memory view`, and one agent's table fails the commands for that agent. A file that is not TOML, holds a table `ai` does not know, holds `[project]` outside a project's own `ai.toml`, or holds `[backup]` outside your own, leaves every table in it in doubt, so it fails every command that reads settings; finding the project reads the project's own `ai.toml` alone. Commands that read no settings, such as `ai memory grep`, never fail on them.

## Credits

`ai memory` ports Victor Taelin's [OptMem](https://github.com/VictorTaelin/OptMem) to Rust. A separate Claude Code model writes its summaries, using the compaction prompt from his [UniiChat](https://gist.github.com/VictorTaelin/91837951a5ce5b38f341ec1ba1df6449) (aka OptChat).

## Licence

[CC0-1.0](LICENSE)
