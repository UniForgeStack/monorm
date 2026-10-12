# Composite primary keys and declarative partitioning

## Outcome

One schema file declares a table with a primary key of several columns,
and splits it into Postgres partitions.
Partitioning and proxies are independent:

- A base model can declare its partitions in `partition` blocks, with or without proxies.
- A proxy (plan 5) can own one list partition.
  Its constraints and indexes then go onto that partition table.

Two features ship in this order:

1. Composite primary keys and composite foreign keys (core).
2. Declarative partitioning (Postgres only), which needs the first,
   because Postgres requires the partition key in the primary key.

Today the parser rejects both (`core/src/dsl/lower.rs:891` and `:1322`),
`docs/schema.md` lists them under "Not done yet",
and `pull` leaves composite keys and partitioned tables out as gaps (`core/src/migrate/pull.rs:350`, `:467`).
The migration model already stores primary-key columns as a list (`core/src/migrate/model.rs:141`).

Checks and partial-index filters below are SQL text, as the parser accepts today.
Plan 12 replaces them with typed schema expressions.

## When a partition needs its own constraints

- **The key only spreads the data** (`hash(shop_id)`, `range(created_at)`):
  every partition holds the same kind of row.
  Constraints belong on the parent, and a partition block is only a bound.
- **The key is a subtype** (`type`, `kind`, `status`):
  each partition holds a different kind of row.
  Its rules belong on the partition, where the bound already fixes the key,
  so a check states only the shape of its own subtype.
  This is true with or without a proxy; a proxy only gives the subtype a model.

Two per-partition objects are needed in both cases:

- A unique rule without the partition key. Postgres refuses it on the parent,
  so each partition declares it, and it is unique only inside that partition.
- An index that differs per partition, for example BRIN on old range partitions.

## Example: base-model partitions

```prisma
model Event {
  id         Int      @default(autoincrement())
  shop_id    Int
  created_at DateTime @default(now())
  payload    Json

  @@id([id, created_at])
  @@index([shop_id, created_at])
  @@check("shop_id > 0")
  @@partition(range: [created_at])
}

partition event_2026_q3 {
of   = Event
from = ["2026-07-01"]
to   = ["2026-10-01"]
@@index([created_at], type: Brin)
}

partition event_2026_q4 {
of   = Event
from = ["2026-10-01"]
to   = ["2027-01-01"]
}

partition event_rest {
of      = Event
default = true
}
```

## Example: proxy-owned partitions

`Tier`, `RewardTarget` and `PriceMode` are left out.

```prisma
enum RewardType {
  PERCENTAGE_DISCOUNT
  FIXED_DISCOUNT
  SET_PRICE
  @@storage(text)
}

model Reward {
  id           Int          @default(autoincrement())
  type         RewardType
  shop_id      Int
  tier_id      Int
  tier         Tier         @relation(fields: [tier_id], references: [id], onDelete: Cascade)
  target       RewardTarget @default(PRODUCT)
  mode         PriceMode    @default(TOTAL)
  content_hash String       @default("")
  deleted_at   DateTime?    @soft_delete.deleted_at @map("_deleted_at")
  percent      Decimal?     @db.Decimal(5, 2)
  amount       Decimal?     @db.Decimal(10, 2)
  price        Decimal?     @db.Decimal(10, 2)

  @@id([id, type])
  @@index([shop_id])
  @@check("mode = 'TOTAL' OR target = 'PRODUCT'", name: "reward_mode_total_unless_product")
  @@partition(list: [type])
  @@query.filter("shop_id == scope.shop and deleted_at == null")
}

model PercentageDiscountReward {
  percent Decimal @db.Decimal(5, 2)

  @@proxy.of(Reward)
  @@proxy.fields(exclude: ["type", "mode", "amount", "price"])
  @@partition.values([PERCENTAGE_DISCOUNT])
  @@map("reward_percentage_discount")
  @@check("percent IS NOT NULL AND percent BETWEEN 0 AND 100 AND amount IS NULL AND price IS NULL")
  @@check("mode = 'TOTAL'")
  @@index([tier_id, content_hash], unique: true, where: raw("_deleted_at IS NULL AND content_hash <> ''"))
}

model FixedDiscountReward {
  amount Decimal @db.Decimal(10, 2)

  @@proxy.of(Reward)
  @@proxy.fields(exclude: ["type", "percent", "price"])
  @@partition.values([FIXED_DISCOUNT])
  @@map("reward_fixed_discount")
  @@check("amount IS NOT NULL AND amount >= 0 AND percent IS NULL AND price IS NULL")
  @@index([tier_id, content_hash], unique: true, where: raw("_deleted_at IS NULL AND content_hash <> ''"))
}

model SetPriceReward {
  price Decimal @db.Decimal(10, 2)

  @@proxy.of(Reward)
  @@proxy.fields(exclude: ["type", "percent", "amount"])
  @@partition.values([SET_PRICE])
  @@map("reward_set_price")
  @@check("price IS NOT NULL AND price >= 0 AND percent IS NULL AND amount IS NULL")
  @@index([tier_id, content_hash], unique: true, where: raw("_deleted_at IS NULL AND content_hash <> ''"))
}
```

