"""Blind judge: for every node either run built with the model, one judge call compares the two
runs' lines, each against its own two sources, in random order.

Usage: judge.py <tree-x> <tree-y> <out> [--judge opus|deepseek-v4-pro] [--seed <n>] [--order <dir>]:
two trees extracted with `git archive`; verdicts, tallies and the spot-check file go in <out>.
`--seed` picks the random A/B order (default the first runs'); `--order` gives each node the
A/B order another judge's verdicts in <dir> used. deepseek-v4-pro reads
DEEPSEEK_API_KEY.
"""

import argparse
import json
import os
import random
import subprocess
import sys
import threading
import urllib.error
import urllib.request
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

import prices
from keep import level, levels

E = Path(__file__).resolve().parent
RULES = E.parents[2] / "src/memory/prompts/compact.txt"
# deepseek-v4-pro allows 500 concurrent requests per account (api-docs.deepseek.com/quick_start/rate_limit, 2026-10-27).
JOBS = {"opus": 6, "deepseek-v4-pro": 64}
SEED = 20261026
CATS = ["unsupported", "contradictions", "meaning_changed", "dropped_negations_or_rules", "facts_lost"]
# Set by the first failed call (a session limit fails every call after it): no new calls start.
FAILED = threading.Event()


def name(t):
    return Path(t).name.removeprefix("t-")


def sources(tree, l, i):
    below = level(tree, l - 1)
    return [below[2 * i][1], below[2 * i + 1][1]]


def message(pair):
    out = []
    for label in "AB":
        p = pair[label]
        out.append(f"Summary {label}'s sources:\n1. {p['sources'][0]}\n2. {p['sources'][1]}\n\nSummary {label}:\n{p['line']}\n")
    return "\n".join(out)


def ask_opus(system, text):
    """Opus 5.5 through `claude -p`, at high effort, without tools or the user's settings."""
    cmd = ["claude", "-p", "--model", "claude-opus-5-5", "--effort", "high", "--no-session-persistence", "--setting-sources", "",
           "--strict-mcp-config", "--system-prompt-file", str(system), "--tools", "", "--safe-mode", "--output-format", "json"]
    r = subprocess.run(cmd, input=text, capture_output=True, text=True, timeout=600)
    if r.returncode:
        raise RuntimeError(r.stderr[-2000:] or r.stdout[-2000:])
    out = json.loads(r.stdout)
    # A list of every event when Claude Code runs verbose; the result is the last.
    res = next(e for e in out if e.get("type") == "result") if isinstance(out, list) else out
    return dict(result=res["result"], usage=res.get("usage"), cost=res.get("total_cost_usd"))


def ask_deepseek(system, text, model="deepseek-v4-pro"):
    """DeepSeek's chat API, thinking on at high effort, JSON output."""
    body = {"model": model, "messages": [{"role": "system", "content": system.read_text()}, {"role": "user", "content": text}],
            "thinking": {"type": "enabled"}, "reasoning_effort": "high", "response_format": {"type": "json_object"},
            "max_tokens": 32768, "stream": False}
    req = urllib.request.Request("https://api.deepseek.com/chat/completions", data=json.dumps(body).encode(),
                                 headers={"Content-Type": "application/json", "Authorization": "Bearer " + os.environ["DEEPSEEK_API_KEY"]})
    try:
        with urllib.request.urlopen(req, timeout=900) as r:
            d = json.load(r)
    except urllib.error.HTTPError as e:
        raise RuntimeError(f"HTTP {e.code}: {e.read().decode(errors='replace')[:2000]}") from None
    msg, u = d["choices"][0]["message"], d["usage"]
    return dict(result=msg["content"], reasoning=msg.get("reasoning_content"), usage=u, created=d["created"],
                model=d["model"], cost=prices.cost(model, d["created"], u))


ASK = {"opus": ask_opus, "deepseek-v4-pro": ask_deepseek}


def judge(ask, system, out, pair):
    path = out / "verdicts" / f"{pair['node']}.json"
    if path.exists():
        return json.loads(path.read_text())
    for attempt in range(3):
        if FAILED.is_set():
            return None
        try:
            raw = ask(system, message(pair))
        except Exception as e:
            FAILED.set()
            print(f"{pair['node']}: {e}", file=sys.stderr)
            return None
        text = raw["result"].strip().removeprefix("```json").removeprefix("```").removesuffix("```").strip()
        try:
            verdict = json.loads(text)
            assert verdict["better"] in ("A", "B", "same") and all(c in verdict[k] for k in "AB" for c in CATS)
            break
        except (ValueError, KeyError, AssertionError):
            if attempt == 2:
                raise
    rec = dict(pair, raw=raw["result"], **{k: v for k, v in raw.items() if k != "result"}, verdict=verdict)
    path.write_text(json.dumps(rec, indent=1, ensure_ascii=False))
    return rec


