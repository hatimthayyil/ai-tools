import statistics
import subprocess
import sys
from collections import defaultdict
from pathlib import Path, PurePosixPath

FLOOR = 512
FILE_RATIO = 10
INSTRUCTION = 200


def tracked_text_files(root):
    names = subprocess.run(
        ["git", "-C", root, "ls-files", "-z"], capture_output=True, check=True
    ).stdout.split(b"\0")
    files = {}
    for name in filter(None, names):
        data = (Path(root) / name.decode()).read_bytes()
        if b"\0" not in data[:8192]:
            files[PurePosixPath(name.decode())] = len(data)
    return files


def file_cap(size):
    return max(FLOOR, size // FILE_RATIO)


def passes(total, cap):
    outputs = []
    size = total
    while True:
        size = max(cap, size // 2)
        outputs.append(size)
        if size == cap:
            return outputs


def iterated_cost(total, outputs):
    fresh = cached = 0
    prefix = 0
    for i, out in enumerate(outputs):
        if i == 0:
            fresh += total + INSTRUCTION
            prefix = total + INSTRUCTION
        else:
            fresh += outputs[i - 1] + INSTRUCTION
            cached += prefix
            prefix += outputs[i - 1] + INSTRUCTION
    return sum(outputs), fresh, cached


def build(files):
    children = defaultdict(set)
    for path in files:
        for parent, child in zip(path.parents, [path, *path.parents]):
            children[parent].add(child)
    caps = {path: file_cap(size) for path, size in files.items()}
    dirs = {}
    for d in sorted(children, key=lambda p: -len(p.parts)):
        total = sum(caps[c] for c in children[d])
        n = len(children[d])
        cap = max(FLOOR, total // n)
        outputs = passes(total, cap)
        caps[d] = cap
        dirs[d] = dict(n=n, total=total, cap=cap, outputs=outputs,
                       iterated=iterated_cost(total, outputs),
                       one_step=(cap, total + INSTRUCTION, 0))
    return caps, dirs


def edit_cost(path, files, dirs, mode):
    written, fresh, cached = file_cap(files[path]), files[path] + INSTRUCTION, 0
    for d in path.parents:
        w, f, c = dirs[d][mode]
        written, fresh, cached = written + w, fresh + f, cached + c
    return written, fresh, cached


def kb(n):
    return f"{n / 1024:,.1f}"


def main():
    root = sys.argv[1] if len(sys.argv) > 1 else "."
    files = tracked_text_files(root)
    caps, dirs = build(files)
    top = PurePosixPath(".")

    print(f"Text files: {len(files)}, {kb(sum(files.values()))} KB; directories: {len(dirs)}\n")

    print("| depth | dirs | mean children | mean S KB | mean T KB | max T KB | passes (min-max) |")
    print("|---|---|---|---|---|---|---|")
    by_depth = defaultdict(list)
    for d, v in dirs.items():
        by_depth[0 if d == top else len(d.parts)].append(v)
    for depth in sorted(by_depth):
        vs = by_depth[depth]
        ks = [len(v["outputs"]) for v in vs]
        print(f"| {depth} | {len(vs)} | {statistics.mean(v['n'] for v in vs):.1f} "
              f"| {kb(statistics.mean(v['total'] for v in vs))} "
              f"| {kb(statistics.mean(v['cap'] for v in vs))} "
              f"| {kb(max(v['cap'] for v in vs))} | {min(ks)}-{max(ks)} |")

    hist = defaultdict(int)
    for v in dirs.values():
        hist[len(v["outputs"])] += 1
    print("\nPasses per directory: " + ", ".join(f"{k}: {hist[k]}" for k in sorted(hist)))

    r = dirs[top]
    print(f"\nRoot: {r['n']} children, S {kb(r['total'])} KB, T {kb(r['cap'])} KB, "
          f"{len(r['outputs'])} passes")
    big = max(dirs, key=lambda d: dirs[d]["total"])
    print(f"Largest directory input: {big} ({dirs[big]['n']} children), S {kb(dirs[big]['total'])} KB, "
          f"{len(dirs[big]['outputs'])} passes")

    file_w = sum(file_cap(s) for s in files.values())
    file_f = sum(s + INSTRUCTION for s in files.values())
    print("\nFull build, KB (file calls are the same in both):\n")
    print("| | calls | written | fresh input | cached input |")
    print("|---|---|---|---|---|")
    print(f"| files | {len(files)} | {kb(file_w)} | {kb(file_f)} | 0 |")
    for mode in ("iterated", "one_step"):
        calls = sum(len(v["outputs"]) if mode == "iterated" else 1 for v in dirs.values())
        w = sum(v[mode][0] for v in dirs.values())
        f = sum(v[mode][1] for v in dirs.values())
        c = sum(v[mode][2] for v in dirs.values())
        print(f"| dirs, {mode.replace('_', '-')} | {calls} | {kb(w)} | {kb(f)} | {kb(c)} |")

    costs = sorted(files, key=lambda p: edit_cost(p, files, dirs, "iterated")[0])
    typical = costs[len(costs) // 2]
    largest = max(files, key=files.get)
    print("\nOne-file edit (file plus every directory above it), KB:\n")
    print("| file | size KB | mode | written | fresh input | cached input |")
    print("|---|---|---|---|---|---|")
    for label, path in (("median edit", typical), ("largest file", largest)):
        for mode in ("iterated", "one_step"):
            w, f, c = edit_cost(path, files, dirs, mode)
            print(f"| {label}: `{path}` | {kb(files[path])} | {mode.replace('_', '-')} "
                  f"| {kb(w)} | {kb(f)} | {kb(c)} |")


if __name__ == "__main__":
    main()
