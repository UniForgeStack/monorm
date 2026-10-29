// Internal benchmark, TypeScript on Node: one call through the unprepared, prepared and direct-IR paths.
//
//   node bench.mjs --url URL --out DIR [--rounds N] [--quick]
//
// The unprepared path is the public query set API, the prepared path is `prepare()`, and the
// direct path sends the prepared IR JSON and bound values to the native engine (`runJson`).
// The three build the same instances. The driver row runs the same SQL on the raw driver
// (node:sqlite or pg). Each case checks that every path returns the same result before it is timed.
// The Python runner (`../python/bench.py --seed`) seeds the database.
import { execFileSync } from "node:child_process";
import { readFileSync, mkdirSync, writeFileSync } from "node:fs";
import { cpus, loadavg, platform, arch } from "node:os";
import { performance } from "node:perf_hooks";
import { join } from "node:path";
import { parseArgs } from "node:util";

const HERE = new URL(".", import.meta.url);
const SRC = new URL("../../../languages/js/dist/src/", HERE);
const { connect, loads, param } = await import(new URL("index.js", SRC));
const { Builder } = await import(new URL("build.js", SRC));
const { allowedWrites } = await import(new URL("protection.js", SRC));

const SPEC = JSON.parse(readFileSync(new URL("../cases.json", HERE), "utf8"));
const SINK = 11; // the user that the insert case writes to

const { values: opt } = parseArgs({
  options: { url: { type: "string" }, out: { type: "string" }, rounds: { type: "string", default: "15" }, quick: { type: "boolean", default: false } },
});
if (!opt.url || !opt.out) throw new Error("usage: bench.mjs --url URL --out DIR [--rounds N] [--quick]");

const sh = (cmd, ...args) => {
  try {
    return execFileSync(cmd, args, { encoding: "utf8", stdio: ["ignore", "pipe", "ignore"] }).trim() || null;
  } catch {
    return null;
  }
};

const median = xs => {
  const s = [...xs].sort((a, b) => a - b);
  const m = s.length >> 1;
  return s.length % 2 ? s[m] : (s[m - 1] + s[m]) / 2;
};

const sqlite = opt.url.startsWith("sqlite");
const backend = sqlite ? "sqlite" : "postgresql";

