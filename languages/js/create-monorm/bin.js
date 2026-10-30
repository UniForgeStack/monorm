#!/usr/bin/env node
// The project starter is `monorm init` (crates/cli); this file only picks the TypeScript defaults.
const args = process.argv.slice(2);
if (args.length === 0) {
  console.error("Usage: npm create monorm <dir>");
  process.exit(1);
}
// `bun create` runs this file under Node too, so the user agent is the only sign of the package manager.
const agent = process.env.npm_config_user_agent ?? "";
const tool = ["bun", "pnpm", "yarn"].find((name) => agent.startsWith(`${name}/`)) ?? "npm";
const given = (flag) => args.some((arg) => arg === flag || arg.startsWith(`${flag}=`));
const defaults = [...(given("--lang") ? [] : ["--lang", "typescript"]), ...(given("--tool") ? [] : ["--tool", tool])];
const { main } = await import("@ufstack/monorm/cli");
process.exit(await main(["init", ...args, ...defaults]));
