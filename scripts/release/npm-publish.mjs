#!/usr/bin/env node
// Publish the npm packages of a release, in dependency order, and skip each version that is already up.
// usage: npm-publish.mjs <dir-with-tgz> --mode real|dry-run
import { execFileSync, spawnSync } from "node:child_process";
import { readdirSync } from "node:fs";
import { join } from "node:path";

const NPM = process.env.NPM_BIN ?? "npm";

// Platform packages first, then the profiles (they depend on the platforms), then the core, then the packages that peer on the core.
function rank(name) {
  if (/^@ufstack\/monorm-native-[a-z]+-(darwin|linux|win32)-/.test(name)) return 0;
  if (name.startsWith("@ufstack/monorm-native-")) return 1;
  if (name === "@ufstack/monorm") return 2;
  if (name === "@ufstack/monorm-auth") return 4;
  return 3;
}

export function readPackage(tgz) {
  const text = execFileSync("tar", ["-xzOf", tgz, "package/package.json"], { encoding: "utf8" });
  const { name, version } = JSON.parse(text);
  return { tgz, name, version };
}

export function plan(dir) {
  const packs = readdirSync(dir).filter((f) => f.endsWith(".tgz")).map((f) => readPackage(join(dir, f)));
  return packs.sort((a, b) => rank(a.name) - rank(b.name) || a.name.localeCompare(b.name));
}

function published({ name, version }) {
  const result = spawnSync(NPM, ["view", `${name}@${version}`, "version"], { encoding: "utf8" });
  return result.status === 0 && result.stdout.trim() === version;
}

function main(args) {
  const dir = args[0];
  const mode = args[args.indexOf("--mode") + 1];
  if (!dir || !["real", "dry-run"].includes(mode)) {
    console.error("usage: npm-publish.mjs <dir> --mode real|dry-run");
    return 2;
  }
  for (const pack of plan(dir)) {
    if (published(pack)) {
      console.log(`skip ${pack.name}@${pack.version}: already published`);
      continue;
    }
    const flags = mode === "real" ? ["--provenance", "--access", "public"] : ["--dry-run", "--access", "public"];
    console.log(`publish ${pack.name}@${pack.version} (${mode})`);
    const result = spawnSync(NPM, ["publish", pack.tgz, ...flags], { stdio: "inherit" });
    if (result.status !== 0) {
      console.error(`npm publish failed for ${pack.name}@${pack.version}`);
      return 1;
    }
  }
  return 0;
}

if (import.meta.url === `file://${process.argv[1]}`) process.exit(main(process.argv.slice(2)));
