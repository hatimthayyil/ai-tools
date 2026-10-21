# Durable ids for objects `ai` creates

Question: are TypeIDs (UUIDv7) the right durable ids, or does an established alternative fit better?

Requirements:
- An id never changes; a slug can. On disk: `<id>--<slug>`.
- Ordered by time; unique without coordination across machines and parallel processes.
- Derived objects (items, branches) take their ids from the source, so a rebuild gives the same ids.
- People see handles (`main`, `bN`, `7.0`, `599`); the CLI resolves them to ids.

Verdict: **TypeID, built on UUIDv7.** It is the only candidate that meets every requirement and is also a standard (RFC 9562 underneath). ULID is the runner-up: it has the same text form and ordering, but it isn't a UUID and has no type prefix.

## Candidates

Crate versions and dates are from the crates.io API, checked 2026-10-21.

| Scheme | Ordered | Collision odds | Text length | Short forms | Rust crate |
|---|---|---|---|---|---|
| UUIDv7 | ms; monotonic within one process | 74 bits after the timestamp | 36 hex with hyphens | poor: time first | `uuid` 1.28.0 (2026-10-10), feature `v7` |
| TypeID | as UUIDv7 | as UUIDv7 | prefix + `_` + 26 | poor: time first | `mti` 1.1.2 (2026-10-19), `type-safe-id` 0.3.3 (2025-07-02), `strong_id` 0.4.0 (2024-05, stale) |
| ULID | ms; monotonic `Generator` | 80 random bits per ms | 26 | poor: time first | `ulid` 3.0.0 (2026-07-16) |
| KSUID | seconds | 128 random bits | 27, base62, case-sensitive | poor: time first | `svix-ksuid` 0.10.0 (2026-05-19); `ksuid` 0.2.0 (2017, dead) |
| Snowflake | ms | none if worker ids are unique; needs them assigned | 19 digits | poor | `snowflaked` 1.0.4 (2026-10-07), `sonyflake` 0.5.1 (2026-06-12), `rs-snowflake` 0.6.0 (2022, stale) |
| cuid2 | no, by design | ~124 bits | 24, base36 | good: random | `cuid2` 0.1.6 (2026-04-19) |
| NanoID | no | 126 bits | 21; alphabet has `-` and `_` | good: random | `nanoid` 0.5.0 (2026-04-13) |
| Content hash (git) | no | 160-bit SHA-1; same content, same id | 40 hex (64 for SHA-256) | good: git sizes `core.abbrev` to stay unique | `gix` (already a dependency) |
| jj change id | no | 128 random bits | 32 reverse-hex letters (`k`–`z`) | very good: shortest unique prefix | none standalone; random bytes (`getrandom`, already a dependency) |
| Denote + suffix | seconds | 5 bits per suffix character within one second | 15 + suffix | poor: time first | none maintained (`denote` 1.0.1, 2022, 9 recent downloads); hand-rolled on `jiff` |

Also seen: `xid` 1.1.1 (2024-04; 20 characters, seconds, hashed machine id and pid) and `scru128` 4.2.0 (2026-05; 25 characters, base36). Neither beats ULID or TypeID here.

### UUIDv7 and TypeID

- **UUIDv7**, RFC 9562: a 48-bit Unix-ms timestamp, a version and variant, then 74 bits of randomness or counter. `uuid::Uuid::now_v7()` is ordered within a process. `ContextV7` reseeds a 42-bit counter with 41 random bits each millisecond, which leaves 32 random bits. Two processes in the same millisecond still differ in about 73 random bits. Clock skew between machines can misorder ids, but it can't make two the same.
- **TypeID** (spec v0.3.0): `prefix_` plus the UUID in 26 lowercase Crockford base-32 characters. The prefix is `[a-z_]`, up to 63 characters, must start and end with a letter, and should be at least 3 characters long. The suffix is the ULID text form of the same 128 bits. It sorts by time as plain text, can't mix case, and can be selected with a double-click.
- **Other UUID versions:** generated TypeIDs must be v7, but the spec says implementations SHOULD encode other UUID versions supplied by the user. `mti` documents versions 1, 3, 4, 5, 6 and 7; `type-safe-id` wasn't checked for v5.
- **A name clash:** the `typeid` crate on crates.io is dtolnay's type-identity crate, not TypeID.
- **Filenames:** `thread_01k8gq3w9xf2v6r4t8m0n2p5ya--ai-chat` is 33 + 2 + slug. The fixed-length suffix makes parsing exact: the slug starts at the first `--` after the id. `ls` sorts by time within one type.

### Why the others lose

