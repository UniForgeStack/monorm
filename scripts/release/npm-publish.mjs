#!/usr/bin/env node
// Publish the npm tarballs of a release and skip each version that is already up.
// usage: npm-publish.mjs <dir-with-tgz> --mode real|dry-run
import { execFileSync, spawnSync } from "node:child_process";
import { readdirSync } from "node:fs";
import { join } from "node:path";

const NPM = process.env.NPM_BIN ?? "npm";

export function readPackage(tgz) {
  const text = execFileSync("tar", ["-xzOf", tgz, "package/package.json"], { encoding: "utf8" });
  const { name, version } = JSON.parse(text);
  return { tgz, name, version };
}

export function plan(dir) {
  return readdirSync(dir).filter((f) => f.endsWith(".tgz")).sort().map((f) => readPackage(join(dir, f)));
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
    // npm refuses a prerelease version without a dist-tag.
    if (pack.version.includes("-")) flags.push("--tag", "next");
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
