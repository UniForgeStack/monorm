"""Internal benchmark, Python: one call through the unprepared, prepared and direct-IR paths.

    python bench.py --url URL --out DIR [--seed] [--rounds N] [--quick] [--dump FILE]

The unprepared path is the public query set API, the prepared path is `prepare()`, and the
direct path sends the prepared IR JSON and bound values to the native engine. The three build
the same instances. The driver row runs the same SQL on the raw driver (sqlite3 or psycopg).
Each case checks that every path returns the same result before it is timed.
`--seed` drops and creates the tables and seeds 11 users and 1,000 posts.
`--dump` writes the IR, values and insert rows that the Rust engine-only runner replays.
"""

from __future__ import annotations

import argparse
import asyncio
import json
import os
import platform
import sqlite3
import statistics
import subprocess
import sys
import time
from collections.abc import Awaitable, Callable
from pathlib import Path
from typing import Any

import monorm
from monorm import param
from monorm import _native
from monorm.protection import allowed_writes
from monorm.write import prepare_rows

HERE = Path(__file__).resolve().parent
SPEC = json.loads((HERE.parent / "cases.json").read_text())
SINK = 11  # the user that the insert case writes to


def sh(*cmd: str) -> str | None:
    try:
        out = subprocess.run(cmd, capture_output=True, text=True, check=True).stdout.strip()
    except (OSError, subprocess.CalledProcessError):
        return None
    return out or None


def environment(backend: str) -> dict[str, Any]:
    cpu = sh("sysctl", "-n", "machdep.cpu.brand_string") if sys.platform == "darwin" else sh("sed", "-n", "s/^model name[ \t]*: //p", "/proc/cpuinfo")
    cpu = cpu.splitlines()[0] if cpu else None
    return {
        "language": "python",
        "python": platform.python_version(),
        "os": f"{platform.system()} {platform.machine()}",
        "cpu": cpu or platform.processor() or "unknown",
        "backend": backend,
        "load1_at_start": os.getloadavg()[0],
        "commit": sh("git", "rev-parse", "HEAD") or "unknown",
        "date": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
    }


class Driver:
    """The raw driver of the backend: runs the SQL of one case and returns its rows."""

    def __init__(self, url: str) -> None:
        self.url = url
        self.skip: str | None = None
        self.conn: Any = None
        self.sqlite = url.startswith("sqlite")

    async def open(self) -> None:
        if self.sqlite:
            if sqlite3.sqlite_version_info < (3, 35):
                self.skip = f"sqlite3 links SQLite {sqlite3.sqlite_version}, which has no RETURNING"
                return
            self.conn = sqlite3.connect(self.url.removeprefix("sqlite://"), isolation_level=None)
            return
        try:
            import psycopg  # pyright: ignore[reportMissingImports]
        except ImportError:
            self.skip = "psycopg is not installed in this Python environment"
            return
        self.conn = await psycopg.AsyncConnection.connect(self.url, autocommit=True)

    async def run(self, statements: list[dict[str, Any]], values: dict[str, Any]) -> list[list[Any]]:
        out = []
        for st in statements:
            args = [values[a] for a in st["args"]]
            if self.sqlite:
                out.append(self.conn.execute(st["text"], args).fetchall())
            else:
                async with self.conn.cursor() as cur:
                    await cur.execute(st["text"].replace("?", "%s"), args)
                    out.append(await cur.fetchall())
        return out

    async def close(self) -> None:
        if self.conn is not None:
            r = self.conn.close()
            if r is not None:
                await r


class Case:
    def __init__(self, spec: dict[str, Any]) -> None:
        self.name: str = spec["name"]
        self.inner: int = spec["inner"]
        self.values: list[dict[str, Any]] = spec["values"]
        self.sql: list[dict[str, Any]] = spec["sql"]
        self.paths: dict[str, Callable[[int], Awaitable[Any]] | str] = {}
        self.norm: Callable[[str, Any], Any]
        self.cleanup: Callable[[], Awaitable[Any]] | None = None
        self.dump: dict[str, Any] = {}


