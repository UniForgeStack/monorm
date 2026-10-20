"""Turn a results directory of run.sh into summary.md.

    python report.py results/<date>-<sha>

One table per side. Per case: the median and a 95% bootstrap interval of the median over every timed sample
(all rounds pooled), and the ratio to a reference. The reference is `pg` on TypeScript (the floor: no ORM work).
Python has no floor contender, so the reference there is Django async, the like-for-like row for our async API;
`orm-all` is also compared with `orm`. A ratio below 1 means faster than the reference.
Every number is measured. The header says whether the load check was overridden.
"""

from __future__ import annotations

import json
import random
import statistics
import sys
from pathlib import Path
from typing import Any

BOOTSTRAP = 2000
Result = dict[str, dict[str, Any]]


def interval(samples: list[float]) -> tuple[float, float]:
    """95% bootstrap interval of the median; a fixed seed keeps the report reproducible."""
    if len(samples) < 2:
        return (samples[0], samples[0]) if samples else (float("nan"), float("nan"))
    rng = random.Random(0)
    meds = sorted(statistics.median(rng.choices(samples, k=len(samples))) for _ in range(BOOTSTRAP))
    return meds[int(0.025 * BOOTSTRAP)], meds[int(0.975 * BOOTSTRAP) - 1]


def fmt(x: float | None) -> str:
    if x is None:
        return "-"
    return f"{x:.3g}" if x < 100 else f"{x:.0f}"


def load(directory: Path, prefix: str = "") -> Result:
    out: Result = {}
    for path in sorted(directory.glob(f"{prefix}*.json")):
        name = path.stem
        if name.endswith("environment") or name == "run":
            continue
        if prefix:
            name = name[len(prefix):]
        elif name.startswith("bun-"):
            continue
        out[name] = json.loads(path.read_text())
    order = ["orm", "orm-all", "django-sync", "django-async", "prisma", "pg"]
    return dict(sorted(out.items(), key=lambda kv: order.index(kv[0]) if kv[0] in order else len(order)))


def case_names(results: Result) -> list[str]:
    seen: list[str] = []
    for res in results.values():
        for case in res:
            if case not in seen:
                seen.append(case)
    return seen


def timing_table(title: str, results: Result, reference: str | None, extra: dict[str, str] | None = None) -> list[str]:
    """Columns: one per contender, `median [lo, hi]` in ms; then ratios to the reference."""
    names = list(results)
    ratio_cols = [n for n in names if n != reference] if reference else []
    lines = [f"### {title}", ""]
    head = ["case"] + [f"{n} (ms)" for n in names] + [f"{n} / {reference}" for n in ratio_cols]
    lines += ["| " + " | ".join(head) + " |", "|" + "---|" * len(head)]
    for case in case_names(results):
        row = [case]
        meds: dict[str, float] = {}
        for n in names:
            item = results[n].get(case)
            if not item:
                row.append("-")
                continue
            lo, hi = interval(item["samples_ms"])
            meds[n] = item["median_ms"]
            row.append(f"{fmt(item['median_ms'])} [{fmt(lo)}, {fmt(hi)}]")
        floor = meds.get(reference) if reference else None
        for n in ratio_cols:
            row.append(f"{meds[n] / floor:.2f}" if floor and n in meds else "-")
        lines.append("| " + " | ".join(row) + " |")
    for note in (extra or {}).values():
        lines += ["", note]
    return lines + [""]


def concurrent_table(results: Result) -> list[str]:
    names = list(results)
    if not any("concurrent" in r for r in results.values()):
        return []
    lines = ["### concurrent (50 tasks, 4 operations each, pool of 10)", "",
             "| contender | ops/s | p99 latency (ms) |", "|---|---|---|"]
    for n in names:
        item = results[n].get("concurrent")
        if item:
            lines.append(f"| {n} | {fmt(item.get('ops_per_s'))} | {fmt(item.get('p99_ms'))} |")
    return lines + [""]


