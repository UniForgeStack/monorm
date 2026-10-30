import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { addonPath } from "../src/native.js";
import { test } from "node:test";

// The version of this package, so `versions.py set` needs no edit here.
const { version } = JSON.parse(readFileSync(new URL("../../package.json", import.meta.url), "utf8")) as { version: string };
const nativeUrl = new URL("../src/native.js", import.meta.url).href;
test("the native loader rejects an incompatible artifact before use", () => {
  const artifacts = fileURLToPath(new URL("../../target/", import.meta.url));
  mkdirSync(artifacts, { recursive: true });
  const directory = mkdtempSync(join(artifacts, "monorm-profile-"));
  const path = join(directory, "mock.cjs");
  const build = { features: ["sqlite"], rustc: "rustc 1.0.0", target: "t", profile: "release", revision: "r" };
  const metadata = { abi: 1, version, language: "node", profile: "sqlite", backends: ["sqlite"], adapters: [], build,
    capabilities: { cli: false, "generate-python": false, "generate-typescript": false } };
  const run = (env: Record<string, string> = {}) => execFileSync(process.execPath,
    ["--input-type=module", "-e", `const { native } = await import(${JSON.stringify(nativeUrl)}); native();`],
    { env: { ...process.env, MONORM_NATIVE: path, ...env }, stdio: "pipe" });
  try {
    writeFileSync(path, `module.exports={setDecimalClass(){},profileMetadata(){return ${JSON.stringify(JSON.stringify(metadata))}}};`);
    run();
    writeFileSync(path, `module.exports={profileMetadata(){return ${JSON.stringify(JSON.stringify({ ...metadata, abi: 2 }))}}};`);
    assert.throws(() => run(), /incompatible monorm native artifact/);
    for (const invalid of [null, [], { ...metadata, capabilities: [] }, { ...metadata, build: undefined },
      { ...metadata, build: { ...build, features: "sqlite" } }, { ...metadata, build: { ...build, revision: null } },
      { ...metadata, adapters: ["reference-loading", "reference-loading"],
        capabilities: { "reference-loading": true } }]) {
      writeFileSync(path, `module.exports={profileMetadata(){return ${JSON.stringify(JSON.stringify(invalid))}}};`);
      assert.throws(() => run(), /incompatible monorm native artifact/);
    }
  } finally { rmSync(directory, { recursive: true, force: true }); }
});

test("an unsupported platform names the bundled platforms and the source build", () => {
  const root = mkdtempSync(join(tmpdir(), "monorm-platform-"));
  try {
    for (const platform of ["freebsd-x64", "linux-riscv64"]) {
      assert.throws(() => addonPath(platform, root), (e: unknown) => e instanceof Error
        && ["darwin-arm64", "linux-arm64", "linux-x64", "win32-x64", platform, "MONORM_NATIVE"].every((word) => e.message.includes(word)));
    }
    assert.throws(() => addonPath("linux-x64", root), /linux-x64 is missing.*MONORM_NATIVE/);
    mkdirSync(join(root, "prebuilt", "linux-x64"), { recursive: true });
    writeFileSync(join(root, "prebuilt", "linux-x64", "monorm.node"), "");
    assert.equal(addonPath("linux-x64", root), join(root, "prebuilt", "linux-x64", "monorm.node"));
    writeFileSync(join(root, "monorm.node"), "");
    assert.equal(addonPath("freebsd-x64", root), join(root, "monorm.node"));
  } finally { rmSync(root, { recursive: true, force: true }); }
});

test("the loaded native artifact embeds its build record", async () => {
  const { native } = await import(nativeUrl);
  const metadata = JSON.parse(native().profileMetadata());
  const build = metadata.build;
  for (const backend of metadata.backends) assert.ok(build.features.includes(backend), backend);
  assert.match(build.rustc, /^rustc /);
  assert.ok(build.target && build.revision);
});

test("the loaded native artifact reports no adapters", async () => {
  const { native, nativeAdapters } = await import(nativeUrl);
  assert.deepEqual(JSON.parse(native().profileMetadata()).adapters, []);
  assert.deepEqual(nativeAdapters(), []);
});
