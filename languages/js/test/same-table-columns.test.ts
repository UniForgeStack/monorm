/** A query accepts a column of any model on the same table and resolves it by name: no database needed. */

import assert from "node:assert/strict";
import { test } from "node:test";

import { Registry, define } from "../src/index.js";

// `define` builds models that share a table without an extension, as a proxy does.
const model = (name: string, table: string, ...fields: string[]) => ({
  name,
  table,
  fields: [{ name: "id", column: "id", type: "int", primary_key: true }, ...fields.map((f) => ({ name: f, column: f, type: f === "published" ? "bool" : "string" }))],
});
const SCHEMA = { dialect: "sqlite", models: [model("Article", "articles", "title", "published"), model("Slim", "articles", "title"), model("Other", "others", "title")] };
// eslint-disable-next-line @typescript-eslint/no-explicit-any
const { Article, Slim, Other } = define(SCHEMA as never, { registry: new Registry() }) as any;

test("a column of a model on the same table resolves on the query model", () => {
  assert.equal(Slim.objects.filter(Article.title.eq("a")).sql(), Slim.objects.filter(Slim.title.eq("a")).sql());
});

test("a field the query model lacks is the normal no-field error", () => {
  assert.throws(() => Slim.objects.filter(Article.published.eq(true)).sql(), /Slim has no field "published"/);
});

test("a column of a model on another table is still rejected", () => {
  assert.throws(() => Other.objects.filter(Article.title.eq("a")).sql(), /belongs to Article/);
});
