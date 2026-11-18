# Model composition (shared primary keys)

A composed model stores its own fields in its own table and gets the fields of its parent from the parent table.
The child primary key is also a foreign key to the parent.
One parent row can have more than one child: a person can be an employee and a customer at the same time.

**Enable:** nothing to install. The extension is built into the `monorm` Python wheel (module `monorm_composition`) and the `monorm` npm package (`monorm/extensions/model-composition`). It has no Python or TypeScript code of its own: the engine runs the writes.
Its id is `monorm.composition`. It asks for the host capabilities `schema-transformations` and `physical-schema`.
Run `monorm generate` again for models that a build with the old id `composition` generated.
How it works, the layout and the tests: [`extensions/model-composition/README.md`](../../extensions/model-composition/README.md).

## Schema

```prisma
model Person {
  id   Int    @id @default(autoincrement())
  name String
  @@map("people")
}
model Employee {
  salary Int
  @@composition.model(parent: "Person", parentRef: "person", childRef: "employee")
  @@map("employees")
}
model Manager {
  level Int
  @@composition.model(parent: "Employee", parentRef: "employee", childRef: "manager")
  @@map("managers")
}
model Customer {
  points Int
  @@composition.model(parent: "Person", parentRef: "person", childRef: "customer")
  @@map("customers")
}
```

`@@composition.model(parent:, parentRef:, childRef:)` takes three string arguments, all required.
`parent` names the parent model. `parentRef` is the to-one relation from the child to the parent.
`childRef` is the reverse to-one relation on the parent.
The child declares no `@id`: the pass creates its primary key with the type of the parent key.
Migrations create one table for each model.
The child key is a foreign key to the parent key with `ON DELETE CASCADE`.
The reverse relation (`childRef`) has no foreign key of its own.
`pull` reads such tables back as ordinary models with a one-to-one relation. It writes no `@@composition.model`.

| Argument | Required | Meaning |
|---|---|---|
| `parent` | yes | The parent model. |
| `parentRef` | yes | The to-one relation from the child to the parent. |
| `childRef` | yes | The reverse to-one relation on the parent. |

The attribute has no optional argument and no default.

## Queries and writes

```python
alice = await Manager.objects.insert(name="Alice", salary=20, level=1)    # 3 rows, one transaction
customer = await Customer.objects.attach(alice.id, {"points": 5})         # add a child to an existing parent
await Manager.objects.filter(Manager.salary == 20).update(level=4)         # inherited field in a filter
person = await Person.objects.load(Person.employee, Person.customer).get()   # one query, joins both children
await Manager.objects.filter(Manager.id == alice.id).delete()              # keeps Employee and Person rows
```

```ts
const alice = await Manager.objects.insert({ name: "Alice", salary: 20, level: 1 });
const customer = await Customer.objects.attach(alice.id, { points: 5 });
await Manager.objects.filter(Manager.salary.eq(20)).update({ level: 4 });
const person = await Person.objects.filter(Person.name.eq("Alice")).load(Person.employee, Person.customer).get();
const more = await Customer.objects.attachMany([[bobId, { points: 1 }]]);
```

Signatures: `Model.objects.attach(parent_id, values)` and `attach_many(items)` in Python; `attach(parentId, values)` and `attachMany(items)` in TypeScript.

- Reads, projections and filters on inherited fields compile to ordinary joins through the parent tables.
- `insert` writes the root row first, then each child row. A composed write outside a transaction opens its own transaction, and a failed child insert rolls back the parent rows.
  Inside a caller transaction it opens no savepoint. A failed statement marks the caller transaction (`TransactionAborted` on the next statement). Wrap the call in a nested `db.transaction()` to recover.
- `attach(parent_id, values)` inserts only the child row for an existing parent. It takes only the local fields of the child and plain values. A second child row for the same parent, or a parent that does not exist, raises `IntegrityError`.
- `insert_many` on PostgreSQL takes all new keys in one statement (`nextval` of the root key sequence, or the key default, for each row) and then runs one multi-row `INSERT` for each level, in one transaction, and one read by key in input order: L + 2 statements for L levels, whatever the row count. Keys that the caller gives skip the key statement. SQLite stays row by row, and `copy=True` keeps its error.
- `attach_many(items)` (TypeScript `attachMany`) takes `(parent_id, values)` pairs and inserts the child levels for existing parents with one multi-row `INSERT` for each level; it returns the rows in input order.
- Column roles (`updated_at`, soft delete, version) live on the root. A change on any level bumps the root; a soft delete updates only the root. See [schema extensions](schema-extensions.md).
- `update` takes a filter on local or inherited fields.
- `delete` through any level model removes every level row of the matched rows, sibling children too, as Django does.
  A model with soft delete keeps its soft delete: `delete()` marks the root row, and `hard_delete()` removes every level.
  Breaking: before, `delete()` through a child model removed only that level and kept the parent and sibling rows. Code that relies on that must delete the child level with raw SQL.
  Each level key needs `ON DELETE CASCADE` (see Limits).
- Generated modules have an `<Model>Attach` type for each composed model.

## Limits

- `update_many` on a composed model is rejected.
- `delete()` (no soft delete) and `hard_delete()` run one `DELETE` on the root row and rely on the `ON DELETE CASCADE` of each level key to remove the other levels, sibling levels included. A composition over tables made by hand or by Django (whose multi-table keys have no database cascade) needs that cascade added first.
- `outer()` and `distinct(on=...)` on inherited columns raise `QueryError: ... requires an explicit owner projection`. Project the columns of the owner model.
- Ordinary (not composed) model writes reject inherited storage.
- The executor is `monorm_engine::multi_table`; [optional-packaging.md](../contracts/optional-packaging.md#logical-fields-and-physical-storage) describes the storage contract.

## Errors

Schema errors have the prefix `extension monorm.composition: pass monorm.composition.lower:`.

| Message | Cause and fix |
|---|---|
| `@composition.model: missing argument childRef` | Give all three arguments. The host raises this before the pass runs. |
| `unknown composition parent Nope` | `parent` names no model. |
| `cyclic model composition` | Models name each other as parents. |
| `E: multiple composed parents` | A model has one `@@composition.model`. |
| `E.name: inherited field override or ambiguity` | The child field has the name of an inherited field. Rename it. |
| `E: child primary key must match parent without an independent default` | A child `@id` must have the type of the parent key and no default. Or declare none. |
| `Person: composition requires one parent primary key` | The parent has a composite key. |
| `E.s: generated relation name collides` | `parentRef` or `childRef` has the name of a field. |
| `model E has no @id field` | The parent has no `@id`. |

Runtime errors:

| Error | Cause and fix |
|---|---|
| `QueryError: a write of fields stored in several tables needs the multi-table write path` | `update_many` on a composed model. Use `update` with a filter. |
| `QueryError: multi-table update cannot change the shared primary key` | `update` set the key. |
| `QueryError: multi-table insert_many needs a value for {table}.{key} in every row: the key has no sequence or default` | Give the key in every row. |
| `TypeError: Customer.name: attach accepts only local child fields` | `attach` took an inherited field. Create the parent first. |
| `TypeError: Person is not a child of a multi-table model` | `attach` on an ordinary model. |
| `IntegrityError` | `attach` for a parent that does not exist, or a second child for one parent. |
