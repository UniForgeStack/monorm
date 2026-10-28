"""Times the write and read seams on a model with no hook.

    python scripts/bench-hook-seam.py LABEL ROUNDS OUT.json

`scripts/bench-hook-seam.sh` runs it on a baseline checkout and on this one, interleaved, and applies the gate.
"""
import asyncio
import json
import statistics
import sys
import time

import monorm

SCHEMA = """datasource db { provider = "sqlite" }
model Item {
  id Int @id @default(autoincrement())
  name String
  qty Int
  note String?
}
"""
WRITE, READ = 10_000, 1_000


async def main(label: str, rounds: int, out: str) -> None:
    registry = monorm.Registry()
    item = monorm.loads(SCHEMA, registry=registry)["Item"]
    db = await monorm.connect("sqlite://:memory:", registry=registry, default=False)
    await db.create_tables()
    rows = [{"name": f"item {i}", "qty": i, "note": None} for i in range(WRITE)]
    write: list[float] = []
    read: list[float] = []
    try:
        for _ in range(rounds + 1):
            await item.objects.using(db).filter(item.id > 0).delete()
            start = time.perf_counter()
            await item.objects.using(db).insert_many(rows)
            write.append(time.perf_counter() - start)
            start = time.perf_counter()
            got = await item.objects.using(db).filter(item.qty < READ).all()
            read.append(time.perf_counter() - start)
            assert len(got) == READ
    finally:
        await db.close()
    # The first round warms the pool and the statement cache.
    result = {"label": label, "insert_many_10000": write[1:], "read_1000": read[1:]}
    with open(out, "w") as f:
        json.dump(result, f)
    for key in ("insert_many_10000", "read_1000"):
        print(f"{label} {key}: median {statistics.median(result[key]) * 1000:.1f} ms over {rounds} rounds")


asyncio.run(main(sys.argv[1], int(sys.argv[2]), sys.argv[3]))
