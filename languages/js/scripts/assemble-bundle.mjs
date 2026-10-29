// Assembles the native parts of the one `monorm` package: the engine and the extension libraries.
//   node scripts/assemble-bundle.mjs                      build for this platform, in place
//   node scripts/assemble-bundle.mjs --collect <dir>      build this platform only into <dir>/<name>/<platform>/ (a CI matrix job)
//   node scripts/assemble-bundle.mjs --libraries <dir>    add the platforms that other jobs collected into <dir>
//   node scripts/assemble-bundle.mjs --check              verify the package content (the `prepack` step)
// `<name>` is `engine` or the directory name of a bundled extension. In place, the package holds
// prebuilt/<platform>/monorm.node and extensions/<short>/{index.js, index.d.ts, monorm-extension.toml, <libraries>}.
// `npm pack` works with one platform; set MONORM_PACK_PLATFORMS=all to make a missing platform an error.
import { execFileSync } from "node:child_process";
import { cpSync, existsSync, mkdirSync, mkdtempSync, readdirSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const PLATFORMS = ["darwin-arm64", "linux-x64", "win32-x64"];
const here = dirname(fileURLToPath(import.meta.url));
const pkgRoot = join(here, "..");
const repo = join(pkgRoot, "..", "..");
const pkg = JSON.parse(readFileSync(join(pkgRoot, "package.json"), "utf8"));
const shorts = Object.values(pkg.monorm.bundled.extensions).map((dir) => dir.replace(/^extensions\//, ""));
const host = `${process.platform}-${process.arch}`;
const args = process.argv.slice(2);
const option = (name) => (args.includes(name) ? args[args.indexOf(name) + 1] : undefined);
const run = (command, argv, options = {}) => execFileSync(command, argv, { cwd: repo, stdio: ["ignore", "inherit", "inherit"], ...options });

function packager() {
  run("cargo", ["build", "-q", "-p", "monorm-extension-package"]);
  return join(process.env.CARGO_TARGET_DIR ?? join(repo, "target"), "debug", process.platform === "win32" ? "monorm-extension-package.exe" : "monorm-extension-package");
}

/** Copies the files of `from` into `to`, without the package.json and README that a standalone package needs. */
function copyBundle(from, to) {
  rmSync(to, { recursive: true, force: true });
  cpSync(from, to, { recursive: true, filter: (file) => !/[\\/](package\.json|README\.md)$/.test(file) });
}

function expectedExports(short, npm) {
  const own = JSON.parse(readFileSync(join(npm, "package.json"), "utf8")).exports;
  return Object.fromEntries(Object.entries(own).map(([key, value]) => [
    key === "." ? `./extensions/${short}` : `./extensions/${short}/${key.slice(2)}`,
    { types: value.types.replace("./", `./extensions/${short}/`), default: value.default.replace("./", `./extensions/${short}/`) },
  ]));
}

function buildHost() {
  const tool = packager();
  const tsc = join(pkgRoot, "node_modules", ".bin", process.platform === "win32" ? "tsc.cmd" : "tsc");
  const work = mkdtempSync(join(tmpdir(), "monorm-bundle-"));
  try {
    for (const short of shorts) {
      const out = join(work, short);
      run(tool, ["package", join(repo, "extensions", short), "--out", out, "--tsc", tsc], { stdio: ["ignore", "ignore", "inherit"] });
      checkExports(short, join(out, "npm"));
      copyBundle(join(out, "npm"), join(pkgRoot, "extensions", short));
    }
  } finally {
    rmSync(work, { recursive: true, force: true });
  }
  run("node", [join(here, "build-native.mjs"), "--release", "--profile", "tooling"]);
}

function checkExports(short, npm) {
  for (const [key, value] of Object.entries(expectedExports(short, npm))) {
    if (JSON.stringify(pkg.exports[key]) !== JSON.stringify(value)) {
      throw new Error(`package.json "exports" ${key} must be ${JSON.stringify(value)}, the entry that the ${short} extension declares`);
    }
  }
}

function collect(dir) {
  const tool = packager();
  mkdirSync(dir, { recursive: true });
  const zig = process.env.MONORM_CARGO_TARGET ? ["--zigbuild", process.env.MONORM_CARGO_TARGET] : [];
  for (const short of shorts) run(tool, ["library", join(repo, "extensions", short), "--out", join(dir, short), ...zig]);
  run("node", [join(here, "build-native.mjs"), "--release", "--profile", "tooling"]);
  mkdirSync(join(dir, "engine", host), { recursive: true });
  cpSync(join(pkgRoot, "monorm.node"), join(dir, "engine", host, "monorm.node"));
}

function merge(dir) {
  for (const name of ["engine", ...shorts]) {
    for (const platform of PLATFORMS) {
      const from = join(dir, name, platform);
      if (!existsSync(from)) continue;
      const to = name === "engine" ? join(pkgRoot, "prebuilt", platform) : join(pkgRoot, "extensions", name);
      mkdirSync(to, { recursive: true });
      cpSync(from, to, { recursive: true });
    }
  }
}

function check() {
  const missing = [];
  const present = PLATFORMS.filter((platform) => existsSync(join(pkgRoot, "prebuilt", platform, "monorm.node")));
  if (present.length === 0) missing.push("prebuilt/<platform>/monorm.node: run `npm run assemble`");
  for (const short of shorts) {
    const dir = join(pkgRoot, "extensions", short);
    for (const file of ["index.js", "index.d.ts", "monorm-extension.toml"]) {
      if (!existsSync(join(dir, file))) missing.push(`extensions/${short}/${file}: run \`npm run assemble\``);
    }
  }
  if (missing.length > 0) throw new Error(`the package is not assembled:\n${missing.join("\n")}`);
  const lacking = PLATFORMS.filter((platform) => !present.includes(platform));
  if (lacking.length > 0) {
    const message = `the package holds the native libraries of ${present.join(", ")} only; missing: ${lacking.join(", ")}`;
    if (process.env.MONORM_PACK_PLATFORMS === "all") throw new Error(message);
    console.warn(`warning: ${message}`);
  }
  if (process.env.MONORM_PACK_PLATFORMS === "all") {
    for (const short of shorts) {
      const suffixes = readdirSync(join(pkgRoot, "extensions", short)).map((file) => /\.(dylib|so|dll)$/.exec(file)?.[1]).filter(Boolean);
      const lacking = ["dylib", "so", "dll"].filter((suffix) => suffixes.length > 0 && !suffixes.includes(suffix));
      if (lacking.length > 0) throw new Error(`extensions/${short} lacks the library for: ${lacking.join(", ")}`);
    }
  }
  for (const [key, value] of Object.entries(pkg.exports)) {
    for (const file of key.startsWith("./extensions/") || key.startsWith("./auth") ? [value.default ?? value, value.types].filter((f) => typeof f === "string") : []) {
      if (!existsSync(join(pkgRoot, file))) throw new Error(`package.json "exports" ${key} points to ${file}, which is missing`);
    }
  }
}

if (args.includes("--check")) check();
else if (option("--collect")) collect(option("--collect"));
else if (option("--libraries")) merge(option("--libraries"));
else buildHost();