- **ULID** meets the requirements just as well; the TypeID suffix is the same text. It loses two things: tools and databases recognise UUIDs, and the type prefix names what an id refers to wherever it's pasted.
- **KSUID** is base62, so two ids can differ only in case, and they collide on case-insensitive filesystems (macOS APFS, Windows by default). Its timestamps are in seconds, and `ksuid` is abandoned.
- **Snowflake** gets uniqueness from assigned worker ids, which is coordination. With random 10-bit worker ids, two machines share one with odds of 1 in 1024.
- **cuid2 and NanoID** are random. cuid2 is deliberately not sortable. NanoID's `-` can produce `--` inside an id, which breaks splitting `<id>--<slug>`.
- **Content hashes** suit immutable things, such as saved session files and summariser call records. A thread, note or memory store changes, so a hash id would change with it.
- **jj change ids** are random so that short prefixes work. They give up time order, so files would no longer list in creation order.
- **Denote + suffix** is a scheme of our own. Denote's ids are unique only because it checks the existing ids in `denote-directory` and adds a second on a collision, which falsifies the time. Its manual concedes duplicates when files from different directories are merged, and it ships no fix. Local time also repeats an hour when the clocks go back. A random suffix fixes the odds, but it is still home-made.

## How established tools pair a durable id with a name

- **git:** objects are named by content hash and never change; branches and tags are mutable names (refs) pointing at them. File renames aren't recorded: git guesses them from content similarity. `<id>--<slug>` gives exact rename detection where git has to guess.
- **jj:** a change id stays fixed while the commit id changes on every rewrite. Bookmarks are mutable names. `jj log` highlights each id's shortest unique prefix, computed within `revsets.short-prefixes` (by default `revsets.log`), so recent work needs one or two letters. A change with two visible commits is "divergent", and `xyz/0` picks one by offset.
- **Zotero:** an item key is 8 random characters from `23456789ABCDEFGHIJKLMNPQRSTUVWXYZ` (about 40 bits), unique within a library because the library checks. Attachments live in `storage/<KEY>/`, and titles and citation keys are names that can change. This works because the server coordinates.
- **Obsidian:** identity is the file path. Renaming in Obsidian rewrites links when "Automatically update internal links" is on. Renames made outside Obsidian, or while the setting was off, break links, and turning it on later doesn't repair them. This is the cost of using the path as the identity.
- **Linear:** an issue has an internal UUID and a display identifier `ENG-123` (team key + number). Moving an issue to another team gives it a new identifier, and old URLs and ids redirect to it. An integration that stored the display key broke on moves until it stored the internal id too (Trunk, 2026-04). This is the same split as handle versus id.

The pattern throughout: a durable id that only the program uses, with names and handles over it that can change.

## Consequences for `ai`

- **Minted ids:** TypeIDs for what `ai` creates: threads, notes, memory messages. Memory message numbers become handles over them.
- **Derived ids are deterministic.** An item's source key is (harness, session, entry), not the entry alone: Claude forks reuse entry uuids, and pi entry ids are 8 hex digits, unique only within a session. For one shape everywhere, an item id can be the UUIDv5 of that key, encoded as a TypeID (`uuid` feature `v5`; TypeID allows other versions). A branch's id is its first item's id.
- **Short forms:** time-first ids make poor prefixes, since everything made on one day starts the same way. Handles fill that role, as `ENG-123` does in Linear. If an id ever needs shortening, its last characters are random: six characters carry 30 random bits.
- **Handles shift.** A handle that is a position in time order moves when another machine's older items are merged in. If handles must never change, number them in arrival order, not in id order.
- **Crates:** `uuid` (features `v7`, `v5`) plus one TypeID crate. `mti` is actively maintained and documents every UUID version. `type-safe-id` gives each id type its own Rust type, but its last release was 15 months ago and v5 support is unverified. Check both crates' APIs before choosing.

## Sources

- TypeID README and spec v0.3.0: <https://github.com/jetify-com/typeid>, <https://github.com/jetify-com/typeid/tree/main/spec>
- `uuid` `now_v7`, `ContextV7`: <https://docs.rs/uuid/1.28.0/uuid/struct.Uuid.html>, <https://docs.rs/uuid/1.28.0/uuid/timestamp/context/struct.ContextV7.html>
- `mti`: <https://docs.rs/mti/1.1.2/mti/>; `type-safe-id`: <https://docs.rs/type-safe-id/0.3.3/type_safe_id/>; `ulid` `Generator`: <https://docs.rs/ulid/3.0.0/ulid/struct.Generator.html>
- Jujutsu glossary and config: <https://github.com/jj-vcs/jj/blob/main/docs/glossary.md>, <https://github.com/jj-vcs/jj/blob/main/docs/config.md>
- Denote manual and `denote.el` 4.2.3 (`denote--find-first-unused-id-as-date`): <https://github.com/protesilaos/denote>
- Zotero `generateObjectKey`: <https://github.com/zotero/utilities/blob/master/utilities.js>
- Obsidian internal links: <https://help.obsidian.md/links>
- Linear moving issues: <https://linear.app/docs/move-to-new-team>, <https://linear.app/docs/editing-issues.md>; Trunk: <https://docs.trunk.io/changelog/2026-04-15-flaky-tests-linked-tickets-survive-moves>
