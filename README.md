# ai-tools

The `ai` CLI: an all-in-one AI tool.

- `ai memory`: permanent memory for AI agents. An agent records what it learns as one-line notes. Claude Code folds older notes into summaries, and `ai memory view` prints the whole memory as one short view.
- `ai notes`: a global knowledge store for AI agents. Each note is one claim, filed in a tree of topics; Claude Code summarises every topic from its children, so an agent can browse the tree or search it.
- `ai files`: summaries of a project's files, rolled up its directory tree, so an agent can read a project from the top down.
- `ai chat`: named conversations that span agent sessions. A thread copies its Claude Code sessions into the repository's git, summarises them into a tree you can zoom, and on `/ai` compacts a session into a view of the whole thread.

## Install

```sh
cargo install ai-tools
```

or with Nix:

```sh
nix profile add github:hatimthayyil/ai-tools
```

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

### ai files

```sh
ai files build          # summarise what changed since the last build, with a progress bar on a terminal
ai files view           # the project's summary
ai files view src/chat  # a directory's or a file's
```

A file's summary is asked for a tenth of its bytes, at least 512; a directory's is written from its children's summaries in passes that each halve the last, down to their mean size, at least 512. A summary may run a quarter over what it was asked for. What git ignores and files that are not text are left out. `build` runs only when you run it, up to 8 calls at once, each directory after its children; a summary is kept under the hash of what it covers, so the next build summarises only what changed, and a file or directory changed since shows no summary. A failed call stops the build, naming the path and the cause; the summaries written before it are kept. Summaries live in `~/.ai/files/<root commit>`, or under `$AI_FILES_DIR` if set, with every call to the model kept byte for byte in `calls/`.

### ai chat

```sh
ai chat start dev [-- claude args]   # Claude Code in a new session of thread dev; an existing thread's view is its context
ai chat resume dev [-- claude args]  # continue dev's newest session, in the directory it ran in
ai chat list                         # the threads here, newest first, and their sessions
ai chat view dev                     # dev's view, from the summaries written so far
ai chat zoom -t dev <id>             # open a line of it; --tool-output adds tool output
ai chat grep -t dev <regex>          # search it
ai chat build -t dev                 # write its summaries now, with a progress bar on a terminal; --rebuild, --model, --dry-run
```

Unnamed, a command reads the session's thread, else the repository's only thread, else the one you pick on a terminal; elsewhere it lists the threads. Inside a session, `/ai attach <name>` and `/ai detach`, so the agent reads its thread with `ai chat view` and `ai chat zoom <id>`. In an attached session, `/ai` replaces the context with `ai view`: the whole memory, then the thread's view, so the agent wakes with no further reads. If it cannot build the thread's view, it says why and compacts nothing. `/compact` and auto-compaction stay Claude Code's own. Threads live in `~/.ai/chat/<root commit>`, or under `$AI_CHAT_DIR` if set, so every worktree and clone of a repository shares them.

A session has one active thread. Detach it explicitly before attaching it elsewhere; its previous thread retains the recorded history.

Each turn's work, the agent's steps between your message and its final reply, is summarised first; the turn's line is then written from your message, that summary and the final reply. A summary line is asked for 50 to 200 words, more where the AI wrote more and ran more tools, and holds at most 512 bytes per 50 words. A command you run, `/ai` included, writes the lines it needs and stops at the first that fails, naming each failed line with its error: `/ai` then compacts nothing, and `ai chat start` starts no session. The background summariser, started after each turn, builds what it can and only logs, to `<thread>.log` beside the threads.

A thread is JSON Lines files: append-only records and summaries, beside byte-for-byte copies of the original transcripts and companion session files. Earlier states are prefixes of those files (`--at`). A newer store format is refused. Re-attaching a session rebuilds its thread from the transcript.

This needs the Claude Code plugin in `share/ai/plugins/claude` of the package (`plugins/claude` here). Load it in every session with the `env` block of `~/.claude/settings.json`:

```json
"env": { "CLAUDE_CODE_PLUGIN_DIRS": "/home/<you>/.nix-profile/share/ai/plugins/claude" }
```

or for one session with `claude --plugin-dir <dir>`.

### Settings

`ai` reads `~/.ai/ai.toml`, then the nearest `ai.toml` at or above the current directory, whose keys win. Every key is optional:

```toml
[chat]
model = "sonnet"   # the summariser's model; default $AI_MEMORY_MODEL, else Sonnet
background = true  # summarise in the background after each turn

[memory]
view-lines = 96    # how many lines `ai memory view` prints
entry-chars = 280  # the longest note, in bytes; at most 512
```

`ai memory config` prints the memory's sizes in effect. An unknown key or a bad value fails the command, naming the file and the key.

## Credits

`ai memory` ports Victor Taelin's [OptMem](https://github.com/VictorTaelin/OptMem) to Rust. A separate Claude Code model writes its summaries, using the compaction prompt from his [UniiChat](https://gist.github.com/VictorTaelin/91837951a5ce5b38f341ec1ba1df6449) (aka OptChat).

## Licence

[CC0-1.0](LICENSE)
