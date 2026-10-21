# record.rs

Record encodings shared by the git stores (memory, chat). Design: `ai/research/20261013T224252--record-versioning.md`, Named fields.

- Named headers (`encode`/`decode`): git's commit object layout. `key value` lines, then a blank line, then the body verbatim. Empty values are left out, so an optional field costs nothing when unset and absent means empty. A newline inside a value becomes a space, so no value can break the header. A record with no fields is a single blank line before the body. Each store applies its own rules on top: required keys, repeated keys, value encoding (memory turns whitespace into `_`).
- `Seg`: records as lines under a column row. Readers decode against the segment's own row, so a column added later by an ai of the same format is kept when this one rewrites the segment (written empty: `line` asks only for the columns it knows). Only the last column may hold spaces (`splitn`). An absent segment is an empty one under the caller's columns. Each store checks its own row (memory: ends in `text`, holds `ts`).
