# Ids, handles and paths

Author: Claude Code (Claude Opus 5.5)

This is a design for review, and nothing in it is implemented. Facts about today's code are from main `837abb3f`. The choice of TypeID comes from `ai/research/20261021T194500--durable-id-alternatives.md` (below, "the id note"). How forks reach a thread comes from `ai/research/20261020T231858--fork-transcripts-in-threads.md` ("the fork note"). Points marked **Recommended** await the user's confirmation; the rest is agreed.

## Terms

- **Id:** the name the CLI uses for an object. It never changes.
- **Slug:** the readable part of a file or directory name. It can change.
- **Handle:** a short name people and agents read and type (`7.0`, `main`, `b3`, `599`). The CLI resolves it to an id. A handle can change; an id can't.
- **Address:** a handle with its context (`ai-chat:b3.7.0`, `memory.599`).

## Ids

**Minted**, as TypeIDs over UUIDv7 (`<prefix>_<26 base-32 characters>`):
- threads, `thread_`;
- notes, `note_`;
- memory messages, `memory_`;
- projects, by `ai init` into the project's `ai.toml` (see Projects).

**Derived**, so a rebuild gives the same ids:
- **Item:** `item_`, the UUIDv5 of (harness, key). The key is unique across the harness:
  - Claude: the entry's uuid, already unique across sessions (`ai/comments/src/chat/claude.rs.md`).
  - pi: the entry plus the session where it was first written, since pi entry ids are 8 hex digits, unique only within a session (the id note, "Consequences").

  The id depends only on the session files, not on the order sessions are attached. Items still dedupe by (harness, key), so a Claude fork's copied entries keep merging into the items already there.

**Branches have no ids.** A branch is a label (`main`, `bN`) pointing at its newest item, stored with the thread. Anything saved for later cites an item, and an item fixes its whole line back to the start, as a commit fixes its history.

**Records** use item ids in place of sequence numbers: parents, line keys, a binding's graft and tip, an agent's call. A range is stored as its first and last item; the items between are found by following parents back from the last, as git's `A..B` does. People still type `8+4`. A rebuild that attaches sessions in another order keeps every key, so it summarises nothing again.

**Recommended:** ids appear only in records and file names, and models never see them. Records store the bare id, since the field names the type; the prefix is printed only when an id is shown for citing.

Crates: `uuid` (features `v7` and `v5`) and one TypeID crate, `mti` or `type-safe-id`, chosen after checking their APIs (the id note). Neither is a dependency today (`Cargo.toml`).

Today:
- A thread is known only by its name, as `threads/<name>` (`src/chat/store.rs:32`).
- An item's source is `SourceId { harness, session, entry }` (`src/chat/model.rs:9`). When one Claude entry makes several items, the k-th after the first has entry `<uuid>.<k>` (`src/chat/claude.rs:229`).
- In the store, an item is a sequence number, its position in the thread, and parents are sequence numbers (`src/chat/record.rs:18`). So are line keys (`level@last`, `src/chat/record.rs:116`) and a binding's graft and tip (`src/chat/record.rs:95`).
- The store dedupes items by harness and entry, not session (`src/chat/store.rs:78`).
- Memory messages are positions `0, 1, …`, stored at `log/<hi>/<lo>` (`src/memory/store.rs:368`). A memory's `origin` is 6 random Crockford characters (`src/memory/store.rs:166`).
- A note's id is its UTC Denote timestamp, moved a second later past any id already taken (`src/notes/leaf.rs:126`). That's Denote's own method, which the id note rules out for anything written in parallel or on two machines.

## On disk

An object stored as a file or directory, such as a project or thread directory or a note file, is named `<id>--<slug>`. Records inside a JSON Lines file, such as items and memory messages, carry their id as a field and have no slug.

- The id never changes; the slug can. Renaming a thread or retitling a note changes only the slug.
- An object is found by its id, whatever its slug says.
- Two files with the same id is an error. So is a name in a store that doesn't parse as `<id>--<slug>`. Both fail at once, and nothing is guessed. Notes already refuse a misnamed leaf (`src/notes/leaf.rs:185`).
- A TypeID suffix has a fixed length and contains no `-`, so the slug starts at the first `--` after the id (the id note).

Notes already split `<id>--<slug>.md` (`src/notes/leaf.rs:161`). Files in the repo tree (`ai/research`, …) keep their Denote names, `<timestamp>--<slug>`, until `ai notes` takes them over.

## Projects

- A project is the directory holding the nearest `ai.toml`, found upward as Cargo finds `Cargo.toml`. A folder that isn't a git repository can be a project.
- `ai init` mints the project's id into its `ai.toml`. The slug comes from `ai.toml`, else from the directory name.
- Nothing is stored in a repository's `.git`.
- Two clones share the committed `ai.toml`, so they are one project. If two projects share a slug, a handle naming it fails with an error naming both.

Today the project id is the oldest root commit reachable from `origin/HEAD`, else from `main`, `master` or `HEAD`, cached as `ai.root` in the repository's git config (`src/memory/prov.rs:55`, `:69`). The chat store is `~/.ai/chat/<root commit>` (`src/chat/store.rs:168`, `:194`).

## Handles

| Handle | Names | Numbered |
|---|---|---|
| `7`, `7.0` | a turn or item on a branch | by position along the branch, computed from the tree, not stored |
| `main` | a branch | moves with the line you carry on |
| `bN` | a branch | on arrival, stored with the thread; a rebuild may renumber |
| `599` | a memory message | on arrival |

