# Compactor eval: one model vs another

Each model rebuilds the full summary tree from the same notes; compare the trees.

1. Notes: write the newest 2^k leaves of the live memory as `YYYY-MM-DD <text>` lines, read-only (`git --git-dir ~/.ai/memory show refs/ai/memory:tree/0/0000/<nn>`, skipping each file's header line; text is the sixth space-separated field). A power of two makes every level complete.
2. Base store: `AI_MEMORY_SUMMARISE=0 AI_MEMORY_DIR=<dir>/base ai memory init`, then `ai memory import <notes>`. Import keeps each note's text, which is all the summariser reads, and drops its provenance. Copy `<dir>/base` to `<dir>/<m>` per model.
3. Build: per model a run directory whose `ai.toml` sets `[summariser] backend` and `model`; from it, `AI_MEMORY_SUMMARISE=0 AI_MEMORY_DIR=<dir>/<m> ai memory build`. An API model runs under `secretspec run -f <repo>/secretspec.toml --reason "<why>" --`. The build's end line gives time, requests, retries and tokens.
4. Extract: `git --git-dir <dir>/<m> archive refs/ai/memory tree | tar -x -C <dir> --one-top-level=t-<m>`.
5. `uv run --no-project python stats.py <dir>/<m> <dir>/t-<m>`: requests, retries, tokens, cost (from `calls/`; DeepSeek at its list price for the hour of each reply), lines over 512 characters, lines without final punctuation, filler leaks. `keep.py <dir>/t-<m>...`: hard-fact keep rate per level.
6. `judge.py <dir>/t-<x> <dir>/t-<y> <dir>/<out> [--judge opus|deepseek-v4-pro] [--order <dir>/<other>]`: one blind judge call per node either run built with the model. The judge sees both lines, each with its own two sources, under `judge.txt` plus the summariser's `src/memory/prompts/compact.txt`, in random order, or with `--order` in the order another judge's verdicts used. `opus` is Opus 5.5 through `claude -p` (no tools, high effort); `deepseek-v4-pro` calls DeepSeek's API (thinking on, high effort) under `secretspec run`, with `SSL_CERT_FILE=/etc/ssl/certs/ca-certificates.crt` for uv's Python. Verdicts go in `<out>/verdicts/` with each call's tokens and cost, and a rerun reuses them; the first failed call stops the run. Tallies go in `<out>/tally.json`, 20 pairs for a human in `<out>/spot-check.md`. Verdicts quote notes: keep them out of the repo.
7. `compare.py <dir>/<out> <dir>/<other>`: two judges on one comparison side by side, on the nodes both judged, how often they preferred the same line, each one's position bias and cost.

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

2026-10-26 result (newest 256 notes, prompts of 0.16.0, `[summariser]` effort medium; Sonnet 5.5 and Opus 5.5 through `claude -p`, DeepSeek Flash 1017 `deepseek/deepseek-flash` through `backend = "api"` with thinking off; each built the same 177 lines: 50 at level 1, 127 above, 78 level-1 lines joined without a call):

| | Sonnet 5.5 | DeepSeek Flash 1017 | Opus 5.5 |
|-|-|-|-|
| wall, 8 calls at once | 113 s | 71 s | 297 s |
| requests (retries) | 199 (22) | 320 (143) | 179 (2) |
| lines needing a retry | 22/177 | 127/177, up to 4 requests | 2/177 |
| first tries over 512 characters | median 537, max 733 | median 655, max 964 | 531, 581 |
| final lines over 512 / unpunctuated / filler leaks | 0 / 0 / 0 | 0 / 0 / 0 | 0 / 0 / 0 |
| tokens: uncached, cache write, cache read, output (thinking) | 398, 206,795, 162,749, 47,928 (12,791) | 98,407, 0, 222,208, 44,361 (0) | 362, 192,714, 131,622, 191,086 (159,921) |
| cost | $1.01 API list price, billed to the subscription | $0.042, all off-peak ($0.084 at peak) | $4.81 API list price, billed to the subscription |
| hard facts kept, 2/4/8/16/32/64/128/256 notes | 86/38/19/8/3/1/1/0% | 87/35/18/8/5/3/1/0% | 85/36/17/7/4/1/2/1% |

Judges, both blind on the same 177 nodes in the same A/B order: Opus 5.5 (`claude -p`, high effort) and DeepSeek V4 Pro (`deepseek-v4-pro`, DeepSeek-V4-Pro-0813, thinking on at high effort). DeepSeek's docs name no separate reasoning model: both its models think by default, and V4 Pro is the larger and dearer. Nodes with at least one finding:

| Sonnet vs DeepSeek Flash | Sonnet, Opus judge | Sonnet, V4 Pro judge | Flash, Opus judge | Flash, V4 Pro judge |
|-|-|-|-|-|
| unsupported claims | 6 | 18 | 37 | 40 |
| contradictions | 5 | 6 | 22 | 11 |
| meaning changed | 74 | 54 | 140 | 92 |
| dropped negations or rules | 89 | 102 | 107 | 106 |
| facts lost while keeping lower-ranked content | 164 | 130 | 171 | 130 |
| any of the first four | 115 | 122 | 160 | 140 |
| judged better | 130 | 110 | 45 | 65 |

The judges preferred the same line on 132 of 177 nodes (75%); each called 2 the same. Position: Opus judge preferred A 100 times and B 75, V4 Pro 101 and 74. Judge cost: Opus $7.47 at API list price (subscription), V4 Pro $3.27, all off-peak. Each judge shares a maker with one of the two runs.

| Sonnet vs Opus | Sonnet, Opus judge | Sonnet, V4 Pro judge | Opus, Opus judge | Opus, V4 Pro judge |
|-|-|-|-|-|
| unsupported claims | 9 | 13 | 11 | 18 |
| contradictions | 7 | 5 | 0 | 7 |
| meaning changed | 79 | 52 | 96 | 78 |
| dropped negations or rules | 88 | 102 | 84 | 105 |
| facts lost while keeping lower-ranked content | 161 | 137 | 148 | 121 |
| any of the first four | 121 | 128 | 131 | 134 |
| judged better | 54 | 88 | 120 | 87 |

The judges preferred the same line on 129 of 177 nodes (73%). Ties: Opus judge 3, V4 Pro 2. Position: Opus judge preferred A 83 times and B 91, V4 Pro 96 and 79. Judge cost: Opus $7.18 at API list price (subscription), V4 Pro $3.37, all off-peak, 128 requests at once, 294 s. The Opus judge judged Opus's own output here.

Level 1 alone, where both lines share the same two notes (50 nodes), Opus judge: Sonnet vs Flash, unsupported 4 vs 16, contradictions 1 vs 4, meaning changed 13 vs 33, dropped negations or rules 9 vs 11, judged better 31 vs 19; Sonnet vs Opus, unsupported 3 vs 4, contradictions 1 vs 0, meaning changed 15 vs 23, dropped negations or rules 10 vs 5, judged better 13 vs 34. DeepSeek Flash faults read by hand: it named a project "deepseek-rs" that no note names and wrote "memlab" for "molab" (100+2); it called the new comment rule the "prior" one (104+2); its top line lists "claude-in-chrome" among the user's rules, from a note saying the user wants it removed.

Raw data (notes input, the three stores with their trees, `calls/` and logs, every judge folder with prompts and verdicts, spot-check files, stats) is in `.ai/local/evals/compactor/20261026T220841--sonnet-deepseek-opus/`, the private `.ai/local` repo, not in this tree.

2026-10-27 result (same 256 notes and prompts; DeepSeek Flash 1017 through `backend = "api"`, thinking on, 64 calls at once; judged against the 2026-10-26 Sonnet 5.5 tree):

| | Sonnet 5.5 (10-26) | Flash, effort medium | Flash, effort max | Flash medium + Flash medium witness |
|-|-|-|-|-|
| wall | 113 s (8 at once) | 447 s | 948 s, stopped, top line unbuilt | 1,280 s |
| lines | 177 | 177 | 176 | 177 |
| summariser requests (retries) | 199 (22) | 187 (10) | 206 (29) | 316 (139, of them 125 answers to the witness) |
| witness requests | – | – | – | 302 (125 rechecks) |
| final lines over 512 / unpunctuated / filler leaks | 0 / 0 / 0 | 0 / 0 / 0, longest 505 | 0 / 0 / 0, longest 511 | 0 / 0 / 0, longest 511 |
| tokens: uncached, cache read, output (thinking) | 398, 162,749, 47,928 (12,791) | 106,014, 67,584, 665,030 (643,355) | 112,306, 82,688, 1,999,823 (1,975,042) | summariser 142,370, 206,848, 1,009,767 (973,281); witness 115,789, 139,904, 1,128,419 (1,118,927) |
| cost at list price | $1.01 (subscription) | $0.83 | $2.43 | $1.26 + $1.39 witness |
| hard facts kept, 2/4/…/256 notes | 86/38/19/8/3/1/1/0% | 85/40/21/10/5/3/3/1% | 84/40/18/8/3/1/1/–% | 86/37/19/9/5/1/1/1% |

Witness rounds per line: 0 for 85 lines, 1 for 72, 2 for 13, 3 for 3, 4 for 3, 6 for 1; 193 findings, 3 contested. Every line resolved.

Judges as on 10-26, each run against Sonnet in one random order shared by both judges. Neither judge finished: the V4 Pro judge stopped when DeepSeek cut the account to 32 requests at once on a low balance, the Opus judge on the Claude monthly spend limit. Counts are on the nodes both judged.

| Sonnet vs Flash medium (172 nodes) | Sonnet, Opus judge | Sonnet, V4 Pro judge | Flash, Opus judge | Flash, V4 Pro judge |
|-|-|-|-|-|
| unsupported claims | 8 | 7 | 9 | 15 |
| contradictions | 5 | 5 | 12 | 5 |
| meaning changed | 79 | 61 | 115 | 71 |
| dropped negations or rules | 89 | 104 | 86 | 94 |
| facts lost while keeping lower-ranked content | 156 | 132 | 159 | 123 |
| any of the first four | 121 | 121 | 134 | 123 |
| judged better | 95 | 79 | 75 | 91 |

Same pick on 107 of 172 (62%). Position: Opus judge A 82, B 88, same 2; V4 Pro A 99, B 71, same 2. Judge cost: Opus $7.53 for 172 verdicts (subscription), V4 Pro $6.71 for 175.

| Sonnet vs Flash max (151 nodes) | Sonnet, Opus judge | Sonnet, V4 Pro judge | Flash, Opus judge | Flash, V4 Pro judge |
|-|-|-|-|-|
| unsupported claims | 7 | 10 | 7 | 22 |
| contradictions | 6 | 6 | 9 | 5 |
| meaning changed | 58 | 53 | 103 | 50 |
| dropped negations or rules | 65 | 91 | 51 | 63 |
| facts lost while keeping lower-ranked content | 140 | 110 | 139 | 76 |
| any of the first four | 91 | 112 | 112 | 88 |
| judged better | 71 | 58 | 77 | 92 |

Same pick on 117 of 151 (77%). Position: Opus judge A 78, B 70, same 3; V4 Pro A 92, B 58, same 1. Judge cost: Opus $5.96, V4 Pro $5.54.

Sonnet vs Flash with witness, Opus judge only, 32 of 177 nodes (V4 Pro judged none): unsupported 2 vs 1, contradictions 1 vs 1, meaning changed 11 vs 20, dropped negations or rules 3 vs 8, facts lost 25 vs 24, any of the first four 12 vs 24, judged better 18 vs 12 (2 the same). Position A 16, B 14. Judge cost $0.95.

DeepSeek spend over both days at list price: about $25, of it $18.89 on V4 Pro judging and $5.95 on Flash summarising (with thinking on, thinking tokens are 96–99% of Flash's output).

Raw data is in `.ai/local/evals/compactor/20261027T063039--deepseek-flash-thinking-witness/`, the private `.ai/local` repo.
