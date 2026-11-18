/** The rule of `delete` on an instance: it removes the own value, and the field then reads `undefined`. */

import assert from "node:assert/strict";
import { test } from "node:test";

import { connect, loads, NotLoaded, Registry } from "../src/index.js";
import { DATABASE_URL } from "./helpers.js";

const SCHEMA = `
model Item {
  id    Int    @id
  title String
  views Int
  @@map("pi_item")
}
`;

for (const provider of ["postgres", "sqlite"] as const) {
  test(`delete on a loaded field of a partial instance reads undefined, like a whole instance (${provider})`, async () => {
    const registry = new Registry();
    const { Item } = loads((provider === "sqlite" ? 'datasource db { provider = "sqlite" }\n' : "") + SCHEMA, { registry }) as unknown as { Item: any }; // eslint-disable-line @typescript-eslint/no-explicit-any
    const db = await connect(provider === "sqlite" ? "sqlite://:memory:" : DATABASE_URL, { registry, default: false, maxConnections: 1 });
    try {
      await db.dropTables();
      await db.createTables();
      await Item.objects.using(db).insertMany([{ id: 1, title: "a", views: 2 }]);
      const partial = await Item.objects.using(db).load(Item.id, Item.title).get(Item.id.eq(1));
      assert.equal(partial.title, "a");
      assert.throws(() => partial.views, NotLoaded);
      delete partial.title;
      assert.equal(partial.title, undefined);
      assert.throws(() => partial.views, NotLoaded);
      partial.title = "b";
      assert.equal(partial.title, "b");
      const whole = await Item.objects.using(db).get(Item.id.eq(1));
      delete whole.title;
      assert.equal(whole.title, undefined);
    } finally {
      await db.dropTables();
      await db.close();
    }
  });
}
