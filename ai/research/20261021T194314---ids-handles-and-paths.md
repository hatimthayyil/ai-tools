# Ids, handles and paths

Author: Claude Code (Claude Opus 5.5)

This is a design for review, and nothing in it is implemented. Facts about today's code are from main `def680a0`. The choice of TypeID comes from `ai/research/20261021T194500---durable-id-alternatives.md` (below, "the id note"). How forks reach a thread comes from `ai/research/20261020T231858---fork-transcripts-in-threads.md` ("the fork note").

## Terms

- **Id:** the name the CLI uses for an object. It never changes.
- **Slug:** the readable part of a file or directory name. It can change.
- **Handle:** a short name people and agents read and type (`7.0`, `main`, `b3`, `599`). The CLI resolves it to an id. A handle can change; an id can't.
- **Address:** a handle with its context (`ai-chat:b3.7.0`, `memory.599`).

## Ids

**Minted**, as TypeIDs over UUIDv7 (`<prefix>_<26 base-32 characters>`):
- threads;
- notes;
- memory messages.

**Derived**, so a rebuild gives the same ids:
- **Item:** the UUIDv5 of (harness, session, entry). The session is part of it because pi entry ids are 8 hex digits, unique only within a session (the id note, "Consequences").
- **Branch:** its first item's id.
- **Project:** its repository's root commit hash.

Crates: `uuid` (features `v7` and `v5`) and one TypeID crate, `mti` or `type-safe-id`, chosen after checking their APIs (the id note). Neither is a dependency today (`Cargo.toml`).

Today:
- A thread is known only by its name, as `threads/<name>` (`src/chat/store.rs:32`).
- An item's source is `SourceId { harness, session, entry }` (`src/chat/model.rs:9`). When one Claude entry makes several items, the k-th after the first has entry `<uuid>.<k>` (`src/chat/claude.rs:229`).
- In the store, an item is a sequence number, its position in the thread, and parents are sequence numbers (`src/chat/record.rs:18`).
- The store dedupes items by harness and entry, not session (`src/chat/store.rs:78`). That way a Claude fork's copied entries merge into the items already there (`ai/comments/src/chat/store.rs.md`).
- Memory messages are positions `0, 1, …`, stored at `log/<hi>/<lo>` (`src/memory/store.rs:368`). A memory's `origin` is 6 random Crockford characters (`src/memory/store.rs:166`).
- A note's id is its UTC Denote timestamp, moved a second later past any id already taken (`src/notes/leaf.rs:126`). That's Denote's own method, which the id note rules out for anything written in parallel or on two machines.
- The project id already is the root commit: the oldest root commit reachable from `origin/HEAD`, else from `main`, `master` or `HEAD`, cached as `ai.root` (`src/memory/prov.rs:55`, `:69`). The chat store is `~/.ai/chat/<root commit>` (`src/chat/store.rs:168`, `:194`).

## On disk

An object stored as a file or directory, such as a thread directory or a note file, is named `<id>--<slug>`. Records inside a JSON Lines file, such as items and memory messages, carry their id as a field and have no slug.

- The id never changes; the slug can. Renaming a thread or retitling a note changes only the slug.
- An object is found by its id, whatever its slug says.
- Two files with the same id is an error. So is a name in a store that doesn't parse as `<id>--<slug>`. Both fail at once, and nothing is guessed. Notes already refuse a misnamed leaf (`src/notes/leaf.rs:185`).
- A TypeID suffix has a fixed length and contains no `-`, so the slug starts at the first `--` after the id (the id note).

Files in the repo tree (`ai/research`, …) keep their Denote names until `ai notes` takes them over.

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

Today:
- Branches have no names. A session reads its own branch, and every other reader gets the newest (`src/chat/cli.rs:432`). The view's branch line says it follows "your session's", "the newest" or "an older one" (`src/chat/view.rs:285`).
- Turn and item ids are positions along the branch (`src/chat/id.rs:5`). Past a split, the same id names a different turn on each branch.

## Addresses

`.` descends a level and `:` picks a branch. `+n` makes a range on the last part, as ids do today (`src/chat/id.rs:48`).

Relative, inside a chat command:

| Form | Means |
|---|---|
| `7.0` | this thread, this branch |
| `:b3.7.0` | this thread, branch `b3` |
| `ai-chat.7.0` | thread `ai-chat`, its default branch |
| `ai-chat:b3.7.0` | thread `ai-chat`, branch `b3` |

