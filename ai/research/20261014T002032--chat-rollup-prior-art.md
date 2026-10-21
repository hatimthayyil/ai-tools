# Chat rollup: prior art for a zoomable session tree

Question: how should `ai chat compact` turn a coding agent's session into a rolled-up, zoomable summary tree that keeps every original turn? This note checks the proposed scheme against prior art:

1. Collapse tool runs into the assistant turn.
2. Normalise to user:assistant pairs.
3. Binary-merge the pairs upward.

Each answer marks **Fact** (cited) and **Inference** (ours) separately.

## Sources at a glance

| System | Unit compacted | Structure | Rewrites built nodes? | Raw kept? |
|---|---|---|---|---|
| OptChat/UniiChat [1] | message | binary, 512 B nodes, aligned `id+n` | never | yes, zoom |
| `ai memory` (ours) [2] | note | same binary tree; `wake` uses cover | never | yes |
| LCM / Volt [3] | token chunk of messages | DAG: leaf summaries, then condensed | no (new node replaces range in context) | yes, `lcm_expand` |
| lossless-claw (LCM port) [4] | ≤20k-token chunk, ≥8 msgs | condensed fan-out ≥4 (hard 2) | no | yes |
| Claude Code /compact [5] | whole history | flat, 9 sections | regenerates each time | no |
| Anthropic API compaction [6] | whole history | flat; "compact again" summarises old summary + new | regenerates | no |
| Codex [7] | whole history | flat summary + recent user msgs verbatim | regenerates | no |
| opencode [8] | whole history; prune old tool outputs | flat | regenerates | no |
| Factory [9][10] | newly truncated span | one anchored summary, merged incrementally | merges into one | no |
| OpenHands [11] | middle events | flat, recent kept | regenerates | no |
| AgentFold [12] | steps | multi-scale summaries | yes (retracts a range) | no |
| VCC / pi-blackhole [13][14] | messages | none; deterministic views plus BM25/regex recall | n/a | yes |

## Local data: what a Claude Code exchange looks like

This corpus is the user's own sessions: 158 top-level sessions in `~/.claude/projects` over 100 KB, sidechains and subagent files excluded. An exchange is one user prompt plus everything up to the next prompt. Counted with jq (script in session scratchpad; not kept).

- **1,439 exchanges.** Tool calls per exchange:
  - p50 2, p90 13, p99 50, max 112;
  - 38% have none;
  - 21 have 50 or more, and 3 have 100 or more.
- **Tool-result bytes per exchange:** p50 821 B, p90 28 KB, p99 124 KB, max 2.2 MB.
- **Share of all 26.3 MB:**

  | Part | Share |
  |---|---|
  | tool results | 65.7% |
  | tool inputs | 19.7% |
  | assistant text | 10.2% |
  | user text | 4.4% |

- **Message count:** about 14.9k tool messages (call + result) against 1.4k prompts.
- **Caveat:** one user, heavy subagent use (subagents excluded), so long autonomous runs are under-represented.

## 1. Turn segmentation

**Facts**
- **Masking keeps calls and drops outputs.**
  - JetBrains' observation masking keeps the agent's reasoning and actions, and replaces observations older than M=10 turns with a placeholder. It "halves cost relative to the Raw Agent while matching, and sometimes slightly exceeding, the solve rate of LLM-Summary" on SWE-bench Verified [15].
  - Example, Qwen3-Coder 480B: 54.8% masking vs 53.8% summary vs 53.4% raw [15].
- **Anthropic does the same.**
  - `clear_tool_uses_20250919` defaults: keep 3 tool uses, trigger at 100k tokens, `clear_tool_inputs: false`. So by default it keeps the calls and clears only the results [16].
  - Anthropic calls tool-result clearing "one of the safest lightest touch forms of compaction" [17].
- **opencode prunes the same way.** It erases tool outputs beyond the newest 40k tokens of tool output, keeps the last 2 user turns, and only acts when at least 20k tokens would be pruned [8].
- **pi-blackhole/VCC collapse tool runs deterministically, with no LLM** [14]:
  - one line per tool call (path, command or query);
  - `cd … &&` and pipe tails stripped from bash, capped at 120 chars;
  - consecutive identical calls collapsed to `xN`;
  - back-to-back identical errors merged;
  - only the last 8 calls per assistant turn kept;
  - thinking and noise tools (TodoWrite, ToolSearch…) dropped;
  - user text capped at 256 tokens, assistant at 200.
