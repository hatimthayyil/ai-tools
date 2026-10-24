# ai-tools

The `ai` CLI: an all-in-one AI tool.

- `ai memory`: permanent memory for AI agents. An agent records what it learns as one-line notes. Claude Code folds older notes into summaries, and `ai memory view` prints the whole memory as one short view.
- `ai notes`: a global knowledge store for AI agents. Each note is one claim, filed in a tree of topics; Claude Code summarises every topic from its children, so an agent can browse the tree or search it.
- `ai files`: summaries of a project's files, rolled up its directory tree, so an agent can read a project from the top down.
- `ai agent`: named agents, each with one thread that spans its sessions. The thread copies its Claude Code sessions into the repository's git, summarises them into a tree you can zoom, and on `/ai` compacts a session into a view of the whole thread.

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

Views print whole on a terminal. Piped, as an agent reads them, they come in parts of at most 20,000 bytes, each ending with the command for the next.

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
ai init  # make this directory a project, once: mints its id into ai.toml, ignores ai.local.toml in .gitignore, prints the id
```

`ai files` and `ai agent` work in a project: the directory of the nearest `ai.toml` at or above the current directory, never `~/.ai/ai.toml`. It need not be a git repository. Its id is a TypeID, `project_` and a UUIDv7; commit `ai.toml`, so every clone and worktree is the same project. `ai` writes nothing to a repository's `.git`.

### ai files

```sh
ai files build          # summarise what changed since the last build, with a progress bar on a terminal
ai files view           # the project's summary
ai files view src/agent  # a directory's or a file's
```

A file's summary is asked for a tenth of its bytes, at least 512; a directory's is written from its children's summaries in passes that each halve the last, down to their mean size, at least 512. A summary may run a quarter over what it was asked for. What git ignores and files that are not text are left out. `build` runs only when you run it, up to 8 calls at once, each directory after its children; a summary is kept under the hash of what it covers, so the next build summarises only what changed, and a file or directory changed since shows no summary. A failed call stops the build, naming the path and the cause; the summaries written before it are kept. Summaries live in `~/.ai/files/<project id>`, or under `$AI_FILES_DIR` if set, with every call to the model kept byte for byte in `calls/`.

### ai agent

```sh
ai agent new dev                      # create agent dev, with an empty thread
ai agent start dev [-- claude args]   # Claude Code in a new session of dev; its thread's view is its context
ai agent resume dev [-- claude args]  # continue dev's newest session, in the directory it ran in
ai agent list                         # the agents here, newest first, and their sessions
ai agent view dev                     # dev's view, from the summaries written so far
ai agent zoom -t dev <id>             # open a line of it; --tool-output adds tool output
ai agent grep -t dev <regex>          # search it
ai agent build -t dev                 # write its summaries now, with a progress bar on a terminal; --rebuild, --model, --dry-run
```

Only `new` creates an agent; `start`, `resume`, `attach` and the reading commands refuse a name it has not made. Unnamed, a command reads the session's agent, else the project's only agent, else the one you pick on a terminal; elsewhere it lists the agents. Inside a session, `/ai attach <name>` and `/ai detach`, so the agent reads its thread with `ai agent view` and `ai agent zoom <id>`. In an attached session, `/ai` replaces the context with `ai view`: the whole memory, then the thread's view, so the agent wakes with no further reads. If it cannot build the thread's view, it says why and compacts nothing. `/compact` and auto-compaction stay Claude Code's own. Threads live in `~/.ai/chat/<project id>`, or under `$AI_CHAT_DIR` if set, so every clone and worktree of a project shares them.

A session is attached to one agent at a time. Detach it explicitly before attaching it to another; the previous agent's thread keeps the recorded history.

Each turn's work, the agent's steps between your message and its final reply, is summarised first; the turn's line is then written from your message, that summary and the final reply. A summary line is asked for 50 to 200 words, more where the AI wrote more and ran more tools, and holds at most 512 bytes per 50 words. A command you run, `/ai` included, writes the lines it needs and stops at the first that fails, naming each failed line with its error: `/ai` then compacts nothing, and `ai agent start` starts no session. The background summariser, started after each turn, builds what it can and only logs, to `<thread>.log` beside the threads.

A thread is JSON Lines files: append-only records and summaries, beside byte-for-byte copies of the original transcripts and companion session files. Earlier states are prefixes of those files (`--at`). A newer store format is refused. Re-attaching a session rebuilds its thread from the transcript.

This needs the [Claude Code plugin](#claude-code-plugin).

### Settings

`ai` reads `~/.ai/ai.toml`, then the nearest `ai.toml` at or above the current directory, then the `ai.local.toml` beside it; each file's keys win over the one before. `ai.local.toml` is one user's overrides of a project's settings: `ai init` adds it to the project's `.gitignore`. It takes the same tables, except `[project]`. Every key is optional:

```toml
[project]          # a project's ai.toml only
id = "project_…"   # written by ai init
name = "widget"    # its name; default the directory's

[chat]
background = true  # summarise in the background after each turn

[summariser]       # what writes every summary: chat, memory, notes, files
backend = "claude" # claude -p; the only backend yet
model = "sonnet"   # in the backend's own spelling

[memory]
view-lines = 96    # how many lines `ai memory view` prints
entry-chars = 280  # the longest note, in bytes; at most 512

[agents.stabiliser]                       # one table per agent that needs one
instructions = "ai/agents/stabiliser.md"  # its own instructions, a Markdown file
worktree = "../ai-tools-stabiliser"       # the directory it runs in
```

An agent's `instructions` is relative to the folder of the file that sets it, so an agent in `~/.ai/ai.toml` keeps its instructions in `~/.ai` and works in every project. Its `worktree` is relative to the project's root, the directory of its `ai.toml`, in whichever file sets it.

`ai memory config` prints the memory's sizes in effect. An unknown key or a bad value fails the command, naming the file and the key.

## Credits

`ai memory` ports Victor Taelin's [OptMem](https://github.com/VictorTaelin/OptMem) to Rust. A separate Claude Code model writes its summaries, using the compaction prompt from his [UniiChat](https://gist.github.com/VictorTaelin/91837951a5ce5b38f341ec1ba1df6449) (aka OptChat).

## Licence

[CC0-1.0](LICENSE)
