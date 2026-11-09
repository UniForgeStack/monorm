# The admin

The admin is in progress.
It reads model lists for a superuser, and `monorm admin` serves them over HTTP.
It has a password login. It has no write or filter yet.
A served API request without a session answers 401.

The admin is one Rust crate (`monorm-admin`) with a thin module for each language.
The Python module is `monorm.admin`, and the TypeScript module is `monorm/admin`.
`schema.prisma` gets no admin attributes: the config lives in code.

## Register a model

`register` takes a model and a config.
A column is a field reference, so pyright, mypy and TypeScript check it.
A column through a to-one relation is one more column in the same statement.

```python
from monorm import admin

admin.register(Post, {"list": {"columns": [Post.title, Post.author.name], "order": [Post.created_at.desc()], "page_size": 25}})
admin.check()  # raises AdminConfigError; needs no database
```

```ts
import { admin } from "monorm/admin";

admin.register(Post, { list: { columns: [Post.title, Post.author.name], order: [Post.createdAt.desc()], pageSize: 25 } });
admin.check(); // throws AdminConfigError; needs no database
```

`check()` rejects an unknown field and a list column through a to-many relation.
It returns the warnings as a list of strings (for example a search field that has no index).
The page size is 1 to 200, and the default is 25.
An order column must be a plain column: not nullable, not an array, and not Json, Decimal or an enum.
The primary key is always the last order column, so a keyset page has a stable order.

## Default resources

With no `register` call, every concrete model is a read-only resource.
After the first `register` call, only the registered models are resources.
A resource is named after its model.

## Lists

A list page is a keyset page.
The cursor holds the order values of the last row.
The next page is one statement with no `OFFSET`.
A page has `next` and `prev` cursors (`null` at an end).
`?cursor=` goes forward, and `?before=` goes back with a `prev` cursor.
`?last=1` gives the last page, and the first page needs no parameter.
A cursor belongs to its sort: with another `sort`, the answer is `400`.
`?page=N` jumps to a page with `OFFSET`.
It works only when the count is exact: if more than 10,000 rows match, the answer is `400`.
A `page` that is too large for the offset also answers `400`.
A cursor page whose rows are gone is empty, and it keeps the cursor back (`prev` after `?cursor=`, `next` after `?before=`).

### Count

The rows come first, and the count is a separate request: `GET {prefix}/counts/{name}?q=`.
The answer is `{"count", "capped"}`.
The count stops at 10,000, and then `capped` is `true` and the UI shows "10,000+".
`?exact=1` counts all rows, under the statement timeout, and `capped` is then `false`.
An exact count on a large table can time out (`504`).

### Sort

`sortable` lists the sort columns of a resource, and `?sort=title` or `?sort=-title` picks one.
A column that is not in the list answers `400`.
Without `sortable`, every list column can sort.
The exception is a large table: with more than 50,000 rows, a sort on a column that leads no index answers `400`.
The service keeps the size, a capped count, for a short time.
A `queryset` or a bypass changes the rows of the request. Then the size is the capped count of the rows that the user can see, and the service does not keep it.
A `sortable` list is used as given, and no size check applies.
The primary key is always the last order column.

### Search

`search` lists the fields that `?q=` looks in.
A term starts with a prefix that picks the mode:

| Start | Mode |
|---|---|
| `=` | Exact match. |
| `^` | Starts with. |
| `@` | Full text. PostgreSQL only: SQLite answers `400`. |
| none | The default for the field. |

The default comes from the schema indexes of the field:
- a trigram index gives contains;
- a btree index gives starts with;
- a full-text index gives full text;
- no index gives contains, and `check()` and the server start warn about it.

A field that has no strategy at all (for example a `DateTime`) is an error in `check()` and in `Site::build`.
`%` and `_` in a term are text, not wildcards.
A term that is a whole number also tries the primary key.
A term is at most 200 characters.

`search_with` (Python) and `searchWith` (TypeScript) replace the fields with your own search.
It is a function `(q, limit)` that returns primary keys, or a prepared query set with a `q` parameter.
The service calls it once for a request, with a limit of 1,000, and it lists the rows with those keys.
The `rows` answer then has `count` and `capped` too, so the UI makes no second call.
A key of the wrong type is a fault of the function, and the answer is `500`.
The function gets no row scope. The service applies the `queryset` filter to the rows with the keys that it found.
So when the function finds more than 1,000 matches, the list shows only the rows of the first 1,000 keys that the `queryset` lets through.

```python
by_body = Note.objects.filter(Note.body.contains(monorm.param("q"))).prepare()
admin.register(Note, {"list": {"search_with": by_body}})
```

```ts
admin.register(Note, { list: { searchWith: async (q, limit) => (await findIds(q, limit)) } });
```

### Filters

`filters` lists the fields of the filter bar.
An entry is a field, a field of a to-one relation (`Comment.author.name`), or a to-one relation (`Comment.author`).
A name that matches nothing is an error in `check()` and in `Site::build`.

| Query | Meaning |
|---|---|
| `?f.level=high` | The field equals the value. |
| `?f.level=high&f.level=low` | Several values for one filter mean "or". Different filters mean "and". |
| `?f.rank:gte=2`, `:gt`, `:lt`, `:lte` | A range, on numbers, dates and times. |
| `?f.nick:empty`, `?f.nick:notempty` | "Is empty" and "is not empty". A text field counts `NULL` and `""` as empty. |
| `?f.author=<key>` | A relation filter. The value is the encoded key of the target, as in the `keys` of a list. |
| `?f.author.name=Bo` | A field of a related model. It adds a subquery to the same statement, not a second statement. |

A value of the wrong type for the field answers `422`.
A date and time value is ISO 8601 with an offset, and a date-only value means midnight UTC.
The metadata of a resource has `filters` with the `kind` (`enum`, `bool`, `number`, `datetime`, `date`, `uuid`, `text` or `relation`) and the `operators` that the filter takes.
`?q=` and the filters work together, and so does the count (`GET {prefix}/counts/{name}?q=&f.level=high`).

`named_filters` (Python) and `namedFilters` (TypeScript) are filters that you write as conditions, for the choices of a person who does not know the data:

