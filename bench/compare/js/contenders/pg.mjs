// `pg` with prepared statements (a name per statement text): the floor, no ORM work.
import { createRequire } from "node:module";

import pg from "pg";

import {
  CONCURRENT_OPS_PER_TASK, CONCURRENT_TASKS, POOL_SIZE, POST, TAG, USER, commentRows, databaseUrl, objs, page, pkFor, plain,
  updateRange, upsertRows,
} from "../common.mjs";

const POST_COLS = POST.join(", ");
const USER_COLS = USER.join(", ");

export async function create(name = "pg") {
  const pool = new pg.Pool({ connectionString: databaseUrl(), max: POOL_SIZE });
  const names = new Map();
  /** A prepared statement: the name comes from the text, so each connection prepares it once. */
  const q = (text, values = [], client = pool) => {
    if (!names.has(text)) names.set(text, `s${names.size}`);
    return client.query({ name: names.get(text), text, values });
  };

  const filter = async (threshold = 500) =>
    (await q(`SELECT ${POST_COLS} FROM posts WHERE published AND views > $1 ORDER BY views DESC, id LIMIT 20`, [threshold])).rows;
  const readN = (n, step) => async i => {
    const [o] = page(i, n, step);
    return (await q(`SELECT ${POST_COLS} FROM posts ORDER BY id LIMIT $1 OFFSET $2`, [n, o])).rows;
  };
  const group = (rows, key) => {
    const m = new Map();
    for (const r of rows) (m.get(String(r[key])) ?? m.set(String(r[key]), []).get(String(r[key]))).push(r);
    return m;
  };
  const insertSql = "INSERT INTO comments (post_id, author_id, body, created_at) SELECT * FROM unnest($1::bigint[], $2::bigint[], $3::text[], $4::timestamptz[]) RETURNING id";
  const cols = r => [r.map(x => x.post_id), r.map(x => x.author_id), r.map(x => x.body), r.map(x => x.created_at)];
  const updateViews = (id, client = pool) => q("UPDATE posts SET views = views + 1 WHERE id = $1", [id], client);

  const oneTask = async (t, i) => {
    const out = [];
    const lat = [];
    for (let k = 0; k < CONCURRENT_OPS_PER_TASK; k++) {
      const n = t * CONCURRENT_OPS_PER_TASK + k;
      const t0 = performance.now();
      if (k % 2 === 0) out.push(objs((await q(`SELECT ${POST_COLS} FROM posts WHERE id = $1`, [pkFor(n + i)])).rows, POST));
      else out.push(objs(await filter(n % 900), POST));
      lat.push(performance.now() - t0);
    }
    return [out, lat];
  };

  const cases = {
    "read-pk": {
      op: async i => (await q(`SELECT ${POST_COLS} FROM posts WHERE id = $1`, [pkFor(i)])).rows,
      norm: raw => objs(raw, POST),
    },
    "read-50": { op: readN(50, 50), norm: raw => objs(raw, POST) },
    "read-1000": { op: readN(1000, 1000), norm: raw => objs(raw, POST) },
    "projection-1000": {
      op: async i => {
        const [o] = page(i, 1000, 1000);
        return (await q("SELECT id, title FROM posts ORDER BY id LIMIT $1 OFFSET $2", [1000, o])).rows;
      },
      norm: raw => raw.map(o => [plain("id", o.id), o.title]),
    },
    filter: { op: () => filter(), norm: raw => objs(raw, POST) },
    "filter-ne-null": {
      op: async i =>
        (await q(`SELECT ${POST_COLS} FROM posts WHERE category_id IS DISTINCT FROM $1 ORDER BY id LIMIT 20 OFFSET $2`, [3, (i % 9) * 20])).rows,
      norm: raw => objs(raw, POST),
    },
    count: { op: async () => (await q("SELECT count(*) AS n FROM posts WHERE published AND views > $1", [500])).rows[0].n, norm: Number },
    aggregate: {
      op: async () => (await q("SELECT author_id, sum(views) AS total FROM posts GROUP BY author_id ORDER BY author_id")).rows,
      norm: raw => raw.map(r => [plain("author_id", r.author_id), Number(r.total)]),
    },
    "join-to-one": {
      op: async i => {
        const [o] = page(i, 50, 50);
        const author = USER.map(c => `u.${c} AS u_${c}`).join(", ");
        return (await q(`SELECT ${POST.map(c => `p.${c}`).join(", ")}, ${author} FROM posts p JOIN users u ON u.id = p.author_id ORDER BY p.id LIMIT $1 OFFSET $2`, [50, o])).rows;
      },
      norm: raw => raw.map(r => [objs([r], POST)[0], objs([Object.fromEntries(USER.map(c => [c, r[`u_${c}`]]))], USER)]),
    },
    "prefetch-to-many": {
      op: async i => {
        const [o] = page(i, 50, 50);
        const users = (await q(`SELECT ${USER_COLS} FROM users ORDER BY id LIMIT $1 OFFSET $2`, [50, o])).rows;
        const posts = (await q(`SELECT ${POST_COLS} FROM posts WHERE author_id = ANY($1::bigint[]) ORDER BY id`, [users.map(u => u.id)])).rows;
        const by = group(posts, "author_id");
        return users.map(u => [u, by.get(String(u.id)) ?? []]);
      },
      norm: raw => raw.map(([u, ps]) => [objs([u], USER)[0], objs(ps, POST)]),
    },
    m2m: {
      op: async i => {
        const [o] = page(i, 50, 50);
        const posts = (await q(`SELECT ${POST_COLS} FROM posts ORDER BY id LIMIT $1 OFFSET $2`, [50, o])).rows;
        const tags = (await q("SELECT pt.post_id, t.id, t.name FROM post_tags pt JOIN tags t ON t.id = pt.tag_id WHERE pt.post_id = ANY($1::bigint[]) ORDER BY t.id", [posts.map(p => p.id)])).rows;
        const by = group(tags, "post_id");
        return posts.map(p => [p, by.get(String(p.id)) ?? []]);
      },
      norm: raw => raw.map(([p, ts]) => [objs([p], POST)[0], objs(ts, TAG)]),
    },
    "insert-1": {
      op: async r => (await q("INSERT INTO comments (post_id, author_id, body, created_at) VALUES ($1, $2, $3, $4) RETURNING id", [r[0].post_id, r[0].author_id, r[0].body, r[0].created_at])).rows[0].id,
      setup: i => commentRows(i, 1),
      norm: Number,
    },
    "bulk-insert-100": {
      op: async r => (await q(insertSql, cols(r))).rows,
      setup: i => commentRows(i, 100),
      norm: raw => raw.map(o => Number(o.id)),
    },
    "bulk-insert-1000": {
      op: async r => (await q(insertSql, cols(r))).rows,
      setup: i => commentRows(i, 1000),
      norm: raw => raw.map(o => Number(o.id)),
    },
    "update-1": { op: async i => (await updateViews(pkFor(i))).rowCount, norm: Number },
    "bulk-update-100": {
      op: async i => {
        const [lo, hi] = updateRange(i);
        return (await q("UPDATE posts SET views = views + 1 WHERE id BETWEEN $1 AND $2", [lo, hi])).rowCount;
      },
      norm: Number,
    },
    "upsert-100": {
      op: async r =>
        (await q(
          "INSERT INTO users (email, name, created_at) SELECT * FROM unnest($1::text[], $2::text[], $3::timestamptz[]) ON CONFLICT (email) DO UPDATE SET name = EXCLUDED.name",
          [r.map(u => u.email), r.map(u => u.name), r.map(u => u.created_at)],
        )).rowCount,
      setup: i => upsertRows(i),
      norm: Number,
    },
    "delete-100": {
      op: async () => (await q("DELETE FROM comments WHERE post_id = $1 AND body = $2", [1, "del-bench"])).rowCount,
      setup: async (i, conn) => {
        await conn.query(
          "INSERT INTO comments (post_id, author_id, body, created_at) SELECT 1, NULL, 'del-bench', now() FROM generate_series(1, 100)",
        );
        return i;
      },
      norm: Number,
    },
    "tx-5": {
      op: async i => {
        const c = await pool.connect();
        try {
          const p = pkFor(i);
          const r = commentRows(i, 2);
          const ins = "INSERT INTO comments (post_id, author_id, body, created_at) VALUES ($1, $2, $3, $4) RETURNING id";
          await c.query("BEGIN");
          const first = (await q(ins, [r[0].post_id, r[0].author_id, r[0].body, r[0].created_at], c)).rows[0].id;
          await q(ins, [r[1].post_id, r[1].author_id, r[1].body, r[1].created_at], c);
          await updateViews(p, c);
          const post = (await q(`SELECT ${POST_COLS} FROM posts WHERE id = $1`, [p], c)).rows;
          await q("DELETE FROM comments WHERE id = $1", [first], c);
          await c.query("COMMIT");
          return post;
        } catch (e) {
          await c.query("ROLLBACK");
          throw e;
        } finally {
          c.release();
        }
      },
      norm: raw => objs(raw, POST),
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
      pg: createRequire(import.meta.url)("pg/package.json").version,
      prepared_statements: "named prepared statements, one name per statement text",
      pool: POOL_SIZE,
      join_note: "join-to-one uses an inner JOIN: posts.author_id is NOT NULL, so the rows equal the ORMs' joins",
    }),
  };
}

