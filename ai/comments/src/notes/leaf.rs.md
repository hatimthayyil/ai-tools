# leaf.rs

- `Front`: frontmatter as ordered `key: value` lines, kept verbatim, unknown keys included, so a parse-render round trip changes nothing. No YAML dependency: values are single lines, lists are `[a, b]`. Layer 1 writes `source`, `confidence`, `checked`, `decay`, `links`; it reads `invalid_at` (validity) and `links`; `supersedes` is only preserved (layer 2).
- Ids: Denote `YYYYMMDDThhmmss` in UTC (permanent ids must not depend on the machine's zone), bumped a second past any id in the store; taken under the write lock, so parallel adds get distinct ids. The id lives only in the file name.
- Title: the claim in a few words (`--title`, required, one line, ≤ `TITLE_MAX` = 80 characters), stored once as the note's first line `# <title>` after the frontmatter and parsed back from there. No `title` field: one source. The body cap counts the body without the title. A file without a `# ` first line parses with an empty title; `label()` then falls back to the body's first line.
- Slug: the title's ASCII words, at most 48 bytes, else `note`. `name` is the file name on disk, `file_name()` the one the title gives; they differ only after a hand edit of the title. Every lookup is by id, so a stale slug breaks nothing, and every write of an existing note (`Store::put_leaf`, from `link`; `mv`'s rename) puts it under the title's name, keeping the id.
- `links()`: frontmatter `links` plus every `[[id]]` in the body (hub notes), once each.
- `LEAF_MAX` = 1024 characters of body, a constant: memory's `entry-chars` knob is a setting tied to the view's line size; notes have no view line to fit.
- `Confidence`: the four design values, required on `add`.
