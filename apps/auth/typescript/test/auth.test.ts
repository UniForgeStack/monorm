import assert from "node:assert/strict";
import { pbkdf2Sync } from "node:crypto";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { test, type TestContext } from "node:test";
import { boundMeta } from "@ufstack/monorm/ext";

import { UserMethods, UserQueries, authenticate, checkPassword, makePassword, needsUpdate, syncPermissions } from "../src/index.js";
import { Auth, PACKAGE, PROVIDERS, VARIANTS, makeAuth, type Provider, type Rows } from "./support.js";

const django = (raw: string): string => `pbkdf2_sha256$600000$salt$${pbkdf2Sync(raw, "salt", 600000, 32, "sha256").toString("base64")}`;

/** `name` on both databases, for each user model: `User`, an override of it, and an `extends` child. */
function each(name: string, body: (t: TestContext, auth: Auth) => Promise<void>): void {
  for (const provider of PROVIDERS) {
    for (const [kind, variant] of Object.entries(VARIANTS)) {
      test(`${name} [${provider}, ${kind}]`, async (t) => {
        const auth = await makeAuth(provider as Provider, variant);
        if (auth === undefined) return t.skip(`${provider} is not reachable`);
        try {
          await body(t, auth);
        } finally {
          await auth.close();
        }
      });
    }
  }
}

test("the schema copies of both packages match the source", () => {
  const source = readFileSync(join(PACKAGE, "../schema.prisma"), "utf8");
  assert.equal(readFileSync(join(PACKAGE, "schema.prisma"), "utf8"), source);
  assert.equal(readFileSync(join(PACKAGE, "../python/orm_auth/schema.prisma"), "utf8"), source);
});

each("the models get the methods and the query set", async (_t, auth) => {
  assert.ok(auth.User.objects instanceof UserQueries);
  const users = await auth.seed();
  assert.equal(typeof users.staff.hasPerm, "function");
  assert.ok(Object.getPrototypeOf(users.staff) !== UserMethods.prototype);
  assert.equal(auth.User._meta.name, auth.variant.model);
});

each("setPassword hashes the snapshot and update writes it", async (_t, auth) => {
  const { direct } = await auth.seed();
  direct.setPassword("s3cret");
  assert.ok(direct.checkPassword("s3cret") && !direct.checkPassword("nope"));
  assert.equal((await auth.stored("direct")).password, "!");
  await direct.update({ password: direct.password });
  assert.ok(checkPassword("s3cret", (await auth.stored("direct")).password));
});

each("checkPassword with update rehashes a Django hash but not a wrong password", async (_t, auth) => {
  const [user] = (await auth.User.objects.using(auth.db).insertMany([{ username: "dj", email: "d@x.y", password: django("s3cret") }]).returning()) as Rows[];
  assert.ok(!user!.checkPassword("wrong", { update: true }) && user!.password.startsWith("pbkdf2_sha256$"));
  assert.ok(user!.checkPassword("s3cret") && user!.password.startsWith("pbkdf2_sha256$"));
  assert.ok(user!.checkPassword("s3cret", { update: true }));
  assert.ok(!needsUpdate(user!.password) && checkPassword("s3cret", user!.password));
});

each("authenticate", async (_t, auth) => {
  const using = auth.options;
  await auth.User.objects.using(auth.db).insertMany([
    { username: "ann", email: "a@x.y", password: makePassword("s3cret") },
    { username: "dj", email: "d@x.y", password: django("s3cret") },
    { username: "off", email: "o@x.y", password: makePassword("s3cret"), isActive: false },
  ]);
  const ann = await authenticate("ann", "s3cret", using);
  assert.equal(ann?.username, "ann");
  assert.equal(await authenticate("ann", "wrong", using), null);
  assert.equal(await authenticate("nobody", "s3cret", using), null);
  assert.equal(await authenticate("off", "s3cret", using), null);
  const dj = await authenticate("dj", "s3cret", using);
  assert.ok(dj !== null && !needsUpdate(dj.password));
  const stored = await auth.stored("dj");
  assert.ok(stored.password === dj.password && checkPassword("s3cret", stored.password));
});

