# jsonl.rs

Append-only JSON Lines files, shared by the stores.

- `read`: the complete lines only: bytes after the last newline are a write in progress or one a crash cut short.
- `append`: one write of all rows, each serialised by serde_json (never multiline) plus a newline. The caller holds the file's writer lock; before appending, an incomplete last line is truncated so the new rows start on a line of their own.
- `decode_all`: errors name the line from 1.