Absolute:
- `chat.<project>.<thread>:<branch>.<turn>.<item>`
- `memory.<n>`: the user's memory
- `memory.<project>.<n>`: a project's memory

Rules:
- A thread without a branch means your session's branch if your session is attached to that thread, and `main` otherwise. That way grep in a session never reads another branch (`ai/spec.md`: "A session sees only its own branch").
- Names contain no `.` or `:`, and a part made only of digits is always a position.

Today:
- `Id::parse` reads positions only: `t`, `t+n`, `t.i`, `t.i+n` (`src/chat/id.rs:48`). The thread comes from `-t <name>`, else from the spec's thread rule (`ai/spec.md`).
- `check_name` allows `A-Z`, `a-z`, `0-9`, `-` and `_`, starting with a letter or digit (`src/chat/store.rs:40`). So `123` is a valid thread name today, and under the rule above it would read as a position.
- Note topics are nested directories that print with dots, as `linguistics.syntax` (`src/notes/topic.rs:60`, `:70`). A topic segment may be all digits (`src/notes/topic.rs:11`).
- The hidden `--at` states use both characters: a thread's state is `items.lines.sessions` (`src/chat/store.rs:127`), and `ai view`'s is `<commit>:<thread>:<state>` (`src/view.rs:56`).

## What changes

- Items, threads, notes and memory messages get ids. Turn numbers, `bN` and memory numbers become handles over them.
- Threads get branch records: each branch's `bN`, and which branch is `main`.
- Readers outside a session read `main`, not the newest branch.
- Thread names can no longer be all digits.
- Notes take TypeIDs in place of Denote ids.
- The id parser reads the address forms above.

## Open questions

1. **Project or thread?** In `ai-tools.ai-chat.7.0` against `ai-chat.7.0`, a leading word could name a project or a thread.
2. **Copied entries and the session in an item's id.** Items dedupe by (harness, entry) today, so a Claude fork's copied prefix merges. With the session in the id, one of two things happens:
   - The merged item keeps the id from the session seen first. A rebuild that attaches sessions in another order then gives other ids.
   - Each copy gets its own id, and copies stop merging.

   Claude entry uuids are unique across sessions (`ai/comments/src/chat/claude.rs.md`). pi's are 8 hex digits within a session.
3. **A branch's first item after another split.** A branch's id is its first item past its split. A later rewind deeper on that branch splits it again. Which item is then "first" for the line that carries on and for the abandoned one, so that neither id changes?
4. **TypeID prefixes:** the word for each type (thread, note, item, memory message). Since a branch's id is its first item's id, does a branch show it with the item prefix, or the same UUID under its own prefix?
5. **Sequence numbers.** Today these are all sequence numbers: parents, line keys (`level@last`, `src/chat/record.rs:116`), a binding's graft and tip (`src/chat/record.rs:95`), and an agent's call. They depend on the order items arrive, so a rebuild can renumber them. Do stored records switch to item ids?
6. **References in stored text.** Summary lines cite items by handle (`[12.3]`), and those handles shift when turns renumber. Do lines store item ids and print handles when read? Which commands print an object's id, so that a note can cite it?
7. **Forks.** Do forks become branches, merged back by their report, and what are their handles? Today a fork's items sit apart in `agents/<harness>/<agent>.jsonl`, which only sync reads (`src/chat/store.rs:762`). Its report opens a work turn (`src/chat/claude.rs:383`, `:413`).
8. **The project handle.** The project id is a 40-hex root commit. What does `<project>` say in an address: the repository's directory name, or `owner/name` from `origin` (`src/memory/prov.rs:89`), which contains `/`? Does the project directory become `<root>--<slug>`?
9. **Store words as names.** Can a thread or project be called `chat` or `memory`? Then `memory.5` could mean a memory message or turn 5 of thread `memory`.
10. **Repo-level memory.** Today there is one memory per user per machine, and each message records its repo's root. Is `memory.<project>` a store of its own, numbered from 0, or the user's memory filtered by root?
11. **Notes.** Do topic directories get ids too? What is a note's address? This design covers neither.
12. **The separator.** Notes and repo files put `---` between id and slug (`src/notes/leaf.rs:159`), but this design uses `--`. Do notes switch to `--` when they take TypeIDs?
13. **`--at` states** use `.` and `:` with other meanings. Should they change, so the two characters mean only what they mean in addresses?
