# config.rs

- Knobs in the memory repo's git config (`ai.memory.wakeLines`, `ai.memory.entryChars`). Per machine. Paging has no knob: one part size, `page::PART`, serves every paged output (page.rs.md). `ENTRY_CHARS`: the note limit, default 280, maximum `NOTE_MAX` = `NODE` (512: a note's leaf is its bare text, so it fits one node). `WAKE_LINES` is gone: the view budget is the spec's constant `VIEW`.
- `ai.memory.origin` lives in the same section but is no knob: store.rs owns it.
- `Config` holds only overrides; unknown `ai.memory.*` keys are ignored.