def cpu_table(results: Result) -> list[str]:
    """The CPU-only split (ms per operation): build the query, make SQL. Decode time is not exposed (shown as -)."""
    lines: list[str] = []
    for n, res in results.items():
        rows = [(case, item["cpu"]) for case, item in res.items() if item.get("cpu")]
        if not rows:
            continue
        lines += [f"**{n}**", "", "| case | build query (ms) | make SQL (ms) | decode rows (ms) |", "|---|---|---|---|"]
        for case, cpu in rows:
            lines.append(f"| {case} | {fmt(cpu.get('build_ms'))} | {fmt(cpu.get('sql_ms'))} | {fmt(cpu.get('decode_ms'))} |")
        lines.append("")
    if not lines:
        return []
    return ["### CPU split (measured, mean of 20 repetitions, no database)", ""] + lines


def versions(env: dict[str, Any]) -> str:
    pk = ", ".join(f"{k} {v}" for k, v in env.get("packages", {}).items())
    build = {n: c.get("build", {}) for n, c in env.get("contenders", {}).items() if n.startswith("orm")}
    built = "; ".join(f"{n}: {b.get('profile', '?')} build, revision {str(b.get('revision', '?'))[:7]}" for n, b in build.items())
    return f"{pk}. {built}."


def main() -> None:
    directory = Path(sys.argv[1])
    run = json.loads((directory / "run.json").read_text()) if (directory / "run.json").exists() else {}
    py = load(directory / "python")
    node = load(directory / "js")
    bun = load(directory / "js", "bun-")
    out = [f"# Benchmark summary {directory.name}", ""]
    if run.get("load_check_ignored") or run.get("fast_build"):
        out += ["> **Smoke run, not a timing run.** "
                + ("The load check was overridden (`--ignore-load`). " if run.get("load_check_ignored") else "")
                + ("Release builds used the fast profile (no fat LTO). " if run.get("fast_build") else "")
                + "Do not quote these numbers.", ""]
    out += [f"All numbers are **measured** on this machine: {run.get('rounds', '?')} rounds x {run.get('iters', '?')} "
            "iterations per case (the warm-up is dropped), the contender order rotated every round, one fresh process "
            "per contender and round. Each cell is `median [95% bootstrap interval of the median]` over every sample.",
            "",
            f"Load before: `{run.get('uptime_before', '?')}`  ",
            f"Load after: `{run.get('uptime_after', '?')}`", ""]
    build_times = directory / "build-times.txt"
    if build_times.exists() and build_times.read_text().strip():
        out += ["Build times (measured): " + ", ".join(build_times.read_text().strip().split("\n")), ""]

    out += ["## Python", ""]
    if py:
        env = json.loads((directory / "python" / "environment.json").read_text())
        out += [versions(env), "",
                "Reference: `django-async`. There is no `pg`-like floor contender on Python. `orm-all` is the `orm` build with every "
                "extension; its ratio column is also against `django-async`, compare it with `orm` by eye.", ""]
        out += timing_table("Time per operation", py, "django-async" if "django-async" in py else None)
        out += concurrent_table(py) + cpu_table(py)
    else:
        out += ["No Python results.", ""]

    out += ["## TypeScript on Node", ""]
    if node:
        env = json.loads((directory / "js" / "environment.json").read_text())
        out += [versions(env), "", "Reference: `pg` (prepared statements, no ORM work), the floor.", ""]
        out += timing_table("Time per operation", node, "pg" if "pg" in node else None)
        out += concurrent_table(node) + cpu_table(node)
    else:
        out += ["No Node results.", ""]

    out += ["## TypeScript on Bun", ""]
    if bun:
        out += ["Three cases only. Reference: `prisma` (there is no `pg` row on Bun).", ""]
        out += timing_table("Time per operation", bun, "prisma" if "prisma" in bun else None)
    else:
        out += ["No Bun results.", ""]

    out += ["## Rust", "", "Skipped: the Rust slot is not built yet (owner decision `#bench-before-rust`).", ""]
    (directory / "summary.md").write_text("\n".join(out))


if __name__ == "__main__":
    main()
