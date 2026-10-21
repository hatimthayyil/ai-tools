# OptChat’s cache repair exposes inherited defects

Author: @codex (GPT-6)  
Date: 2026-10-08

**Confirmed: our frozen `ai chat` retains the original cache-breaking design.** The original ranks merges using a pair’s first message, which can rewrite old prefix lines while newer lines remain intact. The revision corrects that ranking and also batches merges, persists the live view, and changes request layout. Our chat still uses the old ranking, continual folding, restart reconstruction and large cache blocks. These findings do **not** establish the same failure in current standalone `ai memory wake` or its task-only compactor. No provider calls were made, and the author’s 98%+ figure is simulation evidence, not a measurement of our harness. ([Original recipe](https://gist.github.com/VictorTaelin/91837951a5ce5b38f341ec1ba1df6449/f51fe5c910427fd6f384d22823140b1693c76207), [revised recipe](https://gist.github.com/VictorTaelin/91837951a5ce5b38f341ec1ba1df6449/3c190e06f34aba0c69f49042c526093269604935), [our score](/hatimthayyil/code/ai-tools/ai/src/rs/memory/src/view.rs:134))

The sibling `../optchat/optchat.md` already matches the latest gist exactly. The two October 4 revisions differ only in their title; `f51fe5c…` is the pre-fix comparison, and `3c190e0…` is the October 8 rewrite. Saved snapshots and their diff preserve the evidence. ([Revision inventory](</hatimthayyil/code/ai-tools/ai/research/20261008T074205--optchat-cache-revision-review/upstream.md>))

| Mechanism | Original | Revised | Our chat |
|---|---|---|---|
| Merge priority | Age from first message | Age from last message | Original |
| Fold timing | Fit after append/build to 128 KB | Batch from >128 KB to ≤64 KB | Original |
| Restart | Reconstruct view from zero | Restore persisted view | Original |
| View blocks | Large fixed character checkpoints | Stable four-line blocks | Original blocks plus priming |
| Compactions | Separate prompt, full-view context | Same static envelope, smaller view, write coordination | Historical v0.4 followed old design; v0.5 removed context |

The mapping comes from the [upstream comparison](</hatimthayyil/code/ai-tools/ai/research/20261008T074205--optchat-cache-revision-review/upstream.md>) and [local audit](</hatimthayyil/code/ai-tools/ai/research/20261008T074205--optchat-cache-revision-review/local.md>).

## First-message age chooses the wrong prefix

Our `pick` computes `(t - a.id()) / 2^(a.l+2)`, where `a.id()` is the first message of the older sibling. The corrected rule is `(T-last)/2^l`, with `last` the inclusive last message of the pair. **The denominator’s extra factor of four is harmless to ranking; the age origin is the bug.** At `T=10`, with view `0+4,4+4,8+1,9+1`, the old scores are 0.625 for the oldest pair and 0.5 for the newest. Corrected scores are 0.75 and 1.0. The original therefore rewrites messages 0–7; the revision preserves that prefix and merges 8–9. ([Our score](/hatimthayyil/code/ai-tools/ai/src/rs/memory/src/view.rs:134), [coordinate definition](/hatimthayyil/code/ai-tools/ai/src/rs/memory/src/tree.rs:21), [upstream explanation](/hatimthayyil/code/optchat/optchat.md:157))

A changed prefix invalidates reusable content from that point onward. This is cache churn, not loss of the underlying message log or corruption of the summary tree. Earlier stable blocks, tools and system content can still remain cacheable. Chat renders this folded view directly into its request, so the defective ordering reaches actual chat prompts. ([Chat request construction](/hatimthayyil/code/ai-tools/ai/src/rs/chat/src/session.rs:217))

## Batching and persistence preserve folding history

Correcting the score alone leaves two confirmed problems. Our `fit` runs after append and completed summaries, folds whenever the view exceeds 128,000 bytes, and stops as soon as it fits. The revision appends between batches, then folds from above 128,000 to at most 64,000 bytes, retaining batch state while required parents are unfinished. That creates room for future appends without repeatedly rewriting existing lines. ([Our fit](/hatimthayyil/code/ai-tools/ai/src/rs/memory/src/view.rs:112), [completed jobs](/hatimthayyil/code/ai-tools/ai/src/rs/memory/src/compact.rs:271), [revised batching](/hatimthayyil/code/optchat/optchat.md:184))

Our load path also calls `refold`, clears the coordinates and replays the log against whichever summaries are now built. The revised recipe explicitly saves and restores `view.json`. **Restart reconstruction can produce a different partition from the live view**, even with identical log and tree content. The deterministic reproduction produced live `0+8,8+8,16+4` at 20 messages, then `0+16,16+2,18+2` after refolding. This depends on summary availability; it need not occur on every restart. ([Load](/hatimthayyil/code/ai-tools/ai/src/rs/memory/src/view.rs:34), [refold](/hatimthayyil/code/ai-tools/ai/src/rs/memory/src/view.rs:150), [revised persistence](/hatimthayyil/code/optchat/optchat.md:195))

The reproduction compiled and ran successfully using unchanged production `tree.rs` and `view.rs`, with storage/error scaffolding isolated from external dependencies. It verifies wrong ranking, repeated folding after saturation, and delayed-parent restart drift without API calls or shared-memory writes. Run it with `rustc --edition=2024 -Awarnings 'ai/research/20261008T074205--optchat-cache-revision-review/local-reproduction.rs' -o /tmp/optchat-local-audit`, then `/tmp/optchat-local-audit`. ([Reproduction source](</hatimthayyil/code/ai-tools/ai/research/20261008T074205--optchat-cache-revision-review/local-reproduction.rs>))

## Large blocks and priming add separate costs

Our chat still splits at approximately 50,000, 80,000 and 100,000 characters. Below the first checkpoint, the entire rendered `<chat>…</chat>` is one block; appending lines changes that block. A prime marks each current block before the real turn. It can help that following turn reuse the exact current view, but changed views require new primes, and the old whole-block checkpoint does not preserve individual unchanged line boundaries. Prime success also accepts `message_start` even when reported cache counts are zero. ([Block slicing](/hatimthayyil/code/ai-tools/ai/src/rs/memory/src/compact.rs:27), [freshness and priming](/hatimthayyil/code/ai-tools/ai/src/rs/chat/src/prime.rs:46), [success handling](/hatimthayyil/code/ai-tools/ai/src/rs/chat/src/prime.rs:98))

The revision instead uses four-line blocks, marks the last complete block and request end, keeps the static system/tools envelope identical for turns and compactions, and waits for an in-flight shared-prefix writer. Its compactor gets a separate 16–32 KB incremental view. Historical v0.4 replayed the preceding full view under a separate compaction prompt without write coordination. Current v0.5 deliberately removed that context: its Sonnet compactor receives only the task block, while the chat master defaults to Opus with tools. Reintroducing context is a redesign with cost and quality tradeoffs; sharing across different models must not be assumed. ([Revised blocks and coordination](/hatimthayyil/code/optchat/optchat.md:223), [revised compactor](/hatimthayyil/code/optchat/optchat.md:245), [current task input](/hatimthayyil/code/ai-tools/ai/src/rs/memory/src/compact.rs:80), [historical review](/hatimthayyil/code/ai-tools/ai/research/20261007T064116--ai-chat-subscription-usage-review.md))

## Repair chat without restoring expensive compactions

The repair scope is the frozen chat: correct the priority, implement high/low-water batching, persist coordinates and batch state against a validated log/tree snapshot, and adapt stable blocks to Claude Code’s actual breakpoint budget. Measure prime writes and real turns together. Current `wake` derives its short overview through `cover`, and current `nap` schedules context-free jobs independently of folded coordinates; those paths do not demonstrate the chat prefix defect. ([Wake](/hatimthayyil/code/ai-tools/ai/src/rs/memory/src/cli.rs:304), [scheduler](/hatimthayyil/code/ai-tools/ai/src/rs/memory/src/view.rs:167), [chat freeze](/hatimthayyil/code/ai-tools/ai/backlog.md:7))

The gist reports **98.6% turn-prefix and 96.2% compaction-prefix reuse in a simulated 3,000-message replay**. These are simulated token-prefix reuse statistics, not live measurements or request-hit rates. No local ledger establishes how much the confirmed defects contributed to the earlier 35% usage incident; eager compaction, repairs and repeated failures are separate request amplifiers. This review changed no application code, human workspace files or existing unrelated edits. ([Author’s qualification](/hatimthayyil/code/optchat/optchat.md:239), [size repairs and retries](/hatimthayyil/code/ai-tools/ai/src/rs/memory/src/compact.rs:94), [usage review](/hatimthayyil/code/ai-tools/ai/research/20261007T064116--ai-chat-subscription-usage-review.md))

## Changelog

2026-10-08 — @codex (GPT-6): compared original and revised gist with current and historical MyAI paths; confirmed deterministic cache-shape defects; recorded repair scope without implementation changes.
