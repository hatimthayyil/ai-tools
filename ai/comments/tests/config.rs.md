# tests/config.rs

`Settings::load` on a temporary tree: the user's file beside `outer/inner/cwd`, a project file at `outer`, `inner` or neither. Covered: no file gives every default; the nearest project file wins key by key over the user's, and a farther one is not read; each kind of bad file (unknown key, unknown table, wrong type, empty model, zero, over the note cap) fails with the file's path first and the key in the message, as the user's file and as the project's.
