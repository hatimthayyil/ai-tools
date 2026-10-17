# chat/record.rs

Thread format 2 uses serde JSON. The shared memory codec in `src/record.rs` is independent.

- Index segments are arrays of named `Head` records. Sequence numbers and parents are validated against segment positions. Source ids keep their exact whitespace and text.
- Item kinds use adjacent tags: `kind` names the snake-case variant and `content` holds its text or structured fields. Tool input stays a JSON value.
- Bindings and agent records include their harness/session or harness/agent identity; reads verify that identity against the containing Git path.
- Summary lines include timestamp, kind, author fields and text. Chat uses serde's remote derive for `memory::Who`, without changing memory's encoding.
- jiff's serde feature preserves timestamps; JSON preserves multiline metadata and content. Missing required fields and unknown variants fail decoding.
- `Key` prints `level@last`. `check_id` validates ids used as Git path components.
