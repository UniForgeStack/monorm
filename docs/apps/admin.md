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
A wrong config raises `AdminConfigError`, and wrong `[admin]` settings or a missing secret raise `AdminSettingsError` (a subclass of it in Python, a sibling class in TypeScript).
Both have a `code` such as `config.unknown_model`, `config.invalid_value` or `settings.missing`: the part before the first dot is the class.
The text names the option in the spelling of the language: `page_size` in Python and `pageSize` in TypeScript.
The page size is 1 to 200, and the default is 25.
An order column must be a plain column: not nullable, not an array, and not Json, Decimal or an enum.
The primary key is always the last order column, so a keyset page has a stable order.

## Default resources

With no `register` call, every concrete model is a read-only resource.
After the first `register` call, only the registered models are resources, plus the [built-in resources](#built-in-resources-roles-and-the-audit-log) of the admin and auth apps.
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

A foreign key column has a label for each row: `labels[i]` is `{"author_id": "Ada"}` for row `i`.
The same list statement reads the labels, so the statement count does not change.
The label is the first plain text column (`String` or `Text`) of the target list that is not part of the key and has no `see` permission.
A column has no label when the user may not read the target, when the target has a queryset or read defaults, when no column fits, or when the foreign key has more than one column.
Then the UI shows the raw value.
The `columns` of the resource carry `ref: {"resource": <target>}` for each column with a label, and the resource has `name_column`: the label of the column that names a record (or `null`).
The filter chips of the UI use `name_column`, so a chip and a list label show the same name.
The title of a foreign key column is the name of its relation (`Author`), unless `labels` gives one.

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

### Date drill-down

`date_hierarchy` (Python) and `dateHierarchy` (TypeScript) puts a bar over the table that narrows the list by year, then by month, then by day:

```python
admin.register(Order, {"list": {"columns": ["number", "placed"], "date_hierarchy": Order.placed}})
admin.register(Order, {"list": {"columns": ["number", "placed"], "date_hierarchy": {"field": Order.placed, "counts": False}}})
```

The field is a date or datetime field, or one field of a to-one relation (`Order.customer.joined`), and one hop is the limit.
Another type, or an unknown field, is a build error.
The build adds the field to `filters` when it is missing.
`check()` warns, in `Site.warnings`, when no index leads with the field.

A click on a period writes the range filter of the field (`f.placed:gte` and `f.placed:lt`, or `:lte` with the last day for a date field), so the view is in the URL and the other filters and the search still apply.
The bar shows the years first.
After one year it shows the months, after one month the days, and after one day only the breadcrumb (`All dates › 2025 › March`).
Another range shows days when it spans 31 days or fewer, and months otherwise.
The bar shows at most 50 years: older years are one "older" period.
A period with no row is hidden.
`counts: False` shows no number, and the bar then runs one statement for the first and last value.

The route is `GET {prefix}/dates/{resource}`.
It takes the query of a list (`q`, `without`, the filters) and `edges`.
Without `edges` the answer is `{"min": ..., "max": ...}` (the first and last value, or `null`).
With `edges` it is `{"counts": [...]}`, one count for each period between two edges.
`edges` is a comma list of 2 to 61 values in strictly increasing order: RFC 3339 for a datetime field (percent-encode the `+` of an offset) and `YYYY-MM-DD` for a date field.
A bad list answers `400`, and a field that the user may not see answers `403`.
Each request runs one statement.
The metadata of the resource has `date_hierarchy` with `field`, `counts` and `kind` (`null` without the key).

### Display

A bool column shows an icon: a check for true, a cross for false and a dash for `NULL`, each with a text label (`Yes`, `No`, `Empty`) for screen readers.
`badges` shows the value of a column as a coloured badge, and the same badge shows on the detail page:

```python
admin.register(Ticket, {"list": {"columns": ["title", "status", "urgent"], "badges": {Ticket.status: {"open": "green", "closed": "grey", "stuck": "red"}}}})
```

The colours are `grey`, `blue`, `green`, `yellow`, `red` and `purple`.
Each has a light and a dark variant that keep the text at 4.5:1 contrast or more, and the badge needs no inline style.
A value that the map does not name shows grey, and `NULL` shows no badge.
A badge on a bool column replaces the icon.
The keys are text (`true` and `false` for a bool, the digits for an int).
The build refuses a column that is not in the list, a column with a view, a colour outside the palette, a type that has no badge (such as a datetime), and an enum value that does not exist.
The metadata of the column has `badge`, the map.
In TypeScript the key is `badges` too, with a column as the key.

A computed column sorts when it has `order_by`, a field of the model or an expression (`orderBy` in TypeScript).
It also makes a `function` column sortable.
The build refuses an array or enum type, a relation that can be `NULL`, and a field with `write_sql`.
A user who may not see the `order_by` field gets no sort header on the column.
The primary key stays the last order column.

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

`can={"change": expr, "delete": expr}` (`can: { change, delete }` in TypeScript) are conditions of the row. They may read the signed-in user's key through `param("user")`.
The list and detail queries select them as flags, with no host call.
Each row of `GET {prefix}/resources/{name}` has an entry in `flags`, the detail has a `flags` object, and the resource metadata has `can`, the names of the rules.
The UI disables the button of a rule that the record does not allow.
The resource accepts `change` and `delete` as its rule names. Permission rules reject scope values and list parameters.
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
An admin read sets no `scope` value, and a read default that reads one (`@@query.filter("tenant", shop_id == scope.shop)`) is off in every admin read.
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

### CSV export

`GET {prefix}/export/{name}` sends the list as a CSV file (`text/csv; charset=utf-8`, `content-disposition: attachment; filename="<name>.csv"`).
It takes the list state of the query string: `q`, `f.*`, `n.*` and `sort`. It ignores `cursor`, `before`, `last`, `page` and `limit`.
The `queryset` filter of the resource applies, so a user exports only the rows that the user may list. The user needs read access.
The first row holds the column titles of the list. The last column is the key (`Id`). Computed and aggregate columns are in the file.

The server reads the rows with the keyset pages of the list, 1,000 rows each, and sends each page as soon as Rust has encoded it.
The Rust allocator holds a flat peak: a test measures about the same peak at 10,000 and at 100,000 rows (SQLite, plain columns). The test clients buffer the whole body, so the memory of the host process is not measured.
Each page after the first runs in its own short transaction, so the pages are not one snapshot: a row that changes its sort value during the export can be missing or appear twice.
With a search (`q`), the export holds at most 1,000 rows: the search finds 1,000 keys, as the list does, and the file has no warning.
The server reads the first page before it sends the head. A bad request (`400`, `422`, `404`, `403`, `401`) is therefore a normal JSON error, not a cut file.

Cells follow these rules:

- A cell with a comma, a quote, a CR or an LF is in quotes, and a quote inside is doubled. A line ends with CRLF. The file has no BOM.
- A text cell that starts with `=`, `+`, `-`, `@`, a tab or a CR gets a `'` in front, so a spreadsheet does not run it as a formula. A BigInt or Decimal cell that is a plain number (an optional `-`, digits, an optional fraction) is never changed. Any other text in such a column, for example from a host function, gets the `'` like a text cell.
- A null is an empty cell. A date and a time are ISO 8601, as in the list.

The `timeout` setting limits the head of the answer only, so it does not cut a long export.
An export has its own idle timeout (default 30,000 ms; `0` turns it off). The stream ends with an error when the client takes no data for that time, or when the read of one page takes longer. It is a site setting of the Rust crate (`export_idle_timeout`); Python and TypeScript cannot change it.
When a page fails after the head, the body ends with an error, so the file never looks complete, and the server writes one `error` log line `export cut` with the request id.

Each export writes one `AdminLogEntry` with `action` `export`, the `resource`, and `changes` `{"query": "<the raw query string>"}`, so search text is in the log. The row commits before the first byte, so it has no row count and no outcome: a cut export keeps its row. A refused export (`403`) writes a `permission_denied` row, as for a write.
The test client reads `/export/` paths like any other path: `client.get("/admin/api/export/Order?f.status=new")` gives `status`, `header(...)` (`headers` in Python) and `text`.

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
| `/password` | `POST` | `{"old_password", "new_password"}`. The user changes their own password. A weak new password gives `400` `WeakPassword`. |
| `/logout` | `POST` | Ends the session. |

The password change checks the new password with `validate_password` of `app:auth`, so the rules of `[auth] password_rules` apply (see [the auth app](auth.md#password-rules)).
A weak password gives `400` with `{"ok": false, "error": {"kind": "WeakPassword", "message", "errors": [{"code", "message", "params"}]}}`.
The `message` joins the English messages, and the page shows it.
The password stays as it was, and the `password_change_failed` row of the audit holds the rule codes in `changes.codes`, never the password.

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

### Single sign-on (OIDC)

An `[admin.oidc]` section adds a "Sign in with single sign-on" link to the login page.
The flow is the OpenID Connect authorization-code flow with `state`, `nonce` and PKCE (`S256`).
The server checks the signature, `iss`, `aud`, `exp` and `nonce` of the ID token against the keys of the provider.

```toml
[admin.oidc]
issuer = "https://idp.example.com"
client_id = "admin"
roles_claim = "groups"                   # default "roles"; a dotted path such as "realm_access.roles" works
scopes = ["groups"]                      # added to "openid profile email"
hd = "example.com"                       # optional: the only allowed Google Workspace domain
```

```text
MONORM_ADMIN_OIDC_CLIENT_SECRET=...      # the client secret is never a setting
```

| Key | Default | Meaning |
|---|---|---|
| `issuer`, `client_id` | none | Required. The issuer URL, and the client of this admin at the provider. `http://` is allowed only for a loopback issuer. |
| `roles_claim` | `roles` | The claim that lists role names. A string or a list of strings. |
| `scopes` | none | Scopes added to `openid profile email`, such as the scope that carries the roles claim. |
| `hd` | none | The ID token must hold this `hd` claim. The server checks it, not the provider. |
| `username_claim` | `email` | The claim that is the `username` of the user. |
| `discovery_url` | `<issuer>/.well-known/openid-configuration` | Another discovery document. |
| `public_url` | the `Host` header | The origin that the provider sees, when a proxy changes it. |

The callback URL to register at the provider is `<public origin><base_path>/auth/callback`, for example `https://admin.example.com/admin/auth/callback`.
`GET <base_path>/auth/start` starts the flow. A short-lived signed cookie holds `state`, `nonce` and the PKCE verifier, and the callback deletes it.

At the callback the admin:
- Finds the user by `username` (the `username_claim`). A first login creates the user with a random password that nobody can use.
- Refuses an inactive user.
- Sets the role membership of the user to the roles that the claim names and that exist in the `Role` table: it adds the missing rows and removes the others. A claim with no matching role gives no grant.
- Writes a `login` row with `changes` `{"method": "sso", "ip", "request_id", "roles", "created"}`, or a `login_failed` row with `changes.reason`.

A failed login sends the browser to `<base_path>/?sso_error=<word>`, and the login page shows the message.
The words are `inactive`, `domain`, `unavailable`, `denied`, `too_many` and `failed`.
The password login stays; `GET /login` has `"sso": "<base_path>/auth/start"` when OIDC is on.
The callback counts each attempt on the limit of 20 for one IP address (see above), and a success gives the count back.

An organization that moved from allauth fits as an `idp` provider: set `issuer` to it, add the scope of its roles claim to `scopes`, and set `hd` for a Google Workspace domain.

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

A site that registers a resource with `set_password: true` checks the user on every authenticated request, not only at the renewal, so a changed password ends a session at its next request. This costs one short transaction and one `auth.user` host call for each request, for every user of the site.

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

The cookies are `__Secure-monorm-session` (`HttpOnly`) and `__Secure-monorm-csrf`.
Both are `Secure`, `SameSite=Lax` and `Path=<base_path>/`, for example `Path=/admin/`: see [Base path and cookie scope](#base-path-and-cookie-scope).
A browser accepts a `__Secure-` cookie only over HTTPS, or from `localhost`: serve the admin behind TLS.
A `__Host-monorm-*` cookie from an older version is ignored: the user signs in once again.
The session ends after `idle_timeout` without a request (default 30 minutes) or after `session_timeout` (default 12 hours).

The API takes JSON only: a write with another content type answers `415`.
A script or API client sends every write (`POST`) with:
1. The CSRF token: the value of the `__Secure-monorm-csrf` cookie, in the header `X-CSRF-Token`.
2. An `Origin` (or `Referer`) header that equals the `Host` of the request, or an entry of `origins`.

Without them the answer is `403` (`BadCsrfToken` or `BadOrigin`).
The `Origin` check compares the host and the port, not the scheme.
Behind a proxy that ends TLS, the scheme of the request differs from the scheme of the browser, so a scheme check would refuse real users.
Set `origins` where the browser `Origin` differs from the `Host`.
The login itself needs them: send `GET /login` first to get the CSRF cookie.

```sh
curl -c jar -s http://localhost:8000/admin/api/login > /dev/null
token=$(awk '$6=="__Secure-monorm-csrf"{print $7}' jar)
curl -b jar -c jar -H "X-CSRF-Token: $token" -H "Origin: http://localhost:8000" \
  -H "Content-Type: application/json" -d '{"username":"ann","password":"pw"}' \
  http://localhost:8000/admin/api/login
```

Every API response has `Content-Security-Policy` (strict), `X-Frame-Options: DENY`, `Referrer-Policy: same-origin`, `X-Content-Type-Options: nosniff` and `Cache-Control: no-store`.
A UI file has the same headers, except `Cache-Control`: see [The UI](#the-ui).
A body over 16 KiB answers `413`.

### Test client with a login

`test_client(db, user=...)` and `testClient(db, { user })` need no login: they use the in-memory path.
Give a `secret` (and optional `settings` with `idle_timeout`, `session_timeout`, `origins`, `trusted_proxies` and `oidc`, and an `ip`) for a client that runs the full login:

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
For `oidc` the `client_secret` is a key of the settings object (not of `monorm.toml`), and the issuer may be a fake provider on a loopback port.
A `user` client and a `secret` client do not mix.
A wrong secret raises at the first request.

### Host functions and the binding

The login runs `auth.authenticate`, and a password change runs `auth.change_password`.
Each is an async function of `(database, args)`, and it must use the `database` that it gets.
The service runs it inside `Database.bind_request` (Python) or `db.bindRequest` (Node), so it sees the transaction of the request.
A statement on another handle waits for a connection of its own.

`bind_request` and `bindRequest` are public:

```python
with db.bind_request(tx, scope={"user": 1}):
    ...  # every statement of `db` uses `tx`; the caller commits or rolls back
```

```ts
await db.bindRequest(tx, { scope: { user: 1 } }, async () => { /* ... */ });
```

- `tx` is a transaction that something else opened and will end.
- A `transaction()` block in the binding is a savepoint.
- `scope` goes through the same scope context as `scope()`: it checks the keys. The statements publish the same `scope.<key>` settings as `scope()`: none in this release, since no database object reads a key yet.
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
An action that is a plain string is a permission name only. An [action](#actions) with a function has a button and a route, and it names its permission as `<label>.<action>_<resource>`.

```python
site = Admin(permissions="model", label="staff")
site.register(Order, {"allow": ["read", "update"], "actions": ["refund"]})
```

`allow=` limits a resource for every user, superusers too.
It takes `read`, `create`, `update` and `delete`.

A request without a grant gets `403` and one audit row (action `permission_denied`).
The resources and actions that the user cannot use are absent from `GET {prefix}/resources`.
A field follows the page level unless a `see` or `change` binding names it: see [Field permissions](#field-permissions).
The audit row says why in `changes`: `{"needs": [...]}` (one of these names is enough) or `{"reason": "allow"}` (`allow=` excludes read).

### Field permissions

`see=` and `change=` bind a named permission to a field.
A field with no binding follows the page level.

```python
site.register(Product, {
    "list": {"columns": ["name", "code", "cost"], "search": ["name"], "filters": ["cost"], "sortable": ["cost"]},
    "see": {"cost": "see_cost"},
    "change": {"price": "change_price"},
})
```

- `see` keys are column or detail labels. A user without the name cannot see the field.
- `change` keys are form labels. A user without the name sees the field but cannot write it.
- A bound name is a field permission of the resource (`admin.see_cost`), so `sync-permissions` creates it. Use `page` or `model`: with `access` the build fails.
- A key field cannot be bound.
- TypeScript uses the same keys: `see: { cost: "see_cost" }` and `change: { price: "change_price" }`.

Rust applies the bindings in one place, before it builds metadata and queries.
A field that the user cannot see leaves the columns, the detail, the tabs, the search, the filters, the sort, the export, the autocomplete labels, the inlines, the metadata and every response, so it cannot leak through a side door.
A field that the user may see but not change shows read-only in the form.

A forbidden request gives an error, not a silent drop:

- A filter or a sort on a field that the user cannot see gives `400`.
- A write to a field that the user can see but not change gives `422` with "This field is read-only."
- A write to a field that the user cannot see gives `422` with "This field cannot be written."

The service builds the prepared list query and the metadata once for each granted set, not for each request.
A user who holds every bound name, and a resource with no binding, use the page and build nothing.
Measured on a loaded development machine (debug build, `cargo test -p monorm-admin --test field_permissions -- --nocapture`): the first request of a new granted set builds its view in about 0.2 ms, and each later request of that set looks it up in about 2 µs.
The number of views is at most 2^n for n bound names of one page.

These paths do not scan a field by its content, so bind the field by its own label:

- A computed or aggregate column that reads a hidden field.
- A path column such as `author.name`, which a `see` binding on `name` does not hide.
- A named filter or `search_with` that the developer wrote.
- The `changes` of an audit row, which can hold a value that a later reader may not see.

### Built-in resources: roles and the audit log

A schema that imports the auth and admin apps gets four more resources on every site, after the registered ones: `AdminLogEntry`, `Permission`, `Role` and `UserRole`.
A site without a `register` call lists every model anyway.
A `register` call for one of them keeps its settings.
They follow the permission strategy of the site, so each has its own names (`admin.read_role` and `admin.manage_role` in `page`).
- `Role` holds the role name and a `permissions` picker. It offers only the names that `admin.permissions` lists, labeled with the text of the `Permission`, and not any other row of the table.
- `UserRole` is the role membership of a user.
- `Permission` searches on `label`. `sync-permissions` creates its rows.
- `AdminLogEntry` is the audit log. It is read-only for everyone, superusers too: a write gives `403`. A reader who cannot see a field of the logged resource (see [field permissions](#field-permissions)) gets no value of that field in `changes`.

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
| `PATCH {prefix}/resources/{name}` | `{"rows": [{"key", "set", "loaded"}]}` | `200` with `{"ok", "keys", "changed": {key: [names]}}`. See [Edit in the list](#edit-in-the-list). |

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

### Host behavior: the write goes through the bridge

A model with host behavior (hooks, a hook assignment, or a hook default for a client value) writes through the host function `admin.write`.
The write is a query-set write: the host selects the row with the `queryset` filter and the `can` rule, and the model code (hooks, field adapters) runs.
Rust checks the record first (`404`, and `403` for a `can` rule), and the host answers with the key.
A host route needs the [host binding](#host-functions-and-the-binding).
Without it, a write answers `403` with `error.kind` `ReadOnly` and the reason "this write needs host code, and this admin has no host".
Relation chips (`links`) do not go through host code: a host route refuses them.
Guards and the other extension behavior that has no host code are not host behavior: those models stay writable on the plain path.

The admin hides operations blocked by unconditional [write rules](../extensions/write-rules.md). A direct request for a blocked operation returns `403 ReadOnly`. A fully protected model shows the reason on the record and form.
A resource opts in with `allow_protected_writes=True` (`allowProtectedWrites: true` in TypeScript).
The audit row records the opt-in as `allow_protected_writes: true`.

### Services

A service owns the write of one operation.
It is a plain function: a unit test calls it with a `Context` and no admin.

```python
from monorm import admin
from monorm.errors import VersionConflict

async def create(ctx, values):
    async with ctx.database.transaction():
        post = await Post.objects.using(ctx.database).insert(**values, author=ctx.user["key"])
        await ctx.audit(post, changes={"reason": ctx.inputs["reason"]})
    return post

async def update(ctx, qs, changes):
    async with ctx.database.transaction():
        row = await qs.first()
        if row.title != ctx.loaded["title"]:
            raise VersionConflict("changed")
        await qs.update(**changes)

async def delete(ctx, qs):
    await qs.delete()

admin.register(Post, {"create": create, "update": update, "delete": delete, "inputs": [admin.input("reason", type="textarea", required=True)]})
```

```ts
import { admin, type Context } from "monorm/admin";

admin.register(Post, {
  create: async (ctx: Context, values) => { /* ... */ },
  update: async (ctx: Context, qs, changes) => { /* ... */ },
  delete: async (ctx: Context, qs) => { /* ... */ },
  inputs: [admin.input("reason", { type: "textarea", required: true })],
});
```

- `create(ctx, values)`, `update(ctx, qs, changes)` and `delete(ctx, qs)`. For `update` and `delete`, `qs` is a one-row query set that holds the key, the `queryset` filter and the `can` rule. A row that the query set does not find is a `404`.
- The service opens its own transaction and does its own version check against `ctx.loaded`. A service route skips the Rust `409` check.
- `VersionConflict` gives `409` with `conflicts` and the current values.
- The ORM `ValidationError` gives `422` with the field errors and the form messages. The form shows them as for a plain write.
- Any other error gives `500` with the request id.
- A resource with a service for only some operations uses the plain path (or `admin.write`) for the others.

`ctx` holds:

| Name | Value |
|---|---|
| `user` | The user of the admin as a plain mapping (no password hash). |
| `request_id` | The id of the request. It is empty under the test client. |
| `loaded` | The values that the form showed, by field name. Empty on a create. |
| `inputs` | The values of the `admin.input` controls. |
| `database` | The database of the request. |
| `audit(record=None, *, action=None, changes=None)` | Writes the audit row inside the service transaction, with the inputs (a secret input as a mask). Rust fills the `record_label` after the service returns. |

Without a `ctx.audit()` call, Rust writes the audit row after the service returns.
With a call, the row commits with the service transaction and Rust adds none.

The default writer (`admin.write`) runs in the transaction of the request.
It compares the loaded values, writes the record and writes the audit row in that one transaction.
On PostgreSQL the compare holds a row lock (`SELECT ... FOR UPDATE`).
SQLite has no row lock: every transaction holds the whole-database write lock (`BEGIN IMMEDIATE`), so a second writer waits for the first.
A failed write leaves no audit row, a good write leaves exactly one, and two staff users who save at the same time cannot overwrite each other: the second one gets `409`.
Rust adds the label and the diff to that row after the write and writes no second row.
An action writes its audit row (`ctx.audit()` and the summary row) in the transaction of the action: both roll back together when the action fails.

A service that does not call `ctx.audit()` still gets its audit row from Rust after the service commits, in a new transaction.
If that insert fails, the staff user reads "The record was not saved", although the record is saved.
A service owns its check and its transaction: use `ctx.audit()` and a version check in the service when this matters.

### Inputs

`admin.input(name, type=, label=, required=, help=, choices=, secret=)` adds a control to the form that is no field.
The `type` is `text`, `textarea`, `number`, `checkbox` or `select` (`select` needs `choices`).
The UI shows the controls on the create form, the edit form and the delete confirmation.
The value lives for one request, and only the service reads it as `ctx.inputs[name]`.
The audit row stores each input in `changes.inputs`. With `secret=True` it stores the mask `***` in place of the value, for the admin's own row and for the row of `ctx.audit()`. Without `secret=True` it stores the value as given, and staff with access to the audit log read it.
A required input that is empty gives a `422` on the control.
An input on a resource without a service is an `AdminConfigError` of `Site::build`, and `admin.check()` raises it without a database.
In the API, `inputs` is an object in the body of the `POST`, `PATCH` and `DELETE` routes, and each resource lists its `inputs` in `GET {prefix}/resources`.

### Sync services in Python

A sync function runs in a thread, so it cannot use the async database API on the bound transaction.
`Site::build` (and `admin.check()`) rejects a sync service unless it is marked `@admin.threaded`.
A threaded service runs its async database calls through `ctx.run(awaitable)`.
A create gets plain values. An update gets a query set and the values, and a delete gets a query set, so its async methods need `ctx.run` too:

```python
@admin.threaded
def update(ctx, qs, changes):
    row = ctx.run(qs.first())
    ctx.run(qs.update(title=changes["title"]))
```

A threaded service that returns an awaitable instead of running it is a `500` that names `ctx.run`.
TypeScript services are always async.

### `admin.log`

`admin.log(action, user=, resource=, record_key=, label=, changes=, using=)` lets host code outside the admin (a job, a script, a webhook) write an audit row.
It joins the transaction that is open on the database.
`user` is a user mapping or object (its `key`, else its `id`), a user key, or `None`.
In TypeScript, `admin.log(action, { user, resource, recordKey, label, changes, using })`.

### The audit row

Each write adds one `AdminLogEntry`: `action` (`create`, `update`, `delete`), `resource`, `record_key`, `record_label` (the first text value of the record, at most 200 characters; Rust also sets it on the row of `ctx.audit()`), the actor, and the time.
Its `changes` column holds `{"request_id", "ip", "user_agent", "diff": {field: {"from", "to"}}}`.
A create has `from: null` for each field, and a delete has `to: null`.
The row commits with the write, so a failed write leaves none.
The `record_key` of a row that `ctx.audit()` writes is the key text that Rust builds (`a b` is `a%20b`, `a,b` is `a%2Cb`), in services, actions and custom routes.
A context that you build by hand has no Rust, so its row holds the plain key values joined with `,`.
A refused write (`403`) writes a `permission_denied` row instead.
The test client has no service layer, so its `request_id` is `unknown`.
A host route adds `inputs`, `service` (whether a service wrote) and `allow_protected_writes` to `changes`.

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

### Inlines

An inline edits the rows of a to-many relation on the form of the parent.
It wraps the registered page of the child, so the child keeps its fields, form layout, read-only fields, widgets, computed fields, `choices`, `queryset`, `can` rules and services.

```python
from monorm.admin import inline

admin.register(Order, {"inlines": [inline(OrderLine, max=50, style="table")]})
admin.register(OrderLine)
```

In TypeScript, `inline(OrderLine, { max: 50 })` in the `inlines` list.
A bare model in the list means `inline(Model)`.

The wrapper takes these keys:

| Key | Meaning |
|---|---|
| `via` | The relation of the parent. It is required when two relations lead to the child. Two relations with no `via` is an error of `admin.check()`. |
| `save` | `"with_parent"` (default) sends the rows with the parent in one change set and one transaction. `"row"` saves each row alone through the page of the child. |
| `permission` | A bare name, for example `"edit_lines"`. The engine makes it `<label>.edit_lines`, and it replaces the write permission of the parent for this inline. |
| `style` | `"table"` (default) or `"stacked"`. |
| `max` | The row cap, 1 to 200. The default is 20. |
| `label`, `collapsed`, `delete` | The title, the collapsed state, and whether a row may be deleted (default yes). |
| `page` | Page keys that replace the keys of the registered page. A child with no page needs the keys here. |

`page` takes `list`, `queryset`, `can`, `check_messages`, `form`, `readonly`, `widgets`, `relations`, `inputs` and `allow`.
Other page keys have no meaning for a row, and the build refuses them with a reason: `create`, `update` and `delete` (a service writes one record), `inlines` (an inline is never nested), `files` (a change set carries no file), `title` and `group`, `perms` and `label`, `field_permissions`, `see` and `change` (they belong to the registered page of the child), `actions`, `form_component` and `allow_protected_writes`.
The field that points at the parent is fixed by the parent, so a row has no control for it.

An inline is to-many only and is never nested.
A page with `files` cannot be an inline: the build refuses it, and the files stay on the page of the child.
The rows of one inline are read and written with the `queryset` of the inline, and a write of a row outside it fails as if the row did not exist.
On the host path, the registered `queryset` and `can` of the child page also apply to a row write.
A wrapper `queryset` that is wider than the registered one does not widen the rows: the result is the intersection, and a row outside it fails as if it did not exist.

**`with_parent`.**
The write of the parent carries `inlines: {name: {"create": [{"set"}], "update": [{"key", "set", "loaded"}], "delete": [key]}}`.
The admin applies deletes, then updates, then creates, in the transaction of the parent write.
A conflict or a field error names the row: `{name}.new.{i}.{field}` for a new row and `{name}.{key}.{field}` for a stored row.
A new parent takes only `create` rows.
A change of the rows needs the `update` permission of the parent, also with an own `permission`: use `save="row"` for rows that a user may change without it.
With the `page` strategy and no own `permission`, deleting a row needs the `delete` permission of the parent.
A service of the parent reads `ctx.inlines` (Python and TypeScript) and writes the child rows itself.
A parent with host behavior and no service needs `save="row"` or a service: the build says so.
A child page with a service or host behavior needs `save="row"` or a service on the parent: the build says so.

**`row`.**
The page of the parent saves, and each row has its own buttons.
The routes are `POST {prefix}/resources/{name}/{key}/inlines/{inline}` with `{"set"}`, and `PATCH` and `DELETE` on `.../inlines/{inline}/{row}` with `{"set", "loaded"}` and `{"loaded"}`.
Another inline mode answers `404`.
A row goes through the write path of the child page, with its audit row.
The user needs the permission of the inline alone (the own `permission`, or the write permission of the child), and the `read` permission of the parent.
The parent must also take an update: its `allow` lists `update`, its model is not read-only, and its `change` rule does not lock the record. A failed check answers `403`, and a read-only parent shows no row buttons.
The own `permission` of the inline differs by mode: `with_parent` also needs the write permission of the parent, `row` does not.
A new parent has no rows to save yet: the form says to save first.

**Permissions.**
An inline follows the parent unless it has its own `permission`.
In the `model` strategy it checks the permissions of the child model.
A row links to its own page when the user may read the child.

**Audit.**
On the plain path, each changed child row adds one audit row under the resource of the child page, with `inline` (`Parent.name`) in `changes`, and the parent adds its own row.
A `row` save adds the same audit row for its row.
A service writes the audit rows of its child rows itself, with `ctx.audit`.

The detail answer has `inlines: {name: {"rows": [{"key", "values", "flags"}], "truncated", "link"}}`, and the resource list describes each inline (`resources[i].inlines`): its columns, form fields, `ops` and `can` flags for the user.
The UI shows a table or stacked blocks, with "Add row" (off at the cap), "Remove" and "Delete".
A problem in a collapsed inline opens it.
An `admin.input` of the child page has no control in the inline, and a `with_parent` change set carries no inputs: only a `row` save runs the inputs of the child page, through the API.

### Fields with a view

A `Bytes` field can show as another value. The field adapter turns this on (the proto extension does it for a message field): its `admin_mode` hook (TypeScript `adminMode`) returns `"json"` or `"text"`.
The registration then sets `host_behavior` and `views` for you. In a Rust or custom setup, `views` maps a field to `json` or `text`, and it needs `host_behavior`: without it the build fails.

- `json`: the detail and the list show the JSON of `admin_to_view`, and the form edits it in a monospace box. The host calls `admin_from_view` and the adapter to build the stored value. A bad value answers `422` with the field name.
- `text`: the admin shows `N bytes` and the form field is read only. A request that sets the field answers `422`.
- A stored value that does not decode shows as `N bytes`. The version check compares the stored bytes, so a row with such a value can still be edited, repaired with a JSON value, and deleted.
- A save is refused with `422` when the JSON view would drop data from the stored bytes (an unknown field of a newer schema).
- `list.editable` rejects a view field with a build error.
- A filter and a sort on a view column use the stored bytes.
- A model that is only an inline of another resource has no `views` setting, so its field shows as base64 as before.
- The host function is `admin.view`. It takes `{model, field, values}` and returns one value for each stored value.
- The `json` widget pretty-prints every `json` field.

### Edit in the list

`list.editable` names the columns that staff edit in the cells of the list, as Django does with `list_editable`:

```python
admin.register(Ticket, {"list": {"columns": ["title", "status", "owner"], "editable": ["title", "status"]}})
```

```ts
admin.register(Ticket, { list: { columns: ["title", "status", "owner"], editable: ["title", "status"] } });
```

The site refuses a bad name when it builds (`admin.check()` reports it).
Each name must be a column of the list and a scalar field that the update form writes.
A key field, a file field, a relation, a computed or aggregate column and a read-only field of the form are errors.
The page must allow `update`, and an update service must not ask for a required input, because the list has none to give.
The order of the names does not matter: the order of the list columns rules.

The resource in `GET {prefix}/resources` has `editable` (the field names for this user), and each row of the main list has `flags[i].change`.
A user who lacks the `change=` [field permission](#field-permissions) of a field does not get it in `editable`.
A row that its `can=` rule refuses, or that the user may not change, shows plain values.
A tab list is read-only.

`PATCH {prefix}/resources/{name}` saves many rows in one request and one transaction.
The body is `{"rows": [{"key", "set", "loaded"}]}`, at most 200 rows (the `max_page_size` ceiling), each key once.
`set` takes only fields of `editable`, and `loaded` holds every field of `set` and every guard field, as in a form update (a `400` otherwise).
The answers:

| Status | Body |
|---|---|
| `200` | `{"ok": true, "keys": [...], "changed": {key: [names]}}`. A row that sets no new value writes nothing and gets no audit row, but its `loaded` values are still compared (a stale one is a `409`). |
| `400` | `{"ok": false, "rows": {index: {"fields": {field: [messages]}, "form": [messages]}}}`. The server collects the problems of every row first, and writes nothing. |
| `409` | `{"ok": false, "conflicts": {index: {field: {"loaded", "current"}}}}`. A stored value is not the loaded value; nothing is written. |
| `403` | A row is out of reach: the permission, the `can=` rule, or a `queryset` that hides the row. Nothing is written. |
| `404` | A key does not exist. |

`index` is the position of the row in the request.
A row that the `queryset` hides answers `403` here (the form route answers `404`), so the answer tells a user that the key exists.

On the plain path the save locks every row in key order, checks the rules of each, and writes each row and its audit row in one transaction.
A host behavior or an update service goes through one bridge call (`update_rows`): the Python and TypeScript host opens one `database.transaction()` and runs the update function (or the default writer) once for each changed row, in key order (the order that the plain path locks the rows in).
The transactions of a service become savepoints, and `on_commit` runs after the outer commit.
The first error rolls back every row.
A sync `threaded` service reaches the outer transaction through `ctx.run`.
`admin.test_client` sends the same request in both languages.

The UI keeps the changes by row and shows a bar with the count, Save and Discard.
A changed cell is marked, and a cell error or a conflict shows on its cell (a conflict shows the stored value, with "Use that value").
A row error (a form message, or a field that the list does not edit) shows under the first editable cell of its row.
After an error every change stays; after `200` the list reloads its current state.
A page change, sort, filter, search, link to another page or bulk action asks before it drops the changes, and closing the tab warns.
Tab and Shift+Tab move between the cells; the row selection and the link to the detail page stay.
A guard field that is not a column of the list has no value in the page, so the build refuses the editable list: put the guard field in the columns.

## Actions

An action is a function that runs on a set of records. It has a button in the list, in the row, on the detail page or next to the save button.

```python
from monorm import admin

@admin.action(placement="bulk", confirm="Refund these orders?", permission="refund")
async def refund(ctx, qs):
    await qs.update(status="refunded")
    return admin.message(f"Refunded {ctx.count} orders", "success")

site.register(Order, {"actions": [refund]})
```

In TypeScript, `action(async (ctx, qs) => {...}, { name: "refund", placement: "bulk", confirm: "Refund these orders?", permission: "refund" })` marks the function, and `appliesTo` and `bulkDelete` use camel case.
`qs` is a query set of the target records. `ctx` has `count` (at most 10,001), `capped`, `selection` (`keys`, `all` or `filter`), `inputs`, `database`, `user` and `audit()`.
The function is a plain function: a unit test calls it with a fake `ctx` and a query set, with no admin.

### Placements

| `placement` | Where | Target |
|---|---|---|
| `bulk` (default) | The bar over the list, after a selection | The selected rows, or all rows that match the filters |
| `row` | A button in each row | The one row |
| `detail` | The detail page | The one record |
| `submit` | Next to the save button of the form | The record, saved first |
| `list` | The toolbar of the list | All rows that match the filters |

Row and bulk actions also show on the [to-many tabs](#to-many-tabs) of the child resource.
A selection holds the rows of the page. "Select all N matching" selects every row that the filters match, and the request carries the filters of the URL, not a list of keys.
The query set gets the resource's `queryset` filter and, by default, its `change` rule. An action's optional `can` condition adds another requirement. It can read the signed-in user's key through `param("user")`.

```python
from monorm import param

@admin.action(placement="row", permission="refund", can=Order.owner_id == param("user"))
async def refund(ctx, qs):
    await qs.update(status="refunded")


@admin.action(placement="detail", can=Order.status == "closed", replace_change=True)
async def reopen(ctx, qs):
    await qs.update(status="open")
```

`replace_change=True` (`replaceChange: true` in TypeScript) requires `can` and lets the action use its own rule in place of `change`. The action still needs its named permission or the default resource permission, and `applies_to` still applies. Bulk delete uses the resource's `delete` rule.

A row, detail or submit action returns `403 ActionNotAllowed` and records `action_denied` when a visible record fails an action rule. A missing record or one outside the resource's `queryset` returns `404`. Bulk and list actions skip ineligible rows, and `ctx.count` counts eligible rows. A submit action saves the form under the resource's rules first, then checks the action on the saved record. Use a detail action when the record cannot pass the save's `change` rule.
`qs.batches(n)` reads the rows in batches of `n`.
A selection of more than 10,000 rows needs a second confirmation: the request answers `409` with `error.kind` `ConfirmLarge`, and the UI asks again. The request that follows carries `confirm_large: true`, and the function then runs on all rows that match, with no upper bound.
The action's query set carries the same rules into each read or write. A concurrent change can affect which rows still match after the count.

### Confirmation, inputs and files

`confirm="text"` makes the UI ask first. `inputs=[admin.input(...)]` adds the controls of an [input](#inputs) to the dialog, and `type="file"` adds a file control.
A file input arrives in `ctx.inputs` as a mapping with `filename`, `content_type` and `path`. The path names a temporary file that exists for the time of the call. The audit row stores the name, the type and the size, not the bytes.
An action with no confirmation and no inputs runs at once.

### Results

A function returns one of these. `None` is `refresh`, and a string is a message.

| Result | The UI |
|---|---|
| `admin.message(text, level)` | A message that fades after eight seconds. `level` is `info`, `success`, `warning` or `error`. |
| `admin.redirect(to)` | Goes to `to`, a path that starts with `/`. A path under the admin stays in the app. |
| `admin.download(filename, content, content_type)` | Saves the file. |
| `admin.refresh()` | Loads the list again. |

### `applies_to` and bulk delete

`applies_to=` takes a filter such as `Order.status == "new"`. The row buttons are off for the rows that it does not cover, and the query set leaves those rows out.
Every list has a bulk delete for a user who may delete. It passes the filtered query set to the delete service, or deletes it in one statement. `bulk_delete=False` on the resource removes it (`bulkDelete: false` in TypeScript). The resource [inputs](#inputs) show in its dialog.

### Permissions and the audit group

An action with `permission="refund"` needs `<label>.refund_<resource>` and read access. The check does not read `allow` or the read-only reason of the resource, so the function must guard what it writes.
An action with no permission needs update access: `manage` with the `page` strategy and `change_<resource>` with the `model` strategy. The `access` strategy has no such name, so it rejects an action with no permission when the site starts.
One request writes one summary row with `action` `action:<name>` (`bulk_delete` for bulk delete), the count and the selection kind. The rows that the function writes with `ctx.audit()` carry the same `request_id` in `changes`, so one user action is one group.

## Routes and answers

The path prefix is a config value, and the default is `<base_path>/api` (`/admin/api`): see [Base path and cookie scope](#base-path-and-cookie-scope).

| Route | Answer |
|---|---|
| `GET {prefix}/resources` | The `brand`, the navigation groups (`nav`), and the resources that the user may read. Each resource has a `title`, its `fields`, its `key` fields, its columns (with `title`, `widget` and `choices`), order, page size, sort columns and search fields. It also lists the permission names that the user holds. |
| `GET {prefix}/resources/{name}?cursor=&before=&last=&page=&limit=&sort=&q=&f.*=&n.*=` | One page: `columns`, `rows`, `keys`, `labels`, `next` and `prev`. See [Lists](#lists). |
| `GET {prefix}/counts/{name}?q=&f.*=&n.*=&exact=` | `{"count", "capped"}`. See [Count](#count). |
| `GET {prefix}/export/{name}?q=&f.*=&n.*=&sort=` | The list as a CSV file, streamed. See [CSV export](#csv-export). |
| `GET {prefix}/resources/{name}/{key}/{relation}`, `GET {prefix}/counts/{name}/{key}/{relation}` | The list and the count of a to-many tab. See [To-many tabs](#to-many-tabs). |
| `GET {prefix}/resources/{name}/{key}` | One record: `fields`, `values` and `links` to the to-one targets that the user may read. A key is encoded as in [The UI](#the-ui). |
| `POST {prefix}/actions/{name}/{action}`, `POST {prefix}/tab-actions/{name}/{key}/{relation}/{action}` | Run an action. The body is `{"keys", "all", "inputs", "confirm_large"}`. The answer is `{"ok", "count", "capped", "result"}`. See [Actions](#actions). |
| `POST`, `PATCH`, `DELETE` on `{prefix}/resources/{name}[/{key}]` | Create, change and delete one record. See [Writes and forms](#writes-and-forms). |
| `GET {prefix}/autocomplete/{name}/{relation}?q=&cursor=&limit=` | One page of the picker: `items` and `next`. See [Relation fields and files](#relation-fields-and-files). |
| `GET {prefix}/links/{name}/{key}/{relation}` | The records that a many-to-many relation links: `items` and `truncated`. |
| `POST {prefix}/resources/{name}/{key}/inlines/{inline}`, `PATCH`, `DELETE` on `.../inlines/{inline}/{row}` | Save one row of a `row` inline. See [Inlines](#inlines). |
| `POST`, `GET {prefix}/files/{name}/{key}/{field}` | Upload a file (multipart) and download it (attachment). |
| `GET {prefix}/forms/{name}` | The module of a custom form, as `text/javascript` with `cache-control: no-store`; `401`, `403` and `404` as above. See [Custom form component](#custom-form-component). |
| `GET`, `POST`, `PUT`, `PATCH`, `DELETE` on `{prefix}/routes/{path}` | A custom route of the host. The answer is the JSON or the stream that the route function returns. See [Custom routes](#custom-routes). |
| `GET {prefix}/packages/{package}/{file}` | A file of an extension package, with its type and `cache-control: no-cache`; `401` and `404` as above. See [Extension packages](#extension-packages). |

A row is an array that follows `columns`.
`next` is a cursor for the next page, or `null` on the last page.
BigInt and Decimal are JSON strings. A Duration is a JSON string, `[-][D ]HH:MM:SS[.ffffff]`.
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

## Custom routes

`admin.route(method, path, fn)` adds a JSON or stream endpoint of your own at `{prefix}/routes/{path}`.
The admin runs the session, the CSRF check and the permission check, so a route function never handles a login.
A request without a session gets `401`.
A user without `admin.access` or without the `permission` of the route gets `403`.

```python
from monorm.admin import Reply, Stream

async def summary(ctx, request):
    row = await Book.objects.using(ctx.database).filter(Book.id == int(request.params["id"])).first()
    if row is None:
        return Reply({"error": "no such book"}, status=404)
    return {"title": row.title}

admin.route("GET", "books/{id}/summary", summary, permission="library.read_summary")
```

```ts
import { Reply, Stream } from "monorm/admin";

admin.route("GET", "books/{id}/summary", async (ctx, request) => {
  const row = await Book.objects.using(ctx.database).filter(Book.id.eq(Number(request.params.id))).first();
  return row === null ? new Reply({ error: "no such book" }, { status: 404 }) : { title: row.title };
}, { permission: "library.read_summary" });
```

- **Method and path.** The method is `GET`, `POST`, `PUT`, `PATCH` or `DELETE`. A path part in braces (`{id}`) matches one segment and appears in `request.params`. The other parts are letters, digits and `_ . ~ -`.
- **Order.** The route with the most fixed parts wins, so `books/export` beats `books/{id}`. The same method and path twice is a build error. A path with another method gives `405` with the allowed methods; `HEAD` on a `GET` route is another method, so it gives `405` too. A `GET` route skips the CSRF check, so it must not change state: use `POST`, `PUT`, `PATCH` or `DELETE` for a write.
- **Request.** The function gets `ctx` (the same `Context` as a [service](#services), with `ctx.database` for queries) and a `RouteRequest` with `method`, `path`, `params`, `query` and `body`. `query` maps a name to a string, or to a list when the name repeats. `body` is the JSON body, or `None` (`null`) when it is empty. A body over 1 MiB is a `413`.
- **Answer.** Return a JSON value, `Reply(json, status=)` for another status code, or `Stream(chunks, content_type=, filename=)`. A value that JSON cannot hold is an error, except dates, times, UUID and Decimal (text) and bytes (base64).
- **Permission.** `permission` is a full name, `<label>.<name>`. It is part of [`admin.permissions`](#adminpermissions-and-sync-permissions), so `sync-permissions` creates it. A permission needs the page or the model strategy: with the access strategy it is a build error. The build checks the label only, not the name part, so a typo in the name makes a new permission that no role holds. A refused route writes one `permission_denied` audit row; a route that succeeds writes none, so call `ctx.audit()` when it changes data.
- **Streams.** `chunks` is an iterable or an async iterable of `bytes` or `str`. The admin pulls one chunk at a time, so a slow client slows the producer and no layer holds the whole body. The stream closes when it ends, fails, idles past the export timeout or the client leaves. A failure after the first chunk cuts the response. A sync iterator in Python runs in a worker thread.
- **Tests.** `admin.test_client` (`admin.testClient`) sends a request to a route as to any other path. See [Test client](#test-client).

## Widget contract

A widget replaces the input of one field.
It is a module with two exports: `adminMajor` (a number) and a default React component.
`adminMajor` is the version of this contract, and it is `1`.
A package that is built for another major version does not load: the build of the site fails with `package "x" is built for admin major 2; this admin is major 1`.

```js
import { createElement as h } from "react";

export const adminMajor = 1;
export default function Stars({ id, value, onChange, onError, readOnly, disabled, errors }) {
  return h("button", { id, type: "button", disabled: readOnly || disabled, onClick: () => onChange((value ?? 0) + 1) }, String(value ?? 0));
}
```

| Prop | Value |
|---|---|
| `id`, `name` | The id of the input element (a label points at it), and the field name. |
| `value` | The value of the field as the server sent it (`null` on a new record). |
| `onChange(value)` | Sets the value that the form sends, any JSON value. The form sends it only when it differs from the loaded value. |
| `onError(message)` | A message blocks the save and shows with the field. `null` clears it. |
| `field` | `name`, `title`, `type`, `array`, `required`, `nullable`, `choices` and `max_length`. |
| `record` | The loaded value of each field of the form. |
| `readOnly`, `disabled` | The field is read-only, or the form is saving. |
| `errors` | The messages of the server for this field. |
| `call(method, path, body?)` | Calls a [custom route](#custom-routes) (`path` is the part after `routes/`) and resolves to the JSON answer. A status of 400 or more throws. |
| `stream(method, path, body?)` | The same call, but it resolves to the raw `Response`, to read a stream. |

A widget is named `<package>.<widget>` in the `widgets` config, as in `{"widgets": {"stars": "rating.stars"}}` on a resource.
A name of a package that does not exist is a build error.

The widget contract stays the same inside one major version.
A new prop or a new field in `field` is not a major change, so a widget must ignore what it does not know.

## Extension packages

An extension package is a directory of `.js`, `.mjs` and `.css` files that the admin serves to its own page.
It brings widgets, form modules, detail tabs and styles, and a function that also adds routes and actions of its own.
The admin names no package: the host chooses which ones to add.

```python
admin.package("rating", Path(__file__).parent / "rating", widgets={"stars": "stars.js"}, tabs={"summary": "summary.js"}, styles=["rating.css"])
```

```ts
admin.package("rating", new URL("./rating/", import.meta.url).pathname, { widgets: { stars: "stars.js" }, tabs: { summary: "summary.js" }, styles: ["rating.css"] });
```

- **Files.** The admin reads the directory once when it builds the site, and it serves only what it read at `GET {prefix}/packages/{package}/{file}` for a signed-in user. No request reads the disk. The limits are 256 files, 1 MiB for one file and 8 MiB in all (design values). A file over a limit is a build error.
- **Names.** `widgets`, `forms` and `tabs` map a short name to a file of the directory. Outside the package, the name is `<package>.<name>`. A name of a missing file, of a path with `..`, or of a package that is added twice is a build error.
- **Styles.** `styles` lists `.css` files. The page loads them with the first widget of the package, as `<link>` elements of the same origin, so `style-src 'self'` stays the same.
- **One React.** A page with packages carries an import map in `<head>`, so `import "react"`, `"react-dom"`, `"react-dom/client"` and `"react/jsx-runtime"` in a module resolve to the React of the admin. The map is an inline script, and `script-src` allows it by its `sha256` hash. A site without packages has no map and the plain policy. Do not bundle a second React.
- **The kit.** The same map has `monorm-admin/ui`: `import { Button, Card, Badge, toast, Star } from "monorm-admin/ui"` gives a module the admin's own components. They are `Button`, `Input`, `Textarea`, `Select`, `Label`, `Badge`, `Card` (with `CardHeader`, `CardTitle`, `CardDescription`, `CardContent`, `CardFooter`), `Tabs`, `Table`, `Dialog`, `toast`, `cn` and a set of `lucide` icons. A package needs no CSS build for them, and its own CSS file can use the theme variables (`var(--primary)`, `var(--border)`).
The admin does not scan the CSS or the code of a package for Tailwind classes, so a class that only the package uses has no style: write a CSS file that uses the variables. The contract version stays 1: a package that does not import the kit works as before. `examples/admin-package` uses it.
- **The policy.** The rest of the policy does not change: `default-src 'none'`, `script-src 'self'` (and the one hash), `frame-ancestors 'none'` and the `X-Frame-Options` header. A package gets no frame and no inline script.
- **Trust.** A package module is code that every staff user of the admin runs: add only packages that you wrote or reviewed.
- **The form module.** A form module that exports `adminMajor` is a plain default component with the props of [Custom form component](#custom-form-component). A module without `adminMajor` is the older form: a default function from React to a component, and it works as before.

`GET {prefix}/resources` lists the packages in `packages`, and a resource lists its tabs in `package_tabs`.

### Detail tabs

A resource can show a tab of a package on its detail page, after the tabs of its relations.
The tab is a component of the page, not a frame.
It reads its data from a [custom route](#custom-routes).

```python
admin.register(Movie, {"detail_tabs": ["rating.summary"]})   # TypeScript: detailTabs
```

The address of the tab is `{base}/Movie/1?tab=rating.summary`.
The module has the same two exports as a widget, and its default component gets these props:

| Prop | Value |
|---|---|
| `resource` | The resource name. |
| `id` | The key of the record, URL-encoded as in the address. |
| `record` | The loaded field labels and values of the record. |
| `call`, `stream` | As for a widget. In an embed these helpers accept only GET. |
| `embedded` | Whether the component is in a read-only embed page. |

The tab title is the part of the name after the dot, in the same readable form as other titles (`order_notes` is `Order notes`).
A tab of a package that does not exist is a build error.

### Example

`examples/admin-package` has one package for a `Movie` model with an integer `stars` field: a stars widget, a Summary tab, a route that gives the title and the average, and an action that clears the stars.
`rating.py` is for a Python project and `rating.mjs` for a TypeScript project, and both serve the same `rating/` directory.
Each has one function, `install(admin, Movie)`.
The tests run it through the test client on SQLite and PostgreSQL, and the [browser smoke test](#browser-smoke-test) opens the widget and the tab.

## Frame tabs, home cards and embed pages

A frame tab shows another service's page next to a record. A frame card shows a fixed URL on the home page.
The parent admin lists the origins it may frame.

```python
site = Admin(
    frame_origins=["'self'", "https://charts.example.com"],
    frame_cards=[{"name": "operations", "title": "Operations", "url": "https://charts.example.com/ops"}],
)
site.register(Shop, {
    "frames": [{
        "name": "identity", "title": "Identity health", "height": "70vh",
        "url": "/identity/admin/-/embed/shop_health?domain={domain}",
    }],
})
```

TypeScript uses `frameOrigins`, `frameCards`, and `frames`. Both bindings take `name`, `title`, `url`, and optional `height` (default `70vh`).
Use root-relative URLs with `"'self'"` for sibling admins on the same origin. An absolute URL needs an exact HTTPS origin from the list. HTTP is available for `localhost`, `127.0.0.1`, and `[::1]`. Origins have no trailing slash.
A field placeholder fills an entire path segment or query value. It can follow one to-one relation, such as `{owner.domain}`; the user then needs the read grant on the target resource and its `see` permission for that field.
The server percent-encodes values. A null or hidden field makes the frame unavailable. Home cards have fixed URLs.

Frames default to `allow-scripts allow-same-origin`. Turn either off with `scripts: false` or `same_origin: false` (`sameOrigin` in TypeScript).
The optional `sandbox` list adds `allow-forms`, `allow-popups`, `allow-popups-to-escape-sandbox`, or `allow-downloads`.
Treat same-origin scripts as trusted. With the default flags they can remove the sandbox, which only guards against accidental navigation.
The parent page always offers an **Open in new tab** link. To let links inside the iframe open an unsandboxed tab, configure both popup flags explicitly.

The sibling admin can publish one read-only detail tab at `<base>/-/embed/{name}`.

```python
identity = Admin(
    base_path="/identity/admin",
    embeds={
        "shop_health": {
            "resource": "Shop", "tab": "health", "lookup": "domain", "ancestors": ["'self'"],
        },
    },
)
identity.register(Shop, {
    "fieldsets": [{"name": "health", "title": "Health", "fields": ["state", "checked_at"]}],
})
```

TypeScript uses the same `embeds` and `fieldsets` keys, with `basePath` for the site's base path.
The lookup field must have a single-field unique constraint or primary key. The request uses the framed admin's own session, read permission, queryset and field permissions.
A signed-out visitor gets a sign-in link to the framed admin. The session cookies support same-origin and same-site framing.
Only the named embed document accepts its configured ancestors. Its API route (`GET {prefix}/embeds/{name}?{lookup}=<value>`) returns the tab's data with the standard API headers.

To expose a package tab, register it as `{"module": "summary.js", "embeddable": True}` in Python or `{ module: "summary.js", embeddable: true }` in TypeScript.
The resource must list the package tab normally. When its component receives `embedded: true`, it must hide write controls and forms, and open links in new tabs.
Only opt in trusted read-only components and custom GET routes.

## The UI

The server also serves a single-page app at `<base_path>/` (`/admin` by default).
It is React and TypeScript with TanStack Query, TanStack Table and Radix primitives, and it speaks only the routes above.
The build output is committed in `crates/admin/ui/dist`, and the `monorm-admin` crate embeds it.
So the wheel and the npm addon carry the UI, and no other packaging step exists.
The files are static: a `GET` of a UI path needs no session, and a reply carries its own `Content-Security-Policy` (`script-src 'self'`).
A hashed file in `assets/` has `Cache-Control: public, max-age=31536000, immutable`, and `index.html` has `no-store`.
The UI path is the `base_path` setting, and the page paths below show the default `/admin`.

| Path | Page |
|---|---|
| `/admin` | The home page: a card for each navigation group with its pages, and the last 10 audit-log rows that the user may see (only when the user may read `AdminLogEntry`). |
| `/admin/-/embed/{name}` | A named read-only detail tab. |
| `/admin/-/login`, `/admin/-/password` | Sign in, and change the password. Sign out, the theme and the time zone are in the user menu of the header. |
| `/admin/{resource}` | The list. The first column links to the record, and a foreign key column shows the label of the related record as a link. Search, filters, sort, page and page size are in the query string, so a reload, a link and the back button return to the same view. |
| `/admin/{resource}/{key}` | The record: its fields, a link for each to-one relation, and a tab for each to-many relation (`?tab={relation}`). It has Edit and Delete. |
| `/admin/{resource}/-/new`, `/admin/{resource}/{key}/edit` | The forms: see [Forms in the UI](#forms-in-the-ui). |

The list page has these controls:
- A Filters popover holds the filters, and each active filter shows as a chip that the user can remove.
- A Columns menu hides a column, except the first. The choice stays in the browser, for each resource.
- A density toggle (comfortable or compact) stays in the browser.
- A row menu holds the row actions, and a bar over the table holds the bulk actions of the selected rows.
- The footer has the page links and a page size choice (up to `max_page_size`).
- With `list.editable`, the cells of those columns are inputs, and a bar saves or discards the changes: see [Edit in the list](#edit-in-the-list).
- A list shows a skeleton while it loads. An empty list offers to clear the search and filters, or to add a record. An error shows as an alert.

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

### Theme, brand, groups and titles

The UI is built on a kit (Tailwind CSS and Radix primitives) with CSS variables for light and dark.
Dark follows the system. The user menu has a Light, Dark and System switch that the browser keeps in `localStorage`.
A site changes the look with `theme`, and needs no CSS build.

`theme` sets the title, the logo and the brand colour in one small object.
`title` is the name in the header and the page title, `logo` is a path that starts with `/` or an `https` URL (the CSP allows no other image source), and `color` is `#rgb` or `#rrggbb`.
The UI derives a readable text colour for the brand colour, in light and in dark.
`theme.title`, `theme.logo` and `theme.color` are the short form of `brand.name`, `brand.logo` and `brand.colors.primary`.
A value that is set in both is a config error that names both settings. An unknown theme key is an error too.

`brand` sets the name, logo, colors, header links and environment label.
`colors.primary` is the brand colour. `colors.accent` tints the surface of a hovered or selected item, in light and in dark, and keeps the text on it readable.
`groups` is the order of the navigation groups.
A resource names its group with `group`, and its title with `title`.
A group that `groups` does not list is an error, and so is an unknown brand key.
The built-in pages (the audit log, users, roles and permissions) sit in the group "Users and access", which the admin adds at the end of `groups`.
A page that the config puts in another group keeps that group.
A resource of yours may name "Users and access" on purpose: it then shares the group with the built-in pages, with no message.
The metadata is built once per process.

```python
admin = Admin(
    theme={"title": "Acme", "logo": "/static/logo.svg", "color": "#0b5fff"},
    brand={"links": [{"label": "Site", "url": "https://acme.example"}], "environment": "staging"},
    groups=["Content"],
)
admin.register(Post, {"group": "Content", "title": "Articles"})
```

```ts
const admin = new Admin(registry, {
  theme: { title: "Acme", logo: "/static/logo.svg", color: "#0b5fff" },
  brand: { environment: "staging" },
  groups: ["Content"],
});
admin.register(Post, { group: "Content", title: "Articles" });
```

The sidebar lists the groups as sections that collapse, with a filter box. The header button hides the sidebar, and the UI keeps that choice.
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

### Base path and cookie scope

The admin serves all its paths under one base path.
The setting is `base_path` in `[admin]` of `monorm.toml`, the `base_path` option of the Python `Admin`, and the `basePath` option of the TypeScript `Admin`.
The default is `/admin`.
In serve mode, a `base_path` in `monorm.toml` replaces the option of the `Admin` object.

The server normalizes the value: it removes a trailing `/`, so `/bundler/admin/` becomes `/bundler/admin`.
The value must start with one `/`.
It allows only the characters `A-Z a-z 0-9 - . _ ~` and `/`, and it refuses an empty value, `/`, and an empty, `.` or `..` segment.
A wrong value is an error when the site is built, and `admin.check()` reports it.

- The UI is under `<base_path>/`, and the default API prefix is `<base_path>/api`.
- An explicit `prefix` must sit under `<base_path>`, and it cannot be the path of the UI itself.
  Below `<base_path>`, its first segment cannot be `-`, `assets`, `shared`, `index.html` or the name of a resource: the UI uses these paths.
- Every URL that the server builds (page links, `admin.url()`, login and logout redirects, file and export routes, custom routes, the widget import map) starts with `<base_path>`.
- An action redirect is a path that your action gives. Build it with `admin.url()`: the server accepts any path that starts with one `/`.
- The `base_path` attribute of the Python `Admin` and the `basePath` attribute of the TypeScript `Admin` keep the text that you gave, with a trailing `/` if you wrote one. Build a URL with `admin.url()`, not by joining `base_path` and a path.
- `/healthz` and `/readyz` stay at the root of the host, for the probes of the pod.

The session and CSRF cookies have `Path=<base_path>/`.
A browser sends them only to this admin, so two admins on one host (`/a/admin` and `/b/admin`) keep separate sessions: a login or a logout in one has no effect on the other.
The cookie names are fixed, and the path keeps the admins apart.
A cookie with `Path=<base_path>/` is not sent to `<base_path>` without the final `/`. The page at `<base_path>` has no session, and the server writes the asset URLs of the page as absolute paths under `<base_path>/`, so it loads and the app calls the API with the cookie.
The sessions are signed with the secret (see [`MONORM_ADMIN_SECRET`](#monorm_admin_secret)).
Admins that share one secret accept the cookie of each other where the path matches, so give each admin its own secret if the user lists must stay apart.

A gateway passes the full path to the admin: it does not strip the prefix, and the admin does not read `X-Forwarded-Prefix`.
This Traefik route sends `/bundler/admin` to an admin with `base_path = "/bundler/admin"`:

```yaml
http:
  routers:
    bundler-admin:
      rule: "PathPrefix(`/bundler/admin`)"
      service: bundler-admin
  services:
    bundler-admin:
      loadBalancer:
        servers:
          - url: "http://bundler-admin:8000"
```

Give the probes of the pod the root paths `/healthz` and `/readyz`, not the base path.

### Browser smoke test

`crates/admin/ui/e2e` holds the first browser suite of the repository.
It starts a real `python -m monorm admin` process on SQLite (it makes a temporary project, migrates, and seeds a user and two rows), signs in, sorts a list, searches it, opens a record, adds, changes and deletes a record through the forms, saves an inline row and a new inline row with their parent, edits two cells on two rows of a list and saves them, shows a refused cell, opens a section layout and a custom form, opens a widget and a detail tab of the example package (it saves through the widget and reads the route of the tab), and fails on any console error, CSP violation or failed request.
The driver is Playwright (`playwright` 1.65.0, pinned) with `node:test`.
Playwright downloads a browser, so the install is not part of the default `npm install` of any package.
Run it from a checkout, with the `monorm` wheel installed in the active Python (`maturin develop`):

```
cd crates/admin/ui/e2e
npm install
npm run install-browser     # downloads Chromium, once
npm test
```

One more case starts a second admin with `base_path = "/bundler/admin"` and signs in and opens a list there.
A last case starts a fake OpenID provider (`fake-idp.mjs`, on a loopback port) and an admin with `[admin.oidc]`: a login with no `hd` claim shows the domain message, and a login with the claim signs in the user with the role of the claim.
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
| `base_path` | `/admin` | The path of the whole admin: the UI, the API and the cookies (see [Base path and cookie scope](#base-path-and-cookie-scope)). |
| `origins` | none | Extra origins that a write may come from, such as `"https://admin.example.com"`. |
| `trusted_proxies` | none | Peer IP addresses whose `X-Forwarded-For` the login limits believe, such as `"10.0.0.1"`. |
| `oidc` | none | A table for [single sign-on](#single-sign-on-oidc). The client secret is the variable `MONORM_ADMIN_OIDC_CLIENT_SECRET`; a `client_secret` key is an error. |

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
A row-level security policy can read the value with `current_setting('app.tenant')`. The setting name is free: `db_settings` is separate from the `scope.<key>` settings of `monorm.scope()`.
The default writer and the actions of a resource run in that transaction, so the settings and the policies apply to their writes. A service opens its own transaction and does not get them.
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
