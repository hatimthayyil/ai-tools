"""Structure-only cost model for step / exchange-digest / per-message compaction leaves.

Reads Claude Code JSONL logs read-only and prints aggregates only (counts and
byte sizes). Never prints message content.

usage: uv run --no-project python -I stepcost.py main|sub
"""
import glob, json, math, os, sys

FREE = 512            # node size; a source that fits is its own node, no call
CAP = 100_000         # compactor input cap; bigger inputs are chunked
CLIPS = (30_000, 8_000, 2_000)
LEAF_B = 8_192        # scheme (b) digest leaf size
NOISE = {"TodoWrite", "ToolSearch", "TaskCreate", "TaskUpdate", "TaskList", "TaskGet", "TaskOutput"}
REPORT = {"SubagentHandback", "SendMessage", "AskUserQuestion", "ExitPlanMode"}  # input is prose
DROP = ("[Request interrupted", "/compact")
NOT_HUMAN = ("<local-command-stdout", "<local-command-stderr", "<bash-stdout", "<bash-stderr",
             "<local-command-caveat", "<system-reminder", "<task-notification") + DROP
SALIENT = {"Bash": "command", "Read": "file_path", "Write": "file_path", "Edit": "file_path",
           "MultiEdit": "file_path", "NotebookEdit": "notebook_path", "Glob": "pattern",
           "Grep": "pattern", "WebFetch": "url", "WebSearch": "query", "Task": "description",
           "Agent": "description", "Skill": "skill"}
MAIN = sys.argv[1] == "main"


