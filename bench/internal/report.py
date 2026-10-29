"""Write summary.md from the JSON files of one run: python report.py RESULT_DIR.

Per language and case it shows the paths in µs per op (the median of the timed batches) and the four layers:
  builder           = unprepared - prepared (the query set builds the expression tree, the IR and its JSON)
  value binding     = prepared - direct IR (the spec's "JSON encode": `Prepared` caches the JSON, so this holds binding and scope only)
  hop + instances   = direct IR - engine only (the native call, and the model instances that the binding builds)
  engine            = engine only - driver only (parse, plan, generate SQL, decode rows)
  driver            = driver only
A write has no prepared path: its builder share is unprepared - direct IR and its value binding is n/a.
The engine-only row is the Rust run on the same IR; it is the same for both languages.
"""

from __future__ import annotations

import json
import sys
from pathlib import Path
from typing import Any

LANGS = [("python", "Python"), ("js", "TypeScript on Node")]
PATHS = ["unprepared", "prepared", "direct", "engine", "driver"]


def load(path: Path) -> dict[str, Any] | None:
    return json.loads(path.read_text()) if path.exists() else None


Value = float | tuple[str, str] | None  # a median, or (label, reason) for an na or skipped row, or None for a missing row


def us(run: dict[str, Any] | None, case: str, path: str) -> Value:
    if run is None:
        return None
    entry = run["cases"].get(case, {}).get("paths", {}).get(path)
    if entry is None:
        return None
    if "median_us" in entry:
        return float(entry["median_us"])
    return ("n/a", entry["na"]) if "na" in entry else ("skipped", entry["skipped"])


def num(v: Value) -> float | None:
    return v if isinstance(v, float) else None


def diff(a: Value, b: Value) -> float | None:
    x, y = num(a), num(b)
    return None if x is None or y is None else x - y


def cell(v: Value) -> str:
    if isinstance(v, tuple):
        return v[0]
    return "–" if v is None else f"{v:,.1f}"


def layer(v: float | None, total: float | None) -> str:
    if v is None:
        return "–"
    share = f" ({v / total * 100:.0f}%)" if total is not None and total > 0 else ""
    return f"{v:,.1f}{share}"


def main(out: Path) -> None:
    meta = json.loads((out / "run.json").read_text())
    lines = [f"# Internal benchmark, {meta['date']}", ""]
    lines += [
        f"- Commit: `{meta['git_sha']}`. Backend: {meta['backend']}. Rounds: {meta['rounds']}.",
        f"- Load average (1 minute): {meta['load1_at_start']} at the start, {meta['load1_at_end']} at the end (measured).",
    ]
    if not meta["valid"]:
        lines += [f"- **Not a valid run:** {meta['invalid_reason']}. Do not quote these numbers."]
    lines += ["- Every number is measured: the median of the timed batches, in µs per operation.", ""]
    backend = meta["backend"]
    engine_run = load(out / f"rust-{backend}.json")
    for key, title in LANGS:
        run = load(out / f"{key}-{backend}.json")
        if run is None:
            lines += [f"## {title}", "", "No result file.", ""]
            continue
        env = run["environment"]
        runtime = env.get("python") or env.get("node")
        lines += [f"## {title}", "", f"{env['cpu']}, {env['os']}, runtime {runtime}, {env['date']}.", ""]
        lines += ["| case | unprepared | prepared | direct IR | engine only | driver only |", "|---|---|---|---|---|---|"]
        rows = []
        for case in run["cases"]:
            v: dict[str, Value] = {p: us(engine_run if p == "engine" else run, case, p) for p in PATHS}
            if engine_run is None:
                v["engine"] = ("skipped", meta["rust"] or "no Rust result")
            rows.append((case, v))
            lines.append(f"| {case} | " + " | ".join(cell(v[p]) for p in PATHS) + " |")
        notes = sorted({f"{p} {label}: {reason}" for _, v in rows for p, x in v.items() if isinstance(x, tuple) for label, reason in [x]})
        if notes:
            lines += ["", *[f"- {n}" for n in notes]]
        lines += ["", "Layers in µs per op, with the share of the unprepared call:", ""]
        lines += ["| case | builder | value binding | hop + instances | engine | driver |", "|---|---|---|---|---|---|"]
        biggest: dict[str, str] = {}
        for case, v in rows:
            write = num(v["prepared"]) is None
            layers = {
                "builder": diff(v["unprepared"], v["direct"] if write else v["prepared"]),
                "value binding": None if write else diff(v["prepared"], v["direct"]),
                "hop + instances": diff(v["direct"], v["engine"]),
                "engine": diff(v["engine"], v["driver"]),
                "driver": num(v["driver"]),
            }
            total = num(v["unprepared"])
            lines.append(f"| {case} | " + " | ".join(layer(x, total) for x in layers.values()) + " |")
            known = {k: x for k, x in layers.items() if x is not None and k != "driver"}
            if known:
                biggest[case] = max(known, key=lambda k: known[k])
        lines += [""]
        if biggest:
            lines += ["Biggest monorm layer per case (the driver is the floor, not counted): " + "; ".join(f"{c}: {k}" for c, k in biggest.items()) + ".", ""]
    lines += [
        "A layer shows `–` when a row is missing or skipped. A negative layer is noise: the two paths are closer than the run can tell apart.",
        "The prefetch case reads the user and the posts of the user in one engine call that runs two statements.",
        "",
    ]
    (out / "summary.md").write_text("\n".join(lines))


main(Path(sys.argv[1]))
