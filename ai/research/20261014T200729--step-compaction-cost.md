# Step compaction cost on the user's Claude Code logs

Question: what does the two-tier scheme (LLM step leaves, inner tree per exchange, outer tree over exchanges) cost against exchange-digest leaves and OptChat's per-message leaves, measured on the user's logs?

Counts differ from `20261014T002032--chat-rollup-prior-art.md`. This note:

- dedups records across files;
- counts queued prompts as exchange openers;
- folds injected entries into steps.

## Method

Script: `ai/research/20261014T200729--step-compaction-cost/stepcost.py`, run as `uv run --no-project python -I stepcost.py main|sub`. Read-only; prints counts and byte sizes only.

- **Corpus.**
  - Main: `~/.claude/projects/*/*.jsonl` with entrypoint `cli`: 179 sessions with at least one prompt. SDK probe sessions are excluded.
  - Subagents: `*/*/subagents/*.jsonl`, 130 files, reported separately.
  - Records are deduped by `uuid` across files, because forks and resumes copy history. Files are processed in start order.
- **Exchange opener.** A user record that is:
  - not a tool_result, not `isMeta`, not a compact summary;
  - `origin.kind = human`, or has no `origin` and is not command output, a caveat, an interrupt marker or `/compact`.

  `queued_command` attachments with `commandMode = prompt` also open exchanges: these are 81 interjections delivered mid-run. Slash commands open exchanges too, so 163 exchanges have no steps.
- **Injected entries** ride in the current step's run like tool results: task notifications, peer messages, skill bodies, command output. Main has 642 of them, 1.2 MB in all.
- **Other attachments are excluded:** @-file contents 2.3 MB, edited-file diffs 0.6 MB, hook output 0.4 MB, and harness listings and snapshots.
- **Step.** Optional assistant text, then every tool call, result and injected entry up to the next assistant text. Thinking and API-error messages are dropped. The prompt heads the exchange's first step.
- **Rendering.** Text, plus for each call: name, compact JSON input and output. Each input and output is clipped head+tail to C chars (30k, 8k or 2k), with a few bytes of labels.
- **Cost model.**
  - A source ≤ 512 B is a free node. Otherwise it costs one call whose input is the source.
  - Inputs over 100 KB are chunked: ⌈n/100 KB⌉ calls plus merges.
  - A merge is free if the concatenation fits in 512 B; otherwise it is one call over ≤ 1 KB.
  - Called nodes are taken as exactly 512 B.
- **(a) Two-tier.**
  - One leaf per step.
  - The inner tree reduces the steps to one exchange node: k−1 merges.
  - The outer tree is aligned binary over exchanges, per session, building complete nodes only (E − popcount E).
- **(a′) Flat exchange node.** As (a), but the exchange node is one call over all its step lines. The largest exchange has 54 steps, so the input is at most 28 KB.
- **(b) Exchange digest.**
  - Prompt and assistant text verbatim.
  - One line per call: name, salient argument ≤ 200 B, 24 B outcome, first error line.
  - Noise tools dropped; repeats folded.
  - Report tools (`SubagentHandback`, `SendMessage`, `AskUserQuestion`, `ExitPlanMode`) keep their full input.
  - Packed into leaves of ≤ 8 KB at step boundaries, with an aligned tree over the leaves.
- **(c) OptChat.**
  - One leaf per message: user, text, tool call, tool result clipped at 30k, injected entry.
  - Messages over 100 KB are split.
  - Aligned tree over the leaves.
- **Input bytes exclude the prompt template.** `compact.txt` is 2,066 B, so add about 2 KB per call, cacheable. Output is ≤ 512 B per call.

## 1. Steps

