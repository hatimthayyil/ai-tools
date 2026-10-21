# ai chat subscription usage review

Author: @codex (GPT-6)
Date: 2026-10-07

The main design problem is request amplification: saving a tool-heavy turn
creates many Sonnet summary jobs, each carrying the preceding memory view.
Opus cache priming adds another stream of requests. The visible reply's usage
does not include all this work.

Scope: installed `ai 0.4.0`, committed source at `5892350`, and a read-only
inspection of the shared memory. Another session is changing tool logging and
summary visibility in the working tree. Findings below identify the committed
behavior; the pending changes are discussed separately. No live model probes
or subscription-consuming test calls were made for this review.

## Evidence from the actual memory

Snapshot: `7ef958d955db957882e98553899fe33a279fba69`, read with Git object APIs.

| Item | Count |
| --- | ---: |
| Messages in the shared store | 378 |
| Notes from other sessions | 78 |
| ai chat user messages | 8 |
| ai chat assistant messages | 28 |
| ai chat tool calls | 132 |
| ai chat tool results | 132 |
| Model-authored leaf summaries | 122 |
| Model-authored internal summaries | 292 |
| All model-authored summaries | 414 |

The 122 model-authored leaves correspond to long chat records: 30 tool calls,
86 tool results, five assistant messages and one user message. All 78 notes
fit as free leaves. **116 of 122 leaf summarizations were for tool traffic.**

The leaf texts total 102,383 bytes. With IDs, delimiters and tags, the complete
leaf view is 104,933 bytes, below the 128,000-byte budget. The 292 model-authored
internal nodes are available for later folding and zooming, but are not needed
to fit this snapshot's view.

The 414 nodes establish successful summary jobs, not total requests: size
repairs and unsuccessful attempts are not recorded as separate nodes. Some
internal summaries cover earlier notes; do not attribute all 414 to the eight
user messages alone. The store has no sufficient usage ledger to determine
the share of subscription quota consumed by each subsystem.

## Findings

### 1. High: eager construction of the entire tree

`memory/src/view.rs`, `Mem::due` (HEAD lines 186–208), schedules every ready,
unbuilt node. `compact.rs`, `pump` (269–283), launches those jobs regardless of
whether the view needs to shrink. `Session::run` pumps while chatting and while
waiting for input (`chat/src/session.rs:154`).

For N long messages whose summaries are large enough to prevent free joins,
the tree needs N leaf jobs and `N − popcount(N)` internal jobs. For 128 messages
this is **255 Sonnet jobs**, while 128 × 512 bytes is only 65,536 bytes, well
below the view budget. Each job can require up to five size-repair requests.
This is an illustrative calculation from the scheduler, not a measured run.

Build internal nodes when folding needs them, with bounded lookahead if useful.
Zoom could return available children while an optional deeper summary is absent.
Changing the scheduler requires adjusting the current assumptions that every
node will eventually exist; it is not just deleting one pump call.

### 2. High: tool traffic gets the same expensive treatment as durable messages

The committed mapper records calls and results separately
(`chat/src/stream.rs:120,137`). Long records become model jobs; short records
still contribute to later pair merges. The observed 264 tool records dominate
the 300 chat records.

The pending working tree now merges each call with its result and represents
tool-only leaves as empty/free nodes, omitted from the view (`Message::line`,
`Store::push`, `joined`, `Mem::lines`). That should remove direct tool
summarization and many associated merges after the store is rebuilt. It is a
substantial mitigation, beyond merely merging two raw messages. It does not
change the eager scheduling of visible-message ancestors or the full-context
cost of their model jobs.

Keep raw records and provenance recoverable. The assistant must preserve
useful outcomes in its replies when tools disappear from the overview. Verify
this behavior with real tasks, including cancellation and failed tools.

### 3. High: large context replay for very small summary tasks

`compact.rs:249–263` obtains all preceding view lines for a leaf and all view
lines ending by the parent for a merge. `message` wraps them in `<chat>` and
adds the compression task (`81–93`). Each job opens a fresh Sonnet/medium
conversation (`summarize:111–126`, `claude.rs:308–314`). A job merging roughly
1 KB can therefore send nearly the entire 128 KB memory view.

Cache marks start at 50,000 characters, then 80,000 and 100,000. Below the
first mark, a changing context is one cached block ending at `</chat>`.
Appending a new summary changes content before that old endpoint, so the old
endpoint cannot provide a hit for the unchanged earlier lines. Repeated
identical contexts and retries can still hit. Eight concurrent jobs can also
race before a shared prefix has been cached.

Use stable checkpoints appropriate to the actual view size. Bound or batch
compaction context and work, preserving enough context to resolve references;
the original spec explicitly introduced context to improve summary quality.
Benchmark quality before replacing it with isolated, context-free summaries.

The prior research already flagged this risk:
`20261004T172045--shitty-optchat.md`, section 4, reports external warm probes
around 7.3–7.7k input-token equivalents per job and estimates compaction could
outweigh the master turn. Those are historical API-cost proxies from a
different implementation, not measurements of this subscription's quota.