A unique index in each partition makes `(tier_id, content_hash)` unique per type, not per tier.
The shape checks test `IS NOT NULL` because a CHECK passes on NULL.
A future type without a proxy needs a `partition` block on `Reward`,
for example one with `default = true`.

## Schema syntax

- `@@id([a, b])` is Prisma's own attribute, so it shows no IDE error.
  A model has `@id` or `@@id`, never both.
  A `@@id` field can take `@default(autoincrement())`.
  `name:` / `map:` set the constraint name; the default stays `<table>_pkey`.
- A composite foreign key is Prisma's form:
  `@relation(fields: [reward_id, reward_type], references: [id, type])`.
  `references` must match a primary key or a unique constraint of the target, in order.
- `@@partition(list: [type])`, `@@partition(range: [created_at])` or `@@partition(hash: [shop_id])`
  makes the model a partitioned table.
  Keys are field names or `sql("expression")`, as in `@@index`.
- `partition name { ... }` is a top-level block, like `function`.
  `of` names the model or a partitioned partition.
  The bound is one of:
  `values = [A, B]` (list), `from = [..]` and `to = [..]` (range, `MINVALUE` / `MAXVALUE` allowed),
  `modulus = 4` and `remainder = 0` (hash), or `default = true`.
  `@@partition(...)` inside the block partitions the partition again.
  Inside the block: `@@check`, `@@index`, `@@unique`, `@@exclude`, `@@trigger` and `@@comment`, all optional.
  The formatter removes indentation in unknown blocks,
  and each attribute stays on one line (the rules in `docs/prisma-syntax.md`).
  The block name is the table name. `@@renamed_from` on a block renames it.
- `@@partition.values([A, B])` on a proxy makes the proxy own a list partition of its source.
  `@@map` names the partition table; without it, the name is the proxy name in lower case.
  The proxy's `@@check`, `@@index`, `@@unique`, `@@exclude` and `@@trigger` go onto that table.
  Without `@@partition.values`, a proxy still creates no physical object (plan 5).

## Proxy-owned partitions

- The bound sets the key. An insert through the proxy writes the bound value
  when the list has one value; with several values the caller sets the key.
  The proxy can omit the key field, as `type` above, and needs no `@client_default`.
- Reads, counts, updates and deletes through the proxy add `type IN (<values>)`
  to the inherited default filter, so Postgres scans one partition.
  `without_defaults()` keeps this predicate: it is the proxy's storage, not a default.
- The bound and the partition checks are database constraints,
  so the proxy's narrowed fields (non-null `percent`) are enforced, not only warnings.
- A query on the source model reads every partition and returns source instances.
- Plan 5 says a proxy creates no table and changes no physical object.
  `@@partition.values` is the one exception.

## Validation

Reject during definition, with the model and field name:

- A partitioned model whose primary key or parent-level unique constraint
  does not contain every partition key column.
  Postgres refuses these.
  The error says to declare the rule on each partition, where it is unique only inside that partition.
- A partition bound that does not match the strategy,
  a list value that is not a member of the key's enum,
  two partitions or proxies with overlapping list values, and a second `default`.
  Range overlap is left to Postgres.
- `@@partition.values` on a proxy whose source is not a list-partitioned model,
  on a proxy of a proxy, or on a source with more than one list key.
- A partition block or `@@partition.values` for a model without `@@partition`,
  and a model with `@@partition` and no partition.
  The second is legal SQL, but every insert would fail.