- **LCM segments by size, not by exchange** [3][4]:
  - leaf = oldest contiguous raw chunk outside the fresh tail, ≤20k tokens, ≥8 messages;
  - fresh tail = last 64 messages, "expands through the newest user when needed so its assistant/tool suffix cannot be separated from the active instruction";
  - files over 25k tokens are stored out of band and replaced by an ID, the path and a type-aware "exploration summary";
  - file IDs propagate up the DAG.
- **HiAgent chunks working memory by subgoal.** It keeps full action/observation pairs for the current subgoal and summarises finished ones: about 2x success and 3.8 fewer steps across five long-horizon tasks [18].
- **OptChat logs at message granularity** (`user`, `unii`, `tool`, `echo`) [1]:
  - each message is a leaf;
  - tool output is clipped to 30,000 chars head+tail;
  - thoughts are never logged ("summarizing them drew safeguard refusals").
  - Our `ai memory` view hides lines that hold only tool calls (`HIDDEN`, `view.rs`) [2].

**Inference: the proposed scheme**
- **Step (1), folding tool runs into the assistant turn, is right, but do it deterministically.** Tool bytes are 85% of the log. Evidence says dropping old observations costs nothing measurable [15][16], and an LLM pass over tool output would be the dominant compaction cost.
- **Step (2), strict user:assistant pairs, breaks three ways:**
  - **Size.** The exchange is the natural *boundary* but a bad *unit*. A p99 exchange has 50 calls and 124 KB of results, and an autonomous "continue" run can be one exchange for hours. One 512 B line cannot stand for both "thanks" and a two-hour refactor.
  - **Degeneracy.** A session with few prompts gives a tree with few leaves, so there is nothing to fold.
  - **Interjections.** A user message that arrives mid-run has no matching reply. Forcing pairs either drops it or misattributes the work.
- **Step (3), binary merge, is fine once leaves are size-bounded (see §2).**

**Proposed normalisation**
- **Raw (log).** Every message, immutable, with permanent ids. Thinking is not logged [1].
- **Exchange.**
  - Every non-tool-result user message starts one, interjections included.
  - It runs to the next such message.
  - This is HiAgent's "subgoal" with the user as the subgoal setter [18].
- **Digest (deterministic, no LLM),** per exchange:
  - user text verbatim;
  - assistant text verbatim;
  - each tool call as one line `Tool(salient arg) → ok | exit N | error: <first line> [size]`:
    - Edit/Write: path and ±lines;
    - Bash: command (cap ~200 chars) and exit code;
    - Read/Grep/Glob: path or pattern;
    - repeats as `xN`;
  - result bodies dropped (zoomable);
  - large results reduced to a reference line as in LCM [3][14].
- **Level-0 node (leaf) = a digest slice.**
  - An exchange whose digest fits B bytes is one leaf.
  - A longer one is cut into consecutive leaves, only at assistant-text boundaries (between tool runs), so a slice never splits a call from its result.
  - Each continuation slice starts "(exchange e, cont.)".
- **Level-0 line:**
  - the digest itself if ≤ 512 B (free node, as in OptChat [1]);
  - otherwise one LLM "compress" call over the digest plus clipped error outputs.
- **What a tool call keeps, and what it drops:**
  - **Keep:**
    - commands, paths and patterns;
    - exit codes and first error lines;
    - test pass/fail counts;
    - commit hashes;
    - edited paths.
  - **Drop:** result bodies, file contents, thinking, noise tools.
  - This matches masking [15], Anthropic's default [16] and pi-blackhole [14]. Factory's "artifact trail" and "breadcrumbs" list the same items [10].

## 2. Merge structure

**Facts**
- **OptChat** [1]:
  - strict binary, aligned power-of-two nodes `node(l,i)` covering 2^l messages;
  - each node built once and never again;
  - the view is a fold over that tree: merge the most "due" built sibling pair, `due=(T−last)/2^l`;
  - folds in batches from 128 KB down to 64 KB;
  - the view is persisted, never rebuilt.
  - The first-message age bug rewrote old prefix lines. The corrected rule matched Taelin's push at all 20,001 tested steps.
  - Batching cut simulated rewrite cost about 4x (21 vs 80 line-inputs per message).
