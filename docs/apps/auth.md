# The auth app

`monorm-auth` gives your project users, roles, permissions and passwords.
It is one schema and one small package for each language.
It has no native code.
The Python package is `monorm-auth` (import `monorm_auth`).
The npm package is `monorm-auth`.

The app needs the `monorm.inherit` extension.
Use a build that has it, for example `monorm[full]`.
See [inherit.md](../extensions/inherit.md).

## Install and import

```sh
uv pip install monorm-auth      # Python
npm install @ufstack/monorm-auth # TypeScript
```

Import the schema in your main schema file:

```prisma
import "app:monorm-auth" (prefix: "auth_")
```

The tables are `auth_user`, `auth_role`, `auth_permission`, `auth_rolepermission`, `auth_user_roles` and `auth_user_permissions`.
The prefix is optional.
See [Installed app import](../schema/schema.md#installed-app-import) for how the import resolves.
Run `monorm generate` and `monorm makemigrations` as usual.

## Models

| Model | Purpose |
| --- | --- |
| `User` | `username` (unique), `email`, `password`, `is_active`, `is_superuser`, `last_login`, `date_joined`, and the relations `roles` and `permissions`. |
| `Role` | A named set of permissions: `name` (unique) and `permissions`. |
| `Permission` | `label` (the part before the dot in a permission name, for example `blog`; it is not a model name), `codename`, `name`. The pair `label` and `codename` is unique. |
| `RolePermission`, `UserRole`, `UserPermission` | The join models. |

`User` extends the abstract model `AbstractUser`.
The join models `UserRole` and `UserPermission` come from the `{model}` template in the `through:` option.

## Use your own user model

You can add fields in two ways.
The methods, `with_perm` and the permission rules are the same in both ways.

### Override `User`

Add the fields that you need in a model that overrides `User`:

```prisma
import "app:monorm-auth" (prefix: "auth_")

model MyUser {
  bio String?
  @@inherit.override("User")
}
```

`MyUser` takes the table of `User`.
It keeps the join models `UserRole` and `UserPermission`.
`authenticate` and `sync_permissions` find `MyUser` in the registry.
Pass `registry` if you do not use the default registry.

### Extend `AbstractUser`

Use this way when you want a separate model and separate join models:

```prisma
model Account {
  bio String?
  @@inherit.extends("AbstractUser")
}
```

`Account` gets its own join models, `AccountRole` and `AccountPermission`.
The model `User` of the app stays in the schema, and nothing uses it.
The registry cannot tell which model you chose, so pass the model to `authenticate`:
`authenticate(username, password, model=Account)` in Python, and `authenticate(username, password, { model: Account })` in TypeScript.

Call `with_perm` on your own model: `Account.objects.with_perm("blog.add_post")`.

## Passwords

`make_password(raw)` returns an argon2id hash (PHC string).
The parameters are `m=65536`, `t=3` and `p=4`, in both languages.
A hash from one language is valid in the other.

`check_password(raw, encoded)` compares in constant time.
It accepts argon2id hashes and Django `pbkdf2_sha256$` hashes.
An unknown or broken hash gives `False`.
You can move users from Django without a password reset.

`needs_update(encoded)` is true when the hash is not argon2id with the current parameters.

| Python | TypeScript |
| --- | --- |
| `user.set_password(raw)` | `user.setPassword(raw)` |
| `user.check_password(raw, update=True)` | `user.checkPassword(raw, { update: true })` |
| `await authenticate(username, password)` | `await authenticate(username, password)` |

`set_password` changes the object only.
It does not write the row.
Write it yourself: `await user.update(password=user.password)`.

`check_password` with `update=True` sets a new argon2id hash on the object when `raw` is correct and the old hash needs an update.
It does not write the row.

`authenticate` returns the active user, or `None` (`null` in TypeScript).
It writes the new hash when the old hash needs an update.
For an unknown username it still computes one hash, so the time does not show whether the user exists.
It does not change `last_login`.

## Permissions

A permission name has the form `<label>.<codename>`, for example `blog.add_post`.
A name without a label is an error: `ValueError` in Python and `TypeError` in TypeScript.

| Python | TypeScript |
| --- | --- |
| `await user.has_perm("blog.add_post")` | `await user.hasPerm("blog.add_post")` |
| `await user.has_perms([...])` | `await user.hasPerms([...])` |
| `await user.get_all_permissions()` | `await user.getAllPermissions()` |
| `User.objects.with_perm("blog.add_post")` | `User.objects.withPerm("blog.add_post")` |

* A user has a permission when the user holds it directly or through a role.
* An active superuser has every permission.
* An inactive user has no permission, also when the user is a superuser.
* `has_perm`, `with_perm` and `get_all_permissions` use one query each.
* `has_perms` uses one query for each name, and it stops at the first name that the user does not have.
* `with_perm` returns a query set, so you can add more filters.

### Create the default permissions

Call `sync_permissions` once for each set of models, for example after a migration:

```python
from monorm_auth import sync_permissions

await sync_permissions([Post, Comment], "blog")
```

```ts
import { syncPermissions } from "monorm-auth";

await syncPermissions([Post, Comment], "blog");
```

It creates `add`, `change`, `delete` and `view` for each model, in one bulk insert.
The names are `blog.add_post`, `blog.view_comment` and so on.
Rows that exist stay as they are.
The function returns the number of new rows.
The app never calls it by itself.

## Out of scope

* Sessions, tokens and login state belong to your web framework.
* Permissions for one row (object-level permissions).
