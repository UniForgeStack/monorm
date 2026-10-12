# Typed schema expressions for checks, index filters and default filters

## Outcome

Checks, partial-index filters and default query filters are schema expressions,
not quoted SQL text.
A field name in an expression is a reference to the field declaration,
so the compiler checks it, a rename updates it,
and a future schema language server can go to its definition.
The operators are the ones of the Python query API: `& | ~` and comparisons.

```prisma
model Reward {
  percent Decimal? @db.Decimal(5, 2) @check(percent.between(0, 100))
  ...
  @@check(mode == TOTAL | target == PRODUCT, name: "reward_mode_total_unless_product")
  @@index([tier_id, content_hash], unique: true, where: deleted_at == null & content_hash != "")
  @@query.filter(shop_id == scope.shop & deleted_at == null)
}

model PercentageDiscountReward {
  @@proxy.of(Reward)
  @@partition.values([PERCENTAGE_DISCOUNT])
  @@check(percent != null & amount == null & price == null)
}
```

## Current state

- `@check`, `@@check`, `where:` of `@@index` and `@@exclude`, and `sql("...")` keys
  take SQL text as `"..."` or `raw("...")` (`core/src/dsl/lower.rs:106`).
  The compiler does not read the text; Postgres reports a wrong column when the migration runs.
  The text names columns, so it uses `_deleted_at`, not the field `deleted_at`.
- `@@query.filter("...")` has its own small language inside a string,
  with `==`, `and` / `or` / `not`, `scope.<name>` and `parent.default_filter`
  (`query-defaults/src/lib.rs:128`). It checks field names, not types.
- The migration model compares checks and index filters as SQL text with whitespace normalized
  (`core/src/migrate/model.rs:302`).

## The language

One grammar for every context below. It follows `docs/python-api.md` ("Expressions").

- **Names:** a bare name is a field of the model.
  `scope.<name>` and `parent.default_filter` exist only in default filters.
  A dotted path `author.active` is a to-one relation path, in default filters only.
- **Literals:** numbers, `"strings"`, `true`, `false`, `null`, and enum members as bare names.
  An enum member resolves against the enum of the field it is compared with,
  so `mode == TOTAL` is valid and `mode == SHIPPING` is an error.
  A string compared with a `DateTime` field must be RFC 3339.
- **Comparisons:** `== != < <= > >=`. `== null` is `IS NULL` and `!= null` is `IS NOT NULL`.
- **Logic:** `&` (and), `|` (or), `~` (not), and parentheses.
- **Arithmetic:** `+ - * /` and unary `-`.
- **Methods**, with the Python names:
  `.in_([..])`, `.not_in([..])`, `.is_null()`, `.between(a, b)`,
  `.like()`, `.ilike()`, `.contains()`, `.icontains()`, `.startswith()`, `.endswith()`.
- **Functions:** a fixed, typed list, for example `lower()`, `upper()`, `char_length()`,
  `coalesce()`, `abs()`, `now()`. A function outside the list is an error that points to `sql()`.
- **Escape:** `sql("...")` is SQL text inside an expression: `percent >= 0 & sql("percent % 1 = 0")`.
  `raw("...")` and `"..."` as the whole argument stay SQL text, as today.

Precedence, from tightest: method calls, unary `-`, `* /`, `+ -`, comparisons, `~`, `&`, `|`.
This is not Python's order, where `&` binds tighter than `==`
and `a == 1 & b == 2` needs parentheses.
In the schema the SQL order is the useful one, and Python text with parentheses still parses.

Python's `and` / `or` / `not` are errors that name `&` / `|` / `~`.
The `and` / `or` / `not` / `&&` / `||` / `!` of today's filter strings stay valid inside a string only.

## Contexts

| Context | Names | Not allowed |
| --- | --- | --- |
| `@check`, `@@check` | fields of the table | relation paths, `scope`, `parent`, volatile functions |
| `where:` of `@@index`, `@@exclude` | fields of the table | relation paths, `scope`, `parent`, functions that are not immutable |
| index and partition keys (`lower(email)`) | fields of the table | the same as `where:` |
| `@@query.filter`, `@@query.defaults(filter:)` | fields, to-one paths, `scope`, `parent.default_filter` | nothing extra |