### 4. High: failure retries have no total limit

`summarize` permits five requests to repair summary length (`compact.rs:111–137`).
Failures then restart the job indefinitely after a fixed ten seconds
(`finish:301–305`, `step:327–334`). The first error is reported, while later
failures of the same node are suppressed. There is no circuit breaker for
quota exhaustion, repeated refusal or a permanently invalid request.

Failed attempts may be rejected without charge, but a failure after successful
requests can repeatedly consume real usage. No local evidence establishes
that such a retry loop caused the user's reported incident.

Classify failures, stop permanent errors, back off transient ones, and cap
per-node and per-session attempts. A quota error should pause model work.
Also, after five oversized replies the shortest is accepted even if it still
exceeds 512 bytes; the output-size target is not a guaranteed bound.

### 5. Medium: speculative priming spends usage without ensuring reuse

`Session::advance` primes a changed, fully built view after one idle second
(`session.rs:239–251`). `Primer::fresh` requires exact view equality and age
under 270 seconds (`prime.rs:9,46–49`). A completed turn can therefore cause a
new Opus request even if the user never sends another message. Later folding
or another session's notes can make the primed view obsolete.

Priming is not inherently wasteful: previous probes found it improves warm
cross-turn caching. The flaw is unconditional speculative work and absent
measurement of whether each prime benefits a later turn. Cache success is also
assumed on any `message_start`, even if both cache counts are zero
(`prime.rs:98–103,127–131`).

Make speculative priming optional or demand-driven and record the hit gained
by the real turn. Avoid priming during an unfinished compaction backlog.
Do not remove all priming blindly: the historical Claude Code probes found
automatic cache marks prevented adding the view marks to real tool turns.

### 6. Medium: subscription caching is forced to a short lifetime

Every child sets `CLAUDE_CODE_PROMPT_CACHE_TTL=5m` (`memory/src/claude.rs:60`),
and explicit marks omit `ttl` (`user_message:367–369`), also selecting five
minutes. Returning after a longer pause requires rewriting cached prefixes.

Current [Claude Code documentation](https://code.claude.com/docs/en/prompt-caching#which-ttl-each-request-gets)
says main conversations, including `-p` runs, normally request one-hour caching
on included subscription usage. This wrapper overrides that behavior.

Evaluate coordinated one-hour marks for included subscription usage. Changing
only the environment variable is insufficient: explicit marks and automatic
marks must have compatible TTLs. Pay-as-you-go usage has a different tradeoff,
so make this configurable rather than assuming one hour always costs less.

### 7. Medium: usage reporting omits substantial work

`Usage::of` parses only the master turn's final result
(`chat/src/ui/mod.rs:20–31`). The compactor returns text while discarding its
usage fields (`memory/src/claude.rs:396–412`). Priming captures only read/write
counts at `message_start`, and the TUI drops even those counts
(`chat/src/tui/app.rs:91`). Cancelled requests can have no final usage record.

Add a ledger for master, prime, leaf summary, merge and retry requests, including
model, input/read/write/output tokens, cache TTL and incomplete/cancelled status.
Display session totals and background totals. If estimating monetary cost,
label it as an API-price estimate, not subscription quota accounting.

## Recommended sequence

1. Finish and validate the pending tool-hiding migration. Preserve raw data.
2. Instrument every model call and bound retries. This makes savings verifiable.
3. Make internal tree construction demand-driven and batch summary work.
4. Make the chat view budget configurable; test a smaller overview plus useful
   recent context and zoom access. The same 128 KB global view is currently sent
   to every turn, including unrelated tasks and repositories.
5. Measure adaptive cache checkpoints, speculative-prime reuse and coordinated
   subscription TTLs. Keep Opus available for difficult work; offer a routine
   Sonnet profile instead of defaulting all work to Opus/high (`cli.rs:26–29`).

The largest verified waste is unnecessary summary jobs and tool traffic.
Changing the master model alone leaves those design problems in place.

## Validation and limits

Read committed source, compared the changing working tree, inspected the
recorded stream fixtures and existing `nap.log`, and counted message/node
provenance from one pinned Git snapshot. Checked the synthetic tree counts
arithmetically. No application code was changed and no live model requests
were needed.

[Anthropic's cache documentation](https://platform.claude.com/docs/en/build-with-claude/prompt-caching)
confirms exact prefix matching, writes becoming available when the response
begins, and the need to wait before launching parallel requests that rely on a
new cache entry. The wrapper's `DISABLE_PROMPT_CACHING=1` suppresses Claude Code's
automatic marks; earlier local probes show explicit caller marks still work.
It must not be described as disabling all caching.

No exact subscription savings percentage can be established from the available
data. Request counts, cached-token totals and API-equivalent prices are distinct
from the user's subscription utilization.

## Changelog

- 2026-10-07 @codex (GPT-6): reviewed subscription request amplification, counted
  the live store, and distinguished installed behavior from pending changes.