each("authenticate takes the user model", async (_t, auth) => {
  await auth.User.objects.using(auth.db).insertMany([{ username: "ann", email: "a@x.y", password: makePassword("s3cret") }]);
  const ann = await authenticate("ann", "s3cret", { using: auth.db, model: auth.User as never });
  assert.equal(boundMeta(ann.qs()).name, auth.variant.model);
});

each("a wrong password does not rehash", async (_t, auth) => {
  const old = django("s3cret");
  await auth.User.objects.using(auth.db).insertMany([{ username: "dj", email: "d@x.y", password: old }]);
  assert.equal(await authenticate("dj", "wrong", auth.options), null);
  assert.equal((await auth.stored("dj")).password, old);
});

each("hasPerm and hasPerms", async (_t, auth) => {
  const u = await auth.seed();
  assert.ok(await u.staff.hasPerm("blog.add_post"));
  assert.ok(!(await u.staff.hasPerm("blog.change_post")));
  assert.ok(!(await u.staff.hasPerm("shop.add_post")));
  assert.ok((await u.direct.hasPerm("blog.change_post")) && !(await u.direct.hasPerm("blog.add_post")));
  assert.ok(await u.root.hasPerm("anything.at_all"));
  assert.ok(!(await u.idle.hasPerm("blog.add_post")) && !(await u.idle.hasPerm("anything.at_all")));
  assert.ok((await u.staff.hasPerms(["blog.add_post"])) && !(await u.staff.hasPerms(["blog.add_post", "blog.change_post"])));
  assert.ok(await u.staff.hasPerms([]));
});

each("hasPerm rejects a name without a label", async (_t, auth) => {
  const u = await auth.seed();
  await assert.rejects(u.staff.hasPerm("add_post"), /<label>\.<codename>/);
});

each("getAllPermissions", async (_t, auth) => {
  const u = await auth.seed();
  assert.deepEqual(await u.staff.getAllPermissions(), new Set(["blog.add_post"]));
  assert.deepEqual(await u.direct.getAllPermissions(), new Set(["blog.change_post"]));
  assert.deepEqual(await u.root.getAllPermissions(), new Set(["blog.add_post", "blog.change_post", "shop.add_post"]));
  assert.deepEqual(await u.idle.getAllPermissions(), new Set());
});

each("withPerm", async (_t, auth) => {
  await auth.seed();
  const names = (rows: Rows[]): string[] => rows.map((x) => x["username"] as string).sort();
  const users = auth.User.objects.using(auth.db);
  assert.deepEqual(names(await users.withPerm("blog.add_post")), ["root", "staff"]);
  assert.deepEqual(names(await users.withPerm("blog.change_post")), ["direct", "root"]);
  assert.deepEqual(names(await users.withPerm("nothing.here")), ["root"]);
  assert.deepEqual(names(await users.filter(auth.User.username.ne("root")).withPerm("blog.add_post")), ["staff"]);
});

each("syncPermissions adds the missing rows once", async (_t, auth) => {
  class Post {
    static _meta = { name: "Post" };
  }
  class Comment {
    static _meta = { name: "Comment" };
  }
  const using = { using: auth.db, registry: auth.registry };
  assert.equal(await syncPermissions([Post as never], "blog", using), 4);
  assert.equal(await syncPermissions([Post as never, Comment as never], "blog", using), 4);
  assert.equal(await syncPermissions([Post as never, Comment as never], "blog", using), 0);
  assert.equal(await syncPermissions([], "blog", using), 0);
  const rows = (await auth.Permission.objects.using(auth.db).filter(auth.Permission.label.eq("blog")).all()) as Rows[];
  assert.deepEqual(rows.map((r) => r["codename"] as string).sort(), [
    "add_comment", "add_post", "change_comment", "change_post", "delete_comment", "delete_post", "view_comment", "view_post",
  ]);
  assert.ok(rows.some((r) => r["name"] === "Can add post"));
});
