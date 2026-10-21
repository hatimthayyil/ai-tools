# OptChat on Claude Code: cross-turn view cache

Research for `ai chat`. No code changed. Date: 2026-10-04.

## Summary

`ai chat` drives other harnesses; the user pays by subscription, so the agent and compactor run through `claude -p`. Subscription OAuth is refused outside Claude Code and claude.ai (Feb 2026), Agent SDK included.

Cross-turn caching of the view survives this, via a **priming call** proven by `gebeer/shitty-optchat`: a throwaway `claude -p` request writes the view into the cache with our own marks, and the real turn reads it through the API's 20-block lookback. Measured on a 128 KB view (~48k tokens, opus): 61.3k input-token equivalents per turn without priming, 23.8k with it (−61 %), 67.0k cold (+9 %).

## Why the cache is lost without it

- Anthropic writes a cache entry only at a `cache_control` mark. A later request hits only a prefix ending exactly where an earlier request put a mark (looked up from its own marks, up to 20 blocks back).
- Between turns the view keeps its first ~70 % and changes near its end (merges, new lines).
- Claude Code marks the system prompt and the end of the request. Turn N leaves entries at "end of system" and "end of turn N". Turn N+1's view diverges before turn N's end, and no entry sits at the divergence, so the whole view is rewritten (1.25×) every turn.
- The spec's fix (§8): marks inside the view at 50k/80k/100k chars. In-turn caching is unaffected either way.

## Claude Code facts (shitty-optchat probes, CC 2.1.289, OAuth)

- `cache_control` on stream-json user blocks reaches the wire verbatim.
- CC uses 3 of the 4 allowed marks in step 1 and 4 from step 2 on. A view mark makes step 2 fail with `400 … Found 5`. So no view marks in the real turn.
- `DISABLE_PROMPT_CACHING=1` removes CC's marks and keeps the caller's.
- Cache keys ignore mark placement: an unmarked request reads entries written by a marked one.
- Killing the process at the first `message_start` still leaves the entry written. Whether that request is billed: unknown, assume yes.
- `CLAUDE_CODE_PROMPT_CACHE_TTL=5m` is required: CC defaults to 1 h marks on subscriptions; mixing 1 h then 5 m is a 400.
- Isolation without `--safe-mode` (which also kills `--mcp-config`): `--setting-sources "" --strict-mcp-config` keeps hooks, plugins, skills, CLAUDE.md and auto-memory out. `--bare` refuses OAuth.
- Mid-run messages written to stdin during a tool reach the same turn at the next tool boundary (as a `role:"system"` entry). One sent during the final text step starts a follow-up turn in the same conversation: the harness must kill it to keep "one fresh call per message". `--replay-user-messages` reports when a message is consumed.

## Priming

```
prime(view):
  skip if this exact view was primed < 270 s ago
  claude -p, same model/effort/flags/tools/system prompt/MCP as the turn
    env DISABLE_PROMPT_CACHING=1
    one user message: view cut into blocks (marks at 50k/80k/100k + view end), "ok"
  kill at the first message_start
```

- Block split must be identical in the prime and the turn; only marks differ.
- Run it in the background when idle and the view is fully built (debounce ~1 s), so the turn rarely waits.
- Failure is reported once; the turn proceeds unprimed. Cost optimization only.

## Dynamic checkpoint at the divergence point

A mark helps only if an *earlier* request put it at the same prefix. A mark placed on turn N+1 at the point where it diverges from turn N writes a new entry but cannot read one: turn N never marked that spot.

Priming already gives the dynamic part: it marks the end of the *current* view, so the turn always reads the whole view. What stays fixed is where the prime itself reads from the previous prime: the last of 50k/80k/100k still unchanged. The prime writes from there to the view's end.

Refinement, untested: the fold is deterministic, so at prime time simulate the next few messages (average line size, parents assumed built) and place the inner marks just before the first lines predicted to change after ~k messages, for a few k. The next prime then reads up to the real divergence instead of the last fixed mark. Gain is bounded by the gap between the fixed marks (≤ ~30k chars, ~8k tokens written at 1.25× per turn). Measure by replay before adopting.

## Sources

- https://github.com/gebeer/shitty-optchat (SPEC.md §4, §6, §14; `docs/probes/cc-round1.md`, `cc-round2.md`; `src/prime.ts`)
- https://gist.github.com/VictorTaelin/91837951a5ce5b38f341ec1ba1df6449 (identical to `../optchat/optchat.md`)
- https://alternativeto.net/news/2026/2/anthropic-officially-bans-using-subscription-authentication-for-third-party-claude-use
