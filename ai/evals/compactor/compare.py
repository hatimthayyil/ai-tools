"""Two judges on one comparison: each judge's counts per category, position bias, cost, and how
often the two picked the same line.

Usage: compare.py <judge-dir> <judge-dir>: two `judge.py` outputs over the same two trees. Only
the nodes both judged are compared.
"""

import glob
import json
import sys
from pathlib import Path

from judge import CATS


def load(d):
    return {Path(p).stem: json.loads(Path(p).read_text()) for p in glob.glob(f"{d}/verdicts/*.json")}


def pick(r):
    """The run the judge preferred, or "same"."""
    b = r["verdict"]["better"]
    return b if b == "same" else r[b]["run"]


def summary(recs):
    runs = sorted({r[lab]["run"] for r in recs.values() for lab in "AB"})
    s = {m: {c: 0 for c in CATS + ["any of the first four", "judged better"]} for m in runs}
    pos = {"A": 0, "B": 0, "same": 0}
    for r in recs.values():
        v = r["verdict"]
        pos[v["better"]] += 1
        for lab in "AB":
            m = s[r[lab]["run"]]
            for c in CATS:
                m[c] += bool(v[lab][c])
            m["any of the first four"] += any(v[lab][c] for c in CATS[:4])
            m["judged better"] += v["better"] == lab
    cost = sum(r.get("cost") or 0 for r in recs.values())
    return runs, s, pos, cost


if __name__ == "__main__":
    full = [load(d) for d in sys.argv[1:3]]
    # A judge stopped early leaves nodes the other judged; compare only the shared ones.
    both = full[0].keys() & full[1].keys()
    a, b = ({n: f[n] for n in both} for f in full)
    for n in a:
        assert all(a[n][lab]["run"] == b[n][lab]["run"] for lab in "AB"), f"{n}: different A/B order"
    (runs, sa, pa, _), (_, sb, pb, _) = summary(a), summary(b)
    ja, jb = (Path(d).name for d in sys.argv[1:3])
    print(f"| nodes with | " + " | ".join(f"{m}, {j}" for m in runs for j in (ja, jb)) + " |")
    print("|-|" + "-|" * (2 * len(runs)))
    for c in CATS + ["any of the first four", "judged better"]:
        print(f"| {c} | " + " | ".join(str(s[m][c]) for m in runs for s in (sa, sb)) + " |")
    agree = sum(pick(a[n]) == pick(b[n]) for n in a)
    print(f"\nnodes judged by both: {len(a)} ({ja} {len(full[0])}, {jb} {len(full[1])})")
    print(f"same pick: {agree} ({100 * agree / len(a):.0f}%)")
    for j, p, f in ((ja, pa, full[0]), (jb, pb, full[1])):
        print(f"{j}: preferred A {p['A']}, B {p['B']}, same {p['same']}; cost of all its verdicts ${summary(f)[3]:.2f}")
