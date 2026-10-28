// Extension language code at runtime. The schema has no installed package, so the lock stamps
// are added to the IR; the extension modules and `define()` are real.
import assert from "node:assert/strict";
import { test } from "node:test";
import { define, QuerySet, Registry, useQuerySet } from "../src/index.js";
import { InstanceBase } from "../src/ext.js";
import { call, native } from "../src/native.js";

const source = `
model Post {
  id Int @id
  title String
}
model Tag {
  id Int @id
}
`;

type Ctor = new (...args: any[]) => QuerySet<never>;
const mixinA = (Base: Ctor) => class extends (Base as Ctor & { prototype: { chain(): string[] } }) {
  everything() { return (this as never as { withoutDefaults(): unknown }).withoutDefaults(); }
  chain() { return ["a"]; }
};
const mixinB = (Base: Ctor) => class extends (Base as unknown as new (...args: any[]) => { chain(): string[] }) {
  override chain() { return ["b", ...super.chain()]; }
};
class AInstance { label() { return `a${String((this as never as { pk: unknown }).pk)}`; } }

const HASH = "0".repeat(64);

// The lock stamp of a package; the members come from the real module.
function record(extension: string, changes: { overrides?: string[]; dependencies?: Record<string, string> } = {}) {
  return { extension, ...changes };
}

function build(records: ReturnType<typeof record>[], extensions: Record<string, { module: object }>, members: object) {
  const ir = JSON.parse(call(() => native().compileSchema(source, null))) as { behavior?: object };
  ir.behavior = {
    schema_contract: 5,
    ...ir.behavior,
    packages: Object.fromEntries(records.map((r) => [r.extension, {
      manifest_hash: HASH, version: "1.0.0", attributes: [], languages: ["typescript"], databases: ["postgres", "sqlite"], dependencies: r.dependencies,
      language_members: { typescript: { query_set: "queries", instance: "Instance", overrides: r.overrides ?? [] } },
    }])),
  };
  const withHash = Object.fromEntries(Object.entries(extensions).map(([id, e]) => [id, { module: { manifestHash: HASH, ...e.module } }]));
  return define(ir as never, { registry: new Registry(), extensions: withHash, members } as never);
}

const code = (module: object = { queries: mixinA, Instance: AInstance }) => ({ "ext.a": { module } });
const other = { module: { queries: mixinB, Instance: class {} } };

test("members reach only the models that use the extension", () => {
  const { Post, Tag } = build([record("ext.a")], code(), { Post: ["ext.a"] }) as Record<string, any>;
  assert.deepEqual(Post.objects.chain(), ["a"]);
  assert.equal(typeof Post.objects.everything, "function");
  assert.equal(Tag.objects.chain, undefined);
  assert.equal("label" in Post._meta.Row.prototype, true);
  assert.equal("label" in Tag._meta.Row.prototype, false);
});

test("two mixins chain through super in dependency order", () => {
  const records = [record("ext.a"), record("ext.b", { overrides: ["chain"], dependencies: { "ext.a": "*" } })];
  const extensions = { ...code(), "ext.b": other };
  const { Post } = build(records, extensions, { Post: ["ext.a", "ext.b"] }) as Record<string, any>;
  assert.deepEqual(Post.objects.chain(), ["b", "a"]);
  assert.deepEqual(Post.objects.everything().chain(), ["b", "a"]);
});

test("the user query set class comes first, then the extension classes", () => {
  const records = [record("ext.a"), record("ext.b", { overrides: ["chain"], dependencies: { "ext.a": "*" } })];
  const extensions = { ...code(), "ext.b": other };
  class Queries extends QuerySet<never> {
    chain(): string[] {
      // @ts-expect-error the member comes from an extension mixin
      return ["user", ...super.chain()];
    }
  }
  const { Post } = build(records, extensions, { Post: ["ext.a", "ext.b"] }) as Record<string, any>;
  useQuerySet(Post, Queries);
  assert.deepEqual(Post.objects.chain(), ["user", "b", "a"]);
  assert.equal(Post.objects.constructor, Queries);
});

