import { test } from "node:test";
import assert from "node:assert/strict";
import { mkdtemp, realpath, readdir, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { Pending } from "@ufstack/monorm";
import { Operator } from "opendal";
import { Registry, Reference, Upload, FileField, FileFieldError, FileNotLoaded, MissingFile } from "@ufstack/monorm-file-storage";
import { FileAdapter } from "@ufstack/monorm-file-storage/model";

async function environment(run: (registry: Registry, field: FileField, root: string) => Promise<void>) {
  const root = await realpath(await mkdtemp(join(tmpdir(), "file-write-")));
  const registry = new Registry(new Map([["reports", new Operator("fs", { root })]]));
  try { await run(registry, new FileField("file", "reports", true), root); }
  finally { await rm(root, { recursive: true, force: true }); }
}
test("validate defers an upload and prepare stores it", () => environment(async (registry, field, root) => {
  const adapter = new FileAdapter(field, () => registry);
  const pending = adapter.validate(new Upload(Buffer.from("report"), { filename: "report.xlsx" }));
  assert.ok(pending instanceof Pending);
  assert.equal((await readdir(root)).length, 0);
  const json = await adapter.prepare(pending.value);
  assert.deepEqual(adapter.decode(json), Reference.fromJSON(json));
  assert.equal((await readdir(root)).length, 1);
}));
test("references and null validate to stored values; bad ones fail before upload", () => environment(async (registry, field, root) => {
  const adapter = new FileAdapter(field, () => registry);
  const ref = await registry.upload("reports", Buffer.from("report"));
  assert.deepEqual(adapter.validate(ref), ref.toJSON());
  assert.equal(adapter.validate(null), null);
  assert.throws(() => new FileAdapter(new FileField("file", "other"), () => registry).validate(ref), FileFieldError);
  assert.throws(() => new FileAdapter(new FileField("file", "reports"), () => registry).validate(null), MissingFile);
  assert.throws(() => new FileAdapter(field, () => undefined).validate(new Upload(Buffer.from("x"))), FileFieldError);
  assert.equal((await readdir(root)).length, 1);
}));
test("null and unloaded operations fail before provider access", () => environment(async (registry, field, root) => {
  await assert.rejects(field.signedUrl({}, registry), FileNotLoaded);
  assert.throws(() => field.open({ file: null }, registry), MissingFile);
  assert.equal(field.decode(null), null);
  assert.equal((await readdir(root)).length, 0);
}));