- **Our code base has both frontiers** [2]:
  - the fold (`Mem::fit`, still using the old first-message score) for `ai chat`;
  - the stateless `cover` (α-bisection tiling, recomputed from `t`) for `wake`.
  - The revision review confirmed the old score and restart refold change the prefix [19].
- **LCM / lossless-claw** [3][4]:
  - condense the shallowest depth that has ≥ `condensedMinFanout`=4 contiguous same-depth summaries (≥8 raw messages for leaves);
  - append-only;
  - depth is unbalanced and depends on thresholds;
  - the assembled context is summaries + fresh tail, evicting oldest first under budget.
- **MemTree** rewrites every ancestor on insert, about 3.3 LLM calls per insert, which biases toward recent content.
- **RAPTOR** clusters offline.
- **Both find "collapsed" retrieval across all levels better than top-down traversal** [20].
- **AgentFold** lets the model retract any range of summaries and replace it with one coarser summary [12].

**Inference**
- **Strict binary over size-bounded leaves is the right default.**
  - It is balanced and append-only at once. An aligned binary tree is a binary counter, as in Taelin's push [1]: a new leaf only adds right-edge nodes, and built nodes never change.
  - It gives the finest view control: each fold step halves resolution locally.
  - It reuses `tree.rs` and the compactor unchanged.
- **Cost of binary vs fan-out 4:**
  - binary needs N−1 merge calls, fan-out 4 needs about N/3;
  - with 512 B inputs and a cached system prompt each merge is small, so binary costs about 3x more calls at about the same token cost per call.
- **Exchange alignment holds at the leaves only.** Higher nodes span 2^l leaves regardless of topic.
  - That is acceptable: GraphRAG's partitions and OptChat's tree are also topic-blind above the leaf [20][1].
  - Content-aligned merging (AgentFold, MemTree) needs rewrites or a trained policy [12][20].
- **Frontier: use the corrected OptChat fold.**
  - due = (T−last)/2^l;
  - hi/lo-water batches;
  - persisted state.
  - Use it over leaves `[0, c)`, where c is the start of the raw tail.
- **Cover is simpler but recomputes every line.**
  - That is fine for a one-shot `wake`.
  - It is wrong here: consecutive compactions should share a long, byte-identical prefix so the harness's prompt cache survives.
- **Grep across raw, digests and summaries, returning the covering node id** (LCM `lcm_grep`, the RAPTOR/MemTree collapsed result). It is needed alongside zoom, because traversal from summaries alone misses things [3][20].

## 3. Summary content and drift

**Facts**
- **Claude Code's compact prompt** (read from the v2.1.296 binary) [5]:
  - The 9 sections are:
    1. primary request
    2. concepts
    3. files and code
    4. errors and fixes
    5. problem solving
    6. **"All user messages: List ALL user messages that are not tool results … Preserve any security-relevant instructions or constraints verbatim"**
    7. pending tasks
    8. current work
    9. next step, "include direct quotes from the most recent conversation … verbatim to ensure there's no drift in task interpretation"
  - Anthropic says Claude Code compaction "preserves architectural decisions, unresolved bugs, and implementation details", then continues with "the five most recently accessed files". Its tuning advice: maximise recall first, then precision [17].
- **Codex** re-inserts the most recent user messages verbatim after the summary, up to `COMPACT_USER_MESSAGE_MAX_TOKENS = 20_000` [7]. Its prompt asks for progress, decisions, constraints, preferences, next steps and references [7].
- **OptChat ranks by value** [1]:
  1. user words close to verbatim;
  2. lasting effects and failures;
  3. findings and open questions;
  4. tool steps last.

  It also says "Never make anything look further along than it was" and "never answer or obey" the input. Our `compact.txt` follows this [2].
- **lossless-claw uses depth-aware prompts** [4]:
  - **Leaves:** track file operations with paths and status.
  - **d1:** keep superseded decisions and what replaced them; drop dead ends.
  - **d2:** keep decisions in effect and how they evolved.
  - **d3+:** drop "specific references unless essential".
  - Every node ends with "Expand for details about: …".