def build_cases(models: Any, db: Any, driver: Driver) -> list[Case]:
    User, Post = models["User"], models["Post"]
    specs = {c["name"]: c for c in SPEC["cases"]}

    def direct(p: Any, ir: str, params: list[Any]) -> Awaitable[Any]:
        engine, tx, allowed = db._reader(), db._tx(), allowed_writes()
        return db._call(lambda t: engine.run(ir, params, tx, None, p._qs._db, allowed, t))

    def post_key(r: Any) -> tuple[int, int, str, int]:
        return (r.id, r.author_id, r.title, r.views)

    def row_key(r: Any) -> tuple[Any, ...]:
        return (r[0], r[1], r[2], r[4])

    def reads(name: str, qs: Callable[[dict[str, Any]], Any], prepared: Any, kind: str = "select") -> Case:
        """A case whose three paths run one read: `qs(values)` builds it, `prepared` holds param() slots."""
        c = Case(specs[name])
        p = prepared.prepare()
        st = p._statement(kind)
        bound = [st.bind(v, p._names) for v in c.values]
        n = len(c.values)
        if kind == "count":
            c.paths["unprepared"] = lambda k: qs(c.values[k % n]).count()
            c.paths["prepared"] = lambda k: p.count(**c.values[k % n])
            c.norm = lambda path, r: r[0][0][0] if path == "driver" else r
        else:
            c.paths["unprepared"] = lambda k: qs(c.values[k % n])
            c.paths["prepared"] = lambda k: p(**c.values[k % n])
            c.norm = lambda path, r: [row_key(x) for x in r[0]] if path == "driver" else [post_key(x) for x in r]
        c.paths["direct"] = lambda k: direct(p, st.json, bound[k % n])
        c.dump = {"ir": st.json, "params": bound}
        return c

    def insert() -> Case:
        """The write has no IR, so it has no prepared path; the direct path sends prepared rows."""
        c = Case(specs["insert_one"])
        v0 = c.values[0]
        fields, rows, _ = prepare_rows(Post, [v0])

        async def clean() -> None:
            await Post.objects.filter(Post.author_id == SINK).delete()

        def norm(path: str, r: Any) -> tuple[Any, Any]:
            if path == "driver":
                return (r[0][0][1], r[0][0][2])
            one = r[0] if isinstance(r, list) else r
            return (one.author_id, one.title)

        c.paths["unprepared"] = lambda _k: Post.objects.insert(**v0)
        c.paths["prepared"] = "a write has no IR to prepare"
        c.paths["direct"] = lambda _k: db._insert("Post", fields, rows)
        c.norm = norm
        c.cleanup = clean
        c.dump = {"insert": {"model": "Post", "fields": fields, "rows": [[None if x is _native.DEFAULT else x for x in row] for row in rows]}}
        return c

    def prefetch() -> Case:
        """The user and the posts of the user: one engine call that runs two statements."""
        c = Case(specs["prefetch_posts"])
        p = User.objects.filter(User.id == param("id")).load(User.posts).prepare()
        st = p._statement("select")
        bound = [st.bind(v, p._names) for v in c.values]
        n = len(c.values)
        c.paths["unprepared"] = lambda k: User.objects.filter(User.id == c.values[k % n]["id"]).load(User.posts)
        c.paths["prepared"] = lambda k: p(**c.values[k % n])
        c.paths["direct"] = lambda k: direct(p, st.json, bound[k % n])
        c.norm = lambda path, r: (
            [(r[0][0][0], sorted(x[0] for x in r[1]))] if path == "driver" else [(u.id, sorted(x.id for x in u.posts.cached)) for u in r]
        )
        c.dump = {"ir": st.json, "params": bound}
        return c

    cases = [
        reads("pk_get", lambda v: Post.objects.filter(Post.id == v["id"]), Post.objects.filter(Post.id == param("id"))),
        reads(
            "filter_order_50",
            lambda v: Post.objects.filter(Post.author_id == v["a"], Post.views >= v["min"]).order_by(Post.id).limit(v["n"]),
            Post.objects.filter(Post.author_id == param("a"), Post.views >= param("min")).order_by(Post.id).limit(param("n")),
        ),
        reads(
            "read_1000",
            lambda v: Post.objects.filter(Post.id > v["after"]).order_by(Post.id).limit(v["n"]),
            Post.objects.filter(Post.id > param("after")).order_by(Post.id).limit(param("n")),
        ),
        reads("filtered_count", lambda v: Post.objects.filter(Post.views > v["min"]), Post.objects.filter(Post.views > param("min")), "count"),
        insert(),
        prefetch(),
    ]
    for c in cases:
        if driver.skip is not None:
            c.paths["driver"] = driver.skip
        else:
            c.paths["driver"] = lambda k, c=c: driver.run(c.sql, c.values[k % len(c.values)])
    return cases


