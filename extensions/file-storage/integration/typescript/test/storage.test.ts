import { test } from "node:test";
import assert from "node:assert/strict";
import { mkdtemp, readdir, readFile, realpath, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { Operator } from "opendal";
import { CHUNK_SIZE, CapabilityError, Reference, Registry, SizeLimitError, StorageError, UploadError } from "../src/index.js";

async function collect(source: AsyncIterable<Uint8Array>): Promise<Buffer> {
  const parts: Uint8Array[] = []; for await (const part of source) parts.push(part); return Buffer.concat(parts);
}
/** Wraps a real operator; records calls and reports version and presign support. */
function recording(inner: Operator, options: { version?: string; presign?: boolean } = {}) {
  const calls: { method: string; args: unknown[] }[] = [];
  const operator = {
    capability: () => ({ readWithVersion: options.version !== undefined, presignRead: options.presign === true }),
    writer: (...args: unknown[]) => { calls.push({ method: "writer", args }); return (inner.writer as any)(...args); },
    stat: async (...args: unknown[]) => ({ ...(await (inner.stat as any)(...args)), version: options.version }),
    reader: (...args: unknown[]) => { calls.push({ method: "reader", args }); return (inner.reader as any)(args[0]); },
    presignRead: async (...args: unknown[]) => { calls.push({ method: "presignRead", args }); return { url: `https://files/${args[0]}` }; },
  };
  return { operator: operator as unknown as Operator, calls };
}
async function withFs(run: (operator: Operator, root: string) => Promise<void>) {
  const root = await realpath(await mkdtemp(join(tmpdir(), "storage-test-")));
  try { await run(new Operator("fs", { root }), root); }
  finally { await rm(root, { recursive: true, force: true }); }
}
async function* big(count = 200) { for (let i = 0; i < count; i++) yield Buffer.alloc(CHUNK_SIZE, 120); }

test("shared wire vectors and invalid values", async () => {
  const vectors = JSON.parse(await readFile(new URL("../../../../fixtures/references.json", import.meta.url), "utf8"));
  for (const data of vectors) assert.deepEqual(Reference.fromJSON(data).toJSON(), data);
  for (const extra of [{ v: 2 }, { size: -1 }, { size: true }, { size: 2 ** 53 }, { version: null }, { storage: "" }, { secret: "no" }, { key: "\ud800" }]) {
    assert.throws(() => Reference.fromJSON({ v: 1, storage: "local", key: "key", ...extra }));
  }
});
test("upload streams in bounded chunks with a random UUID4 hex key and the default content type", () => withFs(async (operator, root) => {
  const { operator: spy, calls } = recording(operator);
  const registry = new Registry(new Map([["reports", spy]]));
  const ref = await registry.upload("reports", big(), { filename: "report.xlsx" });
  assert.match(ref.key, /^[0-9a-f]{12}4[0-9a-f]{3}[89ab][0-9a-f]{15}$/);
  assert.notEqual(ref.key, (await registry.upload("reports", Buffer.from("x"))).key);
  assert.equal(ref.size, 200 * CHUNK_SIZE);
  assert.equal(ref.contentType, "application/octet-stream");
  assert.deepEqual(calls[0]!.args[1], { contentType: "application/octet-stream" });
  for await (const chunk of registry.open(ref)) assert.ok(chunk.length > 0);
  assert.equal((await collect(registry.open(ref))).length, ref.size);
  const typed = await registry.upload("reports", Buffer.from("x"), { contentType: "text/plain" });
  assert.equal(typed.contentType, "text/plain");
  assert.deepEqual(calls.at(-1)!.args[1], { contentType: "text/plain" });
  assert.equal((await readdir(root)).length, 3);
}));
test("maxSize stops the stream and the error carries the candidate reference", () => withFs(async (operator) => {
  const registry = new Registry(new Map([["reports", operator]]));
  let pulled = 0;
  async function* counted() { for (let i = 0; i < 10; i++) { pulled++; yield Buffer.alloc(CHUNK_SIZE); } }
  const failure = await registry.upload("reports", counted(), { maxSize: CHUNK_SIZE + 1 }).then(() => assert.fail("no error"), (e: unknown) => e);
  assert.ok(failure instanceof UploadError);
  assert.ok(failure.cause instanceof SizeLimitError);
  assert.equal(failure.reference.storage, "reports");
  assert.equal(failure.reference.size, undefined);
  assert.equal(pulled, 2);
  async function* invalid() { yield "not bytes" as unknown as Uint8Array; }
  await assert.rejects(registry.upload("reports", invalid()), (e: unknown) => e instanceof UploadError && e.cause instanceof TypeError);
  const controller = new AbortController();
  async function* cancel() { yield Buffer.from("partial"); controller.abort(); yield Buffer.from("more"); }
  await assert.rejects(registry.upload("reports", cancel(), { signal: controller.signal }), (e: unknown) => e instanceof UploadError && (e.cause as Error).name === "AbortError");
}));
test("an unconfigured storage fails before any write", () => withFs(async (operator, root) => {
  const registry = new Registry(new Map([["reports", operator]]));
  await assert.rejects(registry.upload("other", Buffer.from("x")), StorageError);
  assert.throws(() => registry.operator("other"), StorageError);
  assert.equal((await readdir(root)).length, 0);
}));
test("the version is stored in the reference and passed to open", () => withFs(async (operator) => {
  const { operator: spy, calls } = recording(operator, { version: "v7" });
  const registry = new Registry(new Map([["reports", spy]]));
  const ref = await registry.upload("reports", Buffer.from("data"));
  assert.equal(ref.version, "v7");
  await collect(registry.open(ref));
  assert.deepEqual(calls.find(c => c.method === "reader")!.args[1], { version: "v7" });
  const plain = recording(operator);
  const unversioned = await new Registry(new Map([["reports", plain.operator]])).upload("reports", Buffer.from("data"));
  assert.equal(unversioned.version, undefined);
  await collect(new Registry(new Map([["reports", plain.operator]])).open(unversioned));
  assert.equal(plain.calls.find(c => c.method === "reader")!.args[1], undefined);
}));
test("signedUrl presigns, validates the expiry and raises CapabilityError without presign", () => withFs(async (operator) => {
  const { operator: signing, calls } = recording(operator, { presign: true });
  const registry = new Registry(new Map([["reports", signing], ["plain", operator]]));
  const ref = new Reference({ v: 1, storage: "reports", key: "k" });
  assert.equal(await registry.signedUrl(ref), "https://files/k");
  assert.equal(calls.at(-1)!.args[1], 300);
  assert.equal(await registry.signedUrl(ref, { expiresIn: 60 }), "https://files/k");
  assert.equal(calls.at(-1)!.args[1], 60);
  await assert.rejects(registry.signedUrl(ref, { expiresIn: 0 }), RangeError);
  await assert.rejects(registry.signedUrl(new Reference({ v: 1, storage: "plain", key: "k" })), CapabilityError);
}));
test("a real service without presign raises CapabilityError", async () => {
  const registry = new Registry(new Map([["m", new Operator("memory")]]));
  const ref = await registry.upload("m", Buffer.from("x"));
  await assert.rejects(registry.signedUrl(ref), CapabilityError);
  assert.equal((await collect(registry.open(ref))).toString(), "x");
});
test("the capability flag gates presign, and a service Unsupported error maps to CapabilityError", () => withFs(async (operator) => {
  const ref = new Reference({ v: 1, storage: "s", key: "k" });
  const flagOff = recording(operator, { presign: false }).operator;
  await assert.rejects(new Registry(new Map([["s", flagOff]])).signedUrl(ref), CapabilityError);
  const refusing = { capability: () => ({ presignRead: true }), presignRead: async () => { throw new Error("Unsupported (permanent) at presign"); } } as unknown as Operator;
  await assert.rejects(new Registry(new Map([["s", refusing]])).signedUrl(ref), CapabilityError);
  const broken = { capability: () => ({ presignRead: true }), presignRead: async () => { throw new Error("network down"); } } as unknown as Operator;
  await assert.rejects(new Registry(new Map([["s", broken]])).signedUrl(ref), /network down/);
}));
