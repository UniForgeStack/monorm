// Our `js` package (Node addon), written as docs/api/typescript-api.md shows; untyped models from loads().
import { readFileSync } from "node:fs";
import { join } from "node:path";

import {
  CONCURRENT_OPS_PER_TASK, CONCURRENT_TASKS, POOL_SIZE, POST, ROOT, TAG, USER, commentRows, databaseUrl, objs, page, pkFor,
  plain, sortById, updateRange, upsertRows,
} from "../common.mjs";

const SRC = new URL("../../../../languages/js/dist/src/", import.meta.url);
const lib = await import(new URL("index.js", SRC));
const { native } = await import(new URL("native.js", SRC));
const { camel, connect, func, loads } = lib;

const get = (o, f) => o[camel(f)];
const rows = fields => list => objs(list, fields, get);

export async function create(name = "orm") {
  const models = loads(readFileSync(join(ROOT, "schema.prisma"), "utf8"));
  const { User, Post, Comment, Tag } = models;
  const db = await connect(databaseUrl(), { maxConnections: POOL_SIZE });

  const filter = (threshold = 500) =>
    Post.objects.filter(Post.published, Post.views.gt(threshold)).orderBy(Post.views.desc(), Post.id).limit(20);
  const readN = (n, step) => i => {
    const [o, e] = page(i, n, step);
    return Post.objects.orderBy(Post.id).slice(o, e);
  };
  const projection = i => {
    const [o, e] = page(i, 1000, 1000);
    return Post.objects.load(Post.id, Post.title).orderBy(Post.id).slice(o, e);
  };
  // category_id is nullable: `ne` drops the NULL rows unless orNull() keeps them (Django's exclude() keeps them)
  const filterNeNull = i => Post.objects.filter(Post.categoryId.ne(3n).orNull()).orderBy(Post.id).slice((i % 9) * 20, (i % 9) * 20 + 20);
  const aggregate = () =>
    Post.objects.select({ authorId: Post.authorId, total: func.sum(Post.views) }).groupBy(Post.authorId).orderBy(Post.authorId);
  const joinOne = i => {
    const [o, e] = page(i, 50, 50);
    return Post.objects.orderBy(Post.id).slice(o, e).load(Post.author);
  };
  const prefetch = i => {
    const [o, e] = page(i, 50, 50);
    return User.objects.orderBy(User.id).slice(o, e).load(User.posts);
  };
  const m2m = i => {
    const [o, e] = page(i, 50, 50);
    return Post.objects.orderBy(Post.id).slice(o, e).load(Post.tags);
  };

  const readPkFilter = i => Post.objects.filter(Post.id.eq(pkFor(i)));
  const run = q => q.all();
  const sql = q => q.sql();
  const camelRow = r => ({ postId: BigInt(r.post_id), authorId: r.author_id === null ? null : BigInt(r.author_id), body: r.body, createdAt: r.created_at });
  const reads = (builder, fields) => ({
    op: i => run(builder(i)),
    norm: raw => rows(fields)(raw),
    build: builder,
    sql: i => sql(builder(i)),
  });
  const related = (builder, fields, rel, relFields, many) => ({
    op: i => run(builder(i)),
    norm: raw =>
      raw.map(o => {
        const r = o[rel];
        const items = many ? sortById(r.cached) : [r];
        return [rows(fields)([o])[0], rows(relFields)(items)];
      }),
    build: builder,
    sql: i => sql(builder(i)),
  });

  const oneTask = async (t, i) => {
    const out = [];
    const lat = [];
    for (let k = 0; k < CONCURRENT_OPS_PER_TASK; k++) {
      const n = t * CONCURRENT_OPS_PER_TASK + k;
      const t0 = performance.now();
      if (k % 2 === 0) out.push(rows(POST)([await Post.objects.get(Post.id.eq(pkFor(n + i)))]));
      else out.push(rows(POST)(await filter(n % 900)));
      lat.push(performance.now() - t0);
    }
    return [out, lat];
  };

  const cases = {
    "read-pk": {
      op: i => Post.objects.get(Post.id.eq(pkFor(i))),
      norm: raw => rows(POST)([raw]),
      build: readPkFilter,
      sql: i => sql(readPkFilter(i)), // the filter form: `get` would run the query
    },
    "read-50": reads(readN(50, 50), POST),
    "read-1000": reads(readN(1000, 1000), POST),
    "projection-1000": {
      op: i => run(projection(i)),
      norm: raw => raw.map(o => [plain("id", o.id), o.title]),
      build: projection,
      sql: i => sql(projection(i)),
    },
    filter: reads(() => filter(), POST),
    "filter-ne-null": reads(filterNeNull, POST),
    count: { op: () => Post.objects.filter(Post.published, Post.views.gt(500)).count(), norm: raw => Number(raw) },
    aggregate: {
      op: i => run(aggregate(i)),
      norm: raw => raw.map(r => [plain("author_id", r.authorId), Number(r.total)]),
      build: aggregate,
      sql: i => sql(aggregate(i)),
    },
    "join-to-one": related(joinOne, POST, "author", USER, false),
    "prefetch-to-many": related(prefetch, USER, "posts", POST, true),
    m2m: related(m2m, POST, "tags", TAG, true),
    "insert-1": {
      op: r => Comment.objects.insert(camelRow(r[0])),
      setup: i => commentRows(i, 1),
      norm: raw => Number(raw.id),
    },
    "bulk-insert-100": {
      op: r => Comment.objects.insertMany(r.map(camelRow)).returning(),
      setup: i => commentRows(i, 100),
      norm: raw => raw.map(o => Number(o.id)),
    },
    "bulk-insert-1000": {
      op: r => Comment.objects.insertMany(r.map(camelRow)).returning(),
      setup: i => commentRows(i, 1000),
      norm: raw => raw.map(o => Number(o.id)),
    },
    "update-1": { op: i => Post.objects.filter(Post.id.eq(pkFor(i))).update({ views: Post.views.add(1) }), norm: Number },
    "bulk-update-100": {
      op: i => {
        const [lo, hi] = updateRange(i);
        return Post.objects.filter(Post.id.between(lo, hi)).update({ views: Post.views.add(1) });
      },
      norm: Number,
    },
    "upsert-100": {
      op: r =>
        User.objects
          .insertMany(r.map(u => ({ email: u.email, name: u.name, createdAt: u.created_at })))
          .onConflict(User.email, { update: true, updateFields: [User.name] }),
      setup: i => upsertRows(i),
      norm: Number,
    },
    "delete-100": {
      op: () => Comment.objects.filter(Comment.postId.eq(1), Comment.body.eq("del-bench")).delete(),
      setup: async (i, conn) => {
        await conn.query(
          "INSERT INTO comments (post_id, author_id, body, created_at) SELECT 1, NULL, 'del-bench', now() FROM generate_series(1, 100)",
        );
        return i;
      },
      norm: Number,
    },
    "tx-5": {
      op: i =>
        db.transaction(async () => {
          const p = pkFor(i);
          const r = commentRows(i, 2);
          const first = await Comment.objects.insert(camelRow(r[0]));
          await Comment.objects.insert(camelRow(r[1]));
          await Post.objects.filter(Post.id.eq(p)).update({ views: Post.views.add(1) });
          const post = await Post.objects.get(Post.id.eq(p));
          await Comment.objects.filter(Comment.id.eq(first.id)).delete();
          return post;
        }),
      norm: raw => rows(POST)([raw]),
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
      await db.close();
    },
    info() {
      const meta = JSON.parse(native().profileMetadata());
      return {
        profile: meta.profile,
        build: meta.build,
        capabilities: meta.capabilities,
        native: process.env.ORM_NATIVE ?? "languages/js/orm.node",
        prepared_statements: "engine default: prepared once per connection and cached",
        insert_note: "orm returns full rows: insertMany(...).returning() has no key-only form",
        upsert_note: "onConflict returns a row count, no RETURNING",
        pool: POOL_SIZE,
      };
    },
  };
}
