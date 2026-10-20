# Compactor eval: one model vs another

Each model rebuilds the full summary tree from the same notes; compare the trees.

1. Per model `m`: copy `~/.ai/memory` to `<dir>/m`, then commit on it a tree with `tree/1..` removed (only level-0 leaves kept). Plumbing: `git archive` the ref into a work dir, `rm -rf tree/[1-9]*`, `add -A` with a temp `GIT_INDEX_FILE`, `write-tree`, `commit-tree -p <old>`, `update-ref`.
2. Build: `AI_MEMORY_NAP=0 AI_MEMORY_MODEL=<model id> AI_MEMORY_DIR=<dir>/m ai memory nap` (time it).
3. Extract: `git --git-dir <dir>/m archive refs/ai/memory tree | tar -x -C <dir> --one-top-level=t-m`, then `uv run --no-project python keep.py haiku sonnet` from `<dir>` (copy `keep.py` there): hard-fact keep rate per level.
4. Read `ai memory view` at `VIEW_LINES=16` per copy (`ai memory config VIEW_LINES=16`), and the top node `tree/6/0000/00`.
5. Check faithfulness: grep the summaries for the size reference's content (`scale.txt`: lorem ipsum filler; before 2026-10-08b: export.py, --verbose, write_header, ship tonight) and for `[cut]`/`LIMIT` markers; count lines not ending in punctuation, then read them (a complete clause without a final period is not a cut); list hard facts (keep.py's pattern) absent from a line's source leaves and read each.

2026-10-08 result (83 notes): Sonnet 5.5 207 s, no invented content, 37/79 lines truncated; Haiku 5.5 454 s, more hard facts kept per level (59/36/18% vs 53/28/13% at 4/8/16 notes) but copied scale.txt content into 3 lines, 23/79 truncated. Sonnet stays default.

2026-10-08b result (same 83 notes; retry now asks for a complete rewrite stating the bytes over, `scale.txt` now lorem ipsum filler):

| | Sonnet 5.5 | Haiku 5.5 |
|-|-|-|
| wall | 116 s (was 207) | 456 s (was 454) |
| hard facts kept, 2/4/8/16/32/64 notes | 92/53/26/14/7/7% | 96/52/28/14/4/1% |
| lines not ending in punctuation | 2/79 (was 37), both complete | 6/79 (was 23), all complete |
| over 512 bytes | 5 (513-526) | 0 |
| scale.txt or marker leaks | 0 | 0 (was 3 + 2 `[cut]`) |
| distortions | none found; new hard facts are abbreviations (104933B) | "no API key, OAuth banned" became "no API-key/OAuth ban" (5/0, carried to top); "Stage1 c2b5b31; old memory unreadable" became "Stage1 unreadable" |

Both defects fixed for both models. Haiku no longer copies the example, but still garbles meaning when merging and keeps fewer facts at the top (its top line drops "myai", hashes, timings). Sonnet stays default.

2026-10-08c result (Sonnet 5.5, 89 notes; COMPACT rewritten for notes only, chat kinds and tool wording dropped, zoom findability and "never change meaning: keep negations, who did what, done vs planned" added): 86 lines, hard facts kept 91/55/26/13/8/2% at 2-64 notes (top line 2% vs 7%: it favors user words and decisions over hashes), 2 lines without final punctuation (complete), 4 over 512 (516-525), 0 leaks, no distortions found; "no API key, OAuth banned outside Claude Code" kept intact at every level. Sonnet keeps "note:"/"user:" tags as in its input.
