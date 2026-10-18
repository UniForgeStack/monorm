/** The native row builder sets `annotations` columns as attributes.
 * The public `load()` item arrives later, so these tests inject the IR key. */

import assert from "node:assert/strict";
import { test } from "node:test";

import { QuerySet } from "../src/query.js";
import { Comment, Post, User } from "./blog/models.js";
import { useDatabase } from "./helpers.js";

useDatabase();

type Dynamic = any; // eslint-disable-line @typescript-eslint/no-explicit-any

const ANNOTATION = { name: "n", expr: { t: "func", name: "count", rel: ["comments"] } };

async function withAnnotatedPosts(run: () => Promise<void>): Promise<void> {
  const proto = QuerySet.prototype as Dynamic;
  const original = proto.queryIr;
  proto.queryIr = function (this: Dynamic, op: string, ...args: unknown[]) {
    const result = original.call(this, op, ...args);
    if (op === "select" && this.meta === Post.objects.meta) result[0]["annotations"] = [ANNOTATION];
    return result;
  };
  try {
    await run();
  } finally {
    proto.queryIr = original;
  }
}

async function seed() {
  const [alice] = (await User.objects.insertMany([{ email: "a@example.com", name: "Alice" }]).returning()) as [User];
  const [p1] = (await Post.objects.insertMany([
    { author: alice, title: "p1", body: "" },
    { author: alice, title: "p2", body: "" },
  ]).returning()) as [Post, Post];
  await Comment.objects.insertMany([
    { post: p1, body: "x" },
    { post: p1, body: "y" },
  ]);
}

test("root objects get the annotation", async () => {
  await seed();
  await withAnnotatedPosts(async () => {
    const posts = (await Post.objects.orderBy(Post.title).all()) as Dynamic[];
    assert.deepEqual(posts.map((p) => Number(p.n)), [2, 0]);
  });
});

test("prefetched objects get the annotation", async () => {
  await seed();
  await withAnnotatedPosts(async () => {
    const [alice] = (await User.objects.load(User.posts).all()) as Dynamic[];
    assert.deepEqual(alice.posts.cached.map((p: Dynamic) => Number(p.n)).sort(), [0, 2]);
  });
});