/** The raw driver of the backend: runs the SQL of one case and returns its rows. */
async function openDriver() {
  if (sqlite) {
    let mod;
    try {
      mod = await import("node:sqlite");
    } catch {
      return { skip: "node:sqlite is not available in this Node" };
    }
    const conn = new mod.DatabaseSync(opt.url.replace(/^sqlite:\/\//, ""));
    const cache = new Map();
    const stmt = text => cache.get(text) ?? cache.set(text, conn.prepare(text)).get(text);
    return {
      run: async (statements, v) => statements.map(st => stmt(st.text).all(...st.args.map(a => v[a])).map(Object.values)),
      close: async () => conn.close(),
    };
  }
  let pg;
  try {
    pg = await import("pg");
  } catch {
    return { skip: "pg is not installed (run.sh installs it in bench/internal/js)" };
  }
  const client = new (pg.default?.Client ?? pg.Client)({ connectionString: opt.url });
  await client.connect();
  return {
    run: async (statements, v) => {
      const out = [];
      for (const st of statements) {
        let i = 0;
        const text = st.text.replace(/\?/g, () => `$${++i}`);
        out.push((await client.query({ text, values: st.args.map(a => v[a]), rowMode: "array" })).rows);
      }
      return out;
    },
    close: () => client.end(),
  };
}

const src = readFileSync(new URL("../schema.prisma", HERE), "utf8").replaceAll("BACKEND", backend);
const { User, Post } = loads(src);
const db = await connect(opt.url, { maxConnections: 1 });
const driver = await openDriver();
const specs = Object.fromEntries(SPEC.cases.map(c => [c.name, c]));

const big = v => BigInt(v);
const call = (p, kind) => {
  const st = p["statement"](kind);
  return { st, bound: v => st.bind(v, p.params) };
};
const postKey = r => [Number(r.id), Number(r.authorId), r.title, Number(r.views)];
const rowKey = r => [Number(r[0]), Number(r[1]), r[2], Number(r[4])];

/** A case whose three paths run one read: `qs(values)` builds it, `prepared` holds param() slots. */
function reads(name, qs, prepared, kind = "select") {
  const c = { name, ...specs[name], paths: {}, dump: null };
  const p = prepared.prepare();
  const { st, bound } = call(p, kind);
  const n = c.values.length;
  const params = c.values.map(bound);
  const state = p.qs.state;
  const pv = i => c.values[i % n];
  if (kind === "count") {
    c.paths.unprepared = k => qs(pv(k)).count();
    c.paths.prepared = k => p.count(pv(k));
    c.paths.direct = k => db.runJson(st.json, params[k % n]);
    c.norm = (path, r) => (path === "driver" ? Number(r[0][0][0]) : Number(r));
  } else {
    c.paths.unprepared = k => qs(pv(k)).all();
    c.paths.prepared = k => p.all(pv(k));
    c.paths.direct = async k => new Builder(db.registry, state.db).select(await db.runJson(st.json, params[k % n]));
    c.norm = (path, r) => (path === "driver" ? r[0].map(rowKey) : r.map(postKey));
  }
  return c;
}

/** The write has no IR, so it has no prepared path; the direct path sends prepared rows. */
function insert() {
  const c = { name: "insert_one", ...specs.insert_one, paths: {}, cleanup: null };
  const v0 = { authorId: big(c.values[0].author_id), title: c.values[0].title, body: c.values[0].body };
  const qs = Post.objects;
  const plan = qs["insertPlan"]([v0], undefined, null);
  const insertArgs = [qs.meta.name, plan.prepared.fields, plan.prepared.rows, null, null, null, plan.params];
  const scope = JSON.stringify(plan.scope);
  const noDefaults = JSON.stringify(qs.state.withoutDefaults);
  c.cleanup = async () => void (await Post.objects.filter(Post.authorId.eq(big(SINK))).delete());
  c.paths.unprepared = () => Post.objects.insert(v0);
  c.paths.prepared = "a write has no IR to prepare";
  c.paths.direct = async () => {
    const res = await db.send((tx, trace) => db.engine.insert(...insertArgs, tx, allowedWrites(), null, null, trace, true, scope, noDefaults), {});
    return new Builder(db.registry, qs.state.db).returned(res);
  };
  c.norm = (path, r) => {
    if (path === "driver") return [Number(r[0][0][1]), r[0][0][2]];
    const one = Array.isArray(r) ? r[0] : r;
    return [Number(one.authorId), one.title];
  };
  return c;
}

/** The user and the posts of the user: one engine call that runs two statements. */
function prefetch() {
  const c = { name: "prefetch_posts", ...specs.prefetch_posts, paths: {} };
  const p = User.objects.filter(User.id.eq(param("id"))).load(User.posts).prepare();
  const { st, bound } = call(p, "select");
  const n = c.values.length;
  const params = c.values.map(bound);
  const state = p.qs.state;
  c.paths.unprepared = k => User.objects.filter(User.id.eq(big(c.values[k % n].id))).load(User.posts).all();
  c.paths.prepared = k => p.all(c.values[k % n]);
  c.paths.direct = async k => new Builder(db.registry, state.db).select(await db.runJson(st.json, params[k % n]));
  c.norm = (path, r) =>
    path === "driver" ? [[Number(r[0][0][0]), r[1].map(x => Number(x[0])).sort((a, b) => a - b)]] : r.map(u => [Number(u.id), u.posts.cached.map(x => Number(x.id)).sort((a, b) => a - b)]);
  return c;
}

const cases = [
  reads("pk_get", v => Post.objects.filter(Post.id.eq(big(v.id))), Post.objects.filter(Post.id.eq(param("id")))),
  reads(
    "filter_order_50",
    v => Post.objects.filter(Post.authorId.eq(big(v.a)), Post.views.gte(v.min)).orderBy(Post.id).limit(v.n),
    Post.objects.filter(Post.authorId.eq(param("a")), Post.views.gte(param("min"))).orderBy(Post.id).limit(param("n")),
  ),
  reads(
    "read_1000",
    v => Post.objects.filter(Post.id.gt(big(v.after))).orderBy(Post.id).limit(v.n),
    Post.objects.filter(Post.id.gt(param("after"))).orderBy(Post.id).limit(param("n")),
  ),
  reads("filtered_count", v => Post.objects.filter(Post.views.gt(v.min)), Post.objects.filter(Post.views.gt(param("min"))), "count"),
  insert(),
  prefetch(),
];
for (const c of cases) {
  c.paths.driver = driver.skip ?? (k => driver.run(c.sql, c.values[k % c.values.length]));
}

async function check(c) {
  const got = {};
  for (const [path, fn] of Object.entries(c.paths)) {
    if (typeof fn === "string") continue;
    got[path] = JSON.stringify(c.norm(path, await fn(0)));
    if (c.cleanup) await c.cleanup();
  }
  const ref = got.unprepared;
  for (const [path, value] of Object.entries(got)) {
    if (value !== ref) throw new Error(`${c.name}: ${path} returned ${value}, unprepared returned ${ref}`);
  }
  if ((ref === "[]" || ref === "0") && c.name !== "insert_one") throw new Error(`${c.name}: the check result is empty`);
}

async function measure(c, rounds, inner, warm) {
  const live = Object.keys(c.paths).filter(p => typeof c.paths[p] !== "string");
  if (warm) {
    for (const p of live) {
      for (let k = 0; k < Math.max(inner >> 2, 3); k++) await c.paths[p](k);
      if (c.cleanup) await c.cleanup();
    }
  }
  const samples = Object.fromEntries(live.map(p => [p, []]));
  for (let r = 0; r < rounds; r++) {
    const shift = r % live.length;
    for (const p of [...live.slice(shift), ...live.slice(0, shift)]) {
      const fn = c.paths[p];
      const t = performance.now();
      for (let k = 0; k < inner; k++) await fn(k);
      samples[p].push(((performance.now() - t) / inner) * 1e3);
      if (c.cleanup) await c.cleanup();
    }
  }
  const out = {};
  for (const [p, fn] of Object.entries(c.paths)) {
    if (typeof fn === "string") out[p] = { [p === "prepared" ? "na" : "skipped"]: fn };
    else out[p] = { median_us: median(samples[p]), min_us: Math.min(...samples[p]), samples_us: samples[p] };
  }
  return out;
}

const result = {
  environment: {
    language: "typescript",
    node: process.versions.node,
    os: `${platform()} ${arch()}`,
    cpu: cpus()[0]?.model ?? "unknown",
    backend,
    load1_at_start: loadavg()[0],
    commit: sh("git", "rev-parse", "HEAD") ?? "unknown",
    date: new Date().toISOString().replace(/\.\d+Z$/, "Z"),
  },
  quick: opt.quick,
  cases: {},
};
for (const c of cases) {
  await check(c);
  const [rounds, inner] = opt.quick ? [1, 1] : [Number(opt.rounds), c.inner];
  const paths = await measure(c, rounds, inner, !opt.quick);
  result.cases[c.name] = { checked: true, inner, rounds, paths };
  console.log(`typescript ${backend} ${c.name}: ` + Object.entries(paths).map(([p, v]) => ("median_us" in v ? `${p} ${v.median_us.toFixed(1)}us` : `${p} -`)).join(", "));
}
result.environment.load1_at_end = loadavg()[0];
mkdirSync(opt.out, { recursive: true });
writeFileSync(join(opt.out, `js-${backend}.json`), JSON.stringify(result, null, 1));
await driver.close?.();
await db.close();