- A `@check` on a field can use other fields of the model, as Postgres allows.
- A proxy's default filter sees the proxy's fields.
  A check or index on a proxy-owned partition (plan 11) sees every field of the root table,
  because a partition check must also name the columns that the proxy omits (`amount == null`).
- `@@trigger(when:)` uses `OLD` and `NEW`, and stays SQL text. A later plan can extend the grammar.

## Type checks

Definition fails with file, line and column, and the field name:

- an unknown field, enum member, method or function;
- a comparison of two types that do not match, such as `percent == "x"` or `mode == 3`;
- `null` with an operator other than `==` / `!=`, and a comparison with `null` on a non-null field (a warning);
- a string method on a field that is not a string;
- a name that the context does not allow (`scope.shop` in a check).

## Lowering

- The parser builds one typed expression tree, with a source span on every name.
  It lives in `core/src/dsl/`; `orm-query-defaults` consumes the tree instead of its own tokenizer.
- Checks, index filters and keys lower to SQL text in the IR, per dialect,
  with column names from `@map` and quoted where needed.
  The migration model and differ stay unchanged.
  A native check is portable to SQLite; a `raw()` check is not.
- The SQL text is canonical: one form for one tree, so a second `makemigrations` finds nothing.
- Default filters lower to the filter tree that the engine uses today.

## Compatibility

- `raw("...")` and `"..."` stay SQL text for checks and index filters.
  After this plan ships, a plain `"..."` gives a warning that names `raw()`;
  a later release can make it an error.
- A `@@query.filter("...")` string keeps its current language.
  It gives a warning with the native form.
- A change from a SQL string to the same rule as an expression must give no migration.
  The canonical SQL can differ from the old text.
  Choose one approach in ticket 3:
  compare the two forms through Postgres's own rendering in the shadow database,
  or have `orm fmt --native` keep the old SQL text when the new tree has the same meaning.
- `pull` writes native expressions where the SQL parses into the grammar,
  and `raw("...")` for the rest.

## Prisma editor

Measured with `@prisma/prisma-schema-wasm` 8.1.0-2 (the formatter of the Prisma VS Code extension):

- An attribute with `>=`, `&`, `|`, `!=` or `~` in its arguments is kept as is, on its line.
  The formatter does not align that line, and the rest of the file still formats.
- Method and function calls without operators (`percent.between(0, 100)`, `or(a, b)`)
  are parsed and aligned as usual.
- The linter marks both forms as errors, as it does for our other attributes.

The one-line rule of `docs/prisma-syntax.md` applies: an expression never spans lines.
The formatter tests must cover an operator line inside a model and inside a `partition` block.

## Language server

This plan does not build the server. It makes the server possible:

- Each name keeps its span and the declaration it resolves to.
- A field rename in the server rewrites every reference in expressions.
- Hover shows the field type; completion lists fields, enum members of the compared field, methods and functions.

## Tickets and acceptance

1. Parser, typed tree and type checks, with spans; errors with file, line and column.
2. Lowering for `@check`, `@@check`, `where:` and expression keys, Postgres and SQLite.
3. Migration compatibility for a SQL-to-expression change, and `pull` output.
4. Default filters on the shared parser; warnings for the string forms; docs
   (`schema.md`, `prisma-syntax.md`, `selection-and-defaults.md`).

Cover:

- every operator and method, lowered and run against Postgres and SQLite;
- precedence: `a == 1 & b == 2 | c == 3` without parentheses;
- enum members resolved per field, and a member of the wrong enum rejected;
- a `@map` column in a check and an index filter;
- `scope.shop` rejected in a check, accepted in a filter;
- a volatile function rejected in an index filter;
- a SQL check converted to an expression with no migration;
- a check on a proxy-owned partition that names a field the proxy omits;
- the Prisma formatter keeps every expression line unchanged.