- An enum member of a list key that no partition holds, unless a `default` partition exists.
  This is a warning: inserts with that value fail.
- `@@partition` on SQLite. The extension declares Postgres only.
  Composite keys work on both databases.
- A generic foreign key (plan 8) to a model with a composite key.
  `object_id` is one column; composite targets are later work.
- Identity columns on a partitioned table need Postgres 17.
  Verify this, then either set the floor or emit a sequence default on older servers.

## Migrations

- `CREATE TABLE reward (...) PARTITION BY LIST (type)`, then
  `CREATE TABLE reward_percentage_discount PARTITION OF reward FOR VALUES IN ('PERCENTAGE_DISCOUNT')`,
  then the partition's own checks and indexes.
  Partition blocks and proxy-owned partitions give the same migration objects.
- Parent indexes and checks are created on the parent. Postgres copies them to every partition,
  so the differ must not report the copies as drift.
- A new partition is `CREATE TABLE ... PARTITION OF`.
  A removed partition is `DETACH PARTITION` then `DROP TABLE`, and it is destructive.
  A changed bound is detach, alter, attach.
  A changed partition key or strategy is a table rebuild; mark it unsafe, as other destructive steps.
  A move of a partition from a block to a proxy, or back, changes nothing in the database.
- A changed primary key is drop and add of the constraint.
  Foreign keys that reference it are dropped first and created again after it.
- `pull` reads `pg_partitioned_table` and `pg_inherits` into `@@partition` and `partition` blocks,
  reads composite primary and foreign keys,
  and drops the two gaps above.
  It never creates proxies. The shadow check compares partition bounds too.

## Runtime

A composite key changes every place that assumes one key column.
The largest are `python/orm/model.py`, `js/src/model.ts`, `js/src/query.ts`,
`extensions/generic/src/lib.rs`, `engine/src/introspect.rs` and `python/orm/write.py`.

- Instance identity is the tuple of key values, in `@@id` order.
  Python `pk` is a tuple; TypeScript `pk` is an object keyed by field name.
  A model with one key column keeps today's scalar `pk`.
- `get`, `refresh`, instance `update` / `delete`, `RETURNING` read-back,
  soft delete, optimistic locking and timestamps select by every key column.
- Reference loading and prefetch by `IN (...)` use row values:
  `(id, type) IN (($1, $2), ($3, $4))`.
  Joined loads join on every column of the foreign key.
- Keyset pagination orders by and compares the whole key.
- Bulk `insert_many` with `RETURNING` matches rows by the whole key.
- Proxies keep the root's composite key (plan 5 rejects key changes).
  Composition (plan 6) shares the whole key between parent and child.
- Code generation emits the key type and the composite relation fields for Python and TypeScript.
  Regenerate the blog example only if it changes.

Base-model partitioning adds no runtime code: Postgres routes inserts and moves a row
when an update changes its partition key.
The planner prunes partitions from the filter;
a query without the partition key in its filter scans every partition.
Proxy-owned partitions add only the key value on insert and the key predicate on reads.

## Tickets and acceptance

1. Composite primary keys: parser, IR, migration, `pull`, Python and TypeScript identity.
2. Composite foreign keys: relations, joined and `IN (...)` loads, cascades.
3. `@@partition` and `partition` blocks: parser, validation, migration, `pull`.
4. `@@partition.values` on proxies: validation, physical ownership, insert key, read predicate; docs.

Cover:

- insert, get, update, delete, refresh and pagination on a two-column key;
- a relation and a prefetch through a composite foreign key;
- a range-partitioned base model with no proxy and no partition constraints;
- an insert routed to the right partition, and a check on one partition that rejects a row the others accept;
- a row with its own value column NULL rejected by its partition;
- an insert through a proxy that omits the key, and a proxy read that scans one partition (`EXPLAIN`);
- an update of `type` that moves a row, and the move rejected by the target partition's check;
- a parent unique rule without the partition key rejected at definition;
- overlapping list values between a block and a proxy rejected at definition;
- add, remove and rebound of a partition in `makemigrations`, no drift on inherited indexes,
  and no migration when a partition moves between a block and a proxy;
- `pull` of a partitioned table with a composite key, and the shadow check;
- the rewards example above with soft delete and a scoped default filter;
- the feature-check `all` artifact.
