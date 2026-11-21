/** Linked relations (`path: [...]`) on PostgreSQL and SQLite: filter, `alias()` independence, `load()`, order by, `count()`, writes. */
import assert from "node:assert/strict";
import { mkdirSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { test } from "node:test";
import { connect, exists, load, loads, Migrations, NotLoaded, outer, Registry, scope } from "../src/index.js";
import { hasExtension } from "./artifact-caps.js";
import { scratch } from "./scratch.js";

const URL = process.env["ORM_TEST_DATABASE_URL"] ?? "postgres://postgres:postgres@localhost/orm_test";
const head = (provider: string): string => (provider === "sqlite" ? 'datasource db {\n  provider = "sqlite"\n}\n' : "");
const urlOf = (provider: string): string => (provider === "sqlite" ? "sqlite://:memory:" : URL);

const source = `
model Country {
  id   Int    @id @default(autoincrement())
  name String
  @@map("lrn_country")
}

model City {
  id        Int     @id @default(autoincrement())
  name      String
  countryId Int
  country   Country @relation(fields: [countryId], references: [id])
  @@map("lrn_city")
}

model Address {
  id     Int  @id @default(autoincrement())
  cityId Int
  city   City @relation(fields: [cityId], references: [id])
  @@map("lrn_address")
}

model User {
  id         Int       @id @default(autoincrement())
  name       String
  addressId  Int?
  address    Address?  @relation(fields: [addressId], references: [id])
  posts      Post[]
  comments   Comment[]
  commenters User[]    @relation(path: [posts, comments, author])
  country    Country?  @relation(path: [address, city, country])
  @@map("lrn_user")
}

model Post {
  id        Int       @id @default(autoincrement())
  authorId  Int
  author    User      @relation(fields: [authorId], references: [id])
  published Boolean
  comments  Comment[]
  @@map("lrn_post")
}

model Comment {
  id       Int  @id @default(autoincrement())
  postId   Int
  post     Post @relation(fields: [postId], references: [id])
  authorId Int
  author   User @relation(fields: [authorId], references: [id])
  @@map("lrn_comment")
}
`;

async function withData(provider: string, run: (db: any, User: any, names: (qs: any) => Promise<string[]>, Country: any) => Promise<void>): Promise<void> {
  const registry = new Registry();
  const m: any = loads(head(provider) + source, { registry });
  const db = await connect(urlOf(provider), { registry, default: false });
  await db.dropTables();
  await db.createTables();
  try {
    const { Country, City, Address, User, Post, Comment } = m;
    const [fr, de] = await Country.objects.using(db).insertMany([{ name: "France" }, { name: "Germany" }]).returning();
    const [paris, berlin] = await City.objects.using(db).insertMany([{ name: "Paris", countryId: fr.id }, { name: "Berlin", countryId: de.id }]).returning();
    const [aParis, aBerlin] = await Address.objects.using(db).insertMany([{ cityId: paris.id }, { cityId: berlin.id }]).returning();
    const [ann, bob, cy, dee] = await User.objects
      .using(db)
      .insertMany([
        { name: "ann", addressId: aBerlin.id },
        { name: "bob", addressId: aParis.id },
        { name: "cy", addressId: null },
        { name: "dee", addressId: aParis.id },
      ])
      .returning();
    // ann has two posts, bob one; cy comments twice on ann's two posts (one target through two middle rows), dee on one
    const [p1, p2, p3] = await Post.objects
      .using(db)
      .insertMany([
        { authorId: ann.id, published: true },
        { authorId: ann.id, published: false },
        { authorId: bob.id, published: true },
      ])
      .returning();
    await Comment.objects
      .using(db)
      .insertMany([
        { postId: p1.id, authorId: cy.id },
        { postId: p2.id, authorId: cy.id },
        { postId: p2.id, authorId: dee.id },
        { postId: p3.id, authorId: ann.id },
      ])
      .returning();
    const names = async (qs: any): Promise<string[]> => (await qs.using(db).all()).map((u: any) => u.name).sort();
    await run(db, User, names, Country);
  } finally {
    await db.dropTables();
    await db.close();
  }
}

const CHAIN = `
model LhGroup {
  id   Int     @id @default(autoincrement())
  name String
  tags LhTag[]
  @@map("lhn_group")
}

model LhTag {
  id      Int         @id @default(autoincrement())
  name    String
  groupId Int
  group   LhGroup     @relation(fields: [groupId], references: [id])
  books   LhBook[]    @relation(through: LhBookTag)
  links   LhBookTag[]
  @@map("lhn_tag")
}

model LhAuthor {
  id     Int       @id @default(autoincrement())
  name   String
  books  LhBook[]
  tags   LhTag[]   @relation(path: [books, tags])
  groups LhGroup[] @relation(path: [tags, group])
  @@map("lhn_author")
}

model LhBook {
  id       Int         @id @default(autoincrement())
  authorId Int
  author   LhAuthor    @relation(fields: [authorId], references: [id])
  tags     LhTag[]     @relation(through: LhBookTag)
  links    LhBookTag[]
  @@map("lhn_book")
}

model LhBookTag {
  id     Int    @id @default(autoincrement())
  bookId Int
  tagId  Int
  book   LhBook @relation(fields: [bookId], references: [id])
  tag    LhTag  @relation(fields: [tagId], references: [id])
  @@map("lhn_book_tag")
}
`;

async function withChain(provider: string, run: (db: any, Author: any, names: (qs: any) => Promise<string[]>) => Promise<void>): Promise<void> {
  const registry = new Registry();
  const m: any = loads(head(provider) + CHAIN, { registry });
  const db = await connect(urlOf(provider), { registry, default: false });
  await db.dropTables();
  await db.createTables();
  try {
    const { LhGroup, LhTag, LhAuthor, LhBook, LhBookTag } = m;
    const [warm, cool] = await LhGroup.objects.using(db).insertMany([{ name: "warm" }, { name: "cool" }]).returning();
    const [red, blue, green] = await LhTag.objects
      .using(db)
      .insertMany([{ name: "red", groupId: warm.id }, { name: "blue", groupId: cool.id }, { name: "green", groupId: cool.id }])
      .returning();
    const [ann, bob] = await LhAuthor.objects.using(db).insertMany([{ name: "ann" }, { name: "bob" }, { name: "cy" }]).returning();
    const [b1, b2, b3] = await LhBook.objects.using(db).insertMany([{ authorId: ann.id }, { authorId: ann.id }, { authorId: bob.id }]).returning();
    // ann reaches blue through two books: one target through two middle rows
    await LhBookTag.objects.using(db).insertMany([
      { bookId: b1.id, tagId: red.id },
      { bookId: b1.id, tagId: blue.id },
      { bookId: b2.id, tagId: blue.id },
      { bookId: b3.id, tagId: green.id },
    ]);
    const names = async (qs: any): Promise<string[]> => (await qs.using(db).all()).map((a: any) => a.name).sort();
    await run(db, LhAuthor, names);
  } finally {
    await db.dropTables();
    await db.close();
  }
}

const TREE = `
model Node {
  id            Int    @id @default(autoincrement())
  name          String
  parentId      Int?
  parent        Node?  @relation("Tree", fields: [parentId], references: [id])
  children      Node[] @relation("Tree")
  grandchildren Node[] @relation(path: [children, children])
  grandparent   Node?  @relation(path: [parent, parent])
  @@map("lrn_node")
}
`;

const GUARD = `
model GReader {
  id   Int    @id
  name String
  @@map("lrn_g_reader")
}

model GOrg {
  id      Int       @id
  name    String
  posts   GPost[]
  readers GReader[] @relation(path: [posts, reads, reader])
  @@map("lrn_g_org")
}

model GPost {
  id    Int     @id
  orgId Int
  org   GOrg    @relation(fields: [orgId], references: [id])
  shop  Int
  reads GRead[]
  @@soft_delete.deleted_at("deleted")
  @@query.filter("tenant", shop == scope.shop)
  @@map("lrn_g_post")
}

model GRead {
  id       Int     @id
  postId   Int
  post     GPost   @relation(fields: [postId], references: [id])
  readerId Int
  reader   GReader @relation(fields: [readerId], references: [id])
  @@map("lrn_g_read")
}
`;

function constraintSchema(constraint: string, provider: string, dir = scratch("monorm-linked-")): string {
  const extra = constraint ? `, ${constraint}` : "";
  const path = join(dir, "schema.prisma");
  writeFileSync(
    path,
    `${provider === "sqlite" ? 'datasource db { provider = "sqlite" }\n' : ""}
model LcItem {
  id   Int    @id
  name String
  @@map("lcn_item")
}
view LcItemView {
  id    Int      @id
  name  String
  stats LcStat[]
  @@map("lcn_item_view")
}
model LcStat {
  id     Int        @id
  itemId Int
  item   LcItemView @relation(fields: [itemId], references: [id]${extra})
  twinId Int
  twin   LcItem     @relation("Twin", fields: [twinId], references: [id]${extra})
  @@map("lcn_stat")
}
`,
  );
  const sidecars = join(dir, "views", provider === "sqlite" ? "main" : "public");
  mkdirSync(sidecars, { recursive: true });
  writeFileSync(join(sidecars, "LcItemView.sql"), 'SELECT id, name FROM "lcn_item"');
  return path;
}

for (const provider of ["sqlite", "postgres"]) {
  const dialect = provider;
  const dbProvider = provider === "postgres" ? "postgresql" : provider;

  test(`filter across the chain (${provider})`, async () => {
    await withData(dbProvider, async (_db, User, names) => {
      assert.deepEqual(await names(User.objects.filter(User.commenters.name.eq("cy"))), ["ann"]);
      assert.deepEqual(await names(User.objects.filter(User.commenters.name.eq("ann"))), ["bob"]);
      assert.deepEqual(await names(User.objects.filter(User.commenters.name.eq("nobody"))), []);
    });
  });

  test(`a linked relation is its own path (${provider})`, async () => {
    await withData(dbProvider, async (_db, User, names) => {
      // `commenters` does not share its middle rows with `posts`: ann's unpublished post has dee, her published post has not
      assert.deepEqual(await names(User.objects.filter(User.commenters.name.eq("dee"), User.posts.published.eq(true))), ["ann"]);
      assert.deepEqual(await names(User.objects.filter(User.posts.comments.author.name.eq("dee"), User.posts.published.eq(true))), []);
    });
  });

  test(`load gives each target once (${provider})`, async () => {
    await withData(dbProvider, async (db, User) => {
      const users = new Map((await User.objects.using(db).load(User.commenters).all()).map((u: any) => [u.name, u]));
      const ann: any = users.get("ann");
      assert.deepEqual(ann.commenters.map((c: any) => c.name).sort(), ["cy", "dee"]);
      assert.deepEqual((users.get("bob") as any).commenters.map((c: any) => c.name), ["ann"]);
      assert.deepEqual((users.get("cy") as any).commenters, []);
      assert.deepEqual((users.get("dee") as any).commenters, []);
    });
  });

  test(`a to-one chain loads with joins and orders (${provider})`, async () => {
    await withData(dbProvider, async (db, User, names) => {
      const users = await User.objects.using(db).load(User.country).all();
      assert.deepEqual(
        Object.fromEntries(users.map((u: any) => [u.name, u.country ? u.country.name : null])),
        { ann: "Germany", bob: "France", cy: null, dee: "France" },
      );
      const ordered = await User.objects.using(db).filter(User.addressId.ne(null)).orderBy(User.country.name, User.name).all();
      assert.deepEqual(ordered.map((u: any) => u.name), ["bob", "dee", "ann"]);
      assert.deepEqual(await names(User.objects.filter(User.country.name.eq("France"))), ["bob", "dee"]);
    });
  });

  test(`outer reads through a linked to one (${provider})`, async () => {
    await withData(dbProvider, async (_db, User, names, Country) => {
      // the subquery correlates to each user through the flat hops: cy has no address, so no country, so no row
      const inner = Country.objects.filter(Country.name.eq(outer(User.country.name)));
      assert.deepEqual(await names(User.objects.filter(exists(inner))), ["ann", "bob", "dee"]);
    });
  });

  test(`or null on a linked to one adds the missing row (${provider})`, async () => {
    await withData(dbProvider, async (_db, User, names) => {
      // a linked to-one is a to-one path: the not-null column adds NOT EXISTS, it does not raise
      assert.deepEqual(await names(User.objects.filter(User.country.name.eq("France").orNull())), ["bob", "cy", "dee"]);
    });
  });

  test(`order by a to-many chain is refused (${provider})`, async () => {
    await withData(dbProvider, async (db, User) => {
      await assert.rejects(async () => await User.objects.using(db).orderBy(User.commenters.name).all(), /to-many/);
    });
  });

  test(`count counts distinct targets (${provider})`, async () => {
    await withData(dbProvider, async (db, User) => {
      assert.equal(await User.objects.using(db).filter(User.commenters.name.eq("cy")).count(), 1);
      // ann (cy twice) and bob: users, not paths
      assert.equal(await User.objects.using(db).filter(User.commenters.id.gt(0)).count(), 2);
    });
  });

  test(`not loaded and writes raise (${provider})`, async () => {
    await withData(dbProvider, async (db, User) => {
      const ann: any = await User.objects.using(db).get(User.name.eq("ann"));
      assert.throws(() => ann.commenters, NotLoaded);
      assert.throws(() => ann.country, NotLoaded);
      assert.throws(() => {
        ann.commenters = [];
      }, TypeError);
      assert.throws(() => {
        ann.country = null;
      }, TypeError);
      // no manager: a loaded linked relation is a plain array, so add, remove and set do not exist
      const loaded: any = await User.objects.using(db).load(User.commenters).get(User.name.eq("ann"));
      assert.ok(Array.isArray(loaded.commenters));
      for (const call of ["add", "set"]) assert.equal(loaded.commenters[call], undefined);
      for (const call of ["add", "remove", "set"]) assert.equal(User.commenters[call], undefined);
    });
  });

  test(`a write through a linked relation raises (${provider})`, async () => {
    await withData(dbProvider, async (db, User) => {
      await assert.rejects(async () => User.objects.using(db).insert({ name: "zed", commenters: [] }), /User\.commenters is a linked relation: read-only/);
      await assert.rejects(async () => User.objects.using(db).insert({ name: "zed", country: null }), /User\.country is a linked relation: read-only/);
      await assert.rejects(async () => User.objects.using(db).filter(User.name.eq("ann")).update({ commenters: [] }), /User\.commenters is a linked relation: read-only/);
    });
  });

  test(`a many to many hop in a chain filters loads and counts (${provider})`, async () => {
    await withChain(dbProvider, async (db, Author, names) => {
      assert.deepEqual(await names(Author.objects.filter(Author.tags.name.eq("blue"))), ["ann"]);
      assert.deepEqual(await names(Author.objects.filter(Author.tags.name.eq("green"))), ["bob"]);
      const authors = new Map((await Author.objects.using(db).load(Author.tags).all()).map((a: any) => [a.name, a]));
      assert.deepEqual((authors.get("ann") as any).tags.map((t: any) => t.name).sort(), ["blue", "red"]);
      assert.deepEqual((authors.get("bob") as any).tags.map((t: any) => t.name), ["green"]);
      assert.deepEqual((authors.get("cy") as any).tags, []);
      assert.equal(await Author.objects.using(db).filter(Author.tags.id.gt(0)).count(), 2);
    });
  });

  test(`a linked hop in a chain filters and loads (${provider})`, async () => {
    await withChain(dbProvider, async (db, Author, names) => {
      assert.deepEqual(await names(Author.objects.filter(Author.groups.name.eq("cool"))), ["ann", "bob"]);
      assert.deepEqual(await names(Author.objects.filter(Author.groups.name.eq("warm"))), ["ann"]);
      const authors = new Map((await Author.objects.using(db).load(Author.groups).all()).map((a: any) => [a.name, a]));
      assert.deepEqual((authors.get("ann") as any).groups.map((g: any) => g.name).sort(), ["cool", "warm"]);
      assert.deepEqual((authors.get("bob") as any).groups.map((g: any) => g.name), ["cool"]);
      assert.deepEqual((authors.get("cy") as any).groups, []);
    });
  });

  test(`constraint false reads and writes without a foreign key (${provider})`, async () => {
    const schema = constraintSchema("constraint: false", dialect);
    const registry = new Registry();
    const m: any = load(schema, { registry });
    const sql: string = new Migrations(join(schema, "..", "migrations"), schema).make()[0]!.upSql;
    assert.ok(sql.includes("lcn_stat") && !sql.includes("FOREIGN KEY") && !sql.includes("REFERENCES"), sql);
    const db = await connect(urlOf(dbProvider), { registry, default: false });
    try {
      await db.dropTables();
      await db.createTables();
      const { LcItem, LcItemView, LcStat } = m;
      await LcItem.objects.using(db).insertMany([{ id: 1, name: "one" }, { id: 2, name: "two" }]);
      // a row for a key that no row holds: no constraint refuses it
      await LcStat.objects.using(db).insertMany([{ id: 1, itemId: 1, twinId: 1 }, { id: 2, itemId: 2, twinId: 99 }]);
      const stats = new Map((await LcStat.objects.using(db).load(LcStat.item, LcStat.twin).all()).map((s: any) => [s.id, s]));
      assert.equal((stats.get(1) as any).item.name, "one");
      assert.equal((stats.get(2) as any).item.name, "two");
      assert.equal((stats.get(1) as any).twin.name, "one");
      // as for any relation, a key that no row holds is not a loaded row
      assert.throws(() => (stats.get(2) as any).twin, NotLoaded);
      assert.deepEqual((await LcStat.objects.using(db).filter(LcStat.item.name.eq("two")).all()).map((s: any) => s.id), [2]);
      const view: any = await LcItemView.objects.using(db).load(LcItemView.stats).get(LcItemView.id.eq(1));
      assert.deepEqual(view.stats.cached.map((s: any) => s.id), [1]);
    } finally {
      await db.dropTables();
      await db.close();
    }
  });

  test(`the migration diff follows the constraint flag (${provider})`, () => {
    const schema = constraintSchema("", dialect);
    const migrations = new Migrations(join(schema, "..", "migrations"), schema);
    const first: string = migrations.make()[0]!.upSql;
    // a foreign key to a table, none to a view that cannot hold one
    assert.ok(first.includes("REFERENCES"), first);
    constraintSchema("constraint: false", dialect, join(schema, ".."));
    const drop: string = migrations.make("drop_fk")[0]!.upSql;
    assert.ok(dialect === "postgres" ? drop.includes("DROP CONSTRAINT") : !drop.includes("REFERENCES"), drop);
    constraintSchema("", dialect, join(schema, ".."));
    const add: string = migrations.make("add_fk")[0]!.upSql;
    assert.ok(add.includes("REFERENCES"), add);
  });
}

for (const bad of ["onDelete: Cascade", "onUpdate: Cascade", "deferrable: true"]) {
  test(`constraint false refuses a database action (${bad})`, () => {
    const text = source.replace(
      "author    User      @relation(fields: [authorId], references: [id])",
      `author    User      @relation(fields: [authorId], references: [id], constraint: false, ${bad})`,
    );
    assert.notEqual(text, source);
    assert.throws(() => loads(text, { registry: new Registry() }), /constraint/);
  });
}

for (const provider of ["sqlite", "postgresql"]) {
  test(`a self tree loads and filters (${provider})`, async () => {
    const registry = new Registry();
    const m: any = loads(head(provider) + TREE, { registry });
    const db = await connect(urlOf(provider), { registry, default: false });
    await db.dropTables();
    await db.createTables();
    try {
      const Node = m["Node"];
      const root = await Node.objects.using(db).insert({ name: "root" });
      const [a, b] = await Node.objects.using(db).insertMany([{ name: "a", parentId: root.id }, { name: "b", parentId: root.id }]).returning();
      await Node.objects.using(db).insertMany([{ name: "a1", parentId: a.id }, { name: "a2", parentId: a.id }, { name: "b1", parentId: b.id }]).returning();
      const names = async (qs: any): Promise<string[]> => (await qs.using(db).all()).map((n: any) => n.name).sort();
      const nodes = new Map((await Node.objects.using(db).load(Node.parent, Node.children).all()).map((n: any) => [n.name, n]));
      assert.equal((nodes.get("root") as any).parent, null);
      assert.equal((nodes.get("a1") as any).parent.name, "a");
      assert.deepEqual((nodes.get("a") as any).children.cached.map((c: any) => c.name).sort(), ["a1", "a2"]);
      assert.deepEqual((nodes.get("a1") as any).children.cached, []);
      assert.deepEqual(await names(Node.objects.filter(Node.parent.name.eq("a"))), ["a1", "a2"]);
      assert.deepEqual(await names(Node.objects.filter(Node.children.name.eq("a1"))), ["a"]);
      assert.deepEqual(await names(Node.objects.filter(Node.parent.parent.name.eq("root"))), ["a1", "a2", "b1"]);
    } finally {
      await db.dropTables();
      await db.close();
    }
  });

  test(`a linked relation repeats one relation of a self tree (${provider})`, async () => {
    const registry = new Registry();
    const m: any = loads(head(provider) + TREE, { registry });
    const db = await connect(urlOf(provider), { registry, default: false });
    await db.dropTables();
    await db.createTables();
    try {
      const Node = m["Node"];
      const root = await Node.objects.using(db).insert({ name: "root" });
      const [a, b] = await Node.objects.using(db).insertMany([{ name: "a", parentId: root.id }, { name: "b", parentId: root.id }]).returning();
      await Node.objects.using(db).insertMany([{ name: "a1", parentId: a.id }, { name: "a2", parentId: a.id }, { name: "b1", parentId: b.id }]).returning();
      const names = async (qs: any): Promise<string[]> => (await qs.using(db).all()).map((n: any) => n.name).sort();
      assert.deepEqual(await names(Node.objects.filter(Node.grandchildren.name.eq("a2"))), ["root"]);
      assert.deepEqual(await names(Node.objects.filter(Node.grandparent.name.eq("root"))), ["a1", "a2", "b1"]);
      const nodes = new Map((await Node.objects.using(db).load(Node.grandchildren, Node.grandparent).all()).map((n: any) => [n.name, n]));
      assert.deepEqual((nodes.get("root") as any).grandchildren.map((n: any) => n.name).sort(), ["a1", "a2", "b1"]);
      assert.deepEqual((nodes.get("a") as any).grandchildren, []);
      assert.equal((nodes.get("a1") as any).grandparent.name, "root");
      assert.equal((nodes.get("a") as any).grandparent, null);
    } finally {
      await db.dropTables();
      await db.close();
    }
  });

  async function withGuard(run: (db: any, Org: any, Post: any) => Promise<void>): Promise<void> {
    const registry = new Registry();
    const m: any = loads(head(provider) + GUARD, { registry });
    const db = await connect(urlOf(provider), { registry, default: false });
    await db.dropTables();
    await db.createTables();
    try {
      const { GOrg: Org, GPost: Post, GRead: Read, GReader: Reader } = m;
      await Reader.objects.using(db).insertMany([{ id: 1, name: "r1" }, { id: 2, name: "r2" }]);
      await Org.objects.using(db).insert({ id: 1, name: "org" });
      await scope({ shop: 1 }, () => Post.objects.using(db).insertMany([{ id: 1, orgId: 1, shop: 1 }, { id: 2, orgId: 1, shop: 1 }]));
      await scope({ shop: 2 }, () => Post.objects.using(db).insert({ id: 3, orgId: 1, shop: 2 }));
      await Read.objects.using(db).insertMany([{ id: 1, postId: 1, readerId: 1 }, { id: 2, postId: 2, readerId: 2 }, { id: 3, postId: 3, readerId: 1 }]);
      await run(db, Org, Post);
    } finally {
      await db.dropTables();
      await db.close();
    }
  }

  test(`a soft deleted middle row hides the target (${provider})`, { skip: !hasExtension("monorm.soft-delete") }, async () => {
    await withGuard(async (db, Org, Post) => {
      await scope({ shop: 1 }, async () => {
        let org: any = await Org.objects.using(db).load(Org.readers).get(Org.id.eq(1));
        assert.deepEqual(org.readers.map((r: any) => r.name).sort(), ["r1", "r2"]);
        assert.equal(await Org.objects.using(db).filter(Org.readers.name.eq("r2")).count(), 1);
        await Post.objects.using(db).filter(Post.id.eq(2)).delete();
        org = await Org.objects.using(db).load(Org.readers).get(Org.id.eq(1));
        assert.deepEqual(org.readers.map((r: any) => r.name), ["r1"]);
        assert.equal(await Org.objects.using(db).filter(Org.readers.name.eq("r2")).count(), 0);
      });
    });
  });

  test(`the scope policy of every table on the path applies (${provider})`, { skip: !hasExtension("monorm.soft-delete") }, async () => {
    await withGuard(async (db, Org) => {
      await scope({ shop: 2 }, async () => {
        const org: any = await Org.objects.using(db).load(Org.readers).get(Org.id.eq(1));
        assert.deepEqual(org.readers.map((r: any) => r.name), ["r1"]);
        assert.equal(await Org.objects.using(db).filter(Org.readers.name.eq("r2")).count(), 0);
      });
      await scope({ shop: 1 }, async () => {
        assert.equal(await Org.objects.using(db).filter(Org.readers.name.eq("r2")).count(), 1);
      });
    });
  });
}
