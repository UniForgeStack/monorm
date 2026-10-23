"""Turn a results directory of run.sh into summary.md.

    python report.py results/<date>-<sha>

One table per side. Every contender runs once per round in a fresh process, so the rounds are the independent
units: a cell is the median of the per-round medians with a 95% cluster bootstrap interval (rounds resampled,
not single timed calls). A ratio compares round r of a contender with round r of the reference, takes the
median of those paired ratios, and gives a bootstrap interval of it. The reference is `pg` on TypeScript (the
floor: no ORM work), Django async on Python (no floor contender there) and Prisma on Bun. `orm-all` is also
compared with `orm` in a ratio column. A ratio below 1 means faster than the reference.
Every number is measured. The header says whether the run is valid and whether the load check was overridden.
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


def interval(values: list[float]) -> tuple[float, float]:
    """95% cluster bootstrap interval of the median of per-round values; a fixed seed keeps the report reproducible."""
    if len(values) < 2:
        return (values[0], values[0]) if values else (float("nan"), float("nan"))
    rng = random.Random(0)
    meds = sorted(statistics.median(rng.choices(values, k=len(values))) for _ in range(BOOTSTRAP))
    return meds[int(0.025 * BOOTSTRAP)], meds[int(0.975 * BOOTSTRAP) - 1]


def rounds_of(item: dict[str, Any]) -> list[float]:
    return item.get("batch_medians_ms") or [item["median_ms"]]


def paired(num: dict[str, Any], den: dict[str, Any]) -> tuple[float, float, float]:
    """Median and interval of the per-round ratios num[r] / den[r]."""
    ratios = [a / b for a, b in zip(rounds_of(num), rounds_of(den)) if b]
    if not ratios:
        return (float("nan"),) * 3
    lo, hi = interval(ratios)
    return statistics.median(ratios), lo, hi


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
    order = ["orm", "orm-all", "django-sync", "django-async", "sqlalchemy-sync", "sqlalchemy-async", "prisma", "drizzle", "pg"]
    return dict(sorted(out.items(), key=lambda kv: order.index(kv[0]) if kv[0] in order else len(order)))


def case_names(results: Result) -> list[str]:
    seen: list[str] = []
    for res in results.values():
        for case in res:
            if case not in seen:
                seen.append(case)
    return seen


def timing_table(title: str, results: Result, reference: str | None, extra_pairs: list[tuple[str, str]] | None = None) -> list[str]:
    """Columns: one per contender, `median [lo, hi]` in ms; then paired ratios to the reference (and `extra_pairs`)."""
    names = list(results)
    pairs = [(n, reference) for n in names if reference and n != reference]
    pairs += [(a, b) for a, b in (extra_pairs or []) if a in results and b in results]
    lines = [f"### {title}", ""]
    head = ["case"] + [f"{n} (ms)" for n in names] + [f"{a} / {b}" for a, b in pairs]
    lines += ["| " + " | ".join(head) + " |", "|" + "---|" * len(head)]
    for case in case_names(results):
        row = [case]
        for n in names:
            item = results[n].get(case)
            if not item:
                row.append("-")
                continue
            lo, hi = interval(rounds_of(item))
            row.append(f"{fmt(statistics.median(rounds_of(item)))} [{fmt(lo)}, {fmt(hi)}]")
        for a, b in pairs:
            x, y = results[a].get(case), results[b].get(case)
            if x and y:
                m, lo, hi = paired(x, y)
                row.append(f"{m:.2f} [{lo:.2f}, {hi:.2f}]")
            else:
                row.append("-")
        lines.append("| " + " | ".join(row) + " |")
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
    """The CPU-only split (ms per operation): build the query, make SQL. Decode time is not measured."""
    lines: list[str] = []
    for n, res in results.items():
        rows = [(case, item["cpu"]) for case, item in res.items() if item.get("cpu")]
        if not rows:
            continue
        lines += [f"**{n}**", "", "| case | build query (ms) | make SQL (ms) |", "|---|---|---|"]
        for case, cpu in rows:
            lines.append(f"| {case} | {fmt(cpu.get('build_ms'))} | {fmt(cpu.get('sql_ms'))} |")
        lines.append("")
    if not lines:
        return []
    return ["### CPU split (approximate)", "",
            "Measured with no database, mean of 20 repetitions, median over rounds. Approximate: `.sql()` and Django's "
            "`str(query)` inline the values, so the split is not the work of a real call. Decode time is not measured.", ""] + lines


STATIC_NOTES = [
    "Prisma `upsert-100` runs 100 upserts in one `$transaction`: Prisma has no bulk upsert. orm and Django send one statement.",
    "Django async `concurrent` runs each `aget` through thread-sensitive `sync_to_async`: the calls run one after another on one thread.",
    "psycopg prepares a statement after 5 executions of it. Django inlines LIMIT/OFFSET, so the read-N statements differ per page and are not prepared.",
    "Prisma batches `findUnique` calls of one tick into one query (recalled from the Prisma docs, not checked here), so its first reads in `concurrent` can become about one round trip.",
    "django-sync `concurrent`: an untimed setup closes the main-thread connection first, so all 10 pooled connections serve the 50 threads (each task returns its connection with `close()`).",
    "Prisma `update-1` uses `updateMany` (count only), not `update` (returns the row): the same work as the other contenders.",
    "`pg` `join-to-one` uses an inner JOIN. `posts.author_id` is NOT NULL, so the rows equal the ORMs' joins.",
    "The JS `camelRow`/`data()` row conversion runs inside the timed operation: a user does the same work.",
    "Django `delete-100` (`QuerySet.delete()`) runs BEGIN, DELETE, COMMIT: 3 statements.",
    "orm returns full rows from `bulk-insert-*`: `insertMany(...).returning()` has no key-only form. Prisma and Django return ids only or full instances as noted per contender below.",
    "Warm-up: every case runs 50 untimed calls before the timed calls, the same count for every contender. Timed calls use iterations 1..N, the warm-up uses distinct iteration numbers.",
    "Contenders run one per process and round, in a rotating order, on a database reset before every process.",
]


def notes(*envs: dict[str, Any]) -> list[str]:
    """The fixed notes, then every note the contenders report about themselves (`*_note`, statements, pool)."""
    lines = ["## Notes", ""] + [f"- {n}" for n in STATIC_NOTES]
    seen: set[str] = set()
    for env in envs:
        for name, info in env.get("contenders", {}).items():
            for key, val in info.items():
                if key.endswith("_note") or key in ("prepared_statements", "pool", "query_engine"):
                    line = f"- `{name}` {key.replace('_', ' ')}: {val}"
                    if line not in seen:
                        seen.add(line)
                        lines.append(line)
    return lines + [""]


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
    if run.get("valid") is False:
        out += [f"> **INVALID RUN.** {run.get('invalid_reason') or 'run.json marks this run invalid.'} Do not quote these numbers.", ""]
    if run.get("load_check_ignored") or run.get("fast_build"):
        out += ["> **Smoke run, not a timing run.** "
                + ("The load check was overridden (`--ignore-load`). " if run.get("load_check_ignored") else "")
                + ("Release builds used the fast profile (no fat LTO). " if run.get("fast_build") else "")
                + "Do not quote these numbers.", ""]
    out += [f"All numbers are **measured** on this machine: {run.get('rounds', '?')} rounds x {run.get('iters', '?')} "
            "iterations per case (the warm-up calls are not timed), the contender order rotated every round, one fresh "
            "process per contender and round. Each cell is `median [95% cluster bootstrap interval]` of the per-round "
            "medians; each ratio is the median of the paired per-round ratios with its interval.",
            "",
            f"Load before: `{run.get('uptime_before', '?')}`  ",
            f"Load after: `{run.get('uptime_after', '?')}`", ""]
    loads = [x for p in ("python", "js") if (directory / p / "environment.json").exists()
             for x in json.loads((directory / p / "environment.json").read_text()).get("round_loads", [])]
    if loads:
        out += [f"1-minute load before each round (measured): min {min(loads):.2f}, max {max(loads):.2f}.", ""]
    build_times = directory / "build-times.txt"
    if build_times.exists() and build_times.read_text().strip():
        out += ["Build times (measured): " + ", ".join(build_times.read_text().strip().split("\n")), ""]

    envs: list[dict[str, Any]] = []
    out += ["## Python", ""]
    if py:
        env = json.loads((directory / "python" / "environment.json").read_text())
        envs.append(env)
        out += [versions(env), "",
                "Reference: `django-async`. There is no `pg`-like floor contender on Python. `orm-all` is the `orm` build with every "
                "extension; the `orm-all / orm` column compares the two.", ""]
        out += timing_table("Time per operation", py, "django-async" if "django-async" in py else None, [("orm-all", "orm")])
        out += concurrent_table(py) + cpu_table(py)
    else:
        out += ["No Python results.", ""]

    out += ["## TypeScript on Node", ""]
    if node:
        env = json.loads((directory / "js" / "environment.json").read_text())
        envs.append(env)
        out += [versions(env), "", "Reference: `pg` (prepared statements, no ORM work), the floor. The `orm-all / orm` column compares the two builds.", ""]
        out += timing_table("Time per operation", node, "pg" if "pg" in node else None, [("orm-all", "orm")])
        out += concurrent_table(node) + cpu_table(node)
    else:
        out += ["No Node results.", ""]

    out += ["## TypeScript on Bun", ""]
    if bun:
        out += ["Three cases only. Reference: `prisma` (there is no `pg` row on Bun).", ""]
        out += timing_table("Time per operation", bun, "prisma" if "prisma" in bun else None)
    else:
        out += ["No Bun results.", ""]

    out += ["## Rust", "", "Skipped: there is no Rust runner.", ""]
    bun_env = directory / "js" / "bun-environment.json"
    if bun_env.exists():
        envs.append(json.loads(bun_env.read_text()))
    out += notes(*envs)
    (directory / "summary.md").write_text("\n".join(out))


if __name__ == "__main__":
    main()
