# TypeScript contenders

Runners for `js` (our Node addon), Prisma 7 with `@prisma/adapter-pg` and `pg` with named prepared statements.
The dependencies live in this directory only.

```bash
npm install
node make-prisma-schema.mjs && npx prisma generate   # prisma/ and generated/ come from ../schema.prisma
export BENCH_DATABASE_URL=postgres://postgres:postgres@localhost:55432/bench_nfx1
node run.mjs check                                   # equality check only
node run.mjs run --rounds 2 --iters 5                # check, then timing; writes results/smoke
bun run.mjs run                                      # ours and Prisma; read-pk, read-1000, bulk-insert-1000
node run.mjs run --orm-artifact DIR                  # adds orm-all: ORM_NATIVE=DIR/orm.node
```

The JSON output has the same fields as `../python/`; Bun files carry a `bun-` prefix.
Prisma runs with the session time zone set to UTC, because the adapter drops the offset of a `timestamptz`.