```python
admin.register(Ticket, {"list": {"named_filters": [{
    "name": "urgency", "label": "Urgency", "default": "open",
    "choices": [
        {"name": "open", "label": "Open", "condition": Ticket.closed_at.is_null()},
        {"name": "urgent", "label": "Urgent", "condition": Ticket.priority >= 3},
        {"name": "overdue", "label": "Overdue", "condition": Ticket.due < monorm.func.now()},
    ],
}]}})
```

`?n.urgency=urgent` picks a choice.
Without it the `default` applies, and `?n.urgency=all` (the reserved choice `all`) turns the filter off.
A condition is a fixed expression: it takes int, string, bool and null literals and no `monorm.param`.
A date, decimal or float literal is a build error; compare to a column or to `func.now()` instead.
In TypeScript an integer beyond 2^53 - 1 (a `bigint`) is also a build error.
An unknown choice answers `422`.

### Computed and aggregate columns

A column that is not a field is declared once and listed in `columns` by name.
`computed` has two forms:
- `expr` is an expression that the list statement computes, such as `monorm.func.length(Note.body)`. It sorts when the config says `sortable: True` and the column is not nullable.
- `function` is `async def f(keys)`. It gets the keys of the page in one call and returns one value per key, in the same order. The declared `type` is checked, and a wrong value is a `500`.

`aggregates` shows the `count`, `sum`, `min`, `max` or `avg` of a to-many relation, as a column.
It is one statement for the page, with no N+1.

```python
admin.register(Author, {"list": {
    "columns": [Author.name, "comments", "top_rank"],
    "aggregates": [
        {"name": "comments", "label": "Comments", "relation": Author.comments, "fn": "count"},
        {"name": "top_rank", "label": "Top rank", "relation": Author.comments, "fn": "max", "field": Author.comments.rank},
    ],
}})
```

`labels` (a title for each field) and `empty` (the text for an empty value) change what a column shows.
`empty_fields` sets the text for one field, and it wins over `empty`.
A column name that clashes with a field, or a label or empty text for an unknown field, is a build error.
In TypeScript the keys are `namedFilters`, `emptyFields`, and `computed` and `aggregates` with the same entries (`func.length(Note.body)`, `Author.comments`); `function` is `async (keys) => values`.

### To-many tabs

The detail page shows each to-many relation as a tab, and the tab is the list of the child resource, with its search, filters and sort.
`tabs` picks the relations, and without it every to-many relation that leads to a resource is a tab.
The routes are `GET {prefix}/resources/{name}/{key}/{relation}` and `GET {prefix}/counts/{name}/{key}/{relation}`.
They take the same query as a list.
An unknown record or relation answers `404`.
The permission of the child resource applies, so the UI shows no tab for a resource that the user may not read.

### Row scope

`queryset` limits the rows that staff see.
It is a prepared query set with a `param("user")` slot: the admin fills it with the key of the staff user.

```python
mine = Order.objects.filter(Order.owner_id == param("user")).order_by(-Order.id).prepare()
site.register(Order, {"queryset": mine, "list": {"columns": [Order.id, Order.total]}})
```

The filter applies to the list, the detail, the count, the search and the tabs.
A row outside the filter answers `404` on the detail, and a parent outside it has no tabs.
The order of the query set is the order of the list: the list sets none of its own then, or `check()` fails.
The query set takes a filter and an order only; `load()`, a slice, `from()`, `join()`, `lock()` and `without_defaults()` are build errors, and so are a query set of another model, one without a filter, and a param other than `user`.
In TypeScript the same key is `queryset`, with `param("user")`.

A callable `queryset` says a rule that a query set cannot say.
`async def scope(user)` (Python) and `async (user) => condition` (TypeScript) return a condition, or a list of conditions that mean AND.
It runs once for each request, through the host bridge, and the values stay in the condition.
Use a query set when you can: it costs no host call.

### `can` rules