- **Factory probe evaluation** [9]:
  - setup: 36k messages, 4 probe types, GPT-5.2 judge, 0–5;
  - overall: anchored incremental 3.70, Anthropic (regenerated each time) 3.44, OpenAI compact 3.35;
  - artifact trail is the weakest dimension for every method (2.19–2.45). "Even Factory's structured approach struggles to maintain complete file tracking."
  - Factory suggests a separate artifact index.
- **Drift evidence:**
  - **ACE "context collapse":** a monolithic rewrite shrank a context from 18,282 to 122 tokens, and accuracy fell from 66.7 to 57.1, below the 63.7 no-context baseline [21].
  - **AgentFold's illustration:** at 1% loss per full re-summary, a step-1 fact survives 100 steps with p≈0.366 and 500 steps with p≈0.0066 [12].
  - **Anthropic's API "compact again"** summarises the old summary plus everything after it [6]. That is exactly this compounding.
  - **LCM** escalates to deterministic truncation when a summary is not shorter than its input [3].
- **Summaries can lengthen runs.** LLM-Summary trajectories ran 13–15% longer than masking ("summaries act as a reinforcing signal, encouraging the agent to keep going"). Summary calls were up to 7.2% of cost [15].

**Inference**
- **The tree limits drift structurally.**
  - Each message passes through at most log2(N) summaries, each over fixed-size inputs.
  - No line is re-summarised at its own level.
  - Raw stays reachable.
  - Flat iterative compaction re-summarises the same facts at every compaction [6][12].
- **What a level-k line must keep, by value** (OptChat's ladder plus Claude Code's sections):
  1. user orders, corrections, preferences and reasons, near verbatim;
  2. decisions in effect and superseded ones, marked as such;
  3. state: done / in progress / failed and why, never inflated;
  4. open tasks and questions;
  5. identifiers (paths, commands, commit hashes, error strings), copied exactly, because zoom is driven by the words in the line [1][2];
  6. tool steps last.
- **Keep one uniform prompt.**
  - Depth-aware prompts that drop identifiers at d3+ [4] work against zoom-based recall.
  - Keep "mark superseded" from lossless-claw d1 [4].
- **Artifacts should not depend on the LLM.**
  - Each leaf carries a deterministic set: files read/edited/created/deleted, commits, failing commands.
  - The sets are unioned up the tree and rendered as one bounded "Files touched" block (path, last op, last leaf id).
  - This addresses Factory's weakest dimension [9] the way LCM propagates file IDs [3].
- **Keep user messages verbatim longer than the rest: yes.**
  - Two production agents keep them verbatim or listed [5][7].
  - They are only 4.4% of bytes locally.
  - Render a "user rail" after the fold: the newest user messages verbatim to a fixed budget (Codex uses 20k tokens [7]; start at ~8 KB). The summary lines still cover them.

## 4. Incremental and background cost, caching

**Facts**
- **Re-summarising the whole prefix costs more as the session grows.** Factory summarises only the newly truncated span into an anchored summary [10].
- **The fill/drain gap trades call count against cache invalidation** (T_max/T_retained) [10].
- **LCM** [3]:
  - async compaction above τ_soft, blocking above τ_hard;
  - summaries swapped in atomically between turns;
  - "zero-cost continuity" below τ_soft.
- **lossless-claw's deferred mode** [4]:
  - prepares summaries in background `maintain()` while the cache is hot;
  - *publishes* them (prompt-mutating) only when the cache is cold (TTL 300 s) or near overflow;
  - caps spend at 24 summary calls per 10 min.
- **Context editing "invalidates cached prompt prefixes when content is cleared"**, so clear in large steps (`clear_at_least`) [16].
- **OpenHands** condenses only at a size threshold so cached prompts are reused between condensations. It reports up to 2x lower per-turn cost, linear instead of quadratic growth, and 54% vs 53% solve rate [11].
- **JetBrains:** reusing per-strategy hyperparameters in the hybrid "degraded cost efficiency, due to KV-cache inefficiencies and summary overhead" [15].
- **OptChat:** compactions share the turns' system prompt and tools, and a separate 16–32 KB compaction view. Simulated prefix reuse was 98.6% for turns and 96.2% for compactions [1]. That figure is simulated, not measured [19].

