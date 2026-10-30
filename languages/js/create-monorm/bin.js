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
writeFileSync(pkgPath, JSON.stringify(pkg, null, 2) + "\n");
console.log(`Created ${target}.\n\n  cd ${target}\n  npm install\n  npm start`);
