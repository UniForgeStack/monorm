// Shared pieces of the TypeScript benchmark: parameters per case, database reset, state checksum, plain results.
// The values match ../python/common.py, so every contender writes the same rows.
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import pg from "pg";

export const HERE = dirname(fileURLToPath(import.meta.url));
export const ROOT = dirname(HERE);
export const POOL_SIZE = 10;
export const TABLES = ["users", "categories", "tags", "posts", "comments", "post_tags"];
export const CASES = [
  "read-pk", "read-50", "read-1000", "projection-1000", "filter", "filter-ne-null", "count", "aggregate",
  "join-to-one", "prefetch-to-many", "m2m", "insert-1", "bulk-insert-100", "bulk-insert-1000", "update-1",
  "bulk-update-100", "upsert-100", "delete-100", "tx-5", "concurrent",
];
export const BUN_CASES = ["read-pk", "read-1000", "bulk-insert-1000"];
export const CONCURRENT_TASKS = 50;
export const CONCURRENT_OPS_PER_TASK = 4;

export const POST = ["id", "author_id", "category_id", "title", "body", "views", "published", "created_at"];
export const USER = ["id", "email", "name", "created_at"];
export const TAG = ["id", "name"];
const NUMBERS = new Set(["id", "author_id", "category_id", "post_id", "tag_id", "views"]);

const EPOCH = Date.UTC(2027, 0, 1);
export const ts = i => new Date(EPOCH + i * 1000);

export const databaseUrl = () => process.env.BENCH_DATABASE_URL ?? "postgres://postgres:postgres@localhost:55432/bench_nfx1";
export const pkFor = i => ((i * 7919) % 10_000) + 1;

/** `n` new comments for iteration `i`: the same rows for every contender. */
export const commentRows = (i, n) =>
  Array.from({ length: n }, (_, k) => ({
    post_id: ((i * n + k) % 10_000) + 1,
    author_id: k % 5 === 0 ? null : (k % 1000) + 1,
    body: `new ${i}-${k}`,
    created_at: ts(k),
  }));

/** 100 users: 50 with an existing email (conflict) and 50 with a new one. */
export const upsertRows = i => [
  ...Array.from({ length: 50 }, (_, k) => {
    const e = ((i * 50 + k) % 1000) + 1;
    return { email: `user${e}@example.com`, name: `Renamed ${i}-${k}`, created_at: ts(e) };
  }),
  ...Array.from({ length: 50 }, (_, k) => ({ email: `new${i}-${k}@example.com`, name: `New ${i}-${k}`, created_at: ts(k) })),
];

export const updateRange = i => {
  const lo = ((i * 100) % 9_900) + 1;
  return [lo, lo + 99];
};

export const page = (i, size, step) => {
  const o = (i % 9) * step;
  return [o, o + size];
};

export async function connect() {
  const c = new pg.Client({ connectionString: databaseUrl() });
  await c.connect();
  return c;
}

/** Back to the seed: empty every table, restart the identities, reload seed.sql. */
export async function reset(c) {
  await c.query(`TRUNCATE ${TABLES.join(", ")} RESTART IDENTITY CASCADE`);
  await c.query(readFileSync(join(ROOT, "seed.sql"), "utf8"));
}

/** One checksum per table: equal checksums mean equal rows. */
export async function state(c) {
  const out = {};
  for (const t of TABLES) {
    const { rows } = await c.query(`SELECT count(*)::text AS n, md5(coalesce(string_agg(x::text, '|' ORDER BY id), '')) AS h FROM ${t} AS x`);
    out[t] = `${rows[0].n}:${rows[0].h}`;
  }
  return out;
}

/** A value as plain data: ids and counters as numbers, timestamps as ISO text with a +00:00 offset. */
export function plain(name, v) {
  if (v === null || v === undefined) return null;
  if (v instanceof Date) return v.toISOString().replace(/\.000Z$/, "+00:00").replace(/Z$/, "+00:00");
  if (NUMBERS.has(name) || typeof v === "bigint") return Number(v);
  return v;
}

/** Rows of objects as arrays of plain values, the fields read by `get(object, field)`. */
export const objs = (rows, fields, get = (o, f) => o[f]) => rows.map(o => fields.map(f => plain(f, get(o, f))));

export function p99(values) {
  const s = [...values].sort((a, b) => a - b);
  return s[Math.min(s.length - 1, Math.floor(s.length * 0.99))];
}

export const median = values => {
  const s = [...values].sort((a, b) => a - b);
  const m = s.length >> 1;
  return s.length % 2 ? s[m] : (s[m - 1] + s[m]) / 2;
};

export const sortById = rows => [...rows].sort((a, b) => Number(a.id) - Number(b.id));
