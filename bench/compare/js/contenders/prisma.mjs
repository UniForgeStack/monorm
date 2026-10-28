// Prisma 7 with @prisma/adapter-pg, written as its docs show. The client comes from ../generated (make-prisma-schema.mjs).
import { PrismaPg } from "@prisma/adapter-pg";
import { readFileSync } from "node:fs";

import {
  CONCURRENT_OPS_PER_TASK, CONCURRENT_TASKS, POOL_SIZE, POST, TAG, USER, commentRows, databaseUrl, objs, page, pkFor, plain,
  sortById, updateRange, upsertRows,
} from "../common.mjs";

const { PrismaClient } = await import("../generated/client.ts");
const pkgVersion = name => JSON.parse(readFileSync(new URL(`../node_modules/${name}/package.json`, import.meta.url), "utf8")).version;
const big = n => BigInt(n);
// The generator decides the engine: `prisma-client` with a driver adapter runs the query compiler; `engineType = "binary"|"library"` would load a Rust engine.
const queryEngine = () => {
  const schema = readFileSync(new URL("../prisma/schema.prisma", import.meta.url), "utf8");
  const gen = schema.match(/generator\s+client\s*{[^}]*}/)?.[0] ?? "";
  const provider = gen.match(/provider\s*=\s*"([^"]+)"/)?.[1] ?? "unknown";
  const engine = gen.match(/engineType\s*=\s*"([^"]+)"/)?.[1];
  if (provider === "prisma-client" && !engine) return "TypeScript query compiler (WASM) with the driver adapter; no Rust query engine";
  return `generator ${provider}${engine ? `, engineType ${engine}` : ""}`;
};

