"""What one summariser run took and how its lines came out.

Usage: stats.py <store> <tree>: <store> is the memory copy the run built (its calls/), <tree>
the same store's tree extracted with `git archive`.
"""

import glob
import json
import re
import sys

import prices
from keep import level, levels

LIMIT = 512
FILLER = {"lorem", "ipsum", "dolor", "consectetur", "adipiscing", "eiusmod", "tempor", "incididunt", "aliqua", "nostrud", "ullamco", "reprehenderit", "voluptate", "cillum"}


def role(call):
    """Who a conversation's `ai.call` header opens: the witness or the summariser."""
    return "witness" if call.get("system", "").startswith("Salam. Your task is to witness") else "summariser"


def calls(store):
    """What the calls in `store`'s calls/ took, per role. `retries` counts every request after a
    conversation's first: a summariser's length retries and its answers to a witness."""
    out = {}
    for p in glob.glob(f"{store}/calls/*.jsonl"):
        n = 0
        session_cost = 0.0
        s = None
        for line in open(p):
            d = json.loads(line)
            if d.get("type") == "ai.call":
                s = out.setdefault(role(d), dict(conversations=0, requests=0, retries=0, uncached=0, cache_write=0, cache_read=0, output=0, thinking=0, cost=0.0))
            elif s is None:
                s = out.setdefault("summariser", dict(conversations=0, requests=0, retries=0, uncached=0, cache_write=0, cache_read=0, output=0, thinking=0, cost=0.0))
            if d.get("type") == "ai.reply" and "usage" in d:
                b = d["body"]
                u = b["usage"]
                hit = u.get("prompt_cache_hit_tokens", 0)
                s["uncached"] += u["prompt_tokens"] - hit
                s["cache_read"] += hit
                s["output"] += u["completion_tokens"]
                s["thinking"] += (u.get("completion_tokens_details") or {}).get("reasoning_tokens", 0)
                s["cost"] += prices.cost(b["model"], b["created"], u)
                n += 1
            elif d.get("type") == "result":
                u = d["usage"]
                s["uncached"] += u["input_tokens"]
                s["cache_write"] += u["cache_creation_input_tokens"]
                s["cache_read"] += u["cache_read_input_tokens"]
                s["output"] += u["output_tokens"]
                s["thinking"] += (u.get("output_tokens_details") or {}).get("thinking_tokens", 0)
                # Claude Code's total_cost_usd is the session's running total; its usage is per turn.
                session_cost = d["total_cost_usd"]
                n += 1
        s["cost"] += session_cost
        s["conversations"] += 1
        s["requests"] += n
        s["retries"] += max(n - 1, 0)
    return out


def witness(store):
    """The witness's rounds per line, from summarise.log: how many lines took each number of
    rounds of findings, the findings, and the answers that contested one."""
    rounds, findings, contested, answers = {}, 0, 0, 0
    for line in open(f"{store}/summarise.log"):
        if " witnessed " in line:
            r = int(re.search(r" rounds=(\d+)", line).group(1))
            rounds[r] = rounds.get(r, 0) + 1
        elif " witness found faults" in line:
            findings += int(re.search(r" findings=(\d+)", line).group(1))
        elif " summariser answered" in line:
            answers += 1
            contested += " contested=true" in line
    return dict(lines_by_rounds=dict(sorted(rounds.items())), findings=findings, answers=answers, contested=contested)


def lines(tree):
    leaves = level(tree, 0)
    s = dict(lines=0, over=0, longest=0, unpunctuated=0, filler=0)
    for l in levels(tree)[1:]:
        for i, (model, text) in level(tree, l).items():
            if not model:
                continue
            src = " ".join(leaves[j][1] for j in range(i << l, (i + 1) << l) if j in leaves).lower()
            n = len(text)
            s["lines"] += 1
            s["over"] += n > LIMIT
            s["longest"] = max(s["longest"], n)
            s["unpunctuated"] += not text.rstrip().endswith((".", "!", "?", ")", '"', "'", "`"))
            words = set(text.lower().replace(",", " ").replace(".", " ").split())
            s["filler"] += any(w in words and w not in src for w in FILLER)
    return s


if __name__ == "__main__":
    store, tree = sys.argv[1:3]
    out = {"calls": calls(store), "lines": lines(tree)}
    if "witness" in out["calls"]:
        out["witness"] = witness(store)
    print(json.dumps(out, indent=1))
