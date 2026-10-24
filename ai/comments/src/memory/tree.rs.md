# tree.rs

- `Coord`: node `(l, i)` covers `[i·2^l, (i+1)·2^l)`, printed `id+n` (spec §3), a leaf as a bare `id`; `parse` reads both (`id+1` too: same path). `at` validates `n` a power of two, `id % n == 0`, `id + n ≤ T`.
- `free`: a source that fits `NODE` (512) characters is its own node (spec §3 free nodes). `joined` is the free merge (space, see record.rs).
