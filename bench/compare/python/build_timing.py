"""Time only the query build of the CPU-split cases, no database.

    python build_timing.py run [--repeat 9] [--number 2000]
    python build_timing.py ab PYTHON_A PYTHON_B [--rounds 4]

`run` prints one JSON line: per case the median of `--repeat` runs, in microseconds per call, with the
1-minute load. `ab` runs `run` under both interpreters in turn (A, B, A, B, ...) and prints a table.
The builders are copied from monorm_contender.py. The package name is `monorm`, or `orm` before the rename.
"""

from __future__ import annotations

import argparse
import importlib
import json
import os
import statistics
import subprocess
import sys
import timeit
from pathlib import Path
from typing import Any

HERE = Path(__file__).resolve().parent
SCHEMA = HERE.parent / "schema.prisma"
CASES = ("read-pk", "filter", "filter-ne-null", "read-50")


def pk_for(i: int) -> int:
    return (i * 7919) % 10_000 + 1


def builders() -> dict[str, Any]:
    try:
        m: Any = importlib.import_module("monorm")
    except ImportError:  # a build from before the package rename
        m = importlib.import_module("orm")
    models = m.loads(SCHEMA.read_text())
    Post = models["Post"]

    def read_pk(i: int) -> Any:
        return Post.objects.get(Post.id == pk_for(i))

    def filter_(i: int, threshold: int = 500) -> Any:
        return Post.objects.filter(Post.published, Post.views > threshold).order_by(-Post.views, Post.id).limit(20)

    def filter_ne_null(i: int) -> Any:
        return Post.objects.filter((Post.category_id != 3).or_null()).order_by(Post.id)[(i % 9) * 20 : (i % 9) * 20 + 20]

    def read_50(i: int) -> Any:
        o = (i % 9) * 50
        return Post.objects.order_by(Post.id)[o : o + 50]

    return {"read-pk": read_pk, "filter": filter_, "filter-ne-null": filter_ne_null, "read-50": read_50}


def load1() -> float:
    return os.getloadavg()[0]


def run(repeat: int, number: int) -> dict[str, Any]:
    cases = builders()
    out: dict[str, Any] = {}
    loads = [load1()]
    for name in CASES:
        build = cases[name]
        counter = iter(range(10**9))
        timeit.repeat(lambda: build(next(counter)), number=number // 4, repeat=1)  # warm up
        runs = [t / number * 1e6 for t in timeit.repeat(lambda: build(next(counter)), number=number, repeat=repeat)]
        out[name] = {"median_us": statistics.median(runs), "min_us": min(runs), "max_us": max(runs)}
        loads.append(load1())
    return {"python": sys.version.split()[0], "load_max": max(loads), "cases": out}


def ab(py_a: str, py_b: str, rounds: int, repeat: int, number: int) -> None:
    res: dict[str, list[dict[str, Any]]] = {"A": [], "B": []}
    for r in range(rounds):
        for label, py in (("A", py_a), ("B", py_b)):
            p = subprocess.run(
                [py, str(Path(__file__)), "run", "--repeat", str(repeat), "--number", str(number)],
                capture_output=True, text=True, check=True, cwd="/tmp",  # not the repo root: no local package shadows the installed one
            )
            res[label].append(json.loads(p.stdout.splitlines()[-1]))
            print(f"round {r + 1} {label}: load {res[label][-1]['load_max']:.2f}", file=sys.stderr)
    print("| case | A median (µs) [min, max of rounds] | B median (µs) [min, max of rounds] | B/A | max load |")
    print("|---|---|---|---|---|")
    for name in CASES:
        a = [x["cases"][name]["median_us"] for x in res["A"]]
        b = [x["cases"][name]["median_us"] for x in res["B"]]
        load = max(x["load_max"] for x in res["A"] + res["B"])
        ma, mb = statistics.median(a), statistics.median(b)
        print(f"| {name} | {ma:.2f} [{min(a):.2f}, {max(a):.2f}] | {mb:.2f} [{min(b):.2f}, {max(b):.2f}] | {mb / ma:.2f}× | {load:.2f} |")


def main() -> None:
    ap = argparse.ArgumentParser()
    sub = ap.add_subparsers(dest="cmd", required=True)
    r = sub.add_parser("run")
    r.add_argument("--repeat", type=int, default=9)
    r.add_argument("--number", type=int, default=2000)
    a = sub.add_parser("ab")
    a.add_argument("python_a")
    a.add_argument("python_b")
    a.add_argument("--rounds", type=int, default=4)
    a.add_argument("--repeat", type=int, default=9)
    a.add_argument("--number", type=int, default=2000)
    args = ap.parse_args()
    if args.cmd == "run":
        print(json.dumps(run(args.repeat, args.number)))
    else:
        ab(args.python_a, args.python_b, args.rounds, args.repeat, args.number)


if __name__ == "__main__":
    main()
