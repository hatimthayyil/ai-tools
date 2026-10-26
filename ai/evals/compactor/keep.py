import glob
import re
import sys
from pathlib import Path

FACT = re.compile(r"\b[0-9a-f]{7}\b|#\d+|\b\d+(?:\.\d+)?[A-Za-z%]*\b|[\w.-]+/[\w./*-]+|\"[^\"]{3,60}\"")


def level(t, l):
    """Node index -> (model, text) for level `l` of the extracted tree `t`; unbuilt nodes left out."""
    out = {}
    for p in sorted(glob.glob(f"{t}/tree/{l}/*/*")):
        d, f = p.split("/")[-2:]
        base = (int(d, 16) * 256 + int(f, 16)) * 256
        lines = open(p).read().split("\n")
        assert lines[0].startswith("ts "), p
        for k, line in enumerate(lines[1:]):
            if line:
                ts, origin, agent, model, session, text = line.split(" ", 5)
                out[base + k] = (model, text)
    return out


def levels(t):
    return sorted(int(Path(p).name) for p in glob.glob(f"{t}/tree/*"))


def facts(s):
    return set(FACT.findall(s))


if __name__ == "__main__":
    for t in sys.argv[1:]:
        leaves = level(t, 0)
        for l in levels(t)[1:]:
            r = []
            for i, (_, text) in level(t, l).items():
                src = set().union(*(facts(leaves[j][1]) for j in range(i << l, (i + 1) << l) if j in leaves))
                if src:
                    r.append(len(src & facts(text)) / len(src))
            if r:
                print(f"{Path(t).name} level {l} ({1 << l} notes): {100 * sum(r) / len(r):.0f}% of hard facts kept, {len(r)} lines")
