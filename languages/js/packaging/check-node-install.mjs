// Packs `monorm`, installs the tarball in a fresh Node project and a fresh Bun project, and runs smoke-node.mjs in each.
//   node languages/js/packaging/check-node-install.mjs
// `npm pack` runs `prepack`: it builds `dist/` and checks that `npm run assemble` filled the package.
// Set ORM_TEST_DATABASE_URL to run the PostgreSQL query too. Set MONORM_PACK_PLATFORMS=all to demand all 3 platforms.
import { execFileSync, spawnSync } from "node:child_process";
import { copyFileSync, mkdirSync, readdirSync, rmSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const pkgRoot = resolve(here, "..");
const root = resolve(pkgRoot, "..", "..");
const packs = join(root, "target", "npm-packs");
const env = { ...process.env, npm_config_cache: join(root, "target", "npm-cache") };
const npm = process.platform === "win32" ? "npm.cmd" : "npm";
mkdirSync(packs, { recursive: true });

const packed = JSON.parse(execFileSync(npm, ["pack", "--json", "--pack-destination", packs], { cwd: pkgRoot, env, encoding: "utf8", stdio: ["ignore", "pipe", "inherit"] }))[0];
const tarball = join(packs, packed.filename);
console.log(`${packed.filename}: ${packed.size} bytes packed, ${packed.unpackedSize} unpacked, ${packed.files.length} files`);
const names = packed.files.map((file) => file.path);
for (const gone of ["unscoped/", "packaging/", "native/src/", "test/"]) {
  if (names.some((name) => name.startsWith(gone))) throw new Error(`the tarball holds ${gone}`);
}
for (const needed of ["dist/src/index.js", "dist/src/auth/index.js", "auth/schema.prisma", "extensions/locking/monorm-extension.toml", "extensions/file-storage/model.js"]) {
  if (!names.includes(needed)) throw new Error(`the tarball lacks ${needed}`);
}
if (!names.some((name) => /^prebuilt\/[^/]+\/monorm\.node$/.test(name))) throw new Error("the tarball holds no native addon");

const consumers = [
  { runtime: process.execPath, name: "node", install: (dir) => execFileSync(npm, ["install", "--no-audit", "--no-fund", `monorm@file:${tarball}`], { cwd: dir, env, stdio: "inherit" }) },
  { runtime: "bun", name: "bun", install: (dir) => execFileSync("bun", ["add", `monorm@file:${tarball}`], { cwd: dir, env, stdio: "inherit" }) },
];
for (const { runtime, name, install } of consumers) {
  if (name === "bun" && spawnSync("bun", ["--version"], { stdio: "ignore" }).status !== 0) {
    if (process.env.CI) throw new Error("bun is not installed");
    console.warn("skip bun: not installed");
    continue;
  }
  const dir = join(root, "target", "node-installs", `monorm-${name}`);
  rmSync(dir, { recursive: true, force: true });
  mkdirSync(dir, { recursive: true });
  writeFileSync(join(dir, "package.json"), JSON.stringify({ private: true, type: "module" }));
  install(dir);
  copyFileSync(join(here, "smoke-node.mjs"), join(dir, "smoke.mjs"));
  execFileSync(runtime, ["smoke.mjs"], { cwd: dir, env, stdio: "inherit" });
  // Delete every extension library: a schema without an extension attribute must load without them.
  const extensions = join(dir, "node_modules", "monorm", "extensions");
  for (const short of readdirSync(extensions)) {
    for (const file of readdirSync(join(extensions, short))) {
      if (/\.(dylib|so|dll)$/.test(file)) rmSync(join(extensions, short, file));
    }
  }
  execFileSync(runtime, ["smoke.mjs", "plain"], { cwd: dir, env, stdio: "inherit" });
}