**Inference**
- **Calls per leaf are fixed.** Each leaf costs at most 1 compress call plus 1 merge call amortised, whatever the session length. That is O(1) per leaf, against O(prefix) for flat re-summarisation.
- **Exchange-level leaves cut calls about 10x.**
  - Message-level leaves (OptChat) would mean about 17k leaves for this corpus, hence about 30k+ calls.
  - Exchange-slice leaves mean about 1.5–2k leaves, hence about 3–4k calls.
  - Each call reads about 1–2 KB plus a cached system prompt.
- **Build in the background, publish on `compact`.**
  - **Build:** digests at log time (free); LLM nodes as soon as they are ready, with the cheap model (the existing `nap` pump, JOBS 8).
  - **Publish:** `ai chat compact` renders fold + rail + tail, and waits only for nodes the chosen frontier needs. Unbuilt ones fall back to their digest (deterministic) rather than a placeholder.
- **Cache cost only at publish.** The prompt changes only at publish, which is when the harness rewrites its context anyway. A persisted fold makes consecutive publishes share a long prefix.

## 5. Does hierarchical, zoomable compaction beat flat summaries or masking?

**Facts**
- **No published head-to-head compares a hierarchical summary tree against flat summarisation and masking on coding-agent continuation.**
- **LCM beats Claude Code on OOLONG** [3]:
  - +4.5 points on average (74.8 vs 70.3), at every length from 32k to 1M.
  - The authors attribute the gap mainly to engine-run LLM-Map aggregation, not to the DAG.
  - OOLONG is aggregation, not multi-turn coding, and the results are self-reported.
- **Masking ≈ LLM summary on SWE-bench Verified.** The hybrid (mask first, summarise as last resort) was best: +2.6 pp over raw, −7% cost vs masking, −11% vs summary [15].
- **Factory:** incremental anchored summary > full regeneration > opaque compact [9].
- **Multi-scale and branch-structured contexts win when trained or structured** [12][22][18]:
  - **AgentFold:** keeps context under about 20k tokens over 500 turns, and is 92% smaller than ReAct at 100 turns [12].
  - **Context-Folding:** matches ReAct with about 10x smaller active context and "significantly outperforms" summarisation-based management at the same context size (RL-trained) [22].
  - **HiAgent:** subgoal-chunked summaries give about 2x success [18].
- **Recursive summarisation improved consistency in long dialogues** (MSC) [23]. MemGPT/Letta keep a recursive summary of evicted messages and keep the messages searchable [24].

**Inference**
- **Short tasks don't need a tree.** For single tasks under about 100 turns, recent context plus masking is enough [15]. A tree adds nothing there and could lengthen runs as summaries do [15].
- **The tree is for the "infinite thread".** It earns its keep when flat summaries compound loss [12][21] and when old decisions must stay findable. That is unproven and must be measured.
- **Measure it with Factory's probe method** (recall, artifact, continuation, decision) on our own logs, at real compaction points. Compare:
  - (a) masking + digest tail;
  - (b) Claude Code /compact;
  - (c) the tree view.

## Recommended scheme

1. **Log.**
   - Ingest the harness JSONL into the immutable store.
   - Kinds: `user`, `ai`, `call`, `result`; thinking dropped.
   - Ids are permanent.
2. **Exchange.** Every non-tool-result user message, interjections included, opens one; it runs to the next.
3. **Digest (no LLM).**
   - User and assistant text verbatim.
   - Each call as one line with its salient argument and outcome (exit code, first error line, size); repeats as `xN`.
   - Result bodies, file contents and noise tools dropped.
4. **Leaves (level 0).**
   - Digest slices of ≤ B bytes (start B = 8 KB; tune).
   - An exchange is one leaf when it fits, otherwise it is cut at assistant-text boundaries.
   - Each leaf maps to its raw message range.
   - The leaf line is the digest when ≤ 512 B; otherwise one compress call.
5. **Tree.**
   - Strict binary, aligned, over leaf ids.
   - 512 B nodes, each built once, never rewritten.
   - The existing `tree.rs`/compactor, with `compact.txt` extended:
     - kinds user/ai/call;
     - "mark superseded decisions" [4];
     - "copy paths, commands, hashes, errors exactly" [1].
6. **Artifacts.**
   - Deterministic per-leaf sets (files and operations, commits, failing commands), unioned upward.
   - Rendered as one bounded "Files touched" block.
