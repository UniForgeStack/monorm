// Equality check and timing for the TypeScript contenders; runs on Node and on Bun.
//
//   node run.mjs check [--contenders a,b] [--cases x,y]
//   node run.mjs run   [--rounds 20] [--iters 20] [--max-load 2] [--orm-artifact DIR] [--contenders a,b] [--out DIR]
//   bun run.mjs run    (ours and Prisma; read-pk, read-1000 and bulk-insert-1000 only)
//
// Each contender runs in its own process (`worker`), once per round, with the database reset before every process.
// `run` does `check` first and stops when it fails; the check compares result and database state at iterations
// 0, 1 and the last one. Before each round the runner waits until the 1-minute load is at most `--max-load`.
// Each case runs WARMUP_CALLS untimed calls first, then `--iters` timed calls. `--orm-artifact DIR` adds `orm-all`: the `orm` runner with
// ORM_NATIVE=DIR/orm.node, the `all` extension artifact of scripts/feature-check.sh.
import { spawnSync } from "node:child_process";
import { mkdirSync, readFileSync, unlinkSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { isDeepStrictEqual, parseArgs } from "node:util";

import {
  BUN_CASES, CASES, CONCURRENT_OPS_PER_TASK, CONCURRENT_TASKS, HERE, POOL_SIZE, WARMUP_BASE, WARMUP_CALLS, connect, databaseUrl,
  median, p99, reset, state, waitQuiet,
} from "./common.mjs";

const IS_BUN = typeof Bun !== "undefined";
const RUNTIME = IS_BUN ? "bun" : "node";
const REPO = join(HERE, "..", "..", "..");
const json = v => JSON.stringify(v, (_, x) => (typeof x === "bigint" ? Number(x) : x));
const thenable = x => x !== null && typeof x === "object" && typeof x.then === "function";

const baseContenders = () => (IS_BUN ? ["orm", "prisma"] : ["orm", "prisma", "pg"]);
const defaultCases = () => (IS_BUN ? BUN_CASES : CASES);
const prefix = IS_BUN ? "bun-" : "";

/** The contenders of this run: the runtime's own set plus `orm-all` with an artifact, narrowed by `--contenders`. */
function pick(a) {
  const all = [...baseContenders(), ...(a.artifact ? ["orm-all"] : [])];
  if (!a.contenders) return all;
  const want = a.contenders.split(",");
  const bad = want.filter(n => !all.includes(n));
  if (bad.length) throw new Error(`unknown contender ${bad.join(", ")}; available: ${all.join(", ")}`);
  return all.filter(n => want.includes(n));
}

/** The iterations the equality check runs: the first call, a second one, and the last timed one. */
const checkIterations = iters => [...new Set([0, 1, iters])].sort((x, y) => x - y);

async function make(name) {
  const base = name.startsWith("orm") ? "orm" : name;
  return (await import(`./contenders/${base}.mjs`)).create(name);
}

// ---- worker ---------------------------------------------------------------------------------------------------

async function worker(a) {
  const c = await make(a.contender);
  const conn = await connect();
  const out = { contender: a.contender, runtime: RUNTIME, info: c.info(), cases: {} };
  try {
    for (const name of a.cases.split(",")) {
      const kase = c.cases[name];
      if (a.mode === "check") {
        const results = [];
        for (const i of a.checkAt.split(",").map(Number)) {
          const arg = await (kase.setup ?? (x => x))(i, conn);
          results.push(JSON.parse(json(kase.norm(await kase.op(arg)))));
        }
        out.cases[name] = { result: results };
      } else {
        out.cases[name] = await timeCase(kase, conn, a.iters);
      }
    }
  } finally {
    await conn.end();
    await c.stop();
  }
  writeFileSync(a.out, json(out));
}

async function timeCase(kase, conn, iters) {
  const once = async i => {
    const arg = await (kase.setup ?? (x => x))(i, conn);
    const t0 = performance.now();
    let raw = kase.op(arg);
    if (thenable(raw)) raw = await raw;
    return [performance.now() - t0, raw];
  };
  // warm-up: connections, statement caches, imports; a fixed count, the same for every contender
  for (let k = 0; k < WARMUP_CALLS; k++) await once(WARMUP_BASE + k);
  const samples = [];
  const lat = [];
  for (let i = 1; i <= iters; i++) {
    const [ms, raw] = await once(i);
    samples.push(ms);
    if (kase.concurrent) for (const [, ls] of raw) lat.push(...ls);
  }
  const res = { samples_ms: samples, median_ms: median(samples) };
  if (kase.concurrent) {
    res.ops_per_s = (CONCURRENT_TASKS * CONCURRENT_OPS_PER_TASK) / (res.median_ms / 1000);
    res.p99_ms = p99(lat);
  }
  if (kase.build && kase.sql) {
    const reps = 20;
    let t0 = performance.now();
    for (let i = 0; i < reps; i++) kase.build(i);
    const build = (performance.now() - t0) / reps;
    t0 = performance.now();
    for (let i = 0; i < reps; i++) kase.sql(i);
    res.cpu = { build_ms: build, sql_ms: (performance.now() - t0) / reps };
  }
  return res;
}

// ---- parent ---------------------------------------------------------------------------------------------------

function spawn(name, cases, mode, iters, artifact) { // `iters`: a count when timing, a comma list when checking
  const env = { ...process.env, BENCH_DATABASE_URL: databaseUrl() };
  if (name === "orm-all") {
    if (!artifact) throw new Error("orm-all needs --orm-artifact");
    env.ORM_NATIVE = join(artifact, "orm.node");
  }
  const out = join(tmpdir(), `bench-js-${process.pid}-${Math.random().toString(36).slice(2)}.json`);
  const cmd = [join(HERE, "run.mjs"), "worker", "--contender", name, "--cases", cases.join(","), "--mode", mode, "--iters", String(iters), "--out", out];
  const p = spawnSync(process.execPath, cmd, { env, encoding: "utf8", maxBuffer: 1 << 28 });
  if (p.status !== 0 || p.error) throw new Error(`${name} worker failed:\n${p.error ?? ""}\n${p.stdout.slice(-2000)}\n${p.stderr.slice(-4000)}`);
  const data = JSON.parse(readFileSync(out, "utf8"));
  unlinkSync(out);
  return data;
}

async function check(names, cases, artifact, iters) {
  const at = checkIterations(iters);
  const infos = {};
  const failures = [];
  const conn = await connect();
  for (const kase of cases) {
    const got = {};
    for (const n of names) {
      await reset(conn);
      const data = spawn(n, [kase], "check", at.join(","), artifact);
      infos[n] = data.info;
      got[n] = [data.cases[kase].result, await state(conn)];
    }
    const first = names[0];
    for (const n of names.slice(1)) {
      for (const [what, i] of [["result", 0], ["state", 1]]) {
        if (!isDeepStrictEqual(got[first][i], got[n][i])) {
          failures.push(`${kase}: ${what} differs, ${first} vs ${n}\n    ${first}: ${json(got[first][i]).slice(0, 300)}\n    ${n}: ${json(got[n][i]).slice(0, 300)}`);
        }
      }
    }
    console.log(`check ${kase}: ${failures.some(f => f.startsWith(`${kase}:`)) ? "FAIL" : "ok"}`);
  }
  await conn.end();
  if (failures.length) throw new Error(`equality check failed:\n${failures.join("\n")}`);
  return infos;
}

const uptime = () => spawnSync("uptime", { encoding: "utf8" }).stdout.trim();

async function environment(infos, artifact, before, extra) {
  const conn = await connect();
  const pgVersion = (await conn.query("select version()")).rows[0].version;
  await conn.end();
  const version = p => JSON.parse(readFileSync(join(HERE, "node_modules", p, "package.json"), "utf8")).version;
  return {
    date: new Date().toISOString(),
    git_sha: spawnSync("git", ["rev-parse", "HEAD"], { cwd: REPO, encoding: "utf8" }).stdout.trim(),
    runtime: IS_BUN ? `bun ${Bun.version}` : `node ${process.version}`,
    machine: `${process.platform} ${process.arch}`,
    packages: { pg: version("pg"), prisma: version("prisma"), "@prisma/client": version("@prisma/client"), "@prisma/adapter-pg": version("@prisma/adapter-pg") },
    postgres: pgVersion,
    database: databaseUrl().split("/").pop(),
    pool_size: POOL_SIZE,
    uptime_before: before,
    uptime_after: uptime(),
    orm_artifact: artifact ?? null,
    warmup_calls: WARMUP_CALLS,
    contenders: infos,
    ...extra,
  };
}

async function run(a) {
  const names = pick(a);
  const cases = a.cases ? a.cases.split(",") : defaultCases();
  const before = uptime();
  const infos = await check(names, cases, a.artifact, a.iters);
  mkdirSync(a.out, { recursive: true });
  const conn = await connect();
  const rounds = Object.fromEntries(names.map(n => [n, []]));
  const roundLoads = [];
  for (let r = 0; r < a.rounds; r++) {
    roundLoads.push(Number((await waitQuiet(a.maxLoad)).toFixed(2)));
    const k = r % names.length;
    for (const n of [...names.slice(k), ...names.slice(0, k)]) {
      // the contender order rotates every round
      await reset(conn);
      rounds[n].push(spawn(n, cases, "time", a.iters, a.artifact).cases);
      console.log(`round ${r + 1} ${n} done`);
    }
  }
  await conn.end();
  for (const n of names) {
    const res = {};
    for (const kase of cases) {
      const per = rounds[n].map(rd => rd[kase]);
      const all = per.flatMap(p => p.samples_ms);
      const item = { median_ms: median(all), samples_ms: all, batch_medians_ms: per.map(p => p.median_ms) };
      if (per[0].cpu) item.cpu = Object.fromEntries(Object.keys(per[0].cpu).map(key => [key, median(per.map(p => p.cpu[key]))]));
      for (const key of ["ops_per_s", "p99_ms"]) if (key in per[0]) item[key] = median(per.map(p => p[key]));
      res[kase] = item;
    }
    writeFileSync(join(a.out, `${prefix}${n}.json`), JSON.stringify(res, null, 2));
  }
  const extra = { rounds: a.rounds, iters: a.iters, round_loads: roundLoads, max_load: a.maxLoad };
  writeFileSync(join(a.out, `${prefix}environment.json`), JSON.stringify(await environment(infos, a.artifact, before, extra), null, 2));
  console.log(`wrote ${a.out}`);
}

const [cmd, ...rest] = process.argv.slice(2);
const { values: v } = parseArgs({
  args: rest,
  options: {
    cases: { type: "string", default: "" },
    contenders: { type: "string", default: "" },
    "orm-artifact": { type: "string" },
    rounds: { type: "string", default: "20" },
    iters: { type: "string", default: "20" },
    "max-load": { type: "string", default: "2" },
    out: { type: "string", default: join(HERE, "results", "smoke") },
    contender: { type: "string" },
    mode: { type: "string" },
  },
});
const a = { cases: v.cases, contenders: v.contenders, artifact: v["orm-artifact"], rounds: Number(v.rounds), iters: Number(v.iters), checkAt: v.iters, maxLoad: Number(v["max-load"]), out: v.out, contender: v.contender, mode: v.mode };
if (cmd === "worker") await worker(a);
else if (cmd === "check") await check(pick(a), a.cases ? a.cases.split(",") : defaultCases(), a.artifact, a.iters);
else if (cmd === "run") await run(a);
else throw new Error("usage: run.mjs check|run");