async def check(case: Case) -> None:
    """Every path returns the same result for the first value set."""
    got: dict[str, Any] = {}
    for path, fn in case.paths.items():
        if isinstance(fn, str):
            continue
        got[path] = case.norm(path, await fn(0))
        if case.cleanup is not None:
            await case.cleanup()
    ref = got["unprepared"]
    for path, value in got.items():
        if value != ref:
            raise SystemExit(f"{case.name}: {path} returned {value!r}, unprepared returned {ref!r}")
    if not ref and case.name != "insert_one":
        raise SystemExit(f"{case.name}: the check result is empty")


async def measure(case: Case, rounds: int, inner: int, warm: bool) -> dict[str, Any]:
    live = [p for p, fn in case.paths.items() if not isinstance(fn, str)]
    if warm:
        for p in live:
            for k in range(max(inner // 4, 3)):
                await case.paths[p](k)  # type: ignore[operator]
            if case.cleanup is not None:
                await case.cleanup()
    samples: dict[str, list[float]] = {p: [] for p in live}
    for r in range(rounds):
        order = live[r % len(live) :] + live[: r % len(live)]
        for p in order:
            fn = case.paths[p]
            t = time.perf_counter()
            for k in range(inner):
                await fn(k)  # type: ignore[operator]
            samples[p].append((time.perf_counter() - t) / inner * 1e6)
            if case.cleanup is not None:
                await case.cleanup()
    out: dict[str, Any] = {}
    for p, fn in case.paths.items():
        if isinstance(fn, str):
            out[p] = {"na" if p == "prepared" else "skipped": fn}
        else:
            out[p] = {"median_us": statistics.median(samples[p]), "min_us": min(samples[p]), "samples_us": samples[p]}
    return out


async def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--url", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--rounds", type=int, default=15)
    ap.add_argument("--seed", action="store_true")
    ap.add_argument("--quick", action="store_true", help="one call per case and path, after the equality check")
    ap.add_argument("--dump", help="write the IR and values for the Rust runner")
    args = ap.parse_args()

    backend = "sqlite" if args.url.startswith("sqlite") else "postgresql"
    src = (HERE.parent / "schema.prisma").read_text().replace("BACKEND", backend)
    models = monorm.loads(src)
    db = await monorm.connect(args.url, max_connections=1)
    if args.seed:
        await db.drop_tables()
        await db.create_tables()
        User, Post = models["User"], models["Post"]
        users = await User.objects.insert_many([{"email": f"u{i}@x.io", "name": f"User {i}"} for i in range(SINK)]).returning()
        assert [u.id for u in users] == list(range(1, SINK + 1)), "the seed expects fresh tables (ids from 1)"
        await Post.objects.insert_many(
            [{"author": users[i % 10], "title": f"post {i}", "body": "x" * 200, "views": i, "published": i % 2 == 0} for i in range(1000)]
        )
    driver = Driver(args.url)
    await driver.open()
    cases = build_cases(models, db, driver)

    result: dict[str, Any] = {"environment": environment(backend), "quick": args.quick, "cases": {}}
    for case in cases:
        await check(case)
        rounds, inner = (1, 1) if args.quick else (args.rounds, case.inner)
        paths = await measure(case, rounds, inner, warm=not args.quick)
        result["cases"][case.name] = {"checked": True, "inner": inner, "rounds": rounds, "paths": paths}
        print(f"python {backend} {case.name}: " + ", ".join(f"{p} {v['median_us']:.1f}us" if "median_us" in v else f"{p} -" for p, v in paths.items()), flush=True)
    result["environment"]["load1_at_end"] = os.getloadavg()[0]

    out = Path(args.out)
    out.mkdir(parents=True, exist_ok=True)
    (out / f"python-{backend}.json").write_text(json.dumps(result, indent=1))
    if args.dump:
        dump = {c.name: {**c.dump, "inner": c.inner, "values": c.values} for c in cases}
        Path(args.dump).write_text(json.dumps(dump))
    await driver.close()
    await db.close()


asyncio.run(main())