- After a rewind, `main` follows the line you carry on, and the abandoned line takes the next `bN`.
- When two sessions carry on from one point, `main` stays with the session already on it, and the other session's line takes the next `bN`.
- `bN` goes by arrival, not by time, so handles in use never shift. A fork's transcript reaches the thread only when the fork reports (the fork note), and another machine's older items can merge in later (the id note, "Handles shift").

**Forks are branches.** A fork's branch starts at the turn that launched it and merges back at its report turn. Its turns carry on from the launch: a fork started in turn 78 has turns 79, 80 and so on, on its own branch. It takes the next `bN` on arrival and is shown with its description. The trunk gains nothing, since its report turn is already there, so merging a fork shifts no trunk numbers.

Today:
- Branches have no names. A session reads its own branch, and every other reader gets the newest (`src/chat/cli.rs:432`). The view's branch line says it follows "your session's", "the newest" or "an older one" (`src/chat/view.rs:285`).
- Turn and item ids are positions along the branch (`src/chat/id.rs:5`). Past a split, the same id names a different turn on each branch.
- A fork's items sit apart in `agents/<harness>/<agent>.jsonl`, which only sync reads (`src/chat/store.rs:762`). Its report opens a work turn (`src/chat/claude.rs:383`, `:413`).

## Handles in summary text

**Recommended:** summary text keeps the handles the model wrote (`[12.3]`), and `ai` never rewrites them to ids. Finding citations in free text is unreliable: one written as `(12.3)` or "items 12.3 to 12.5" gets missed, and a quoted `[12.3]` gets rewritten wrongly.

Handles in text stay right because positions don't move during incremental builds:
- new turns go at the end;
- sessions that carry on from one point get separate branches;
- forks get their own branches.

Numbers change only when what counts as a turn changes. That calls for a full rebuild, summaries included.

**Recommended:** one plumbing command prints the id behind a handle, as `git rev-parse` does, for citing from outside the thread.

## Addresses

`.` descends a level and `:` picks a branch. `+n` makes a range on the last part, as ids do today (`src/chat/id.rs:48`).

Relative, inside a chat command:

| Form | Means |
|---|---|
| `7.0` | this thread, this branch |
| `:b3.7.0` | this thread, branch `b3` |
| `ai-chat.7.0` | thread `ai-chat`, its default branch |
| `ai-chat:b3.7.0` | thread `ai-chat`, branch `b3` |
| `ai-tools.ai-chat.7.0` | project `ai-tools`, thread `ai-chat` |

Absolute:
- `chat.<project>.<thread>:<branch>.<turn>.<item>`
- `memory.<n>`: the user's memory
- `memory.<project>.<n>`: a project's memory, reserved and left out of this work

Rules:
- **Recommended:** a relative address fills from the right, `[project.]thread[:branch][.turn[.item]]`. The words before the first number decide: one is a thread, two are a project and a thread. A lone word is a thread.
- A thread without a branch means your session's branch if your session is attached to that thread, and `main` otherwise. That way grep in a session never reads another branch (`ai/spec.md`: "A session sees only its own branch").
- Names contain no `.` or `:`, and a part made only of digits is always a position. Thread names can't be all digits or a store word (`chat`, `memory`, …), so `memory.5` is always a memory message. Under the recommended rule a project can come first, so project slugs can't be store words either.

**`--at`** stays and becomes an opaque token, like the page tokens APIs hand out. A long view comes in parts, and each part ends with the command for the next, including `--at`, so the next part shows the same snapshot while the reader's own tool calls add items. Only the CLI writes and reads it, so it never clashes with addresses.

Today:
- `Id::parse` reads positions only: `t`, `t+n`, `t.i`, `t.i+n` (`src/chat/id.rs:48`). The thread comes from `-t <name>`, else from the spec's thread rule (`ai/spec.md`).
- `check_name` allows `A-Z`, `a-z`, `0-9`, `-` and `_`, starting with a letter or digit (`src/chat/store.rs:40`). So `123` is a valid thread name today.
- Note topics are nested directories that print with dots, as `linguistics.syntax` (`src/notes/topic.rs:60`, `:70`). A topic segment may be all digits (`src/notes/topic.rs:11`).
- The hidden `--at` states use both characters: a thread's state is `items.lines.sessions` (`src/chat/store.rs:127`), and `ai view`'s is `<commit>:<thread>:<state>` (`src/view.rs:56`).

## Notes

Notes take TypeIDs. Topics will be citeable too; topic ids and note addresses are designed with `ai notes`.

## What changes

- Items, threads, notes, memory messages and projects get ids. Turn numbers, `bN` and memory numbers become handles over them.
- Stored records switch from sequence numbers to item ids.
- Threads get branch records: each branch's `bN`, and which branch is `main`. Forks arrive as branches.
- Readers outside a session read `main`, not the newest branch.
- Projects are found by `ai.toml`, which holds their id and slug; `ai.root` leaves `.git`.
- Thread names can no longer be all digits or store words.
- Notes take TypeIDs in place of Denote ids.
- The id parser reads the address forms above.
- `--at` states become opaque tokens.

## Open

1. **pi `/fork`:** does it copy the parent's entries into the new file, or only point at the parent? The item key works either way; whoever builds pi support checks.
2. **Claude forks:** how a fork's transcript stores the conversation it inherited. It affects how sync joins the fork's branch, not the model; whoever builds forks as branches checks.
3. **Repo-level memory:** a project memory of its own, numbered from 0 and maybe shared with collaborators, or the user's memory filtered to one repo.
