# segments.rs

Segmented files: append-only files kept as a directory of fixed-size segments, so a store's git repository (repo.rs.md) commits only the small open segment when a file grows. The agent store keeps every growing file this way (agent/store.rs.md).

- Layout: the file `p` is a directory `p/` holding `000000.<ext>`, `000001.<ext>`, … with `p`'s extension (none for a name without one), numbered from 0 without gaps. The directory keeps the file's own name, so an archived session file keeps its path. Anything else in it, or a gap, is corruption; a whole file where segments belong is an error naming it.
- `CAP`, 1 MiB, a constant: a line goes to the last segment, unless that segment ends a line and the line would take it past `CAP`; then the line starts the next segment. So a segment is cut only at a line's end, a line longer than `CAP` gets a segment to itself, and an unfinished last line keeps its segment open until it ends. Every segment but the last ends a line; reading checks it.
- Closed segments never change, so a commit stores each once and its stat matches the index from then on (repo.rs.md, Stat cache). Each commit after an append stores only the open segment, at most 1 MiB before compression.
- `read_all`: the segments joined, byte for byte; empty when `p` does not exist. `read`: the complete lines of that, as `jsonl::read` gives a whole file's.
- `extend`: raw bytes, kept as given; an archive's unfinished last line is content. A missing file is made with an empty first segment even for no bytes, so an empty source file is still a file of the store, and git tracks it.
- `append`: rows as JSON lines (`jsonl::encode`), first truncating the incomplete line a crash may have left in the last segment, as `jsonl::append` does. The caller holds the file's writer lock.
- `write`: replaces the whole file with new segments, for an archived source that changed other than by growing.
- `is_segmented`: a directory holding a file. In the archive every regular file is a segment, so a directory holding files is a segmented file and one holding directories is a parent; `archive` uses this when a source path changes between file and directory.
- No fsync, as in `jsonl.rs`.
