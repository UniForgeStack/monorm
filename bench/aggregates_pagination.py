"""Compare scalar and LATERAL aggregates after an unindexed parent sort.

Uses a disposable PostgreSQL database named by ORM_TEST_DATABASE_URL: drops and
creates the blog tables. Defaults to 100,000 parents, ten posts each, LIMIT 20
OFFSET 10. Both forms use the ORM's SQL, fetched through the same raw API.
"""

from __future__ import annotations

import argparse
import asyncio
import json
import os
import statistics
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "examples"))

from blog.models import User  # noqa: E402

import orm  # noqa: E402


async def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--url", default=os.environ.get("ORM_TEST_DATABASE_URL"), required="ORM_TEST_DATABASE_URL" not in os.environ
    )
    parser.add_argument("--parents", type=int, default=100_000)
    parser.add_argument("--posts-per-parent", type=int, default=10)
    parser.add_argument("--rounds", type=int, default=5)
    parser.add_argument("--out", type=Path)
    args = parser.parse_args()
    if min(args.parents, args.posts_per_parent, args.rounds) < 1:
        parser.error("parents, posts-per-parent and rounds must be positive")

    db = await orm.connect(args.url)
    try:
        await db.drop_tables()
        await db.create_tables()
        await db.execute(
            "INSERT INTO users (email, name) "
            f"SELECT 'u' || n || '@x.io', md5(n::text) FROM generate_series(1, {args.parents}) AS n"
        )
        await db.execute(
            "INSERT INTO posts (author_id, title, body, views) "
            f"SELECT u.id, 'post', '', n FROM users AS u CROSS JOIN generate_series(1, {args.posts_per_parent}) AS n"
        )
        await db.execute("ANALYZE users")
        await db.execute("ANALYZE posts")
        query = User.objects.select(User.id, orm.func.count(User.posts), orm.func.max(User.posts.views)).order_by(
            User.name
        )
        sql = {
            "scalar": query.limit(20).offset(10).sql(),
            # Deliberately apply pagination AFTER planning to reproduce the old form.
            "lateral": query.sql() + " LIMIT 20 OFFSET 10",
        }
        assert "LATERAL" not in sql["scalar"]
        assert "LATERAL" in sql["lateral"]
        scalar = await db.fetch(sql["scalar"])
        lateral = await db.fetch(sql["lateral"])
        # Raw column names differ between the two plans.
        assert [(row["id"], row["count"], row["max"]) for row in scalar] == [
            (row["id"], row["c1"], row["c2"]) for row in lateral
        ]
        samples: dict[str, list[float]] = {name: [] for name in sql}
        for round_number in range(args.rounds):
            # Alternate order after both warm-ups to reduce ordering bias.
            for name in list(sql) if round_number % 2 == 0 else list(reversed(sql)):
                start = time.perf_counter()
                await db.fetch(sql[name])
                samples[name].append((time.perf_counter() - start) * 1000)
        results = {
            "measurement": "measured (loaded machine), median milliseconds after warm-up",
            "parents": args.parents,
            "posts_per_parent": args.posts_per_parent,
            "rounds": args.rounds,
            "limit": 20,
            "offset": 10,
            "median_ms": {name: statistics.median(values) for name, values in samples.items()},
            "sql": sql,
            "plans": {
                name: await db.fetch("EXPLAIN (ANALYZE, BUFFERS) " + statement) for name, statement in sql.items()
            },
        }
        print(json.dumps(results, indent=2))
        if args.out:
            args.out.write_text(json.dumps(results, indent=2) + "\n")
    finally:
        await db.drop_tables()
        await db.close()


if __name__ == "__main__":
    asyncio.run(main())
