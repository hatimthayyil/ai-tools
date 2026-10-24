# tests/config.rs

`Settings::load` on a temporary tree: the user's file beside `outer/inner/cwd`, a project file at `outer`, `inner` or neither. Covered: no file gives every default; the nearest project file wins key by key over the user's, and a farther one is not read; each kind of bad file (unknown key, unknown table, wrong type, empty model, zero, over the note cap) fails with the file's path first and the key in the message, as the user's file and as the project's.

The private settings, `.ai/local/ai.toml`, win key by key over the project's; only the nearest project's count; an agent's instructions add up across the three files, the private settings' relative to the project's root; `[project]` in them fails; an `ai.local.toml` that is a file of its own fails, saying to move it. In a linked git worktree they are read from the main checkout, and `Project::init` there links the main checkout's.
