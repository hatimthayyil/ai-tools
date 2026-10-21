# Simulation: `ai files` summary sizes and costs

Author: Claude Code (Claude Opus 5.5)

Sizes and costs of the proposed `ai files` rules on this repo (`ba7e120f`), with no model calls. Script: [simulate.py](20261021T234637--file-summary-simulation/simulate.py), run with `uv run --no-project <script> <repo>`.

## Rules simulated

- **Files:** git-tracked text files, so `.gitignore` is honoured. Cap = max(512 B, size ÷ 10).
- **Directories:** children's caps total S over n children (files and subdirectories). Cap T = max(512 B, S ÷ n).
- **Iterated:** passes in one session, each halving the previous output, the last landing on T. A directory with one child takes one pass.
- **One-step:** one call from S to T.
- **Worst case:** every summary is exactly its cap. The 1.25× accept margin raises every figure by up to a quarter.
- **Input:** each pass adds a 200 B instruction. The fixed prompt is left out; it costs one prompt per call, mostly cached.
- **Cache:** pass k reads everything up to pass k−1's request from cache. The draft pass k−1 wrote is new to the cache, so it counts as fresh input with the new instruction.
- **Edits:** redo the file and every directory above it, cold.

## Tree

211 text files, 1,980.5 KB; 40 directories. 111 files are under 5 KB, so their cap is the 512 B floor; 21 are under 512 B, so their worst-case summary is longer than the file.

| depth | dirs | mean children | mean S KB | mean T KB | max T KB | passes |
|---|---|---|---|---|---|---|
| 0 | 1 | 18.0 | 16.4 | 0.9 | 0.9 | 5 |
| 1 | 5 | 5.4 | 7.4 | 1.2 | 3.2 | 1–4 |
| 2 | 8 | 11.9 | 14.8 | 0.9 | 1.8 | 1–6 |
| 3 | 17 | 3.7 | 4.1 | 0.9 | 3.9 | 1–4 |
| 4 | 5 | 8.2 | 4.5 | 0.6 | 0.8 | 1–5 |
| 5 | 4 | 1.5 | 0.8 | 0.6 | 0.8 | 1 |

- **Passes per directory:** 1: 21, 2: 2, 3: 7, 4: 6, 5: 3, 6: 1.
- **Root:** 18 children, S 16.4 KB, T 0.9 KB, 5 passes. The whole repo ends in under 1 KB, about 2,200:1.
- **Largest directory input:** `ai/research`, 40 children, S 59.5 KB, 6 passes.
- **Every level averages about 0.9 KB.** S ÷ n makes a directory the size of an average child, and most children sit at the 512 B floor.

## Full build, KB

File calls are the same under both rules.

| | calls | written | fresh input | cached input |
|---|---|---|---|---|
| files | 211 | 232.4 | 2,021.8 | 0 |
| dirs, iterated | 91 | 248.7 | 498.0 | 1,241.9 |
| dirs, one-step | 40 | 34.9 | 274.2 | 0 |

- **Directories, iterated vs one-step:** 7.1× written, 1.8× fresh input, plus 1.2 MB cached reads.
- **Whole build:** iterated writes 481 KB, one-step 267 KB (1.8×). Iterated directories write more than all file summaries together.

## One-file edit, KB

The file, then every directory above it.

| file | size | mode | written | fresh input | cached input |
|---|---|---|---|---|---|
| `tests/fixtures/claude/interject-inline.jsonl` (median edit cost) | 1.8 | iterated | 43.6 | 88.6 | 185.9 |
| | | one-step | 6.0 | 49.3 | 0 |
| `ai/research/20261008T074205--optchat-cache-revision-review/upstream-gist-api.json` (largest) | 141.0 | iterated | 121.3 | 356.5 | 676.5 |
| | | one-step | 21.2 | 253.9 | 0 |

- **Typical edit:** iterated writes 7.3× as much as one-step, almost all in the directories above the file. One-step is dominated by reading the file and each directory's children.
- **Largest file:** a 141 KB JSON capture, whose own summary is 14.1 KB. Its edit reruns `ai/research`'s six passes. It is a candidate for an `ai.toml` ignore.

## Reading

- Iterated passes cost output, not input. They write about S per directory where one-step writes T. Caching keeps the extra input cheap: 1.2 MB of the 1.7 MB directory input is cached reads.
- Output grows with fan-out. `ai/research` (40 children) dominates every edit beneath it.
- The ÷n rule keeps the root small even for this shallow repo (depth 5, 18 entries at the top).
