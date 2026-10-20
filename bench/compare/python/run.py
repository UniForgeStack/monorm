"""Equality check and timing for the Python contenders.

    python run.py check [--contenders a,b] [--cases x,y]
    python run.py run   [--rounds 2] [--iters 5] [--orm-artifact PATH] [--out DIR]

Each contender runs in its own process (`worker` mode), one process per contender per round, with the
database reset before every process. `run` does `check` first and stops when it fails.
"""

from __future__ import annotations

import argparse
import asyncio
import datetime as dt
import importlib
import importlib.metadata as md
import json
import os
import platform
import statistics
import subprocess
import sys
import tempfile
import time
from pathlib import Path
from typing import Any

from common import CASES, CONCURRENT_OPS_PER_TASK, CONCURRENT_TASKS, HERE, POOL_SIZE, ROOT, connect, database_url, p99, reset, state

BASE = ("orm", "django-sync", "django-async")
REPO = HERE.parents[2]


def make(name: str) -> Any:
    if name.startswith("orm"):
        from orm_contender import OrmContender

        return OrmContender(name)
    from django_contender import DjangoContender

    return DjangoContender(asynchronous=name == "django-async")


# ---- worker ---------------------------------------------------------------------------------------------------


async def worker(args: argparse.Namespace) -> None:
    c = make(args.contender)
    await c.start()
    conn = connect()
    out: dict[str, Any] = {"contender": args.contender, "info": c.info(), "cases": {}}
    try:
        for name in args.cases.split(","):
            case = c.cases[name]
            if args.mode == "check":
                arg = case.setup(0, conn)
                raw = case.op(arg)
                raw = await raw if asyncio.iscoroutine(raw) or hasattr(raw, "__await__") else raw
                out["cases"][name] = {"result": json.loads(json.dumps(case.norm(raw), default=str))}
            else:
                out["cases"][name] = await time_case(case, conn, args.iters)
    finally:
        conn.close()
        await c.stop()
    Path(args.out).write_text(json.dumps(out))


async def time_case(case: Any, conn: Any, iters: int) -> dict[str, Any]:
    async def once(i: int) -> tuple[float, Any]:
        arg = case.setup(i, conn)
        t0 = time.perf_counter()
        raw = case.op(arg)
        if hasattr(raw, "__await__"):
            raw = await raw
        return (time.perf_counter() - t0) * 1000, raw

    await once(0)  # warm-up: connections, statement caches, imports
    samples: list[float] = []
    lat: list[float] = []
    for i in range(1, iters + 1):
        ms, raw = await once(i)
        samples.append(ms)
        if case.concurrent:
            lat += [x for _, ls in raw for x in ls]
    res: dict[str, Any] = {"samples_ms": samples, "median_ms": statistics.median(samples)}
    if case.concurrent:
        ops = CONCURRENT_TASKS * CONCURRENT_OPS_PER_TASK
        res["ops_per_s"] = ops / (statistics.median(samples) / 1000)
        res["p99_ms"] = p99(lat)
    if case.build and case.sql:
        reps = 20
        t0 = time.perf_counter()
        for i in range(reps):
            case.build(i)
        build = (time.perf_counter() - t0) / reps * 1000
        t0 = time.perf_counter()
        for i in range(reps):
            case.sql(i)
        sql = (time.perf_counter() - t0) / reps * 1000
        res["cpu"] = {"build_ms": build, "sql_ms": sql, "decode_ms": None}
    return res


# ---- parent ---------------------------------------------------------------------------------------------------


def spawn(name: str, cases: list[str], mode: str, iters: int, artifact: str | None) -> dict[str, Any]:
    env = os.environ | {"BENCH_DATABASE_URL": database_url()}
    if name == "orm-all":
        assert artifact, "orm-all needs --orm-artifact"
        env |= {"PYTHONPATH": f"{artifact}/py:{HERE}", "ORM_PROFILE": "custom"}
    else:
        env["PYTHONPATH"] = str(HERE)
    with tempfile.NamedTemporaryFile(suffix=".json", delete=False) as f:
        out = f.name
    cmd = [sys.executable, str(HERE / "run.py"), "worker", "--contender", name, "--cases", ",".join(cases),
           "--mode", mode, "--iters", str(iters), "--out", out]
    p = subprocess.run(cmd, env=env, capture_output=True, text=True)
    if p.returncode:
        raise SystemExit(f"{name} worker failed:\n{p.stdout[-2000:]}\n{p.stderr[-4000:]}")
    data = json.loads(Path(out).read_text())
    os.unlink(out)
    return data


