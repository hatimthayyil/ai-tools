# ai-tools

The `ai` CLI: an all-in-one AI tool.

- `ai memory`: permanent memory for AI agents. An agent records what it learns as one-line notes. Claude Code folds older notes into summaries, and `ai memory view` prints the whole memory as one short view.
- `ai notes`: a global knowledge store for AI agents. Each note is one claim, filed in a tree of topics; Claude Code summarises every topic from its children, so an agent can browse the tree or search it.
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
ai init  # make this directory a project, once: mints its id into ai.toml, links ai.local.toml to .ai/local/ai.toml, ignores both ai.local.toml and .ai/local/ in .gitignore, prints the id
```

`ai agent` works in a project: the directory of the nearest `ai.toml` at or above the current directory, never `~/.ai/ai.toml`. It need not be a git repository. Its id is a TypeID, `project_` and a UUIDv7; commit `ai.toml`, so every clone and worktree is the same project. `ai` writes nothing to a repository's `.git`.

A project keeps three places for ai: `ai/`, the agents' visible workspace; `.ai/`, hidden and tracked by the project's git; and `.ai/local/`, one user's private data, which the project's git ignores. `.ai/local/` holds the agents' threads and the private settings, in the project's main checkout. It is a git repository of its own, made on its first write, so they keep their history. It commits on events, once each, naming the command: creating an agent, attaching, detaching, resuming, `/ai` and other builds, a session's end (`ai agent attach dev`, `ai agent end-session`). A turn's sync, a view and the background summariser write without committing; the next event commits what they wrote. No commit is made when nothing changed. Its `.gitignore` leaves out locks and logs. A command that fails still commits what it wrote; a commit that fails is an error.

### ai agent

```sh
ai agent new dev                      # create agent dev, with an empty thread
ai agent resume dev [-- claude args]  # continue dev's newest session, in the directory it ran in,
                                      # else a new one, whose context is the thread's view;
                                      # --harness: claude is the only one yet
ai resume dev [-- claude args]        # the same, shorter
ai agent list                         # the agents here, newest first, and their sessions
ai agent view dev                     # dev's view, from the summaries written so far
ai agent zoom -a dev <id>             # open a line of it; --tool-output adds tool output
ai agent grep -a dev <regex>          # search it; repeat -a to search several agents
ai agent build -a dev                 # write its summaries now, with a progress bar on a terminal; --rebuild, --model, --dry-run
```

Only `new` creates an agent; `resume`, `attach` and the reading commands refuse a name it has not made. Unnamed, a command reads the session's agent, else the project's only agent, else the one you pick on a terminal; elsewhere it lists the agents. Inside a session, `/ai new <name>` creates an agent and attaches the session to it, and `/ai attach <name>` and `/ai detach` attach and detach it, so the agent reads its thread with `ai agent view` and `ai agent zoom <id>`. `/ai` replaces the context with `ai view`: the whole memory, then, in an attached session, the thread's view, so the agent wakes with no further reads. If it cannot build the thread's view, it says why and compacts nothing. An unattached session compacts into the memory alone. `/compact` and auto-compaction stay Claude Code's own. Threads live in `.ai/local/agents/` in the project's main checkout, so its linked worktrees share them; `ai init` adds `/.ai/local/` to `.gitignore`.

A session is attached to one agent at a time. Detach it explicitly before attaching it to another; the previous agent's thread keeps the recorded history.

Each turn's work, the agent's steps between your message and its final reply, is summarised first; the turn's line is then written from your message, that summary and the final reply. A summary line is asked for 50 to 200 words, more where the AI wrote more and ran more tools, and holds at most 512 bytes per 50 words. A command you run, `/ai` included, writes the lines it needs and stops at the first that fails, naming each failed line with its error: `/ai` then compacts nothing, and `ai agent resume` starts no new session. The background summariser, started after each turn, builds what it can and only logs, to `<thread>.log` beside the threads.

A thread is JSON Lines files: append-only records and summaries, beside byte-for-byte copies of the original transcripts and companion session files. Each file is a directory of 1 MiB segments, so a commit of a growing thread stores only the newest segment. Earlier states are prefixes of those files (`--at`). A newer store format is refused; an older one is converted, once you agree, the first time an `ai agent` command runs on a terminal. Re-attaching a session rebuilds its thread from the transcript.

This needs the [Claude Code plugin](#claude-code-plugin).

### Settings

`ai` reads `~/.ai/ai.toml`, then the nearest `ai.toml` at or above the current directory, then the project's private settings, `.ai/local/ai.toml` beside it; each file's keys win over the one before. The private settings are one user's overrides of a project's settings, kept in the private store so they have history. `ai init` creates them and links `ai.local.toml` to them, so you find them beside `ai.toml`; it adds the link to the project's `.gitignore`. An `ai.local.toml` that is a file of its own, not the link, is an error that says to move it. In a linked git worktree, `ai` reads the private settings from the same folder in the main checkout, so one file serves every worktree. They take the same tables, except `[project]`. Every key is optional:

```toml
[project]          # a project's ai.toml only
id = "project_…"   # written by ai init
name = "widget"    # its name; default the directory's

[chat]
background = true  # summarise in the background after each turn

[agent.view]
lines = 64         # the most lines the view folds the older turns into

[summariser]       # what writes every summary: agents, memory, notes
backend = "claude" # claude -p; the only backend yet
model = "sonnet"   # in the backend's own spelling

[memory]
entry-chars = 280  # the length a note aims for, in bytes, at most 512; 40 more are accepted

[memory.view]
lines = 96         # how many lines `ai memory view` prints

[agents.stabiliser]                       # one table per agent that needs one
instructions = "ai/agents/stabiliser.md"  # its own instructions, a Markdown file
worktree = "../ai-tools-stabiliser"       # the directory it runs in
```

An agent's `instructions` is relative to the folder of the file that sets it, so an agent in `~/.ai/ai.toml` keeps its instructions in `~/.ai` and works in every project; in the private settings, it is relative to the project's root, where their link `ai.local.toml` sits. Instructions add up: the agent gets the user's, then the project's, then the private settings', each file's text in turn. Its `worktree` is relative to the project's root, the directory of its `ai.toml`, in whichever file sets it; in a linked git worktree, the root in the main checkout, so `../dev` names the same directory from every checkout.

`ai agent resume` starts a new session in the agent's `worktree`, else in the current directory, and adds its `instructions` to the harness's system prompt, for a new session and a resumed one (Claude Code's `--append-system-prompt`). `ai` creates no worktree: a missing one fails, printing the `git worktree add` command. The worktree must be a git worktree of the project's main checkout, at a commit holding its `ai.toml`, so its sessions find the threads in the main checkout's `.ai/local/agents`; a plain copy, or another clone, has its own. A session in the worktree reads the private settings from the main checkout.

`ai memory config` prints the memory's sizes in effect. An unknown key or a bad value fails the command, naming the file and the key.

## Credits

`ai memory` ports Victor Taelin's [OptMem](https://github.com/VictorTaelin/OptMem) to Rust. A separate Claude Code model writes its summaries, using the compaction prompt from his [UniiChat](https://gist.github.com/VictorTaelin/91837951a5ce5b38f341ec1ba1df6449) (aka OptChat).

## Licence

[CC0-1.0](LICENSE)
