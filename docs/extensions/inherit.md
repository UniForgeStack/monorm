# Abstract models and model overrides

The `orm.inherit` extension (crate `orm-inherit`, `extensions/inherit`) lets one schema share model shape and lets a consumer replace an app model.
It has no host feature and no capability.
Its pass runs in the `declaration` phase, so the host and the other extensions see only ordinary models.

**Enable:** select the `orm-inherit` crate in your build.
See [build-time-extensions.md](../contracts/build-time-extensions.md).

## Schema

```prisma
model AbstractUser {
  id    Int    @id @default(autoincrement())
  email String @unique
  roles Role[] @relation("{model}_roles")
  @@inherit.abstract
  @@query_set("UserQueries")
  @@model_methods("UserMethods")
}

model Timestamped {
  created_at DateTime @default(now())
  @@inherit.abstract
}

model User {
  @@inherit.extends(["AbstractUser", "Timestamped"])
}

model MyUser {
  bio String?
  @@inherit.override("User")
}
```

- `@@inherit.abstract`: no table, no migration, no runtime model.
  `@@map`, `@@renamed_from` and `@@partition` on an abstract model are errors, and so is a relation to it.
- `@@inherit.extends("A")` or `@@inherit.extends(["A", "B"])`: the child copies fields, relations, indexes, constraints, `@@query_set` and `@@model_methods` of each abstract base.
  The child keeps its own table name.
  A field that two bases declare, or a class that two bases declare, is an error unless the child declares its own.
- `{model}` in a relation name of an abstract model becomes the lower-case child name (`User` gives `Role.user_roles`).
  The pass adds the back-relation field on the target.
  On a concrete model, `{model}` in a relation name is an error.
- `@@inherit.override("User")`: the child copies `User` and takes its table.
  `User` gets no table and no runtime model.
  Relations to `User` point to the child, and the migration changes the table and keeps its rows.
  Two models that override the same model is an error.
  If the override has its own `@@map`, the migration renames the base table to that name.
  To remove such an override, use two steps: remove the `@@map` and migrate, then remove the override.
  If you remove both at once, the base model comes back with its default table name, the migration cannot match the renamed table, and it creates a new table and drops the old one with its rows.

## Get the active model

`model_for("User")` (Python, `orm_ext.orm_inherit`) and `modelFor("User")` (TypeScript, `orm-ext/orm-inherit`) return `MyUser`.
They return the name's own model when nothing overrides it.
The Python function returns `Any`; the TypeScript function returns the registered model without a static type.

```python
from orm_ext.orm_inherit import model_for

Account = model_for("User")
```

## Types of an abstract model

The abstract model has no runtime class.
Code generation writes a type for each one, from `behavior.type_models`:

| | Python | TypeScript |
|---|---|---|
| Fields | `AbstractUserFields` | `AbstractUserData` |
| Spec | none | `AbstractUserSpec` |

In the Python stub, `AbstractUserFields` is a class that derives from `Model`, and each child class inherits it.
The runtime module has `AbstractUserFields = Model`.
A bare `Protocol` does not work: `QuerySet` bounds its parameter to `Model`, and mypy and pyright reject it.
Fields that are nested (embedded or composed) are not in these types.

Known limit: these types carry fields only, not relations.
A methods class on an abstract model cannot type `self.roles`; mypy reports an unknown attribute.

### A generic query-set class

A query-set class that an abstract model names in `@@query_set` must be generic.
The child classes use it with the child spec.
Codegen does not reject a class that is not generic, but then the type checker does not check the class against the fields of the abstract model.

```python
from typing import TYPE_CHECKING, Self
from orm import QuerySet

if TYPE_CHECKING:
    from models import AbstractUserFields

class UserQueries[M: AbstractUserFields](QuerySet[M]):
    def by_email(self, email: str) -> Self:
        return self.filter(self._model.email == email)
```

```ts
export class UserQueries<S extends AbstractUserSpec> extends QuerySet<S> {
  byEmail(email: string): this {
    return this.filter({ email } as never) as never;
  }
}
```

Generated TypeScript passes the spec in an instantiation expression, `useQuerySet(User, () => _q0.UserQueries<UserSpec>)`.
A bare generic class loses its type argument and `tsc --strict` rejects it.

### A methods class

A Python methods class cannot annotate `self` with the abstract type.
Derive it from the type under `TYPE_CHECKING`:

```python
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from models import AbstractUserFields as _Fields
else:
    _Fields = object

class UserMethods(_Fields):
    def domain(self) -> str:
        return self.email.split("@")[1]
```

In TypeScript, annotate `this`: `domain(this: AbstractUserData): string`.

## Checks

`crates/core/tests/abstract_typing.rs` runs mypy --strict, pyright and `tsc --strict` on the generated types with the classes above.
`scripts/feature-check.sh` runs `extensions/inherit/tests/public/python.py` and `node.mjs` on each artifact that has the extension.
