# Upstream OptChat cache revision

Authored by @upstream_cache_review (GPT-6), 2026-10-08.
Changelog: initial review; downloaded public gist metadata; preserved exact original, pre-fix, and fixed source; compared revisions and checked the cache claim.

## Which upstream versions are being compared?

### Takeaway

The sibling `../optchat/optchat.md` is already the new UniiChat specification, not the original. Its clean git checkout contains all three upstream revisions, so the two old versions are recoverable without changing that checkout.

### Cited Findings

- The gist API lists initial revision `26ad1070fd7c1c766d4d75d5c64c1e385d9dacd2`, committed **2026-10-04 02:36:42 UTC**; title-only revision `f51fe5c910427fd6f384d22823140b1693c76207`, **2026-10-04 02:42:39 UTC**; cache-fix rewrite `3c190e06f34aba0c69f49042c526093269604935`, **2026-10-08 01:58:24 UTC**. The rewrite adds 403 and deletes 706 lines. The gist `updated_at` timestamp, 2026-10-08 06:36:06 UTC, is later than its last source revision; use the revision commit time when describing the source change. — [Gist API](https://api.github.com/gists/91837951a5ce5b38f341ec1ba1df6449)
- Comparing the first two commits locally finds exactly one changed line: the title changes from “one endless chat with an AI that remembers everything” to “an endless chat where the AI remembers everything.” The cache instructions are otherwise identical. — [Initial source](https://gist.github.com/VictorTaelin/91837951a5ce5b38f341ec1ba1df6449/26ad1070fd7c1c766d4d75d5c64c1e385d9dacd2); [pre-fix source](https://gist.github.com/VictorTaelin/91837951a5ce5b38f341ec1ba1df6449/f51fe5c910427fd6f384d22823140b1693c76207)
- `../optchat` has no tracked or untracked changes (`git status --short` empty), origin is the gist, and its file is byte-for-byte equal (`cmp` exit 0) to `.files["optchat.md"].content` in the downloaded current gist API response. The new title is “UniiChat: one chat that never ends.” — [Latest source](https://gist.github.com/VictorTaelin/91837951a5ce5b38f341ec1ba1df6449/3c190e06f34aba0c69f49042c526093269604935); [local latest snapshot](./upstream-latest-3c190e0.md)

Saved artifacts:

| Artifact | Provenance / SHA-256 |
|---|---|
| `upstream-initial-26ad107.md` | `git show 26ad107:optchat.md`; `72acb642924d4f5b2e79f61711ea2b2c2fed25cdd8f5701069fe1c21e412d884` |
| `upstream-old-f51fe5c.md` | `git show f51fe5c:optchat.md`; `8f6997e8944d85e4df53b5704bf7c4e393e4da361071181e9cc2f7d9d1b6e430` |
| `upstream-latest-3c190e0.md` | Gist API content, exact extraction with `jq -j`; `12f300f760af82bc07bc5201051d1267824ded09c9def8186e4f8144368038d8` |
| `upstream-gist-api.json` | Public GitHub response, including latest content and revision history |
| `upstream-old-to-new.diff` | Unified diff of pre-fix and current snapshots |

Reproduce the diff from this directory:

```sh
diff -u --label upstream-old-f51fe5c.md --label upstream-latest-3c190e0.md upstream-old-f51fe5c.md upstream-latest-3c190e0.md
```

The snapshot markdown is unmodified upstream content; its authorship belongs to Victor Taelin. This note and the artifact inventory supply agent authorship metadata without altering source evidence.

### Inferences

- Any implementation developed against either October 4 revision inherited the same defective age rule if it followed the formula literally. A comparison using only today's sibling file would miss the original bug because it has already been updated.

### Gaps

- No independent download of the two old raw URLs was necessary: the sibling git history already contains the exact gist commits. GitHub's API independently confirms their SHAs and dates.

## What changed, and why did the original hurt caching?

### Takeaway

The author explicitly identifies an incorrect age origin: the original measured sibling pairs from their first message, causing old prefix lines to merge ahead of recent lines. The rewrite also changes merge timing, persistence, block placement and compaction inputs; changing the formula alone does not implement the complete fix.

### Cited Findings

| Concern | Original instruction / snapshot lines | Revised instruction / snapshot lines |
|---|---|---|
| Pair priority | `due=(T-start)/2^(l+2)`; choose maximum. Old **422–440** | `due=(T-last)/2^l`, where `last` is the pair's inclusive last message; choose maximum, oldest tie first. New **159–182** |
| Merge timing | `fit()` after each appended message and each built node; shrink only until ≤128,000 bytes. Old **414–434** | Append-only between batches; trigger at >128,000 bytes and merge down to ≤64,000. Keep the batch active if built parents cannot yet reach the low-water limit. New **184–193** |
| Restart | Explicitly do not save view; reconstruct from message zero. Old **452–455** | Persist `view.json`, restore it, never rebuild from log because the rebuilt partition differs. New **195–197** |
| Anthropic view blocks / marks | Marks at line boundaries before 50k, 80k and 100k characters; request-end mark. Old **650–669** | Stable blocks of **4 lines**; one mark on last complete block, another at request end; rely on 20-block lookback. New **223–229** |
| Concurrent cache writes | No instruction to coordinate writers | If another request is writing a shared marked prefix, wait until its response begins, so concurrent compactions do not all pay to write it. New **230–232** |
| Compaction prompt and tools | Dedicated `COMPACT` system prompt and no tools; bare summary text with no IDs. Old **214–221**, **255–261** | Exactly the turn system prompt and tools (tools not called). New **245–250** |
| Compaction view | Prefix of the main full-size view; same ~64k-token context. Old **222–254** | Separate incremental 16–32 KB view, append new lines, merge on >32 KB or a main-view batch; only built lines up to node. New **253–260** |
| Compaction ruler | Real example of a 512-byte summary. Old **230–245**, **262–264** | 512 dashes; example content was being copied. New **262–287** |

All original-column findings: [pre-fix source](https://gist.github.com/VictorTaelin/91837951a5ce5b38f341ec1ba1df6449/f51fe5c910427fd6f384d22823140b1693c76207). All revised-column findings: [fixed source](https://gist.github.com/VictorTaelin/91837951a5ce5b38f341ec1ba1df6449/3c190e06f34aba0c69f49042c526093269604935).

- New §3.2 explicitly attributes the bug to `first`, not to the extra denominator factor: dividing every pair's score by four does not change their ranking. It supplies the `T=10` view `0+4, 4+4, 8+1, 9+1`: old ranking merges messages 0–7, correct ranking merges 8–9. The author reports old-rule agreement with rollback push at only 481/20,001 steps, versus full agreement for the corrected rule when using push's list length as the budget. — [Fixed source, §3.2](https://gist.github.com/VictorTaelin/91837951a5ce5b38f341ec1ba1df6449/3c190e06f34aba0c69f49042c526093269604935#32-how-optmem-uses-it)
- New §3.3 reports 53/192 lines rewritten per message by continuous fixed-size merging, versus roughly 2 per message amortized with batches; its 30,000-message simulation reports 80 versus 21 line-inputs per message. This is a timing improvement separate from the age correction. — [Fixed source, §3.3](https://gist.github.com/VictorTaelin/91837951a5ce5b38f341ec1ba1df6449/3c190e06f34aba0c69f49042c526093269604935#33-why-the-cache-holds)
- New request order remains stable tools, stable system prompt, view, fresh task/message. Dates/devices belong after the view. New §7's mistakes now includes first-message age, per-message merging, and view rebuilding on restart. — [Fixed source, §§3.3, 6, 7](https://gist.github.com/VictorTaelin/91837951a5ce5b38f341ec1ba1df6449/3c190e06f34aba0c69f49042c526093269604935)

### Inferences

- Concrete independent arithmetic confirms the supplied counterexample. At `T=10`, old scores are `(10-0)/16=0.625` for 0–7 and `(10-8)/4=0.5` for 8–9, so it rewrites the oldest line. New scores are `(10-7)/4=0.75` and `(10-9)/1=1`, selecting the recent pair.
- In node coordinates, a sibling pair beginning at index `i`, level `l`, has `last=(i+2)*2^l-1`. Thus `(T-last)/2^l=(T+1)/2^l-i-2`; dropping the uniform `-2` gives the new prose's alternate `(T+1)/2^l-i` score. Use floating-point or an exact rational comparison rather than truncating integer division.
- Caching benefits from a stable prefix, so early line mutation invalidates everything following it. The bug does not delete message history or corrupt the summary tree: it increases prompt work and makes old summaries coarser sooner.
- Static shared system/tools permit shared caching only when the actual provider caching key and model agree. The current reference uses a cheap compactor and allows different master models, so sharing should not be assumed across model identities.
- “The last call's whole view is a prefix” is conceptual shorthand. Closing `</chat>` markup and incomplete trailing blocks change position as lines append; retaining stable complete 4-line blocks before those endings is what permits reuse in the structured provider request.

### Gaps

- No upstream implementation or simulation script accompanies this single-file gist. The author-reported 20,001-step and 30,000-message results cannot be rerun from these source snapshots alone.
- This subtask audits upstream instructions only; the sibling MyAI implementation is audited separately by `/root/local_cache_audit`.

## What evidence supports the X statement and 98%+ claim?

### Takeaway

The revised primary source confirms that the original recipe had a bug and explains a revised preservation strategy. Its numerical cache evidence is explicitly simulation-based: 98.6% of turn prefixes and 96.2% of compaction prefixes, not a demonstrated 98%+ request hit rate for our implementation.

### Cited Findings

- New §3.3 states that replaying 3,000 messages against a **model of Anthropic's cache** yielded **98.6%** cached turn-prefix reads and **96.2%** compaction-prefix reads (snapshot **239–241**). — [Fixed source, §3.3](https://gist.github.com/VictorTaelin/91837951a5ce5b38f341ec1ba1df6449/3c190e06f34aba0c69f49042c526093269604935#33-why-the-cache-holds)
- The stated misses are after a pause exceeding cache lifetime, after a compaction batch, and on a model/account switch (snapshot **233–237**). The API policy in the reference remains five-minute entries and no renewal pings; Claude Code subscription entries are separately described as one hour. — [Fixed source, §3.3](https://gist.github.com/VictorTaelin/91837951a5ce5b38f341ec1ba1df6449/3c190e06f34aba0c69f49042c526093269604935#33-why-the-cache-holds)
- The new document's first/last-age correction directly corroborates the substance of the user's pasted X warning. — [Fixed source, §3.2](https://gist.github.com/VictorTaelin/91837951a5ce5b38f341ec1ba1df6449/3c190e06f34aba0c69f49042c526093269604935#32-how-optmem-uses-it)

### Inferences

- A percentage of cached prefix tokens/bytes and a percentage of requests with any cache hit are different metrics. The new prose specifies prefix reuse; do not present 98.6% as a request-count success rate.
- An aggregate combining turns and compactions can fall below 98%; the reference reports 96.2% for compactions and offers no single weighted total.
- Real-world confirmation requires provider usage telemetry over representative workloads and pauses. No billable probes were run for this review.

### Gaps

- The exact X update was not independently retrieved. Primary-site access to `https://x.com/VictorTaelin` failed, and targeted searches for the pasted quote did not find its status URL. Treat the quote as user-provided evidence, not an independently verified X citation.
- The cache simulation dataset, script, request timing distribution, token counts, model identities, account switches and provider usage records are absent from the gist. Its result is an author-reported simulation, not independently reproduced provider performance.