`can={"change": expr, "delete": expr}` (`can: { change, delete }` in TypeScript) are conditions of the row.
The list and detail queries select them as flags, with no host call.
Each row of `GET {prefix}/resources/{name}` has an entry in `flags`, the detail has a `flags` object, and the resource metadata has `can`, the names of the rules.
The UI disables the button of a rule that the record does not allow.
Only `change` and `delete` are rule names.
A `can` rule does not replace the permission: the user needs both.
A write checks the rule on the locked row: see [Writes and forms](#writes-and-forms).

### Read defaults and the bypass permission

A read default of a model, such as the `deleted` default of `@@soft_delete`, hides rows in the list and the detail.
Each named default that reads no `scope` value has a bypass permission, `<label>.bypass_<default>_<resource>` (for example `admin.bypass_deleted_order`).
A user with it, and with `admin.access` and the read grant of the resource, can add `?without=deleted` (several names are separated by commas) to the list, the count and the detail.
A superuser has it.
An unknown name answers `400`, and a name without the permission answers `403` with one audit row.
`GET {prefix}/resources` lists in `bypass` the names that the user may use, and the UI shows a "Show deleted" checkbox for each.
On a tab route `without` applies to the child only.
The `queryset` filter is never lifted.
The name is the name of the default, not of the model. A related model that the statement joins, and that has a default of the same name, shows its hidden rows too.
A read default that reads a `scope` value, such as a tenant filter, has no bypass permission: see "Tenancy" below.

### Tenancy

The admin ignores tenancy: staff are not tenants, and the admin has no tenant picker.
An admin read sets no `scope` value, and a read default that reads one (`@@query.filter("tenant", "shop_id == scope.shop")`) is off in every admin read.
A model with a tenant read default shows the rows of all tenants, and never raises the missing-scope `QueryError`.
`?without=<name>` of such a default answers `400`, because it is always off.
Read defaults that read no `scope` value, such as soft delete, stay on, and their bypass permission works as above.
PostgreSQL row-level security still applies to the database role of the admin connection.
Give that role the rights it needs, for example `BYPASSRLS` or a policy that lets it read the rows of all tenants.
Use `queryset` to limit the rows of staff when you need it.

### Statement timeout

On PostgreSQL every request runs `SET LOCAL statement_timeout` first, and the default is 15 seconds.
`Admin(statement_timeout=...)` (Python, seconds) and `new Admin(registry, { statementTimeout })` (TypeScript, seconds) change it, and `0` turns it off.
A cancelled statement answers `504`, on a list, a count, the size check of a sort and a detail page.
The largest value is 2,147,483 seconds (the limit of PostgreSQL); a larger value is an error in `check()`.
SQLite has no statement timeout.

## Test client

`test_client` (Python) and `testClient` (TypeScript) send a request to the router in memory.
No socket opens, and no port is used.

```python
client = admin.test_client(db, user={"is_superuser": True})
page = (await client.get("/admin/api/resources/Post")).json()
```

```ts
const client = admin.testClient(db, { user: { is_superuser: true } });
const page = (await client.get("/admin/api/resources/Post")).json();
```

The response has `status`, `text`, `queries` (the statements that the request ran), `sql` (their text, with placeholders) and `json()`.

## Login and sessions

The served admin signs in a user with a password.
The login calls `authenticate` of `app:auth` in your own process (Python or Node), through a bridge.
The Rust service never hashes or compares a password.
An inactive user cannot sign in.
Only a superuser reads resources: the resources are superuser-only for now.

| Route (under the API prefix, for example `/admin/api`) | Method | Meaning |
|---|---|---|
| `/login` | `GET` | Sets the CSRF cookie. Needs no session. |
| `/login` | `POST` | `{"username", "password"}`. On success it sets the session cookie. |
| `/me` | `GET` | The signed-in user. |
| `/password` | `POST` | `{"old_password", "new_password"}`. The user changes their own password. |
| `/logout` | `POST` | Ends the session. |

A wrong password and an unknown user give the same `401` answer.
The time of the two answers is equal because `authenticate` of `app:auth` hashes a dummy password for an unknown user.
Three limits count failed logins in 15 minutes, and the login answers `429` with `Retry-After` when one is full, even for the right password:
- 5 for one username from one IP address. An attacker at one address cannot lock the owner out from another address.
- 20 from one IP address, for any username.
- 50 for one username from any address.

A counter table holds at most 10,000 keys. When it is full, it drops the oldest keys.
Every login, failed login, password change and failed password change writes an `AdminLogEntry` row.
A failed login keeps `user_key` empty and puts the typed name in the label, because the text is not an account.
A `429` writes a `login_blocked` row, at most one a minute for each key. The attempts themselves are not limited by this.

### `MONORM_ADMIN_SECRET`

The session cookie is `s1.<payload>.<tag>`: the payload is signed with HMAC-SHA256 and the key is the environment variable `MONORM_ADMIN_SECRET`.
The secret is at least 32 bytes.
`monorm admin` stops with exit code 2 and a message that names the variable when it is missing or short.
It is never a setting in a file.
The server keeps no session in a store, so two processes with the same secret accept the same session.
Change the secret to end every session at once.

The failed-login counters are local to one process. With N processes the limits are up to N times higher.

### Renewal: when a change takes effect

At most every `idle_timeout / 4` seconds (never less than 1, never more than 60), a request renews the session.
At the renewal the service reads the table `AdminSessionRevocation` and asks the host for the user again, in one short transaction.
It ends the session (`401`, and the cookies are cleared) when:
- the session is in the table, because the user logged out or changed the password with that session;
- the user is gone or inactive;
- the user was a superuser and is not any more;
- the password hash changed. The cookie holds an HMAC of the hash, never the hash.

So a change shows at the next renewal, not at once. The delay is at most `idle_timeout / 4` (at most 60 seconds).
A logout or a password change ends the session in the same process at once, and in another process at its next renewal.
A password change ends every other session of the user at the next renewal of each, and gives the changed session a new id, with a new CSRF token in the answer (`{"ok": true, "user", "csrf"}`).
If the revocation write fails, the logout answers `500` and the session stays.
If the read at a renewal fails, the request answers `500` and the cookie stays, so the next request tries again: the session is never kept alive by an error.
The rows of the table have an end time, the end of the absolute timeout of the session.

### Behind a proxy: `trusted_proxies`

The client IP is the address of the socket, unless the setting `trusted_proxies` lists it.
For a request from a listed address, the service reads `X-Forwarded-For` from the right and takes the first address that is not listed.
A request from any other address cannot choose its IP with that header.
The list takes exact IP addresses, not ranges. It is empty by default: behind a load balancer that you did not list, every login shares one IP counter.

### Cookies, CSRF and headers

The cookies are `__Host-monorm-session` (`HttpOnly`) and `__Host-monorm-csrf`.
Both are `Secure`, `SameSite=Lax` and `Path=/`.
A browser accepts a `__Host-` cookie only over HTTPS, or from `localhost`: serve the admin behind TLS.
The session ends after `idle_timeout` without a request (default 30 minutes) or after `session_timeout` (default 12 hours).

The API takes JSON only: a write with another content type answers `415`.
A script or API client sends every write (`POST`) with:
1. The CSRF token: the value of the `__Host-monorm-csrf` cookie, in the header `X-CSRF-Token`.
2. An `Origin` (or `Referer`) header that equals the `Host` of the request, or an entry of `origins`.

Without them the answer is `403` (`BadCsrfToken` or `BadOrigin`).
The `Origin` check compares the host and the port, not the scheme.
Behind a proxy that ends TLS, the scheme of the request differs from the scheme of the browser, so a scheme check would refuse real users.
Set `origins` where the browser `Origin` differs from the `Host`.
The login itself needs them: send `GET /login` first to get the CSRF cookie.

```sh
curl -c jar -s http://localhost:8000/admin/api/login > /dev/null
token=$(awk '$6=="__Host-monorm-csrf"{print $7}' jar)
curl -b jar -c jar -H "X-CSRF-Token: $token" -H "Origin: http://localhost:8000" \
  -H "Content-Type: application/json" -d '{"username":"ann","password":"pw"}' \
  http://localhost:8000/admin/api/login
```

Every API response has `Content-Security-Policy` (strict), `X-Frame-Options: DENY`, `Referrer-Policy: same-origin`, `X-Content-Type-Options: nosniff` and `Cache-Control: no-store`.
A UI file has the same headers, except `Cache-Control`: see [The UI](#the-ui).
A body over 16 KiB answers `413`.

### Test client with a login

`test_client(db, user=...)` and `testClient(db, { user })` need no login: they use the in-memory path.
Give a `secret` (and optional `settings` with `idle_timeout`, `session_timeout`, `origins` and `trusted_proxies`, and an `ip`) for a client that runs the full login:

```python
client = admin.test_client(db, secret="x" * 32)
await client.login("ann", "pw")
page = (await client.get("/admin/api/resources/Post")).json()
```

```ts
const client = admin.testClient(db, { secret: "x".repeat(32) });
await client.login("ann", "pw");
```

The client keeps the cookies and sends the CSRF token and `Origin` for you.
A `user` client and a `secret` client do not mix.
A wrong secret raises at the first request.

### Host functions and the binding

The login runs `auth.authenticate`, and a password change runs `auth.change_password`.
Each is an async function of `(database, args)`, and it must use the `database` that it gets.
The service runs it inside `Database.bind_request` (Python) or `db.bindRequest` (Node), so it sees the transaction of the request.
A statement on another handle waits for a connection of its own.

`bind_request` and `bindRequest` are public:

```python
with db.bind_request(tx, scope={"user": 1}, tenant="acme"):
    ...  # every statement of `db` uses `tx`; the caller commits or rolls back
```

```ts
await db.bindRequest(tx, { scope: { user: 1 }, tenant: "acme" }, async () => { /* ... */ });
```

- `tx` is a transaction that something else opened and will end.
- A `transaction()` block in the binding is a savepoint.
- `tenant` is the tenant id that `tx` began under: `db.tenant(sameId)` works, and another id raises `QueryError`.
- `on_commit` in the binding raises `TransactionRequired`, because the commit happens outside.

When a request times out, Python cancels the host task, and Node cannot cancel a Promise: its work runs on and the service discards the result.

## Permissions

A staff user sees and does only what the grants of the user allow.
Deny is the default.
An active superuser has every permission, and an inactive user has none.
The grants come from the auth tables (`Permission`, `Role`, and the join models of `User`).
The service loads them once per request, in one statement, and none for a superuser.
A grant change shows at the next request.
A deactivated user or a removed superuser flag shows at the next session renewal: see [Renewal](#renewal-when-a-change-takes-effect).

`Admin(permissions=...)` (Python) and `new Admin(registry, { permissions })` (TypeScript) choose a strategy.
Every strategy needs `admin.access` first: it opens the site.

| Strategy | Names per resource |
|---|---|
| `"page"` (default) | `<label>.read_<resource>` and `<label>.manage_<resource>`. Manage includes read and delete. |
| `"model"` | `add_`, `change_`, `delete_` and `view_` with the resource name. Read needs view or change. |
| `"access"` | None. `admin.access` is the only name. |

The label is `admin` by default, for every resource. `label=` on the site or on one resource changes it.
A pattern such as `orders.*` selects a name only when the label of that resource is `orders`.
The resource name is the lowercase model name.
With `perms=["read", "manage", "delete"]` on a resource, `page` adds `<label>.delete_<resource>`, and delete needs it.
A resource can also list `field_permissions` and `actions`. These add names such as `admin.see_name` and `admin.export_role` in `page` and `model`.

```python
site = Admin(permissions="model", label="staff")
site.register(Order, {"allow": ["read", "update"], "actions": ["refund"]})
```

`allow=` limits a resource for every user, superusers too.
It takes `read`, `create`, `update` and `delete`.

A request without a grant gets `403` and one audit row (action `permission_denied`).
The resources and actions that the user cannot use are absent from `GET {prefix}/resources`.
The columns of a resource that the user can read are all listed: only the names in `field_permissions` follow the grants.
The audit row says why in `changes`: `{"needs": [...]}` (one of these names is enough) or `{"reason": "allow"}` (`allow=` excludes read).

### `admin.permissions` and `sync-permissions`

`admin.permissions` lists the names of the chosen strategy.
`monorm admin sync-permissions` (and `npx monorm admin sync-permissions`) passes them to the auth `sync_permissions`, then seeds the roles.
It prints how many names it created and which names are stale.
A stale name is in the tables but not in the strategy. The command never deletes it.
Run it at deploy, after `migrate`.
The binary `monorm` cannot run it: it needs your admin module.

```text
permissions: 5 created, 1 stale
stale: admin.read_old
roles: 2 created, 4 grants added
```

### Pattern grants

`roles` maps a role name to patterns over `<label>.<codename>`.
`*` matches any text, `?` matches one character, and a leading `-` removes the names that it matches.
`[` and `]` are an error in both languages.
The command creates a missing role and adds the missing grants.
A second run changes nothing, and it removes no role and no grant.

```python
site = Admin(roles={"support": ["admin.*", "-admin.manage_*"], "reader": ["admin.read_role"]})
```

```ts
const site = new Admin(registry, { roles: { support: ["admin.*", "-admin.manage_*"], reader: ["admin.read_role"] } });
```

A pattern that matches no name is an error, and so is a run before the first sync.
A role that matches `admin.access` through its patterns can open the site; add `admin.access` when it is not in a pattern.

## Writes and forms

The admin creates, changes and deletes one record at a time.
A write needs the permission of the operation (see [Permissions](#permissions): `manage_` or `add_`, `change_` and `delete_`, by strategy) and, when the resource sets `allow`, the operation in it.
The UI shows Add, Edit and Delete only for the operations that the user holds (`ops` in `GET {prefix}/resources`).
The server checks them again on each request.

| Route | Body | Answer |
|---|---|---|
| `POST {prefix}/resources/{name}` | `{"set": {field: value}}` | `201` with `{"ok", "resource", "key", "values"}`. |
| `PATCH {prefix}/resources/{name}/{key}` | `{"set": {...}, "loaded": {...}}` | `200` with `{"ok", "key", "changed": [names]}`. |
| `DELETE {prefix}/resources/{name}/{key}` | `{"loaded": {...}}` (optional) | `200` with `{"ok", "key"}`. |

The body is at most 1 MB (the upload route takes 25 MiB).
Each resource in `GET {prefix}/resources` has `writable`, `read_only_reason`, `sections` (the `create` and `edit` layouts), `form_component` (whether a module replaces the form) and `form`: for each writable field its `name`, `required`, `default`, `readonly_on_update` (the key fields), `readonly` (the config), `widget`, `max_length`, `relation` (the picker of a relation field, or `null`) and `file` (whether the field holds a file). The resource has `chips`, the many-to-many relations that the form edits.

### The plain path

All of it runs in Rust, in one transaction.
The write selects the row with `SELECT ... FOR UPDATE`, with the `queryset` filter and the `can` rule.
A row outside the `queryset` is a `404`, and a row that its `can` rule does not allow is a `403` with an audit row.
It then compares the stored values with `loaded`, the values that the form showed.
It writes only the fields in `set`, writes one audit row, and commits.
A change that sets no new value writes nothing and no audit row, and answers `changed: []`.
A write to a field of a key, or to a field that the form does not have, is a `422` for that field.
`loaded` must hold every field in `set` and every guard field (a `400` when it does not), so the UI sends the loaded value of each field of the form.
A value counts by what it stores: `"10"` for an integer field equals a stored `10`, and a decimal compares by number.
SQLite has no row lock, so the write takes the whole-database write lock instead (`BEGIN IMMEDIATE`), and the compare and the write cannot interleave with another write.

### Conflicts: `409`

When a stored value is not the loaded value, nothing is written.
The answer is `409` with `{"ok": false, "error": {"kind": "Conflict"}, "conflicts": {field: {"loaded", "current"}}}`.
The form shows the current value under each field, and a "Reload the record" link replaces the form with the stored values.
A delete compares every field in `loaded`.
A model with a guard field (an extension that checks a version) gets the stricter check on that field too.

### Field errors: `422`

`{"ok": false, "error": {"kind": "Invalid"}, "fields": {field: [messages]}, "form": [messages]}`.
The server checks, before it writes: the type, a required field, an enum value, `@db.VarChar` length, the shape of a nested type, JSON, and a field that the form does not have.
The database then answers for the rest:

- `@unique`: "A record with this value already exists." on the field.
- A foreign key: "No such record." on the field. A delete of a row that others still refer to is a message of the form.
- A one-column `@check`: "This value is not allowed." on the field.
- A named `@@check`: a message of the form. Set the text with `check_messages` in the config of the resource (`{"check_messages": {"no_banned_title": "That title is not allowed."}}`). Without it the text is "The values break the rule <name>." A name that is not a named `@@check` of the model fails when the site builds.

Any other failure is not a `500` page: it is a form message "The record was not saved: an error occurred. Request id: <id>" (`error.kind` is `ServerError`).
The log holds the cause under the same request id.

### Read-only models

A model with host behavior (hooks, a hook assignment, or a hook default for a client value) or `@@protected_write` is read-only in the admin until the admin runs host code.
A write answers `403` with `error.kind` `ReadOnly` and the reason, `writable` is `false`, and the UI shows the reason on the record and on the form.
Guards and the other extension behavior that has no host code are not host behavior: those models stay writable.

### The audit row

Each write adds one `AdminLogEntry`: `action` (`create`, `update`, `delete`), `resource`, `record_key`, `record_label` (the first text value of the record, at most 200 characters), the actor, and the time.
Its `changes` column holds `{"request_id", "ip", "user_agent", "diff": {field: {"from", "to"}}}`.
A create has `from: null` for each field, and a delete has `to: null`.
The row commits with the write, so a failed write leaves none.
A refused write (`403`) writes a `permission_denied` row instead.
The test client has no service layer, so its `request_id` is `unknown`.

### Forms in the UI

| Path | Page |
|---|---|
| `/admin/{resource}/-/new` | The create form. A URL parameter named like a field fills it, for example `?author_id=1`. |
| `/admin/{resource}/{key}/edit` | The change form with the stored values. |

A widget follows the type unless the config sets one (see [Layout, read-only fields and widgets](#layout-read-only-fields-and-widgets)): a checkbox for a Boolean, a select for an enum, a text area for long text, JSON and lists, a date or time input, and a text input for the rest.
A key field is read-only on the change form.
The save bar stays at the bottom and has "Save" (back to the record), "Save and continue" (stay in the form, on the new record for a create) and "Save and add another" (an empty create form).
The form sends only the fields that changed, and never a read-only field.
"Add" on a to-many tab opens the create form of the child, with the field that points at the parent filled in.
Delete asks first, in a dialog on the record page.

### Layout, read-only fields and widgets

Four resource settings shape the form.
`Site::build` checks every name in them, so an unknown field, section or widget stops the start with an error that names the setting.

```python
admin.register(Book, {
    "form": {
        "create": [{"title": "Basics", "fields": ["title", "author_id"]}],
        "edit": [
            {"title": "Basics", "fields": ["title", "author_id"]},
            {"title": "Internal", "fields": ["version", "notes"], "collapsed": True},
        ],
    },
    "readonly": ["version"],
    "widgets": {"notes": "textarea", "Status": "radio"},
    "form_component": "forms/book.js",
})
```

TypeScript uses the same keys, with `formComponent` for the last one.

- `form` is a list of sections for both forms, or `{"create": [...], "edit": [...]}`. A section has `fields`, an optional `title` and `collapsed`. A collapsed section needs a title and starts closed; a field error inside it opens it. A field of the form that no section names goes in a last section without a title. A field may appear once in a layout, and a layout may name only fields of that form.
- `readonly` lists fields that the admin shows but never writes, on both forms. The form disables them and does not send them. A request that sets one gets `422` with `This field is read-only.`. A read-only guard field (a version counter) still goes in `loaded` and the server still compares it. A required field with no default cannot be read-only, because a create would never fill it: that is a build error.
- `widgets` maps a key to a widget. The widgets are `text`, `textarea`, `number`, `date`, `datetime`, `checkbox`, `select`, `radio`, `json`, `list`, `email` and `url`. A widget has to fit the type of its field: `checkbox` for a Boolean, `select` and `radio` for an enum, `number` for a number, and so on. A wrong fit is a build error.
- The `Admin` setting `widgets` (Python `widgets=`, TypeScript `widgets:`) has the same keys and applies to every resource.

A key is one of these, from the most specific to the least:

| Key | Names |
|---|---|
| `title` or `Book.title` | One field. |
| `Geo.lat` | A subfield of a nested type with `columns` storage. It applies wherever the type is used, at any depth. `Address.geo.lat` names the same subfield from the outer type `Address`, and the outer type wins. |
| `Status` | An enum: every field of it. |
| `Address` | A nested type stored as JSON: the field of it. Only `json` fits, and it is the default already. |
| `String`, `Text`, `Int`, `DateTime` ... | A scalar type: every field of it. |

The field wins over the path, the path over the type, and the type over the default of the column.
A `Model.field` key on a resource names only a field of that resource form: another model, or a field that the form does not hold, is a build error.
At one level the resource setting wins over the site-wide one.

### Custom form component

`form_component` names a JavaScript file that replaces the whole form of one resource.
The server reads the file when it builds the site, at most 512 KB, and a missing or too large file is a build error.
A relative path is relative to the working directory of the process, so give an absolute path, for example `Path(__file__).parent / "book.js"`.
Staff get the module from `GET {prefix}/forms/{name}` (`401` without a session, `403` without read permission on the resource, `404` when the resource has no module), and the UI loads it with a dynamic `import()` from the same origin, so the content security policy stays `script-src 'self'`.
The module is code that every staff user of the admin runs: only put files there that you wrote or reviewed.

The module default-exports a function from React to a component:

```js
export default (React) => function BookForm({ creating, values, errors, saving, save }) {
  const [title, setTitle] = React.useState(values.title ?? "");
  return React.createElement("form", { onSubmit: (e) => { e.preventDefault(); save({ title }); } },
    React.createElement("input", { value: title, onChange: (e) => setTitle(e.target.value) }),
    (errors.fields.title ?? []).map((m) => React.createElement("p", { key: m }, m)),
    React.createElement("button", { disabled: saving }, "Save"));
};
```

| Prop | Value |
|---|---|
| `resource`, `creating` | The resource name, and whether the form creates a record. |
| `values` | The value of each form field as the server sent it (`null` for a new record, or the URL prefill). |
| `errors` | `{fields: {name: [messages]}, form: [messages]}` of the last save. |
| `conflicts` | The current value of each field that changed since the form loaded (`409`). |
| `saving` | True while a save runs. |
| `save(set)` | Sends the fields in `set`. On success the UI goes to the record. It resolves to `false` when the server refuses the values, and `errors` has the reason. |
| `reload()` | Loads the record again. |

On an edit, the UI sends `loaded` for every form field, so the conflict check and the guard fields work as with the built-in form.

### Relation fields and files

A to-one field that holds a foreign key shows a search box, and a many-to-many relation shows chips.
A relation is opt-in: name it in `relations` of the resource config.
It needs `resources` to be set, and the target must be a resource too.

```python
admin.register(Book, {"relations": {"author": {"create": True}, "tags": {}}, "files": {"cover": "media"}})
```

Each entry of `relations` takes `label`, `choices` and `create`.
The key of the entry is the name of the relation (`author`, not `author_id`).

**Autocomplete.**
`GET {prefix}/autocomplete/{name}/{relation}?q=&cursor=&limit=` answers `{"items": [{"value", "label"}], "next"}`.
It pages by keyset on the key of the target, so it stays fast on millions of rows.
`q` searches the `search` fields of the target.
The target must have a label (`label`, or its first plain text column) and a `search`, and the label must be a column of its list.
Without them `Site::build` fails.
The picker applies the `queryset` filter of the target.
It ignores tenancy and the `can` rules: `can` is a flag of one row, not a filter of a read.

**`choices=`.**
`choices` takes a prepared query set of the target.
It limits the picker, and the server checks it again on a write: a value outside it is a `422` for the field.
An update checks only the values that changed.
`choices` combines with the `queryset` of the target.

**Create modal.**
`create: True` adds "New <target>…" to the picker.
It opens a dialog with the create form of the target and fills the field with the new record.
The form keeps the values that the user typed.
The dialog has no inlines and no chips.
The button shows only for a user who may create a target record. `Site::build` fails when the target does not allow create or is read-only.

**Many-to-many chips.**
A relation through a join model shows chips on the form.
`GET {prefix}/links/{name}/{key}/{relation}` answers `{"items", "truncated"}` for the saved links: the target `queryset` applies, and the list stops at 200.
A write takes `"links": {relation: {"add": [keys], "remove": [keys]}}` next to `set`.
The links and the fields commit in the one write transaction, with one audit row (`links.<relation>` in `diff`).
A save that fails leaves the links as they were.
A write has at most 500 keys on each side.
The join model must be writable and must have no other required field.

**Upload and download.**
`files` maps a field to a storage of the [file storage](../extensions/file-storage.md) registry.
Pass the registry as `Admin(storage=registry)`.
`POST {prefix}/files/{name}/{key}/{field}` takes a `multipart/form-data` body with one `file` part and answers `{"ok", "key", "changed", "value"}`.
It is the only multipart route.
The CSRF token, the `Origin` check and the permission to change the record apply, and the body is at most 25 MiB.
A field in `readonly` takes no upload: the answer is a 422.
An upload checks the permission and the record first, then stores the file with no transaction open, then writes the reference and the audit row in a new transaction that checks again.
A failure after the storage call leaves the stored file unused.
`GET {prefix}/files/{name}/{key}/{field}` reads the file from the storage and streams it with `Content-Disposition: attachment` and the stored content type.
The browser never sees a storage URL.
A new record has no key yet: the form says to save first, then upload.
A replaced file stays in the storage.

## Routes and answers

The path prefix is a config value, and the default is `/admin/api`.

| Route | Answer |
|---|---|
| `GET {prefix}/resources` | The `brand`, the navigation groups (`nav`), and the resources that the user may read. Each resource has a `title`, its `fields`, its `key` fields, its columns (with `title`, `widget` and `choices`), order, page size, sort columns and search fields. It also lists the permission names that the user holds. |
| `GET {prefix}/resources/{name}?cursor=&before=&last=&page=&limit=&sort=&q=&f.*=&n.*=` | One page: `columns`, `rows`, `keys`, `next` and `prev`. See [Lists](#lists). |
| `GET {prefix}/counts/{name}?q=&f.*=&n.*=&exact=` | `{"count", "capped"}`. See [Count](#count). |
| `GET {prefix}/resources/{name}/{key}/{relation}`, `GET {prefix}/counts/{name}/{key}/{relation}` | The list and the count of a to-many tab. See [To-many tabs](#to-many-tabs). |
| `GET {prefix}/resources/{name}/{key}` | One record: `fields`, `values` and `links` to the to-one targets that the user may read. A key is encoded as in [The UI](#the-ui). |
| `POST`, `PATCH`, `DELETE` on `{prefix}/resources/{name}[/{key}]` | Create, change and delete one record. See [Writes and forms](#writes-and-forms). |
| `GET {prefix}/autocomplete/{name}/{relation}?q=&cursor=&limit=` | One page of the picker: `items` and `next`. See [Relation fields and files](#relation-fields-and-files). |
| `GET {prefix}/links/{name}/{key}/{relation}` | The records that a many-to-many relation links: `items` and `truncated`. |
| `POST`, `GET {prefix}/files/{name}/{key}/{field}` | Upload a file (multipart) and download it (attachment). |
| `GET {prefix}/forms/{name}` | The module of a custom form, as `text/javascript` with `cache-control: no-store`; `401`, `403` and `404` as above. See [Custom form component](#custom-form-component). |

A row is an array that follows `columns`.
`next` is a cursor for the next page, or `null` on the last page.
BigInt and Decimal are JSON strings.
A date and a time are ISO 8601 with an offset.

| Status | When |
|---|---|
| 401 | The request has no user. |
| 403 | The user lacks a permission (see [Permissions](#permissions)). The request writes an audit row. |
| 404 | The resource, the path or the record is unknown. |
| 400 | The cursor, the limit, the sort, the search term, the filter name or the key is not valid. |
| 409 | A write found a stored value that is not the loaded value. |
| 422 | A filter value has the wrong type, or a named filter has no such choice. A write has a field error or a message of the form. |
| 413 | An upload is larger than 25 MiB. |
| 504 | The statement timeout cancelled a statement. |

An error body is `{"ok": false, "error": {"kind": "...", "message": "..."}}`.

## The UI

The server also serves a single-page app at `/admin`.
It is React and TypeScript with TanStack Query, TanStack Table and Radix primitives, and it speaks only the routes above.
The build output is committed in `crates/admin/ui/dist`, and the `monorm-admin` crate embeds it.
So the wheel and the npm addon carry the UI, and no other packaging step exists.
The files are static: a `GET` of a UI path needs no session, and a reply carries its own `Content-Security-Policy` (`script-src 'self'`).
A hashed file in `assets/` has `Cache-Control: public, max-age=31536000, immutable`, and `index.html` has `no-store`.
The UI path `/admin` is a constant; only the API `prefix` is configurable, and `prefix` cannot be `/admin`.

| Path | Page |
|---|---|
| `/admin/-/login`, `/admin/-/password` | Sign in, and change the password. Logout is in the header menu. |
| `/admin/{resource}` | The list. The first column links to the record. Search, filters, sort, page and page size are in the query string, so a reload, a link and the back button return to the same view. |
| `/admin/{resource}/{key}` | The record: its fields, a link for each to-one relation, and a tab for each to-many relation (`?tab={relation}`). It has Edit and Delete. |
| `/admin/{resource}/-/new`, `/admin/{resource}/{key}/edit` | The forms: see [Forms in the UI](#forms-in-the-ui). |

A key is stable and easy to bookmark.
Each key field is percent-encoded (only `A-Z a-z 0-9 - . _ ~` stay as they are), and a composite key joins the fields with `,` in key order.
A date and time shows in the browser time zone.
The header menu can switch to UTC.
A key of `.` or `..` and a Decimal key have no page URL: `admin.url()` raises for them.
The first `admin.url()` call builds the whole site, so it raises on any config error.
A `Date` has no time of day, so it shows as the server sent it.

To build the app, run `npm install && npm run build` in `crates/admin/ui` and commit `dist`.
`npm run size` prints the first-load size against the budget of 300 KB gzip.
CI rebuilds the app and fails when the committed `dist` differs (Node 24).

### Brand, groups and titles

`brand` sets the name, logo, colors, header links and environment label.
The logo is a path that starts with `/`, or an `https` URL (the CSP allows no other image source).
`groups` is the order of the navigation groups.
A resource names its group with `group`, and its title with `title`.
A group that `groups` does not list is an error, and so is an unknown brand key.
The metadata is built once per process.

```python
admin = Admin(
    brand={"name": "Acme", "logo": "/static/logo.svg", "colors": {"primary": "#0b5fff"},
           "links": [{"label": "Site", "url": "https://acme.example"}], "environment": "staging"},
    groups=["Content", "Access"],
)
admin.register(Post, {"group": "Content", "title": "Articles"})
```

```ts
const admin = new Admin(registry, {
  brand: { name: "Acme", environment: "staging" },
  groups: ["Content", "Access"],
});
admin.register(Post, { group: "Content", title: "Articles" });
```

The sign-in page shows no brand, because the metadata route needs a session.

### `admin.url`

`admin.url(Model, key)` gives the path of the record page, such as `/admin/Book/7`.
The key is one value, the key values in order, or an object of key fields.
A model that is not registered, or a key of the wrong shape, is an error.

```python
admin.url(Book, 7)                       # "/admin/Book/7"
admin.url(Membership, {"org": 1, "user": "a,b"})   # "/admin/Membership/1,a%2Cb"
```

```ts
admin.url(Book, 7n);
```

Both languages also export `url` at module level for the default admin.

### Browser smoke test

`crates/admin/ui/e2e` holds the first browser suite of the repository.
It starts a real `python -m monorm admin` process on SQLite (it makes a temporary project, migrates, and seeds a user and two rows), signs in, sorts a list, searches it, opens a record, adds, changes and deletes a record through the forms, opens a section layout and a custom form, and fails on any console error, CSP violation or failed request.
The driver is Playwright (`playwright` 1.65.0, pinned) with `node:test`.
Playwright downloads a browser, so the install is not part of the default `npm install` of any package.
Run it from a checkout, with the `monorm` wheel installed in the active Python (`maturin develop`):

```
cd crates/admin/ui/e2e
npm install
npm run install-browser     # downloads Chromium, once
npm test
```

`MONORM_PYTHON` names another Python than `python`.
In a source checkout the built-in apps import from the repository, so the test sets `PYTHONPATH` to the repository root.

## Start the admin

`monorm admin init` writes three things: the `[admin]` settings, an admin module that registers every model, and the `import "app:..."` line in `schema.prisma`.
It refuses to run when the settings already have an `admin` section or when the admin module exists.

```
python -m monorm admin init [--models MODULE]
npx monorm admin init [--models MODULE]
```

The Python module is `admin.py`, with `app = "admin:admin"`, and it imports the models from `models` (run `monorm generate` first, or pass `--models`).
The TypeScript module is `admin.ts`, with `app = "./admin.ts:admin"`, and it loads the schema itself unless you pass `--models`.
The schema import is `import "app:monorm-admin" (prefix: "admin_")` in Python and `import "app:admin" (prefix: "admin_")` in TypeScript.
It adds the tables `admin_adminlogentry` and `admin_userpreference`.
The admin schema imports nothing from `app:auth`, so the admin works with any user model.

After `makemigrations` and `migrate`, start the server with no flags:

```
python -m monorm admin
npx monorm admin
```

The standalone `monorm` binary cannot start the admin, because the admin module and the `setup` hook are your code.
It stops with a message that names the two commands above.

The server binds a socket only in this command.
`Admin.test_client` and `testClient` never open a port.

### Settings

The settings come from the same file as every other `monorm` command: `monorm.toml`, `[tool.monorm]` in `pyproject.toml`, or the `"monorm"` key in `package.json`.

```toml
[admin]
app = "admin:admin"          # module:name of the Admin object (required)
setup = "myproject.boot:setup"  # optional hook, sync or async
host = "127.0.0.1"
port = 8000
timeout = 30                 # seconds, for every request
db_settings = { "app.tenant" = "acme" }
```

| Key | Default | Meaning |
|---|---|---|
| `app` | none | Where to import the `Admin` object. A TypeScript value is a file path, such as `./admin.ts:admin`. |
| `setup` | none | A function that runs once, after the app import and before the server starts. |
| `host`, `port` | `127.0.0.1`, `8000` | The address. Port `0` picks a free port. |
| `timeout` | `30` | The request time limit in seconds. |
| `db_settings` | none | PostgreSQL settings that every admin transaction sets (see below). |
| `idle_timeout` | `1800` | Seconds without a request that end a session. |
| `session_timeout` | `43200` | Seconds after login that end a session in any case. |
| `origins` | none | Extra origins that a write may come from, such as `"https://admin.example.com"`. |
| `trusted_proxies` | none | Peer IP addresses whose `X-Forwarded-For` the login limits believe, such as `"10.0.0.1"`. |

`MONORM_ADMIN_HOST` and `MONORM_ADMIN_PORT` override `host` and `port`.
An unknown key under `[admin]` is an error that names the key and the settings file.

The `setup` hook connects the database, the extensions and the file storage, with the same calls as any other `monorm` program.
When the hook did not connect a database, `monorm admin` connects with `MONORM_DATABASE_URL`.
If neither exists, it stops with `no database`.

The exit code is 0 after a clean stop, 2 for a wrong setting, and 1 for any other failure.
A failure includes a database without the admin tables: the message names the missing tables and says to run `makemigrations` and `migrate`.

### Database settings and row-level security

`db_settings` is a table of text values.
On PostgreSQL, the admin runs `set_config(key, value, true)` at the start of each request transaction for every entry.
A row-level security policy can read the value with `current_setting('app.tenant')`.
The value is static for the whole process.
SQLite has no such settings: the admin ignores the key and writes one message at startup.

### Health, logs and shutdown

| Route | Answer |
|---|---|
| `GET /healthz` | 200 when the process runs. It does not touch the database. |
| `GET /readyz` | 200 when `SELECT 1` answers within 2 seconds. Otherwise 503. It also answers 503 after a stop starts. |

Both routes sit at the root, outside the path prefix, and need no user.

Each request writes one JSON line to stderr, with a request id, the method, the path, the status and the time.
The response carries the same id in the `x-request-id` header.
The server takes the id from the request header when it has 1 to 128 printable ASCII characters. Otherwise it makes a new UUID.

The host stops the server on SIGTERM and SIGINT.
The server answers 503 on `/readyz`, stops to accept connections, lets the running requests end, and then the process exits with code 0.

### Timeout

One middleware applies the `timeout` setting to every request.
A request that is over the limit gets status 504 and the error kind `Timeout`.
The admin then rolls back the transaction of that request explicitly, so a cancelled request never leaves a transaction open.

The two languages differ in what the cancel stops:

- Python: the timeout cancels the coroutine of the request.
- Node: a Promise cannot be cancelled. The work of the request runs on, and the admin discards its result.

A Node `setup` hook or a long query in Node code can therefore use time after the client has its 504.

### Several processes

The admin keeps no session store, cache or queue: the session is a signed cookie (see `MONORM_ADMIN_SECRET`).
Only the failed-login counters are in the process. The revocations are rows in the database, so every process sees them at its next renewal.
Give every process the same `MONORM_ADMIN_SECRET`. Run as many processes as you need on one database, each with its own port, behind any load balancer.

### Kubernetes

Use the two health routes as probes, and let Kubernetes send SIGTERM.

```yaml
livenessProbe:  { httpGet: { path: /healthz, port: 8000 } }
readinessProbe: { httpGet: { path: /readyz, port: 8000 } }
```

Set `host = "0.0.0.0"` in the container, because the default `127.0.0.1` is not reachable from outside the pod.
Make `terminationGracePeriodSeconds` longer than `timeout`, so a request in progress can end before the kill.
The admin has no drain delay: if your load balancer needs a pause after SIGTERM, add a `preStop` sleep.
