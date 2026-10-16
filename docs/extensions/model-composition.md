# Model composition (shared primary keys)

A composed model stores its own fields in its own table and gets the fields of its parent from the parent table.
The child primary key is also a foreign key to the parent.
One parent row can have more than one child: a person can be an employee and a customer at the same time.

**Enable:** select the `orm-model-composition` crate (`extensions/model-composition/`) in your build.
See [build-time-extensions.md](../contracts/build-time-extensions.md#select-feature-extensions).

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
```

`@@composition.model(parent:, parentRef:, childRef:)` takes three string arguments, all required.
`parent` names the parent model. `parentRef` is the to-one relation from the child to the parent.
`childRef` is the reverse to-one relation on the parent.
The child declares no `@id`: the pass creates its primary key with the type of the parent key.
Migrations create one table for each model and an ordinary foreign key from the child key to the parent key.

## Queries and writes

```python
alice = await Manager.objects.insert(name="Alice", salary=20, level=1)    # 3 rows, one transaction
customer = await Customer.objects.attach(alice.id, {"points": 5})         # add a child to an existing parent
await Manager.objects.filter(Manager.salary == 20).update(level=4)         # inherited field in a filter
person = await Person.objects.select_related(Person.employee, Person.customer).get()
await Manager.objects.filter(Manager.id == alice.id).delete()              # keeps Employee and Person rows
```

```ts
const alice = await Manager.objects.insert({ name: "Alice", salary: 20, level: 1 });
const customer = await Customer.objects.attach(alice.id, { points: 5 });
await Manager.objects.filter(Manager.salary.eq(20)).update({ level: 4 });
```

- Reads, projections and filters on inherited fields compile to ordinary joins through the parent tables.
- `insert` writes the root row first, then each child row. Each composed write runs in one transaction or savepoint; a failed child insert rolls back the parent rows.
- `attach(parent_id, values)` inserts only the child row for an existing parent. It takes only the local fields of the child and plain values. A second child row for the same parent, or a parent that does not exist, raises `IntegrityError`.
- `update` takes a filter on local or inherited fields.
- `delete` removes the child row. Parent rows and sibling children stay.
- Generated modules have an `<Model>Attach` type for each composed model.

## Limits

- `update_many` on a composed model is rejected.
- Relation links, `outer()` and `distinct(on=...)` on inherited columns are not supported.
- Ordinary (not composed) model writes reject inherited storage.
- The executor is `orm_engine::composed`; [build-time-extensions.md](../contracts/build-time-extensions.md#logical-fields-and-physical-storage) describes the storage contract.
