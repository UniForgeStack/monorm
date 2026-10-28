// Build one profile's platform artifact; do not publish.
import { execFileSync } from "node:child_process";
import { copyFileSync, mkdirSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
const root = resolve(dirname(fileURLToPath(import.meta.url)), "..", "..", "..");
const profile = process.argv[2];
if (!["postgres", "sqlite", "combined", "tooling"].includes(profile)) throw new Error("expected a named profile");
const release = process.argv.includes("--release");
const cargoTarget = process.env.CARGO_TARGET_DIR ?? join(root, "target");
execFileSync("cargo", ["build", "-p", "monorm-node", "--no-default-features", "--features", `profile-${profile}`, ...(release ? ["--release"] : [])], { cwd: root, stdio: "inherit" });
const platform = `${process.platform}-${process.arch}`;
if (!["darwin-arm64", "linux-x64", "win32-x64"].includes(platform)) throw new Error(`unsupported prebuilt platform ${platform}`);
const output = join(root, "target", "node-profiles", `${profile}-${platform}`);
mkdirSync(output, { recursive: true });
const filename = { darwin: "libmonorm_node.dylib", win32: "monorm_node.dll" }[process.platform] ?? "libmonorm_node.so";
copyFileSync(join(cargoTarget, release ? "release" : "debug", filename), join(output, "monorm.node"));
writeFileSync(join(output, "index.cjs"), "module.exports = require('./monorm.node');\n");
writeFileSync(join(output, "package.json"), JSON.stringify({
  name: `@ufstack/monorm-native-${profile}-${platform}`, version: "0.1.0", main: "index.cjs",
  files: ["monorm.node", "index.cjs"], os: [process.platform], cpu: [process.arch], engines: { node: ">=20" },
}, null, 2) + "\n");
console.log(output);
