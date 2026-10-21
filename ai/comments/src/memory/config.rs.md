# config.rs

- Knobs under `[memory]` in `ai.toml` (config.rs.md): `view-lines`, `entry-chars`. Paging has no knob: one part size, `page::PART`, serves every paged output (page.rs.md). `entry-chars`: the note limit, default 280, maximum `NOTE_MAX` = `NODE` (512: a note's leaf is its bare text, so it fits one node). `view-lines`: how many lines `view` prints, default 96.
- `Config` is the `[memory]` table itself and holds only what a file sets; `over` layers a project's over the user's. Each knob is checked as it is read (`Knob::read`), so a bad value fails with the TOML error's line and column; an unknown key fails too (`deny_unknown_fields`).
- The store's origin is no knob: store.rs owns it.
