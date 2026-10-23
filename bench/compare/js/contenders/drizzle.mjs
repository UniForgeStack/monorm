// Drizzle ORM on node-postgres, written as its docs show: the SQL-like builder, and relational queries (`db.query`) for relations.
import { readFileSync } from "node:fs";

import { and, asc, between, count, desc, eq, gt, isNull, ne, or, relations, sql, sum } from "drizzle-orm";
import { drizzle } from "drizzle-orm/node-postgres";
import { bigint, boolean, integer, pgTable, text, timestamp, varchar } from "drizzle-orm/pg-core";
import pg from "pg";

import {
  CONCURRENT_OPS_PER_TASK, CONCURRENT_TASKS, POOL_SIZE, POST, TAG, USER, commentRows, databaseUrl, objs, page, pkFor, plain,
  sortById, updateRange, upsertRows,
} from "../common.mjs";

const id = name => bigint(name, { mode: "number" });
const createdAt = () => timestamp("created_at", { withTimezone: true }).notNull();

const users = pgTable("users", {
  id: id("id").primaryKey().generatedByDefaultAsIdentity(),
  email: varchar("email", { length: 254 }).notNull().unique(),
  name: varchar("name", { length: 100 }).notNull(),
  createdAt: createdAt(),
});
const tags = pgTable("tags", {
  id: id("id").primaryKey().generatedByDefaultAsIdentity(),
  name: varchar("name", { length: 50 }).notNull().unique(),
});
const posts = pgTable("posts", {
  id: id("id").primaryKey().generatedByDefaultAsIdentity(),
  authorId: id("author_id").notNull().references(() => users.id),
  categoryId: id("category_id"),
  title: varchar("title", { length: 200 }).notNull(),
  body: text("body").notNull(),
  views: integer("views").notNull().default(0),
  published: boolean("published").notNull().default(false),
  createdAt: createdAt(),
});
const comments = pgTable("comments", {
  id: id("id").primaryKey().generatedByDefaultAsIdentity(),
  postId: id("post_id").notNull().references(() => posts.id),
  authorId: id("author_id").references(() => users.id),
  body: text("body").notNull(),
  createdAt: createdAt(),
});
const postTags = pgTable("post_tags", {
  id: id("id").primaryKey().generatedByDefaultAsIdentity(),
  postId: id("post_id").notNull().references(() => posts.id),
  tagId: id("tag_id").notNull().references(() => tags.id),
});

const schema = {
  users, tags, posts, comments, postTags,
  usersRelations: relations(users, ({ many }) => ({ posts: many(posts) })),
  postsRelations: relations(posts, ({ one, many }) => ({
    author: one(users, { fields: [posts.authorId], references: [users.id] }),
    postTags: many(postTags),
  })),
  postTagsRelations: relations(postTags, ({ one }) => ({
    post: one(posts, { fields: [postTags.postId], references: [posts.id] }),
    tag: one(tags, { fields: [postTags.tagId], references: [tags.id] }),
  })),
};

const camel = f => f.replace(/_([a-z])/g, (_, c) => c.toUpperCase());
const get = (o, f) => o[camel(f)];
const rows = fields => list => objs(list, fields, get);
const values = r => ({ postId: r.post_id, authorId: r.author_id, body: r.body, createdAt: r.created_at });

