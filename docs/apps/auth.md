# The auth app

The auth app gives your project users, roles, permissions and passwords.
It is one schema and one small module for each language.
Only the password rules use native code: they call the Rust core of the `monorm` package.
It comes with the `monorm` package of each registry: the Python module is `monorm_auth`, and the npm module is `monorm/auth`.

The app needs the `monorm.inherit` extension, which is built into the same package.
See [inherit.md](../extensions/inherit.md).

## Install and import

```sh
pip install monorm      # Python: import monorm_auth
npm install monorm      # TypeScript: import ... from "monorm/auth"
```

Import the schema in your main schema file:

```prisma
import "app:<name>" (prefix: "auth_")
```

The tables are `auth_user`, `auth_role`, `auth_permission`, `auth_rolepermission`, `auth_user_roles` and `auth_user_permissions`.
The prefix is optional.
`<name>` is `monorm-auth` in a Python project and `auth` in a TypeScript project.
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
import "app:<name>" (prefix: "auth_")

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

## Password rules

`validate_password(password, user=None)` (TypeScript: `validatePassword(password, user)`) checks a new password against a list of rules.
It returns a list of errors, and an empty list means the password is good.
It does not raise for a weak password.
`create_user` and `set_password` do not call it, because a script or an import may need a weak password.
The admin password change calls it, and so should your own forms and commands.

```python
from monorm_auth import validate_password

errors = await validate_password(new_password, user)
if errors:
    show([e.message for e in errors])
```

```ts
import { validatePassword } from "monorm/auth";

const errors = await validatePassword(newPassword, user);
```

An error has a `code`, an English `message` and `params`.
The `code` is stable: translate the message in your project by `code` and `params`.
`user` is a model, an object, a dict or `None`; it can be a user that has no row yet.
`password_help_texts()` (TypeScript: `passwordHelpTexts()`) gives one help text for each rule, for a form.

### The default rules

| Rule | Option | Code and `params` |
| --- | --- | --- |
| `min_length` | `min`, default `8` | `password_too_short` `{min}` |
| `common` | `path`, a file of one password for each line | `password_too_common` |
| `numeric` | | `password_entirely_numeric` |
| `user_similarity` | `attributes`, default `["username", "email"]`; `max_similarity`, default `0.7` | `password_too_similar` `{attribute}` |

The built-in rules run in one Rust function, so Python and TypeScript give the same result.
The length counts code points after Unicode NFC.
`numeric` accepts decimal digits of any script, such as `٣`; it does not accept `½` or a CJK numeral.
`common` and `user_similarity` compare the lower-case NFC text.
`common` also trims the password, as Django does.
`user_similarity` is the `quick_ratio` check of Django: a password is too close to an attribute when its character overlap with the whole value, or with one word of the value, is at least `max_similarity`.
An attribute with no text value is skipped.

The common-password list is `crates/core/data/common-passwords.txt`, inside the wheel and the npm package.
It holds 10,001 entries (about 73 KB), made lower case and without duplicates.
The source is [`Passwords/Common-Credentials/10k-most-common.txt`](https://github.com/danielmiessler/SecLists/blob/913b327317496d062bcc7cace524aaad8a693be2/Passwords/Common-Credentials/10k-most-common.txt) of SecLists, at commit `913b327317496d062bcc7cace524aaad8a693be2`.
SecLists has the MIT license; the text is in `common-passwords.LICENSE` next to the list.
Set `path` to use your own list.

### Choose the rules

Set `password_rules` in the `[auth]` table of the settings file (`passwordRules` in `package.json`, with `maxSimilarity`).
No setting gives the four default rules.
An empty list turns the rules off.
The rules run in list order, and the errors come in that order.

```toml
[auth]
password_rules = [
  { rule = "min_length", min = 15 },
  { rule = "common" },
  { rule = "user_similarity", attributes = ["username", "email", "first_name"] },
  { rule = "myproject.rules:NoCompanyName", word = "acme" },
]
```

A name with no `:` is a built-in rule.
An unknown rule, an unknown option or a bad value is a setting error.
The message names the file and the key.
You can also give a list in code: `validate_password(pw, user, rules=[rules.MinLength(min=12)])`.
In TypeScript the built-in rules are `rules.minLength`, `rules.common`, `rules.numeric` and `rules.userSimilarity`.

### Custom rules

A custom rule is `module:attr` in the settings.
The other keys of its entry go to it as options.
A rule has two methods.
`validate(password, user)` returns a list of `PasswordError`, and may be `async`.
`help_text()` (TypeScript: `helpText()`) returns a string.

```python
from monorm_auth import PasswordError

class NoCompanyName:
    def __init__(self, word):
        self.word = word

    def validate(self, password, user=None):
        if self.word in password.lower():
            return [PasswordError("password_has_company", f"The password has {self.word}.", {"word": self.word})]
        return []

    def help_text(self):
        return f"Your password can't have {self.word}."
```

A custom rule that cannot be imported is a setting error.
A custom rule gets the password as typed. The built-in rules get the NFC form.
A custom rule must not put the password in `params`: `params` reach the browser and the audit row.

`validate_password` without `rules` reads the settings of the current directory at each call.
For a form, call `load_rules()` (TypeScript: `loadRules()`) once at start and pass the list as `rules`.
A custom rule then keeps its state.
The admin does this: it loads the rules once, and a setting error stops it with exit code `2`.
It reads `[auth]` from the current directory, so start it from the project directory.

### Length and NIST

NIST SP 800-63B asks for at least 15 characters when the password is the only factor.
It asks for at least 8 when the password is one of several factors (MFA).
The default is 8.
If an account has no second factor, set `min_length` to `15`.

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

### Create permissions

`sync_permissions` takes the names to create, as `<label>.<codename>`, and creates the absent ones in one bulk insert.
It has no strategy of its own: the caller decides the names.

```python
from monorm_auth import sync_permissions

await sync_permissions(["orders.see_total", "shops.see_credential"])
```

```ts
import { syncPermissions } from "monorm/auth";

await syncPermissions(["orders.see_total", "shops.see_credential"]);
```

Rows that exist stay as they are.
The function deletes nothing: the caller finds the stale names.
A name that is not `<label>.<codename>` raises an error before any row is written.
One string in place of a list raises `TypeError` in both languages, also before any row is written.
The `name` of a new row is `Can ` and the codename with spaces, for example `Can see total`.
The function returns the number of new rows.
The app never calls it by itself.

For the Django-style set, pass the names that `model_permission_names` gives:

```python
from monorm_auth import model_permission_names, sync_permissions

await sync_permissions(model_permission_names([Post, Comment], "blog"))
```

```ts
import { modelPermissionNames, syncPermissions } from "monorm/auth";

await syncPermissions(modelPermissionNames([Post, Comment], "blog"));
```

It gives `add`, `change`, `delete` and `view` for each model: `blog.add_post`, `blog.view_comment` and so on.
Call `sync_permissions` once for each set of models, for example after a migration.

## Out of scope

* Sessions, tokens and login state belong to your web framework.
* Permissions for one row (object-level permissions).
