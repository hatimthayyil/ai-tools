# ai-tools

The `ai` CLI: an all-in-one AI tool.

- `ai memory`: permanent memory for AI agents. An agent records what it learns as one-line notes. Claude Code folds older notes into summaries, and `ai memory view` prints the whole memory as one short view.
- `ai notes`: a global knowledge store for AI agents. Each note is one claim, filed in a tree of topics; Claude Code summarises every topic from its children, so an agent can browse the tree or search it.
- `ai agent`: named agents, each with one thread that spans its sessions. The thread copies its Claude Code sessions into the repository's git, summarises them into a tree you can zoom, and on `/ai` compacts a session into a view of the whole thread.
- `ai harness`: every harness's own files, its transcripts above all, kept under `~/.ai/harness`, each folder linked from where its harness looks for it.

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
- `~/.ai`, what belongs to this machine and never goes in git: the threads' locks and compactor logs in `~/.ai/agents/<project id>/`, the harnesses' files in `~/.ai/harness/`, archives.

A thread keeps its items, lines, session tracks, attachment changes and subagents, never a copy of a session's files: it records each file it read (its harness, its path from the harness's home, its length and git blob id) in `sources.jsonl`. The harness's own files, in `~/.ai/harness`, are the only copy, so a sync that finds a recorded file gone, or a transcript that no longer begins with the bytes the thread read, is an error naming the file and saying to restore it from the backup of `~/.ai/harness`.

Events commit the project's folder of `~/ai` as one commit naming the command: creating or renaming an agent, attaching, detaching, resuming, `/ai` and other builds, a session's end (`ai agent attach dev`, `ai agent end-session`). A commit touches no other path of `~/ai`, and of its index only that folder's entries, so what you stage there yourself stays staged. A turn's sync, a view and the background summariser write without committing; the next event commits what they wrote. A command that fails still commits what it wrote.

Agents an older ai kept in `.ai/local/` of the project are refused until they move. On a terminal, an `ai agent` command, `ai resume` or `ai init` offers the move: the threads and private settings go to the project's folder in `~/ai`, each copied session file becomes a record of the harness's own file, and a copy the harness no longer holds whole is restored into the harness's files first; the old store is archived whole, its git history included, in `~/.ai/archive/<project id>/`, then removed from the project.

### ai agent

```sh
ai agent new dev                      # create agent dev, with an empty thread
ai agent rename dev reviewer          # rename it; its sessions stay attached, and each [agents.dev]
                                      # table or reports-to naming dev is printed, not changed
ai agent resume dev [-- claude args]  # continue dev's newest session, in the directory it ran in,
                                      # else a new one, whose context is the thread's view;
                                      # --harness: claude is the only one yet
ai resume dev [-- claude args]        # the same, shorter
ai agent list                         # the agents here, newest first, and their sessions
ai agent view dev                     # dev's view, from the summaries written so far
ai agent zoom -a dev <id>             # open a line of it; --tool-output adds tool output
ai agent grep -a dev <regex>          # search it; repeat -a to search several agents
ai agent build -a dev                 # write its summaries now: its plan, then a bar per level (a line per summary off a terminal); --rebuild, --model; --dry-run prints the plan only; --preview writes each line and stores nothing
```

Only `new` creates an agent; `resume`, `attach` and the reading commands refuse a name it has not made. Unnamed, a command reads the session's agent, else the project's only agent, else the one you pick on a terminal; elsewhere it lists the agents. Inside a session, `/ai new <name>` creates an agent and attaches the session to it, and `/ai attach <name>` and `/ai detach` attach and detach it, so the agent reads its thread with `ai agent view` and `ai agent zoom <id>`. `/ai` replaces the context with `ai view`: the whole memory, then, in an attached session, the thread's view, so the agent wakes with no further reads. If it cannot build the thread's view, it says why and compacts nothing. An unattached session compacts into the memory alone. `/compact` and auto-compaction stay Claude Code's own. Threads live in the project's folder of `~/ai`, so every checkout and clone of the project shares them.

A session is attached to one agent at a time. Detach it explicitly before attaching it to another; the previous agent's thread keeps the recorded history.

