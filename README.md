# ai-tools

The `ai` CLI: an all-in-one AI tool.

- `ai memory`: permanent memory for AI agents. An agent records what it learns as one-line notes. Claude Code folds older notes into summaries, and `ai memory wake` prints the whole memory as one short view.
- `ai notes`: a global knowledge store for AI agents. Each note is one claim, filed in a tree of topics; Claude Code summarises every topic from its children, so an agent can browse the tree or search it.
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
ai memory wake                  # read the memory
ai memory note "what happened"  # record one line
ai memory zoom <id>             # open a line of the view
ai memory grep <regex>          # search every message
```

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

### ai chat

```sh
ai chat start dev [-- claude args]   # Claude Code in a new session of thread dev; an existing thread's view is its context
ai chat resume dev [-- claude args]  # continue dev's newest session, in the directory it ran in
ai chat list                         # the threads here and their sessions
ai chat view -t dev                  # dev's view, from the summaries written so far
ai chat zoom -t dev <id>             # open a line of it; --tool-output adds tool output
ai chat grep -t dev <regex>          # search it
ai chat summarise -t dev             # write its summaries now; --rebuild, --model, --dry-run
```

Inside a session, `/ai attach <name>` and `/ai detach`; `-t` defaults to the session's thread, so the agent reads it with `ai chat view` and `ai chat zoom <id>`. In an attached session, `/ai` replaces the context with the thread's view; if it cannot build the view, it says why and compacts nothing. `/compact` and auto-compaction stay Claude Code's own. Threads live in `~/.ai/chat/<root commit>`, or under `$AI_CHAT_DIR` if set, so every worktree and clone of a repository shares them; `git config ai.chat.model` picks the summariser's model.

A session has one active thread. Detach it explicitly before attaching it elsewhere; its previous thread retains the recorded history.

A thread is JSON Lines files: append-only records and summaries, beside byte-for-byte copies of the original transcripts and companion session files. Earlier states are prefixes of those files (`--at`). A newer store format is refused. Re-attaching a session rebuilds its thread from the transcript.

This needs the Claude Code plugin in `share/ai/plugins/claude` of the package (`plugins/claude` here). Load it in every session with the `env` block of `~/.claude/settings.json`:

```json
"env": { "CLAUDE_CODE_PLUGIN_DIRS": "/home/<you>/.nix-profile/share/ai/plugins/claude" }
```

or for one session with `claude --plugin-dir <dir>`.

## Credits

`ai memory` ports Victor Taelin's [OptMem](https://github.com/VictorTaelin/OptMem) to Rust. A separate Claude Code model writes its summaries, using the compaction prompt from his [UniiChat](https://gist.github.com/VictorTaelin/91837951a5ce5b38f341ec1ba1df6449) (aka OptChat).

## Licence

[CC0-1.0](LICENSE)