for (const members of [{ Post: ["ext.a"] }, { Tag: ["ext.a"] }, { Post: ["ext.a"], Tag: ["ext.a", "ext.b"] }]) {
  test(`a shared user query set keeps each model's extensions: ${JSON.stringify(members)}`, () => {
    const records = [record("ext.a"), record("ext.b", { overrides: ["chain"], dependencies: { "ext.a": "*" } })];
    const extensions = { ...code(), "ext.b": other };
    class Parent extends QuerySet<never> {
      chain(): string[] {
        // @ts-expect-error the member comes from an extension mixin
        return ["user", ...(super.chain?.() ?? [])];
      }
    }
    class Queries extends Parent {}
    const { Post, Tag } = build(records, extensions, members) as Record<string, any>;
    useQuerySet(Post, () => Queries);
    useQuerySet(Tag, () => Queries);
    const expected = (ids: string[] = []) => ["user", ...(ids.includes("ext.b") ? ["b"] : []), ...(ids.includes("ext.a") ? ["a"] : [])];
    for (const Model of [Post, Tag, Post, Tag]) {
      assert.deepEqual(Model.objects.chain(), expected(members[Model._meta.name as keyof typeof members]));
      assert.deepEqual(Model.objects.filter(Model.id.eq(1)).chain(), expected(members[Model._meta.name as keyof typeof members]));
      assert.equal(typeof Model.objects.everything, members[Model._meta.name as keyof typeof members]?.length ? "function" : "undefined");
      assert.equal("everything" in Model.objects, Boolean(members[Model._meta.name as keyof typeof members]?.length));
      assert.ok(Model.objects instanceof Queries);
    }
  });
}

test("an instance override calls super down the chain to the host member", () => {
  const records = [
    record("ext.a", { overrides: ["toJSON"] }),
    record("ext.b", { overrides: ["toJSON"], dependencies: { "ext.a": "*" } }),
  ];
  const instance = (tag: string) => class extends InstanceBase<{ toJSON(): object }> {
    override toJSON() { return { ...super.toJSON(), [tag]: true }; }
  };
  const none = (Base: Ctor) => class extends Base {};
  const extensions = {
    "ext.a": { module: { queries: none, Instance: instance("a") } },
    "ext.b": { module: { queries: none, Instance: instance("b") } },
  };
  const { Post, Tag } = build(records, extensions, { Post: ["ext.a", "ext.b"], Tag: ["ext.a"] }) as Record<string, any>;
  assert.deepEqual(Object.create(Post._meta.Row.prototype).toJSON(), { a: true, b: true });
  assert.deepEqual(Object.create(Tag._meta.Row.prototype).toJSON(), { a: true });
});

test("an undeclared collision fails at define", () => {
  const filter = (Base: Ctor) => class extends (Base as unknown as new (...args: any[]) => object) { filter() { return this; } } as never;
  assert.throws(() => build([record("ext.a")], { "ext.a": { module: { queries: filter, Instance: class {} } } }, { Post: ["ext.a"] }), /Post\.filter: extension ext\.a query set member collides with a host member; declare it as an override/);
  class Titled { title() { return "t"; } }
  assert.throws(() => build([record("ext.a", { overrides: ["title"] })], { "ext.a": { module: { queries: mixinB, Instance: Titled } } }, { Post: ["ext.a"] }), /Post\.title: .* field or a relation/);
});

test("two extensions with one name collide unless overridden", () => {
  const records = [record("ext.a"), record("ext.b", { dependencies: { "ext.a": "*" } })];
  const extensions = { ...code(), "ext.b": other };
  assert.throws(() => build(records, extensions, { Post: ["ext.a", "ext.b"] }), /Post\.chain: extension ext\.b query set member collides with a member of extension ext\.a/);
});

test("no extension changes nothing", () => {
  const { Post } = define(call(() => native().compileSchema(source, null)), { registry: new Registry() }) as Record<string, any>;
  assert.equal(Object.getPrototypeOf(Post.objects).constructor, QuerySet);
});
