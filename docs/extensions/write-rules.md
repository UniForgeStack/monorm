# Write rules

`monorm.write-rules` ships with the MonOrm Python and TypeScript packages.
Use it to enforce policy before ORM writes, in the database, or in both places.

```prisma
model Ledger {
  id     Int @id
  amount Int
  @@write_rules.protect(operation: [update, delete])
}

model Post {
  id     Int    @id
  status String
  slug   String @write_rules.read_only
  @@write_rules.protect(operation: [update, delete], condition: "OLD.status = 'published'", layer: db)
}
```

With no arguments, `@@write_rules.protect` refuses inserts, updates, and deletes
in both layers. Use `operation:` to select `insert`, `update`,
`update_of(field, ...)`, or `delete`. `layer: app` checks ORM writes before SQL
runs, including both possible operations of an upsert. `layer: db` uses
`BEFORE` row triggers that apply to every database client. `layer: both` is the
default.

Use `condition:` with `layer: db` to restrict a rule to matching rows. The
condition can be a SQL string or a [trigger change helper](../schema/schema.md#model-attributes-).
An insert condition can reference `NEW`, a delete condition can reference `OLD`,
and an update condition can reference both. `action: drop` is database-only
and silently skips matching rows. The default `action: refuse` raises
`WriteProtected` in either binding.

`@write_rules.read_only` permits the initial value. The application layer
refuses an update that assigns the field. The database layer compares the old
and new values and refuses a change, including a change to or from null.

## Scoped writes

In Python, use `with monorm.allow_writes(Ledger):`. In TypeScript, use
`await allowWrites([Ledger], async () => { ... })`. The scope opens the
model's application rules. To open a database rule, set `skippable: true` and
connect with a PostgreSQL session role listed in the datasource's
`bypass_roles`. Database rules default to `skippable: false`.
Use the model-based scope to skip write rules because MonOrm generates each
database skip name from the physical table and rule name.

SQLite implements database refusals with `RAISE(ABORT)` and drops with
`RAISE(IGNORE)`. A skippable `layer: both` rule keeps its application layer on
SQLite. A skippable `layer: db` rule is a schema error.

Declare rules on the root model of a proxy. An inherited model receives the
rules declared on its base. With the extension installed, `pull` recreates
attributes from the generated triggers. Application-only rules live in schema
source and have no database trigger to pull.

Replace an old `@@protected_write` attribute with
`@@write_rules.protect(layer: app)` to preserve its application-only behavior.

## Extension authors

Extensions can push neutral `WriteRule` records into
`SchemaIr.behavior.write_rules`. The core validates the owner, operations and
layer, and builds database triggers. The engine enforces the application
layer. An extension that registers a `WriteAssignment` can use the `insert`
kind to fill an omitted value from the time, an expression without stored
columns, or a hook.