| | main | subagents |
|---|---|---|
| exchanges (with ≥1 step) | 1,399 (1,236) | 130 (130) |
| steps | 4,104 | 1,145 |
| steps/exchange p50 / p90 / p99 / max | 2 / 6 / 21 / 54 | 5 / 20 / 57 / 59 |
| tool calls/step p50 / p90 / p99 / max | 1 / 4 / 12 / 30 | 2 / 15 / 54 / 99 |
| text-only steps (chars p50) | 844 (1,597) | 48 (2,620) |
| prompts ≤ 512 B | 1,314 / 1,398 (p50 87 B) | 0 / 130 (p50 3.0 KB) |
| free steps (≤ 512 B, any clip) | **376 (9.2%)** | **72 (6.3%)** |

Bytes per step, p50 / p90 / p99 / max (total):

| clip per output | main | subagents |
|---|---|---|
| 30k | 2.6K / 12.1K / 48.1K / 193K (23.0 MB) | 6.0K / 54.2K / 197K / 385K (23.1 MB) |
| 8k | 2.6K / 10.8K / 36.3K / 92.7K (19.5 MB) | 6.0K / 40.9K / 137K / 232K (18.4 MB) |
| 2k | 2.4K / 6.7K / 17.5K / 40.9K (13.4 MB) | 3.7K / 22.4K / 87.3K / 168K (10.9 MB) |

Main steps by size at the 30k clip:

| ≤ 512 | 0.5–1K | 1–2K | 2–8K | 8–30K | 30–100K | > 100K |
|---|---|---|---|---|---|---|
| 376 | 411 | 917 | 1,729 | 548 | 117 | 6 |

Main bytes:

| Part | Size |
|---|---|
| tool output | 14.0 MB |
| tool input | 5.2 MB |
| assistant text | 2.7 MB |
| injected | 1.2 MB |
| user | 0.26 MB |

Subagents are almost all tool traffic: 17.4 MB output, 5.7 MB input, 0.2 MB text.

## 2. Steps over a 100 KB compactor input

| clip | main | subagents |
|---|---|---|
| 30k | 6 (0.15%), 0.8 MB | 57 (5.0%), 9.2 MB |
| 8k | 0 | 41 (3.6%), 5.5 MB |
| 2k | 0 | 7 (0.6%), 0.8 MB |

These are long silent tool streaks: at 30k the oversized steps hold a median of 7 calls in main (max 28) and 42 in subagents (max 99).

## 3. LLM calls and compactor input

Main: 179 sessions, 1,399 exchanges.

| scheme | calls | /exchange | /session p50 / p90 / max | input |
|---|---|---|---|---|
| (a) two-tier, clip 30k | 7,731 | 5.5 | 17 / 96 / 1,421 | 27.0 MB |
| (a) clip 8k | 7,719 | 5.5 | 17 / 96 / 1,421 | 23.5 MB |
| (a) clip 2k | 7,719 | 5.5 | 17 / 96 / 1,421 | 17.5 MB |
| (a′) flat exchange node, 30k | 5,723 | 4.1 | 14 / 77 / 905 | 26.0 MB |
| (a′) flat exchange node, 2k | 5,711 | 4.1 | 14 / 77 / 905 | 16.5 MB |
| (b) exchange digest, 8 KB leaves | 2,402 | 1.7 | 4 / 36 / 312 | 5.4 MB |
| (c) per message (OptChat) | 24,518 | 17.5 | 48 / 298 / 4,543 | 35.5 MB |

- **(a) at 30k, by part:**

  | Part | Calls | Per exchange |
  |---|---|---|
  | leaves | 3,766 | 2.7 |
  | chunk calls (6 chunked steps) | 18 | |
  | inner merges | 2,858 | 2.0 |
  | outer merges | 1,089 | 0.8 |

- **(b):** 1,207 leaves and 1,195 merges.
- **(c):** 7,350 leaves and 17,168 merges.
- **The largest session** takes 18% of (a)'s calls.

Subagents: 130 files, one exchange each, so there are no outer merges.

