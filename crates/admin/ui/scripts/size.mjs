// Prints the first-load size: the index page plus the scripts and styles it loads, raw and gzip.
import { readFileSync } from "node:fs";
import { gzipSync } from "node:zlib";
import { join } from "node:path";

const dist = new URL("../dist/", import.meta.url).pathname;
const html = readFileSync(join(dist, "index.html"), "utf8");
const files = ["index.html"];
for (const m of html.matchAll(/(?:src|href)="\/admin\/(assets\/[^"]+)"/g)) files.push(m[1]);
let raw = 0;
let gz = 0;
for (const f of files) {
  const bytes = readFileSync(join(dist, f));
  const packed = gzipSync(bytes, { level: 9 }).length;
  raw += bytes.length;
  gz += packed;
  console.log(`${f}  ${bytes.length} B  gzip ${packed} B`);
}
console.log(`first load: ${raw} B raw, ${gz} B gzip (${(gz / 1024).toFixed(1)} KB, budget 300 KB)`);
if (gz > 300 * 1024) {
  console.error("over the 300 KB gzip budget");
  process.exit(1);
}