def blen(s): return len(s.encode("utf-8", "replace"))
def clip(s, c): return s if len(s) <= c else s[: c // 2] + s[-(c - c // 2):]


def user_text(r):
    c = (r.get("message") or {}).get("content")
    if isinstance(c, str):
        return c
    if isinstance(c, list) and not any(b.get("type") == "tool_result" for b in c):
        return "\n".join(b.get("text", "") if b.get("type") == "text" else "[image]" for b in c)
    return None


def classify(r):
    """('human'|'inject'|None, text) for a user record or queued_command attachment."""
    if r.get("type") == "attachment":
        a = r.get("attachment") or {}
        if a.get("type") != "queued_command" or not isinstance(a.get("prompt"), str):
            return None, None
        return ("human" if a.get("commandMode") == "prompt" else "inject"), a["prompt"]
    txt = user_text(r)
    if txt is None or r.get("isCompactSummary") or not txt.strip() or txt.lstrip().startswith(DROP):
        return None, None
    if r.get("isMeta") or (r.get("isSidechain") and MAIN):
        return "inject", txt
    o = r.get("origin")
    if o is not None:
        return ("human" if o.get("kind") == "human" else "inject"), txt
    return ("inject" if txt.lstrip().startswith(NOT_HUMAN) else "human"), txt


def result_text(b):
    c = b.get("content")
    if isinstance(c, str):
        return c
    if isinstance(c, list):
        return "\n".join(x.get("text", "") if x.get("type") == "text" else f"[{x.get('type')}]" for x in c)
    return ""


def item(name, js, sal):
    return {"name": name, "in_raw": blen(js), "in": {c: blen(clip(js, c)) for c in CLIPS},
            "sal": min(blen(sal), 200), "key": hash((name, sal)),
            "out_raw": 0, "out": {c: 0 for c in CLIPS}, "err": 0, "has_out": False}


def set_out(k, t, err=False):
    k["out_raw"] = blen(t); k["out"] = {c: blen(clip(t, c)) for c in CLIPS}; k["has_out"] = True
    if err:
        k["err"] = min(blen(t.split("\n", 1)[0]), 120)


def call_rec(b):
    name, inp = b.get("name", "?"), b.get("input") or {}
    js = json.dumps(inp, ensure_ascii=False, separators=(",", ":"))
    key = SALIENT.get(name)
    sal = inp.get(key) if isinstance(inp, dict) and key and isinstance(inp.get(key), str) else None
    if sal is None:
        sal = next((v for v in inp.values() if isinstance(v, str)), "")[:120] if isinstance(inp, dict) else ""
    return item(name, js, sal)


def parse(path, seen):
    """Exchanges: {'u': user bytes, 'steps': [{'tb','tc','calls':[item]}]}.
    Injected entries (task notifications, peer messages, skill bodies, command
    output) ride in the current step's run as pseudo-calls named 'inject'."""
    exs, cur, step, calls = [], None, None, {}

    def ensure_step(new):
        nonlocal cur, step
        if cur is None:
            cur = {"u": 0, "steps": []}; exs.append(cur)
        if new or step is None:
            step = {"tb": 0, "tc": 0, "calls": []}; cur["steps"].append(step)

    for line in open(path, encoding="utf-8", errors="replace"):
        try:
            r = json.loads(line)
        except ValueError:
            continue
        t = r.get("type")
        if t not in ("user", "assistant", "attachment"):
            continue
        if r.get("isSidechain") and MAIN and t != "user":
            continue
        u = r.get("uuid")
        if u is not None:
            if u in seen:
                continue
            seen.add(u)
        if t in ("user", "attachment"):
            kind, txt = classify(r)
            if kind == "human":
                cur = {"u": blen(txt), "steps": []}; exs.append(cur); step = None; calls = {}
            elif kind == "inject" and cur is not None:   # pre-prompt preamble is dropped
                ensure_step(False)
                k = item("inject", "", ""); set_out(k, txt); step["calls"].append(k)
            elif t == "user":
                for b in (r.get("message") or {}).get("content") or []:
                    if isinstance(b, dict) and b.get("type") == "tool_result" and b.get("tool_use_id") in calls:
                        set_out(calls[b["tool_use_id"]], result_text(b), b.get("is_error"))
            continue
        if r.get("isApiErrorMessage"):
            continue
        for b in (r.get("message") or {}).get("content") or []:
            bt = b.get("type")
            if bt == "text":
                s = b.get("text", "")
                if not s.strip() or s.strip() == "(no content)":
                    continue
                ensure_step(step is None or bool(step["calls"]) or cur["steps"][-1] is not step)
                step["tb"] += blen(s); step["tc"] += len(s)
            elif bt == "tool_use":
                ensure_step(False)
                k = call_rec(b); calls[b.get("id")] = k; step["calls"].append(k)
    return exs

# --- renderings --------------------------------------------------------------


def step_bytes(s, c):
    return (s["tb"] + 4 if s["tb"] else 0) + sum(
        len(k["name"]) + 7 + k["in"][c] + (k["out"][c] + 8 if k["has_out"] else 0) for k in s["calls"])


def step_digest(s):
    n, prev = (s["tb"] + 4 if s["tb"] else 0), None
    for k in s["calls"]:
        if k["name"] in NOISE or k["key"] == prev:   # noise dropped; repeats fold to xN
            continue
        prev = k["key"]
        n += len(k["name"]) + 24 + k["err"] + (k["in_raw"] if k["name"] in REPORT else k["sal"])
    return n


def step_messages(s):
    m = [s["tb"]] if s["tb"] else []
    for k in s["calls"]:
        if k["name"] != "inject":
            m.append(len(k["name"]) + 1 + k["in_raw"])
        if k["has_out"]:
            m.append(k["out"][30_000])
    return m

# --- tree cost ---------------------------------------------------------------


class Cost:
    def __init__(self): self.calls = 0; self.inp = 0; self.by = {}; self.stage = "leaf"

    def add(self, n, b, stage=None):
        st = stage or self.stage
        self.calls += n; self.inp += b; self.by[st] = self.by.get(st, 0) + n


def leaf(size, cost):
    if size <= FREE:
        return size
    n = math.ceil(size / CAP)
    if n == 1:
        cost.add(1, size)
    else:
        cost.add(n, size, "chunk"); cost.add(n - 1, (n - 1) * 2 * FREE, "chunk-merge")
    return FREE


def merge(a, b, cost):
    if a + b + 1 <= FREE:
        return a + b + 1
    cost.add(1, a + b)
    return FREE


def reduce_all(nodes, cost):        # closed exchange: reduce to one root
    while len(nodes) > 1:
        nxt = [merge(nodes[i], nodes[i + 1], cost) for i in range(0, len(nodes) - 1, 2)]
        if len(nodes) % 2:
            nxt.append(nodes[-1])
        nodes = nxt
    return nodes[0]


def aligned(nodes, cost):           # OptChat tree: only complete aligned nodes
    while len(nodes) > 1:
        nodes = [merge(nodes[i], nodes[i + 1], cost) for i in range(0, len(nodes) - 1, 2)]


def split(size, cap):
    return [cap] * (size // cap) + ([size % cap] if size % cap else []) if size > cap else [size]


def scheme_a(exs, c):
    cost, ex_nodes = Cost(), []
    for e in exs:
        sizes = [step_bytes(s, c) for s in e["steps"]] or [0]
        sizes[0] += e["u"] + 6 if e["u"] else 0          # the prompt heads the first step
        cost.stage = "leaf"; ls = [leaf(x, cost) for x in sizes]
        cost.stage = "inner"; ex_nodes.append(reduce_all(ls, cost))
    cost.stage = "outer"; aligned(ex_nodes, cost)
    return cost


def scheme_a_flat(exs, c):
    """(a) with the exchange node as one call over all its step lines."""
    cost, ex_nodes = Cost(), []
    for e in exs:
        sizes = [step_bytes(s, c) for s in e["steps"]] or [0]
        sizes[0] += e["u"] + 6 if e["u"] else 0
        cost.stage = "leaf"; ls = [leaf(x, cost) for x in sizes]
        cost.stage = "exchange"
        ex_nodes.append(ls[0] if len(ls) == 1 else leaf(sum(ls) + len(ls) - 1, cost))
    cost.stage = "outer"; aligned(ex_nodes, cost)
    return cost


def scheme_b(exs):
    cost, leaves = Cost(), []
    for e in exs:
        units = ([e["u"] + 6] if e["u"] else []) + [step_digest(s) for s in e["steps"]]
        cur = 0
        for u in units:
            for piece in split(u, LEAF_B):
                if cur and cur + piece > LEAF_B:
                    leaves.append(cur); cur = 0
                cur += piece
        if cur:
            leaves.append(cur)
    ls = [leaf(x, cost) for x in leaves]
    cost.stage = "merge"; aligned(ls, cost)
    return cost


def scheme_c(exs):
    cost, msgs = Cost(), []
    for e in exs:
        if e["u"]:
            msgs += split(e["u"], CAP)
        for s in e["steps"]:
            for m in step_messages(s):
                msgs += split(m, CAP)
    ls = [leaf(x, cost) for x in msgs]
    cost.stage = "merge"; aligned(ls, cost)
    return cost

# --- report ------------------------------------------------------------------


def pct(xs, p):
    xs = sorted(xs)
    return xs[min(len(xs) - 1, int(p / 100 * len(xs)))] if xs else 0


def row(name, xs):
    if not xs:
        return f"{name}: n=0"
    return (f"{name}: n={len(xs)} p50={pct(xs,50)} p90={pct(xs,90)} p99={pct(xs,99)} "
            f"max={max(xs)} mean={sum(xs)/len(xs):.0f} sum={sum(xs)}")


def first_meta(f):
    ts, ep = "", None
    for line in open(f, encoding="utf-8", errors="replace"):
        try:
            r = json.loads(line)
        except ValueError:
            continue
        ts = ts or r.get("timestamp", "")
        ep = ep or r.get("entrypoint")
        if ts and ep:
            break
    return ts, ep


def final_reply(e):
    """Chars of the closing prose: the last text-only step (trailing injected
    entries ignored), or the input of a final report tool call; else 0."""
    for s in reversed(e["steps"]):
        real = [k for k in s["calls"] if k["name"] != "inject"]
        if not real and not s["tb"]:
            continue
        if not real:
            return s["tc"]
        return real[-1]["in_raw"] if real[-1]["name"] in REPORT else 0
    return 0


def main():
    root = os.path.expanduser("~/.claude/projects")
    files = glob.glob(f"{root}/*/*.jsonl") if MAIN else glob.glob(f"{root}/*/*/subagents/*.jsonl")
    meta = {f: first_meta(f) for f in files}
    if MAIN:
        files = [f for f in files if meta[f][1] == "cli"]
    files.sort(key=lambda f: meta[f][0])
    seen, sessions = set(), []
    for f in files:
        exs = parse(f, seen)
        if any(e["u"] for e in exs):
            sessions.append(exs)
    exs = [e for s in sessions for e in s]
    steps = [s for e in exs for s in e["steps"]]
    items = [k for s in steps for k in s["calls"]]
    inj = [k for k in items if k["name"] == "inject"]
    print(f"sessions={len(sessions)} exchanges={len(exs)} with_steps={sum(1 for e in exs if e['steps'])} "
          f"steps={len(steps)} tool_calls={len(items)-len(inj)} injected={len(inj)} "
          f"injected_bytes={sum(k['out_raw'] for k in inj)}")
    print(row("steps/exchange (>=1 step)", [len(e["steps"]) for e in exs if e["steps"]]))
    print(row("calls/step", [sum(1 for k in s["calls"] if k["name"] != "inject") for s in steps]))
    print(row("user msg bytes", [e["u"] for e in exs if e["u"]]))
    print(f"user msgs <=512B: {sum(1 for e in exs if 0 < e['u'] <= FREE)}/{sum(1 for e in exs if e['u'])}")
    print(f"text-only steps: {sum(1 for s in steps if not s['calls'])}  no-text steps: {sum(1 for s in steps if not s['tb'])}")
    to = [s["tc"] for s in steps if not s["calls"]]
    print(row("text-only step chars", to))
    for c in CLIPS:
        b = [step_bytes(s, c) for s in steps]
        print(row(f"step bytes clip={c}", b))
        print(f"  free(<=512)={sum(1 for x in b if x <= FREE)}/{len(b)} "
              f">8K={sum(1 for x in b if x > 8192)} >30K={sum(1 for x in b if x > 30000)} "
              f">100K={sum(1 for x in b if x > CAP)} bytes_in_>100K={sum(x for x in b if x > CAP)}")
    b = [step_bytes(s, 30_000) for s in steps]
    edges = [512, 1024, 2048, 8192, 30000, CAP, 10**12]
    print("step bytes buckets clip=30K:", [sum(1 for x in b if lo < x <= hi) for lo, hi in zip([-1] + edges, edges)],
          "edges", edges)
    big = [s for s in steps if step_bytes(s, 30_000) > CAP]
    print(row("calls in >100K steps (clip 30K)", [len(s["calls"]) for s in big]))
    print(row("step digest bytes", [step_digest(s) for s in steps]))
    print(f"digest free(<=512)={sum(1 for s in steps if step_digest(s) <= FREE)}/{len(steps)}")

    print("\n== cost (calls; input bytes excl. prompt template; output <= 512 B/call)")
    schemes = [(f"a two-tier clip={c}", lambda x, c=c: scheme_a(x, c)) for c in CLIPS]
    schemes += [(f"a' flat exchange node clip={c}", lambda x, c=c: scheme_a_flat(x, c)) for c in (30_000, 2_000)]
    schemes += [("b exchange-digest 8K", scheme_b), ("c per-message 30K", scheme_c)]
    for name, fn in schemes:
        per = [fn(s) for s in sessions]
        calls = [p.calls for p in per]
        by = {}
        for p in per:
            for k, v in p.by.items():
                by[k] = by.get(k, 0) + v
        print(f"{name}: calls={sum(calls)} per_exchange={sum(calls)/len(exs):.2f} "
              f"per_session p50={pct(calls,50)} p90={pct(calls,90)} max={max(calls)} "
              f"input={sum(p.inp for p in per)/1e6:.1f}MB top_session={max(calls)/sum(calls):.0%} {by}")

    print("\n== restatement proxy (raw output bytes of a step's run vs next assistant text)")
    pairs = []
    for e in exs:
        for i, s in enumerate(e["steps"]):
            if s["calls"]:
                nxt = e["steps"][i + 1]["tc"] if i + 1 < len(e["steps"]) else None
                pairs.append((s, sum(k["out_raw"] for k in s["calls"]), nxt))
    for lo in (0, 2_000, 8_000, 30_000):
        sel = [n for _, o, n in pairs if o >= lo]
        has = [n for n in sel if n is not None]
        print(f"out>={lo}: steps={len(sel)} next<200={sum(1 for n in has if n < 200)} "
              f"next>=200={sum(1 for n in has if n >= 200)} none={len(sel)-len(has)} "
              f"next_chars p50={pct(has,50)} p90={pct(has,90)}")
    print("by item in the run: steps, next<200 / with next, next p50, item out p50")
    groups = {"Agent": {"Agent", "Task"}, "inject": {"inject"}, "Web": {"WebFetch", "WebSearch"},
              "Read": {"Read"}, "Bash": {"Bash"}, "Edit/Write": {"Edit", "Write", "MultiEdit"}}
    for g, names in groups.items():
        sel = [(sum(k["out_raw"] for k in s["calls"] if k["name"] in names), n)
               for s, _, n in pairs if any(k["name"] in names for k in s["calls"])]
        has = [n for _, n in sel if n is not None]
        print(f"  {g}: steps={len(sel)} next<200={sum(1 for n in has if n < 200)}/{len(has)} "
              f"next_p50={pct(has,50)} out_p50={pct([o for o, _ in sel],50)}")
    print("exchange level: total raw output vs closing prose (text-only last step or final report tool)")
    for lo in (8_000, 30_000, 100_000):
        sel = [e for e in exs if sum(k["out_raw"] for s in e["steps"] for k in s["calls"]) >= lo]
        fin = [final_reply(e) for e in sel]
        print(f"ex out>={lo}: n={len(sel)} final<200={sum(1 for f in fin if f < 200)} "
              f"final>=500={sum(1 for f in fin if f >= 500)} final p50={pct(fin,50)} p90={pct(fin,90)}")
    tools = [k for k in items if k["name"] != "inject"]
    print(f"bytes: tool output={sum(k['out_raw'] for k in tools)/1e6:.1f}MB "
          f"injected={sum(k['out_raw'] for k in inj)/1e6:.2f}MB assistant text={sum(s['tb'] for s in steps)/1e6:.1f}MB "
          f"tool input={sum(k['in_raw'] for k in tools)/1e6:.1f}MB user={sum(e['u'] for e in exs)/1e6:.2f}MB")


main()
