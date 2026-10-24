# files/store.rs

The summaries of `ai files`, keyed by content hash, so an unchanged file or directory is never summarised twice.

- Keys are the hashes the tree walk puts on its nodes: a file's content, a directory's children. Nothing is keyed by path, so a renamed or copied file, or a directory holding the same children, finds its summary under the same key.
- The caller passes the directory (`.ai/files/` in the project's main checkout, from the CLI). `Store::open` checks its format and writes nothing.

Layout (format 1)
- `format`: `1`, written before the first write (`file::ensure_format`); `Store::open` refuses another format (`file::check_format`), as the agent store does.
- `summaries.jsonl`: one `Summary` per line (`key`, `ts`, `who`, `text`), appended. `who` is `memory::Who`'s serde derive, as chat lines store it.
- `summaries.lock`: the writers' lock.

Writing
- `put` holds the lock, reads the file again under it and appends only if the key is not stored: the first write per key wins and the stored summary comes back, so concurrent builds agree on one text per key.
- Append-only, as chat: a crash can leave an incomplete last line, which readers skip and the next writer truncates (`jsonl::append`). No fsync: a lost summary is built again.
- Rows for content no longer in the tree stay: a key is the content, and the same content may come back.

Reading
- `load` decodes every row into a map, first row per key; a corrupt row is an error naming the file and the line. A build loads once and looks up each node's key.
