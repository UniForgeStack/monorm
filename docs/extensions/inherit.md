# Abstract models and model overrides

The `monorm.inherit` extension (crate `monorm-inherit`, `extensions/inherit`) lets one schema share model shape and lets a consumer replace an app model.
It asks the host for two capabilities, `schema-transformations` and `physical-schema`, and for no host feature.
Its pass runs in the `declaration` phase, so the host and the other extensions see only ordinary models.

**Enable:** nothing to install. The extension is built into the `monorm` Python wheel (module `monorm_inherit`) and the `monorm` npm package (`monorm/extensions/inherit`); see [optional-packaging.md](../contracts/optional-packaging.md#extension-contracts).
How it works and where the code is: [extensions/inherit/README.md](../../extensions/inherit/README.md).

## Schema

```prisma
model AbstractUser {
  id    Int    @id @default(autoincrement())
  email String @unique
  roles Role[] @relation("{model}_roles")
  teams Team[] @relation(through: "{model}Team")
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

model Admin {
  @@inherit.extends("AbstractUser")
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
  The pass adds this back relation on the target.
  In a concrete model, `{model}` in a relation name is an error.
- A list relation with no `through:` stays one-to-many.
  The target gets a nullable foreign key for each child (`Role.user_roles_id`).
  A role belongs to one user only.
- An abstract model can extend another abstract model.
  Declarations that other extensions add (for example `@@history.track`) are copied to each child with the rest of the base.
- A child can redeclare a primary key field and change its type and default (`id String @id @default(uuid())`).
  A primary key on another field is an error.
  Redeclare the primary key field of the base (`id`) instead.

### A many-to-many relation

`through: "{model}Team"` on a list relation of an abstract model makes the relation many-to-many.
The pass generates one join model for each child.
In the name of the join model, `{model}` becomes the name of the child (`UserTeam` for `User`, `AdminTeam` for `Admin`).
The pass sets `through` on the relation of the child and on the back relation of the target.
`{model}` in a `through:` name is an error in a relation that no pass derives, for example in a concrete model.

- The join table is `<child table>_<field>`, where `<field>` is the name of the relation field (`auth_user_teams` for `User.teams` when `User` has the table `auth_user`).
- The join model has an `id` (auto-increment primary key) and two required foreign key fields.
  A foreign key field is named from the lower-case model name, an underscore, and the name of the primary key field (`user_id`, `team_id`).
  Each foreign key has `onDelete: Cascade`.
- The pair of foreign keys has a `@@unique` constraint.
  The migration names it from the join table and the two fields (`auth_user_teams_user_id_team_id_key`).
  The migration shortens a name of more than 63 characters.
  Each join table has its own name, so two children never share a constraint name.
- With no relation name, the back relation on the target has the lower-case child name, an underscore, and the field name (`Team.user_teams`, `Team.admin_teams`).
- The join model has no extra fields.
  For extra fields, write the join model by hand.
- A join model needs a one-field primary key on the child and on the target.
  A composite primary key is an error.
- A join model name that matches an existing model is an error.

### An override

A child with `@@inherit.override("User")` copies `User` and takes its table.
`User` gets no table and no runtime model.
Relations to `User` point to the child, and the migration changes the table and keeps its rows.
A generated join model of `User` keeps its name and its table, and points to the child, so the link rows stay.
A hand-written `through:` relation on `User` and its join model follow the child in the same way.

- An override cannot change the type of the primary key of `User`.
  The length of the primary key is part of the type (`@db.VarChar(36)`).
  The foreign keys that point to `User` keep the old type.
  The error message is "`MyUser.id`: an override cannot change the key type of `User`; incoming foreign keys keep it".
- Two overrides of the same model are an error.
- If the override has its own `@@map`, the migration renames the base table to that name.
- To remove such an override, use two migrations.
  In the first migration, replace the `@@map` with `@@renamed_from("<mapped name>")` on the override.
  The table gets its default name back.
  In the second migration, remove the override.
  If you make both changes in one migration, the base model comes back with its default table name.
  The migration cannot match the renamed table, so it creates a new table and drops the old table with its rows.

## Get the active model

`model_for(name, registry=None)` (Python, `monorm_inherit`) and `modelFor(name, registry = defaultRegistry)` (TypeScript, `monorm/extensions/inherit`) return `MyUser` for `"User"`.
They return the name's own model when nothing overrides it.
Pass the registry that you gave to `loads`; the default is the default registry.
The Python function returns `Any`; the TypeScript function returns the registered model without a static type.

```python
from monorm_inherit import model_for

Account = model_for("User")
```

## Use

The generated models are ordinary models.
`monorm.loads` (Python) and `loads` (TypeScript) return the concrete models only: `MyUser`, `Admin`, `Role`, `Team`, `UserTeam` and `AdminTeam` for the schema above.
The abstract models and the overridden `User` are not in the result.

```python
import monorm
from monorm_inherit import model_for

models = monorm.loads(schema_text, registry=registry)
User = model_for("User", registry)                  # MyUser
db = await monorm.connect(url, registry=registry, default=False)
await db.create_tables()
user = await User.objects.using(db).insert(email="a@b.c", bio="hi")
await Role.objects.using(db).insert(name="staff", user_roles_id=user.pk)
roles = await user.roles.using(db).all()            # one-to-many through the generated foreign key
teams = await user.teams.using(db).all()            # many-to-many through UserTeam
```

```ts
import { connect, loads, Registry } from "monorm";
import { modelFor } from "monorm/extensions/inherit";

const registry = new Registry();
const models = loads(schemaText, { registry });
const User = modelFor("User", registry);             // MyUser
const db = await connect(url, { registry, default: false });
await db.createTables();
const user = await User.objects.using(db).insert({ email: "a@b.c", bio: "hi" });
await models.Role.objects.using(db).insert({ name: "staff", userRolesId: user.pk });
const roles = await user.roles.using(db).all();
const teams = await user.teams.using(db).all();
```

The child column and member names follow the usual rules: snake case in Python (`user_roles_id`), camel case in TypeScript (`userRolesId`).
Abstract models add no table, so `makemigrations` and `sqlmigrate` show tables for the concrete models only.
`monorm pull` reads the database and writes ordinary models: it writes no `@@inherit.*` attribute back.

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
from monorm import QuerySet

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

## Errors

The schema load fails with a message of this form:
`extension monorm.inherit: pass monorm.inherit.expand: schema.prisma:5:2: @@inherit.extends: <text>`.
The table lists the `<text>` of the main causes.

| Message | Fix |
|---|---|
| `extends cycle: A -> B -> A` | Remove one `@@inherit.extends`. |
| `` `U` is not abstract; use `@@inherit.override("U")` `` | Add `@@inherit.abstract` to `U`, or use `@@inherit.override`. |
| `` `U` is abstract; use `@@inherit.extends` `` | An override takes a concrete model. |
| `M and N both override U` | A model has one override. |
| `U is itself an override; override the concrete model, not a chain` | Override the model that holds the table. |
| `C extends itself`, `C overrides itself` | Name another model. |
| `unknown model Nope` | Define the base in the same schema set. |
| `A is abstract and has no table; @@map is not allowed on it` | Remove `@@map`, `@@renamed_from` or `@@partition` from the abstract model. |
| `B.a: A is abstract and has no table; a relation to it is not allowed` | Relate to a child. |
| `A is abstract and cannot override a model` | Move `@@inherit.override` to a concrete model. |
| `C.n: the field is in both A and B; redeclare it in C` | Declare the field in the child. The same holds for a relation (`the relation is in both ...`). |
| `C: @@query_set is in both A and B; declare one on C` | The same for `@@model_methods` and other copied declarations. |
| `C: the primary key of A (id) differs from the one of B (code)` | Make the bases agree, or redeclare the key in the child. |
| `` `C.id`: an override cannot change the key type of `U`; incoming foreign keys keep it `` | Keep the key type of the base. |
| `C.teams: a generated join model needs a one-field primary key on C and T` | Give both models a one-field key, or write the join model by hand. |
| `C.teams: a generated back relation needs a one-field primary key on C` | Give the child a one-field primary key. |
| `C.teams: the join model CT already exists; name it differently with {model} in through:` | Rename the join model or the existing model. |
| `C.teams: a generated join model needs two different models; ...` | A relation from a model to itself needs a hand-written join model. |
| `A.r and C.r both generate T.c_r; put {model} in the relation name` | Two children generate the same back relation. Put `{model}` in the relation name. |
| `A has an explicit index or constraint name "n" and two or more children; ...` | Remove the name. It would repeat in each child table. |
| `inheritance across schema batches is not supported; declare the base and the child in one schema set` | Load the base and its children together. |
| `lists A twice` | List each base once. |
| `has a second @@inherit.extends; list every base in one` | Write one `@@inherit.extends([...])`. |

`{model}` in a relation name or a `through:` name of a concrete model fails in the host, with no extension prefix:
`relation C.ts: {model} in a relation name is only expanded in a relation that an extension pass derives; name this relation without it`.

## Limits

- Inheritance works inside one schema set.
  A child cannot extend an abstract model that another batch loaded.
- An override of an override is an error: override the concrete model.
- An abstract model cannot override a model.
- The generated types carry fields only (see above).
- `pull` writes no inheritance attribute: the result is plain models.

## Checks

`crates/core/tests/abstract_typing.rs` runs mypy --strict, pyright and `tsc --strict` on the generated types with the classes above.
`scripts/check-extension-packages.sh` runs `extensions/inherit/tests/public/python.py` and `node.mjs` in fresh consumers that install the package.
