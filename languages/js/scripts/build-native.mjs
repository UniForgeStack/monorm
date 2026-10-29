// Builds the Rust addon (languages/js/native) for this platform and writes it twice:
// prebuilt/<platform>/monorm.node (where the published package keeps it) and monorm.node (a source checkout).
//   node scripts/build-native.mjs [--release] [--profile tooling]
import { execFileSync } from "node:child_process";
import { copyFileSync, mkdirSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const root = join(here, "..", "..", "..");
const profileIndex = process.argv.indexOf("--profile");
const profile = profileIndex < 0 ? undefined : process.argv[profileIndex + 1];
const featuresIndex = process.argv.indexOf("--features");
const features = featuresIndex < 0 ? undefined : process.argv[featuresIndex + 1];
if ((profileIndex >= 0 && !["postgres", "sqlite", "combined", "tooling"].includes(profile)) ||
    (featuresIndex >= 0 && !features) || (profile && features)) throw new Error("choose --profile postgres|sqlite|combined|tooling or --features <exact Cargo features>");
const release = process.argv.includes("--release");
execFileSync("cargo", ["build", "-p", "monorm-node", ...(release ? ["--release"] : []), ...(profile || features ? ["--no-default-features", "--features", profile ? `profile-${profile}` : features] : [])], { cwd: root, stdio: "inherit" });
const lib = { darwin: "libmonorm_node.dylib", win32: "monorm_node.dll" }[process.platform] ?? "libmonorm_node.so";
const built = join(process.env.CARGO_TARGET_DIR ?? join(root, "target"), release ? "release" : "debug", lib);
const platform = `${process.platform}-${process.arch}`;
mkdirSync(join(here, "..", "prebuilt", platform), { recursive: true });
copyFileSync(built, join(here, "..", "prebuilt", platform, "monorm.node"));
copyFileSync(built, join(here, "..", "monorm.node"));
console.log(`wrote prebuilt/${platform}/monorm.node and monorm.node`);