Each turn's work, the agent's steps between your message and its final reply, is summarised first; the turn's line is then written from your message, that summary and the final reply. A summary line is asked for 50 to 200 words, more where the AI wrote more and ran more tools, and holds at most 512 characters per 50 words. A command you run, `/ai` included, writes the lines it needs, however long they take, and stops at the first that fails, naming each failed line with its error: `/ai` then compacts nothing, and `ai agent resume` starts no new session. Press Esc while `/ai` runs to cancel it: its model calls stop, nothing is compacted, the lines it wrote stay, and the next `/ai` goes on from them. The background summariser, started after each turn, builds what it can and only logs, to `<thread>.log` beside the threads.

A thread is JSON Lines files: append-only records and summaries, beside byte-for-byte copies of the original transcripts and companion session files. Each file is a directory of 1 MiB segments, so a commit of a growing thread stores only the newest segment. A store of another format is refused. Re-attaching a session rebuilds its thread from the transcript.

This needs the [Claude Code plugin](#claude-code-plugin).

### ai harness

```sh
ai harness status  # where each harness folder stands
ai harness link    # link each folder that does not exist yet; never moves data
ai harness move    # move what nothing is using, finish moves cut short, prune old backups
ai harness prune   # delete every backup nothing wrote to since its move, without waiting
```

Each harness writes its transcripts and session files into its own folder: Claude Code `projects`, Codex `sessions` and `archived_sessions`, pi `sessions`, DSH `sessions` and `attachments`, Gemini `tmp`, opencode `storage` and `opencode.db`, Hermes `sessions` and `state.db`. `ai harness` keeps each one at `~/.ai/harness/<harness>/<path>` (folder mode 700) and puts a link where it was, so the harness and everything that recorded the old path find it there. A harness's folder is where the harness itself looks: its variable (`CLAUDE_CONFIG_DIR`, `CODEX_HOME`, `PI_CODING_AGENT_DIR`, `DSH_HOME`, `GEMINI_CLI_HOME`, `XDG_DATA_HOME`, `HERMES_HOME`) when that is an absolute path, else its default under `$HOME`. The link goes where the folder really sits, its parent resolved, so a folder reached through a linked parent is replaced there. A folder already under `~/.ai/harness`, as an agent's own harness folder is, is left alone, and so is a link to anywhere else.

`link` makes the missing folders and links them, so a harness installed later writes straight into `~/.ai/harness`; a database is linked only once it exists, since SQLite must not meet a link to nothing. A folder that still holds data waits for `move`.

`move` moves a folder only while nothing can write to it: no process of its harness runs, matched on its first four arguments, and no process has a file or its working folder in it or where it goes, both read from `/proc` and checked again after the copy. It refuses a database whose `-wal` is not empty, a place under `~/.ai/harness` holding other data, and a copy that would leave less than 1G free. It copies the folder beside its place, compares every entry's kind, mode, size and SHA-1 (collision-checked, as git hashes) with the original, swaps in the link with termination signals held back, then reads the copy back through the link; anything that differs puts the original back. The original stays as `<folder>.moved-to-ai`, and the move's record, every entry's hash among it, as `<place>.move.json`. A move cut off is finished or redone by the next `move`. Fourteen days after its move, `move` deletes a backup that still matches its record, meaning nothing wrote to it since; one that does not is kept and reported. A folder in use, and every problem that needs you, is reported; problems make it exit with failure.

With Home Manager, the flake's module runs `link` on every switch and `move` from a user unit started with the user manager: at boot when the user lingers, else at the first login, which never waits for it. Harness services such as `hermes-gateway.service` start after it. Home Manager starts the unit once at the switch that adds it, where `move` leaves alone whatever is in use:

```nix
{
  imports = [ inputs.ai-tools.homeModules.default ];
  services.ai-harness.enable = true;
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

[summariser.witness]              # a model that checks every summary; unset, none
backend = "api"                   # the keys of [summariser] but jobs, with its defaults
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
```

An agent's `instructions` is relative to the folder of the file that sets it, so an agent in `~/.ai/ai.toml` keeps its instructions in `~/.ai` and works in every project; in the private settings, it is relative to the project's root, where their link `ai.local.toml` sits. Instructions add up: the agent gets the user's, then the project's, then the private settings', each file's text in turn. Its `worktree` is relative to the project's root, the directory of its `ai.toml`, in whichever file sets it; in a linked git worktree, the root in the main checkout, so `../dev` names the same directory from every checkout.

An agent's `reports-to` names the agents it reports to; it may have several, so the agents form a hierarchy that need not be a tree. A file that sets `reports-to` replaces the list from the files before it, and `reports-to = []` clears it. A command that reads the hierarchy refuses one where an agent reports to itself, to a name no `[agents.<name>]` table defines, or, through others, to an agent below it; no command reads it yet. The agents below an agent, through any number of levels, are the threads it may read besides its own; never a sibling's or a superior's. Nothing enforces that yet: every session counts as the user's.

`ai agent resume` starts a new session in the agent's `worktree`, else in the current directory, and adds its `instructions` to the harness's system prompt, for a new session and a resumed one (Claude Code's `--append-system-prompt`). `ai` creates no worktree: a missing one fails, printing the `git worktree add` command. The worktree must be a checkout of the project, holding its `ai.toml` with the project's id, so its sessions find the project's threads in `~/ai`. A session in the worktree reads the same private settings.

With `backend = "api"`, `ai` calls the provider's API through [genai](https://github.com/jeremychone/rust-genai). `model` names the provider, one of genai's adapters (`deepseek`, `openai`, `anthropic`, `gemini`, `open_router`, `groq`, `together`, `fireworks`, …), then the model as that provider spells it. The key comes from the provider's environment variable (`DEEPSEEK_API_KEY`, `OPENAI_API_KEY`, …), never from `ai.toml`; a command that needs the summariser fails when it is unset. Each call is kept, like a `claude -p` one, in the store's `calls/`: the request, the provider's reply as it came, and its token counts. Costs count as zero unless the provider reports them (OpenRouter does). In this repository, `secretspec set DEEPSEEK_API_KEY` stores the key once, and `secretspec run -- ai memory build` passes it.

`effort` is one of genai's reasoning efforts, `zero` (or `none`), `minimal`, `low`, `medium`, `high`, `xhigh` and `max`, or a budget of thinking tokens as a number. With `backend = "api"` it goes to the provider as genai maps it: DeepSeek takes `zero` as thinking off and any other as thinking on, mapping `minimal` and `low` to its low, `medium` to `xhigh` to its high, `max` to its max, and a budget to its default; unset, nothing goes and DeepSeek thinks at its default, high. With `backend = "claude"` it is `claude --effort`, so only `low`, `medium`, `high`, `xhigh` and `max` work. Each call's thinking tokens are kept in its record and logged with the rest. `jobs` sets how many calls `ai memory build`, `ai agent build` and `ai notes build` run at once.

With `[summariser.witness]`, a second model, the witness, checks every summary the summariser writes, for memory, agent threads and notes alike. It reads the request the summariser answered, which holds the sources, and the summary. It reports what the summary says that the sources do not, what it contradicts, where it changes their meaning, and any "not", "never" or rule of the user's that it drops; what was left out to fit the length is no fault. The summariser then rewrites the summary, held to the same limit as before, or contests a finding with its reason, and the witness checks again with everything said so far. They go back and forth until the witness finds nothing wrong, with no limit on rounds; a call that fails fails the summary. The log records each round (`witness found faults`, `summariser answered`, then `witnessed`), and the witness's calls are kept in `calls/` and counted with the summariser's. Its keys are those of `[summariser]` but `jobs`, with the same defaults, not the values set there. Its prompt is `src/memory/prompts/witness.txt`.

`ai memory config` prints the memory's sizes in effect.

An unknown key or a bad value fails the commands that read its table, naming the file and the key, and no other: a bad `[summariser]` key fails `ai memory build`, never `ai memory view`, and one agent's table fails the commands for that agent. A file that is not TOML, holds a table `ai` does not know, or holds `[project]` outside a project's own `ai.toml` leaves every table in it in doubt, so it fails every command that reads settings; finding the project reads the project's own `ai.toml` alone. Commands that read no settings, such as `ai memory grep`, never fail on them.

## Credits

`ai memory` ports Victor Taelin's [OptMem](https://github.com/VictorTaelin/OptMem) to Rust. A separate Claude Code model writes its summaries, using the compaction prompt from his [UniiChat](https://gist.github.com/VictorTaelin/91837951a5ce5b38f341ec1ba1df6449) (aka OptChat).

## Licence

[CC0-1.0](LICENSE)