7. **View on `compact`.** Rendered in this order:
   1. **Fold:** the corrected OptChat fold over leaves `[0, c)`; due = (T−last)/2^l, hi/lo-water batches, persisted.
   2. **Artifacts block.**
   3. **User rail:** the newest user messages verbatim, about 8 KB.
   4. **Tail:** from c, the last few exchanges as digests and the current exchange raw, with its tool results older than the last ~3–10 calls masked [15][16]. The tail always reaches back to the newest user message [4].
8. **Background.**
   - The cheap-model pump builds nodes as soon as they are ready.
   - `compact` waits only for frontier nodes, and falls back to digests for unbuilt ones.
   - The prompt changes only at publish.
9. **Tools.**
   - `zoom id+n`: node → children → leaf digest → raw messages, paged.
   - `grep`: across raw, digests and lines, returning the covering line id.
10. **Evaluate before adding complexity.** Run Factory-style probes on our logs: tree vs masking-only vs flat compact. Fan-out, depth-aware prompts and content-aligned merging wait until the data asks for them.

## Sources

1. OptChat/UniiChat spec: `/hatimthayyil/code/optchat/optchat.md`
2. `src/memory/{tree.rs,view.rs,cover.rs,compact.rs}`, `src/memory/prompts/compact.txt`
3. Ehrlich & Blackman, "LCM: Lossless Context Management", arXiv 2605.04050: https://arxiv.org/abs/2605.04050
4. lossless-claw: https://github.com/Martian-Engineering/lossless-claw (`docs/architecture.md`, `src/summarize.ts`, README config)
5. Claude Code v2.1.296 compact prompt, extracted from `~/.local/share/claude/versions/2.1.296`
6. Anthropic, Compaction on demand: https://platform.claude.com/docs/en/build-with-claude/compaction-on-demand ; overview: https://platform.claude.com/docs/en/build-with-claude/compaction
7. Codex: https://github.com/openai/codex/blob/main/codex-rs/core/src/compact.rs ; https://github.com/openai/codex/blob/main/codex-rs/prompts/templates/compact/prompt.md
8. opencode `packages/opencode/src/session/compaction.ts` (local checkout `/hatimthayyil/code/opencode`)
9. Factory, "Evaluating context compression": https://factory.com/news/evaluating-compression
10. Factory, "Compressing context": https://factory.com/news/compressing-context
11. OpenHands, "Context condensation for more efficient AI agents": https://openhands.dev/blog/openhands-context-condensensation-for-more-efficient-ai-agents ; SDK: https://docs.openhands.dev/sdk/guides/context-condenser
12. Ye et al., AgentFold, arXiv 2510.24699: https://arxiv.org/abs/2510.24699
13. VCC, arXiv 2603.29678; `/hatimthayyil/code/VCC/README.md`
14. pi-blackhole `docs/vcc-compaction.md` (`/hatimthayyil/code/pi-blackhole`)
15. Lindenbauer et al., "The Complexity Trap", arXiv 2508.21433v3: https://arxiv.org/abs/2508.21433
16. Anthropic, Context editing: https://platform.claude.com/docs/en/build-with-claude/context-editing
17. Anthropic, "Effective context engineering for AI agents": https://www.anthropic.com/engineering/effective-context-engineering-for-ai-agents
18. Hu et al., HiAgent, ACL 2025, arXiv 2408.09559: https://arxiv.org/abs/2408.09559
19. `ai/research/20261008T074205--optchat-cache-revision-review.md`
20. `ai/research/20261013T222410--notes-prior-art.md` (RAPTOR, MemTree, GraphRAG)
21. Zhang et al., ACE, arXiv 2510.04618: https://arxiv.org/abs/2510.04618
22. Sun et al., Context-Folding, arXiv 2510.11967: https://arxiv.org/abs/2510.11967
23. Wang et al., "Recursively summarizing enables long-term dialogue memory", arXiv 2308.15022: https://arxiv.org/abs/2308.15022
24. Packer et al., MemGPT, arXiv 2310.08560: https://arxiv.org/abs/2310.08560 ; Letta compaction: https://docs.letta.com/guides/core-concepts/messages/compaction
25. Cognition, "Don't build multi-agents" (compression model for "key details, events, and decisions"; fine-tuned smaller model): https://cognition.com/blog/dont-build-multi-agents