def check(names: list[str], cases: list[str], artifact: str | None) -> dict[str, dict[str, Any]]:
    infos: dict[str, dict[str, Any]] = {}
    failures: list[str] = []
    conn = connect()
    for case in cases:
        got: dict[str, tuple[Any, dict[str, str]]] = {}
        for n in names:
            reset(conn)
            data = spawn(n, [case], "check", 0, artifact)
            infos[n] = data["info"]
            got[n] = (data["cases"][case]["result"], state(conn))
        first = names[0]
        for n in names[1:]:
            for what, a, b in (("result", got[first][0], got[n][0]), ("state", got[first][1], got[n][1])):
                if a != b:
                    failures.append(f"{case}: {what} differs, {first} vs {n}\n    {first}: {str(a)[:300]}\n    {n}: {str(b)[:300]}")
        print(f"check {case}: {'FAIL' if any(f.startswith(case + ':') for f in failures) else 'ok'}", flush=True)
    conn.close()
    if failures:
        raise SystemExit("equality check failed:\n" + "\n".join(failures))
    return infos


def environment(infos: dict[str, dict[str, Any]], artifact: str | None, before: str) -> dict[str, Any]:
    conn = connect()
    pg = conn.execute("select version()").fetchone()[0]  # type: ignore[index]
    conn.close()
    pkgs = {p: md.version(p) for p in ("orm", "django", "psycopg", "psycopg-binary", "psycopg-pool", "asgiref")}
    return {
        "date": dt.datetime.now(dt.UTC).isoformat(),
        "git_sha": subprocess.run(["git", "rev-parse", "HEAD"], cwd=REPO, capture_output=True, text=True).stdout.strip(),
        "python": platform.python_version(),
        "machine": platform.platform(),
        "packages": pkgs,
        "postgres": pg,
        "database": database_url().rsplit("/", 1)[-1],
        "pool_size": POOL_SIZE,
        "uptime_before": before,
        "uptime_after": uptime(),
        "orm_artifact": artifact,
        "contenders": infos,
    }


def uptime() -> str:
    return subprocess.run(["uptime"], capture_output=True, text=True).stdout.strip()


def run(args: argparse.Namespace) -> None:
    names = list(BASE) + (["orm-all"] if args.orm_artifact else [])
    cases = args.cases.split(",") if args.cases else list(CASES)
    before = uptime()
    infos = check(names, cases, args.orm_artifact)
    out = Path(args.out)
    out.mkdir(parents=True, exist_ok=True)
    (out / "environment.json").write_text(json.dumps(environment(infos, args.orm_artifact, before), indent=2))
    conn = connect()
    rounds: dict[str, list[dict[str, Any]]] = {n: [] for n in names}
    for r in range(args.rounds):
        order = names[r % len(names):] + names[: r % len(names)]  # rotate the contender order every round
        for n in order:
            reset(conn)
            rounds[n].append(spawn(n, cases, "time", args.iters, args.orm_artifact)["cases"])
            print(f"round {r + 1} {n} done", flush=True)
    conn.close()
    for n in names:
        res: dict[str, Any] = {}
        for case in cases:
            per = [rd[case] for rd in rounds[n]]
            allsamples = [s for p in per for s in p["samples_ms"]]
            item: dict[str, Any] = {
                "median_ms": statistics.median(allsamples),
                "samples_ms": allsamples,
                "batch_medians_ms": [p["median_ms"] for p in per],
            }
            for k in ("cpu", "ops_per_s", "p99_ms"):
                if k in per[0]:
                    item[k] = per[0][k] if k == "cpu" else statistics.median(p[k] for p in per)
            res[case] = item
        (out / f"{n}.json").write_text(json.dumps(res, indent=2))
    (out / "environment.json").write_text(json.dumps(environment(infos, args.orm_artifact, before), indent=2))
    print(f"wrote {out}")


def drive(coro: Any) -> Any:
    """Run a coroutine that never suspends, without an event loop."""
    try:
        coro.send(None)
    except StopIteration as stop:
        return stop.value
    raise RuntimeError("sync contender suspended")


def main() -> None:
    ap = argparse.ArgumentParser()
    sub = ap.add_subparsers(dest="cmd", required=True)
    for c in ("check", "run"):
        p = sub.add_parser(c)
        p.add_argument("--cases", default="")
        p.add_argument("--orm-artifact", default=None)
        p.add_argument("--rounds", type=int, default=2)
        p.add_argument("--iters", type=int, default=5)
        p.add_argument("--out", default=str(HERE / "results" / "smoke"))
        p.add_argument("--contenders", default="")
    w = sub.add_parser("worker")
    for k in ("contender", "cases", "mode", "out"):
        w.add_argument(f"--{k}", required=True)
    w.add_argument("--iters", type=int, default=0)
    a = ap.parse_args()
    if a.cmd == "worker":
        if a.contender == "django-sync":
            drive(worker(a))  # the sync ORM refuses to run while an event loop is running
        else:
            asyncio.run(worker(a))
    elif a.cmd == "check":
        names = a.contenders.split(",") if a.contenders else list(BASE) + (["orm-all"] if a.orm_artifact else [])
        check(names, a.cases.split(",") if a.cases else list(CASES), a.orm_artifact)
    else:
        run(a)


if __name__ == "__main__":
    main()