export async function create(name = "prisma") {
  // The adapter reads a timestamptz as UTC after dropping the session offset, so the session runs in UTC.
  const url = new URL(databaseUrl());
  url.searchParams.set("options", "-c timezone=UTC");
  const prisma = new PrismaClient({ adapter: new PrismaPg({ connectionString: url.toString(), max: POOL_SIZE }) });
  await prisma.$connect();

  const filter = (threshold = 500) =>
    prisma.post.findMany({ where: { published: true, views: { gt: threshold } }, orderBy: [{ views: "desc" }, { id: "asc" }], take: 20 });
  const readN = (n, step) => i => {
    const [o] = page(i, n, step);
    return prisma.post.findMany({ orderBy: { id: "asc" }, skip: o, take: n });
  };
  const data = r => ({ post_id: big(r.post_id), author_id: r.author_id === null ? null : big(r.author_id), body: r.body, created_at: r.created_at });

  const related = (query, fields, rel, relFields, pick) => ({
    op: i => query(i),
    norm: raw => raw.map(o => [objs([o], fields)[0], objs(sortById(pick(o[rel])), relFields)]),
  });

  const oneTask = async (t, i) => {
    const out = [];
    const lat = [];
    for (let k = 0; k < CONCURRENT_OPS_PER_TASK; k++) {
      const n = t * CONCURRENT_OPS_PER_TASK + k;
      const t0 = performance.now();
      if (k % 2 === 0) out.push(objs([await prisma.post.findUnique({ where: { id: big(pkFor(n + i)) } })], POST));
      else out.push(objs(await filter(n % 900), POST));
      lat.push(performance.now() - t0);
    }
    return [out, lat];
  };

  const cases = {
    "read-pk": { op: i => prisma.post.findUnique({ where: { id: big(pkFor(i)) } }), norm: raw => objs([raw], POST) },
    "read-50": { op: readN(50, 50), norm: raw => objs(raw, POST) },
    "read-1000": { op: readN(1000, 1000), norm: raw => objs(raw, POST) },
    "projection-1000": {
      op: i => {
        const [o] = page(i, 1000, 1000);
        return prisma.post.findMany({ select: { id: true, title: true }, orderBy: { id: "asc" }, skip: o, take: 1000 });
      },
      norm: raw => raw.map(o => [plain("id", o.id), o.title]),
    },
    filter: { op: () => filter(), norm: raw => objs(raw, POST) },
    "filter-ne-null": {
      // `not` drops the NULL rows (SQL semantics); Django's exclude() keeps them, so the NULL branch is explicit
      op: i =>
        prisma.post.findMany({
          where: { OR: [{ category_id: { not: 3n } }, { category_id: null }] },
          orderBy: { id: "asc" },
          skip: (i % 9) * 20,
          take: 20,
        }),
      norm: raw => objs(raw, POST),
    },
    count: { op: () => prisma.post.count({ where: { published: true, views: { gt: 500 } } }), norm: Number },
    aggregate: {
      op: () => prisma.post.groupBy({ by: ["author_id"], _sum: { views: true }, orderBy: { author_id: "asc" } }),
      norm: raw => raw.map(r => [plain("author_id", r.author_id), Number(r._sum.views)]),
    },
    "join-to-one": related(
      i => {
        const [o] = page(i, 50, 50);
        return prisma.post.findMany({ orderBy: { id: "asc" }, skip: o, take: 50, include: { author: true } });
      },
      POST, "author", USER, a => [a],
    ),
    "prefetch-to-many": related(
      i => {
        const [o] = page(i, 50, 50);
        return prisma.user.findMany({ orderBy: { id: "asc" }, skip: o, take: 50, include: { posts: true } });
      },
      USER, "posts", POST, a => a,
    ),
    // PostTag is an explicit model (Prisma has no `through:` relation), so the tags come through post_tags
    m2m: related(
      i => {
        const [o] = page(i, 50, 50);
        return prisma.post.findMany({ orderBy: { id: "asc" }, skip: o, take: 50, include: { post_tags: { include: { tag: true } } } });
      },
      POST, "post_tags", TAG, a => a.map(pt => pt.tag),
    ),
    "insert-1": {
      op: r => prisma.comment.create({ data: data(r[0]) }),
      setup: i => commentRows(i, 1),
      norm: raw => Number(raw.id),
    },
    "bulk-insert-100": {
      op: r => prisma.comment.createManyAndReturn({ data: r.map(data), select: { id: true } }),
      setup: i => commentRows(i, 100),
      norm: raw => raw.map(o => Number(o.id)),
    },
    "bulk-insert-1000": {
      op: r => prisma.comment.createManyAndReturn({ data: r.map(data), select: { id: true } }),
      setup: i => commentRows(i, 1000),
      norm: raw => raw.map(o => Number(o.id)),
    },
    "update-1": {
      op: i => prisma.post.updateMany({ where: { id: big(pkFor(i)) }, data: { views: { increment: 1 } } }),
      norm: raw => raw.count,
    },
    "bulk-update-100": {
      op: i => {
        const [lo, hi] = updateRange(i);
        return prisma.post.updateMany({ where: { id: { gte: big(lo), lte: big(hi) } }, data: { views: { increment: 1 } } });
      },
      norm: raw => raw.count,
    },
    // Prisma has no bulk upsert: one upsert per row in one transaction
    "upsert-100": {
      op: r =>
        prisma.$transaction(
          r.map(u =>
            prisma.user.upsert({
              where: { email: u.email },
              create: { email: u.email, name: u.name, created_at: u.created_at },
              update: { name: u.name },
              select: { id: true },
            }),
          ),
        ),
      setup: i => upsertRows(i),
      norm: raw => raw.length,
    },
    "delete-100": {
      op: () => prisma.comment.deleteMany({ where: { post_id: 1n, body: "del-bench" } }),
      setup: async (i, conn) => {
        await conn.query(
          "INSERT INTO comments (post_id, author_id, body, created_at) SELECT 1, NULL, 'del-bench', now() FROM generate_series(1, 100)",
        );
        return i;
      },
      norm: raw => raw.count,
    },
    "tx-5": {
      op: i =>
        prisma.$transaction(async tx => {
          const p = big(pkFor(i));
          const r = commentRows(i, 2);
          const first = await tx.comment.create({ data: data(r[0]) });
          await tx.comment.create({ data: data(r[1]) });
          await tx.post.updateMany({ where: { id: p }, data: { views: { increment: 1 } } });
          const post = await tx.post.findUniqueOrThrow({ where: { id: p } });
          await tx.comment.delete({ where: { id: first.id } });
          return post;
        }),
      norm: raw => objs([raw], POST),
    },
    concurrent: {
      op: i => Promise.all(Array.from({ length: CONCURRENT_TASKS }, (_, t) => oneTask(t, i))),
      norm: raw => raw.map(r => r[0]),
      concurrent: true,
    },
  };

  return {
    name,
    cases,
    async stop() {
      await prisma.$disconnect();
    },
    info() {
      const version = pkgVersion("@prisma/client");
      return {
        prisma: version,
        adapter: `@prisma/adapter-pg ${pkgVersion("@prisma/adapter-pg")}`,
        query_engine: queryEngine(),
        // adapter-pg names a statement only when `statementNameGenerator` is set (dist/index.mjs, `name:` in the query config); this runner sets none
        prepared_statements: "unnamed statements: the pg driver re-parses each call (adapter-pg without statementNameGenerator)",
        pool: POOL_SIZE,
        insert_note: "bulk inserts use createManyAndReturn with select { id: true }: ids only, monorm returns full rows",
        upsert_note: "upsert-100 is 100 upserts in one $transaction: Prisma has no bulk upsert, monorm and Django send one statement",
        find_unique_note: "findUnique calls in one tick are batched by Prisma into one query; this runner issues them one at a time",
        update_note: "update-1 uses updateMany (no row read back); update() would add a SELECT",
      };
    },
  };
}