export async function create(name = "drizzle") {
  const pool = new pg.Pool({ connectionString: databaseUrl(), max: POOL_SIZE });
  const db = drizzle({ client: pool, schema });

  const filter = (threshold = 500) =>
    db.select().from(posts).where(and(eq(posts.published, true), gt(posts.views, threshold))).orderBy(desc(posts.views), asc(posts.id)).limit(20);
  const readPk = i => db.select().from(posts).where(eq(posts.id, pkFor(i)));
  const readN = (n, step) => i => db.select().from(posts).orderBy(asc(posts.id)).limit(n).offset(page(i, n, step)[0]);
  const projection = i => db.select({ id: posts.id, title: posts.title }).from(posts).orderBy(asc(posts.id)).limit(1000).offset(page(i, 1000, 1000)[0]);
  // category_id is nullable: `ne` drops the NULL rows; Django's exclude() keeps them, so the NULL branch is explicit
  const filterNeNull = i =>
    db.select().from(posts).where(or(ne(posts.categoryId, 3), isNull(posts.categoryId))).orderBy(asc(posts.id)).limit(20).offset((i % 9) * 20);
  const aggregate = () =>
    db.select({ authorId: posts.authorId, total: sum(posts.views) }).from(posts).groupBy(posts.authorId).orderBy(asc(posts.authorId));
  const relational = (table, with_) => i => db.query[table].findMany({ with: with_, orderBy: t => [asc(t.id)], limit: 50, offset: page(i, 50, 50)[0] });
  const joinOne = relational("posts", { author: true });
  const prefetch = relational("users", { posts: true });
  const m2m = relational("posts", { postTags: { with: { tag: true } } });
  const bulkInsert = r => db.insert(comments).values(r.map(values)).returning({ id: comments.id });
  const plusOne = { views: sql`${posts.views} + 1` };

  const oneTask = async (t, i) => {
    const out = [];
    const lat = [];
    for (let k = 0; k < CONCURRENT_OPS_PER_TASK; k++) {
      const n = t * CONCURRENT_OPS_PER_TASK + k;
      const t0 = performance.now();
      if (k % 2 === 0) out.push(rows(POST)(await readPk(n + i)));
      else out.push(rows(POST)(await filter(n % 900)));
      lat.push(performance.now() - t0);
    }
    return [out, lat];
  };

  /** A read case: `op` runs the query, `build` makes it, `sql` makes it and renders its SQL. */
  const read = (builder, norm) => ({ op: i => builder(i), norm, build: builder, sql: i => builder(i).toSQL().sql });
  const related = (builder, fields, pick, relFields) =>
    read(builder, raw => raw.map(o => [rows(fields)([o])[0], rows(relFields)(sortById(pick(o)))]));

  const cases = {
    "read-pk": read(readPk, rows(POST)),
    "read-50": read(readN(50, 50), rows(POST)),
    "read-1000": read(readN(1000, 1000), rows(POST)),
    "projection-1000": read(projection, raw => raw.map(o => [plain("id", o.id), o.title])),
    filter: read(() => filter(), rows(POST)),
    "filter-ne-null": read(filterNeNull, rows(POST)),
    count: {
      op: async () => (await db.select({ n: count() }).from(posts).where(and(eq(posts.published, true), gt(posts.views, 500))))[0].n,
      norm: Number,
    },
    aggregate: read(aggregate, raw => raw.map(r => [plain("author_id", r.authorId), Number(r.total)])),
    "join-to-one": related(joinOne, POST, o => [o.author], USER),
    "prefetch-to-many": related(prefetch, USER, o => o.posts, POST),
    m2m: related(m2m, POST, o => o.postTags.map(pt => pt.tag), TAG),
    "insert-1": {
      op: async r => (await db.insert(comments).values(values(r[0])).returning({ id: comments.id }))[0].id,
      setup: i => commentRows(i, 1),
      norm: Number,
    },
    "bulk-insert-100": { op: bulkInsert, setup: i => commentRows(i, 100), norm: raw => raw.map(o => Number(o.id)) },
    "bulk-insert-1000": { op: bulkInsert, setup: i => commentRows(i, 1000), norm: raw => raw.map(o => Number(o.id)) },
    "update-1": { op: async i => (await db.update(posts).set(plusOne).where(eq(posts.id, pkFor(i)))).rowCount, norm: Number },
    "bulk-update-100": {
      op: async i => (await db.update(posts).set(plusOne).where(between(posts.id, ...updateRange(i)))).rowCount,
      norm: Number,
    },
    "upsert-100": {
      op: async r =>
        (await db.insert(users).values(r.map(u => ({ email: u.email, name: u.name, createdAt: u.created_at })))
          .onConflictDoUpdate({ target: users.email, set: { name: sql`excluded.name` } })).rowCount,
      setup: i => upsertRows(i),
      norm: Number,
    },
    "delete-100": {
      op: async () => (await db.delete(comments).where(and(eq(comments.postId, 1), eq(comments.body, "del-bench")))).rowCount,
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
        db.transaction(async tx => {
          const p = pkFor(i);
          const r = commentRows(i, 2);
          const [first] = await tx.insert(comments).values(values(r[0])).returning({ id: comments.id });
          await tx.insert(comments).values(values(r[1]));
          await tx.update(posts).set(plusOne).where(eq(posts.id, p));
          const post = await tx.select().from(posts).where(eq(posts.id, p));
          await tx.delete(comments).where(eq(comments.id, first.id));
          return post;
        }),
      norm: rows(POST),
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
      await pool.end();
    },
    info: () => ({
      "drizzle-orm": JSON.parse(readFileSync(new URL("../node_modules/drizzle-orm/package.json", import.meta.url), "utf8")).version,
      prepared_statements: "Drizzle default: unnamed statements, no prepare (`.prepare(name)` is opt-in)",
      pool: `pg.Pool max=${POOL_SIZE}`,
      relation_note: "join-to-one, prefetch-to-many and m2m use relational queries (db.query): one SQL statement with json subqueries per relation",
      insert_note: "bulk-insert-* returns ids only (`returning({ id })`), as Prisma and pg do",
    }),
  };
}