| scheme | calls | /file p50 / p90 / max | input |
|---|---|---|---|
| (a) clip 30k | 2,272 (incl. 197 chunk calls) | 11 / 39 / 117 | 24.6 MB |
| (a) clip 2k | 2,146 | 9 / 39 / 117 | 12.4 MB |
| (a′) clip 2k | 1,250 | 6 / 21 / 60 | 11.9 MB |
| (b) | 607 | 4 / 8 / 22 | 2.6 MB |
| (c) | 19,376 | 123 / 292 / 739 | 33.2 MB |

## 4. Restatement proxy

The proxy compares the raw output bytes of a step's run with the length of the assistant text that opens the next step. "none" means the exchange ended after the run. Percentages are over steps that have a next text.

| run output | main: steps, next < 200 chars, next p50 | subagents: same |
|---|---|---|
| any | 3,260, 42%, 291 | 1,097, 94%, 85 |
| ≥ 2 KB | 1,345, 43%, 278 | 607, 95%, 80 |
| ≥ 8 KB | 434, 44%, 260 | 348, 93%, 68 |
| ≥ 30 KB | 99, 51%, 189 | 171, 90%, 62 |

- **Main, by what the run contained** (share of short next texts; next p50):

  | Run contained | Short next text | Next p50 (chars) |
  |---|---|---|
  | Agent | 17% | 1,071 |
  | WebFetch/WebSearch | 30% | 669 |
  | Bash | 41% | 304 |
  | injected entry | 48% | 236 |
  | Read | 49% | 201 |
  | Edit/Write | 52% | 187 |
- **Closing prose.** This is the last text-only step, or the final report tool's input.
  - Main exchanges with ≥ 30 KB of output: 76% close with ≥ 500 chars (p50 2.0K) and 19% with < 200 chars.
  - Subagents with ≥ 30 KB: 80% close with ≥ 500 chars (p50 3.6K, mostly `SubagentHandback`) and 19% with < 200.

## Reading (inference)

- **The free-step case is rare.** Once outputs are rendered, 9% of main steps and 6% of subagent steps fit in 512 B. Even final replies have a median of 1.6K chars. So (a) costs about one leaf call per step.
- **The inner binary tree is 37% of (a)'s calls,** about one merge per extra step. A flat exchange node, one call over the step lines, cuts calls by 26% in main and 42% in subagents. Its input stays ≤ 28 KB.
- **The clip sets input bytes, not calls.** Going from 30k to 2k cuts main input by 35% (27.0 → 17.5 MB). In subagents it shrinks the chunking problem from 57 steps to 7.
- **(a) costs 3.2× (b)'s calls and 5× its input in main, and 3.7× and 9.5× in subagents.** (c) costs 3.2× (a)'s calls in main and 8.5× in subagents.
- **Main sessions restate at the end of an exchange and in about half of mid-run texts. Subagents almost never restate mid-run, only in their final report.**
  - Where (a) adds information over (b): subagent logs, the roughly half of main steps followed by a short text, and the 19% of output-heavy exchanges that end without closing prose.

## Caveats

- **Scope of the data.** One user and one harness. This is a live snapshot: running sessions moved totals by about 0.1% between runs.
- **Bytes, not tokens.** Called nodes are taken as 512 B, which slightly overstates merge cost. The prompt template is excluded.
- **Step boundaries follow the model's habits.** Main-session Opus writes text before most runs (1 call per step at the median). Subagents rarely do.
- **The restatement proxy is length only.** A long next text can be a plan; a short one can still carry the key number.
- **Background agents.** Main-session `Agent` results are mostly launch acknowledgements (p50 1 KB). Reports arrive as task notifications, counted as injected entries.
- **Choices that shift exchange counts by a few percent:**
  - rewinds and abandoned branches inside a file are counted;
  - 34 interjections stored as text inside tool_result records (older versions) do not open exchanges;
  - the injected preamble before the first prompt is dropped.
- **(b)'s digest sizes approximate the chat-design digest.** Error outputs are not added to leaf input.
- **The outer tree is per session.** A thread spanning sessions adds a few merges.
