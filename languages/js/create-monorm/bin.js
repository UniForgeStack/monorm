#!/usr/bin/env node
import { cpSync, existsSync, readFileSync, readdirSync, renameSync, writeFileSync } from "node:fs";
import { basename, dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const target = process.argv[2];
if (!target) {
  console.error("Usage: npm create monorm <dir>");
  process.exit(1);
}
const dest = resolve(target);
if (existsSync(dest) && readdirSync(dest).length > 0) {
  console.error(`${target} is not empty.`);
  process.exit(1);
}
cpSync(join(dirname(fileURLToPath(import.meta.url)), "template"), dest, { recursive: true });
// npm drops .gitignore files from a package, so the template keeps the file under another name.
renameSync(join(dest, "_gitignore"), join(dest, ".gitignore"));
const pkgPath = join(dest, "package.json");
const pkg = JSON.parse(readFileSync(pkgPath, "utf8"));
pkg.name = basename(dest);
// `bun create` runs this file under Node too, so the user agent is the only sign of Bun.
const bun = (process.env.npm_config_user_agent ?? "").startsWith("bun/");
if (bun) pkg.scripts.start = "bun index.ts";
writeFileSync(pkgPath, JSON.stringify(pkg, null, 2) + "\n");
const tool = bun ? "bun" : "npm";
console.log(`Created ${target}.\n\n  cd ${target}\n  ${tool} install\n  ${tool} start`);