def pairs(x, y, seed=SEED, order=None):
    """Every node either run built with the model, its runs as A and B: shuffled, or as the
    verdicts in `order` had them."""
    rng = random.Random(seed)
    for l in levels(x)[1:]:
        lx, ly = level(x, l), level(y, l)
        for i in sorted(lx):
            # A run stopped before its top lines leaves those nodes out.
            if i not in ly or not (lx[i][0] or ly[i][0]):
                continue
            node = f"{i << l}+{1 << l}"
            runs = [(name(x), lx[i][1], sources(x, l, i)), (name(y), ly[i][1], sources(y, l, i))]
            rng.shuffle(runs)
            if order:
                then = json.loads((Path(order) / "verdicts" / f"{node}.json").read_text())
                runs.sort(key=lambda r: then["B"]["run"] == r[0])
                assert [r[1] for r in runs] == [then["A"]["line"], then["B"]["line"]], node
            yield {"node": node, "level": l, **{lab: dict(run=r, line=t, sources=s) for lab, (r, t, s) in zip("AB", runs)}}


def faults(v, lab):
    return sum(len(v[lab][c]) for c in CATS[:4])


def gap(r):
    """How far apart the judge put the pair: 0 for a tie with equal fault counts."""
    v = r["verdict"]
    return (v["better"] != "same") + abs(faults(v, "A") - faults(v, "B"))


def spot(recs, out, seed=SEED):
    rng = random.Random(seed)
    differ = [r for r in recs if r["verdict"]["better"] != "same" and faults(r["verdict"], "A") != faults(r["verdict"], "B")]
    first = rng.sample(differ, min(10, len(differ)))
    rest = [r for r in recs if r not in first]
    rng.shuffle(rest)
    second = sorted(rest, key=gap)[: 20 - len(first)]
    order = lambda r: (r["level"], int(r["node"].split("+")[0]))
    pick = sorted(first, key=order) + sorted(second, key=order)
    doc = ["# Spot-check: 20 node pairs\n",
           f"Pairs 1-{len(first)}: the judge preferred one line and found more faults in the other. "
           f"Pairs {len(first) + 1}-{len(pick)}: the judge found the least difference.\n",
           "Each pair is shown as the judge saw it, as A and B in random order. Read both against their sources, decide, "
           "then compare with the judge's findings. Which model wrote A and B is in the key at the end.\n"]
    for k, r in enumerate(pick, 1):
        v = r["verdict"]
        doc.append(f"## {k}. Node {r['node']} (level {r['level']}, {1 << r['level']} notes)\n")
        for lab in "AB":
            p = r[lab]
            doc.append(f"**{lab}'s sources**\n\n1. {p['sources'][0]}\n2. {p['sources'][1]}\n\n**{lab}** ({len(p['line'])} characters)\n\n> {p['line']}\n")
        doc.append(f"**Judge:** better = {v['better']}. {v['why']}\n")
        for lab in "AB":
            found = [f"{c}: " + "; ".join(v[lab][c]) for c in CATS if v[lab][c]]
            doc.append(f"- {lab}: " + (" | ".join(found) if found else "no findings"))
        doc.append("")
    doc.append("## Key\n")
    doc += [f"{k}. A = {r['A']['run']}, B = {r['B']['run']}" for k, r in enumerate(pick, 1)]
    (out / "spot-check.md").write_text("\n".join(doc) + "\n")


def tally(recs):
    t = {}
    for r in recs:
        v = r["verdict"]
        for lab in "AB":
            m = t.setdefault(r[lab]["run"], {"items": dict.fromkeys(CATS, 0), "nodes": dict.fromkeys(CATS, 0), "better": 0, "nodes_with_any": 0})
            for c in CATS:
                m["items"][c] += len(v[lab][c])
                m["nodes"][c] += bool(v[lab][c])
            m["nodes_with_any"] += any(v[lab][c] for c in ("unsupported", "contradictions", "meaning_changed", "dropped_negations_or_rules"))
            m["better"] += v["better"] == lab
    t["same"] = sum(r["verdict"]["better"] == "same" for r in recs)
    t["nodes"] = len(recs)
    t["judge_cost"] = sum(r["cost"] or 0 for r in recs)
    return t


if __name__ == "__main__":
    a = argparse.ArgumentParser()
    a.add_argument("x")
    a.add_argument("y")
    a.add_argument("out", type=Path)
    a.add_argument("--judge", choices=list(ASK), default="opus")
    a.add_argument("--seed", type=int, default=SEED)
    a.add_argument("--order")
    args = a.parse_args()
    out = args.out
    (out / "verdicts").mkdir(parents=True, exist_ok=True)
    system = out / "judge-prompt.txt"
    system.write_text((E / "judge.txt").read_text() + RULES.read_text())
    (out / "seed.txt").write_text(f"{args.seed}\n")
    todo = list(pairs(args.x, args.y, args.seed, args.order))
    with ThreadPoolExecutor(JOBS[args.judge]) as pool:
        recs = list(pool.map(lambda p: judge(ASK[args.judge], system, out, p), todo))
    if FAILED.is_set():
        sys.exit(f"Stopped after a failed call: {sum(r is not None for r in recs)} of {len(todo)} nodes judged; rerun to judge the rest.")
    (out / "tally.json").write_text(json.dumps(tally(recs), indent=1))
    spot(recs, out, args.seed)
    print(json.dumps(tally(recs), indent=1))
