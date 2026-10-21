# Changelog

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
