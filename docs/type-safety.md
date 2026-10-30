# Type safety

The type checker catches most query errors before the query runs.
This page compares the type safety of MonOrm with Django and SQLAlchemy (Python) and with Prisma (TypeScript).
It makes no claim about ease of use.

## The evidence

Two test files hold the checks, and CI runs both:

* TypeScript: [`languages/js/test/typing/check.ts`](../languages/js/test/typing/check.ts) and [`nested.ts`](../languages/js/test/typing/nested.ts), with `tsc` (`npm run typecheck`).
  They hold 133 `@ts-expect-error` lines (each must be a type error) and 140 exact type assertions (`same<...>()`).
* Python: [`languages/python/tests/typing/check_models.py`](../languages/python/tests/typing/check_models.py), with mypy and pyright in strict mode.
  It holds 58 lines marked `# E:` (each must be an error) and 174 `assert_type` calls.

Counts are from `main` on 2026-10-31.

## Types come from generated models

Run `python -m monorm generate` or `npx monorm generate` to write a typed module from `schema.prisma`.
`load(path)` and `loads(source)` make the same models at run time, but untyped.
Prisma also needs a generate step (`prisma generate`).

## What the type checker catches

All of these are type errors in both languages, unless the row says otherwise:

| Error | Example |
|---|---|
| A misspelled or unknown field | `insert(email="a", nope=1)`, `User.posts.nope` |
| A missing required field in an insert | `insert(email="a")` without `name` |
| A value of the wrong type | `update(views="many")`, `User.email < 1` |
| A value that is not a member of the enum | `insert(role="boss")` |
| An operator for another column type | `like()` on an `Int` column, `lower()` of an `Int` column, array operators on a scalar column |
| A comparison with `null` on a column that is not nullable | TypeScript |
| An ordering through a to-many relation, which repeats rows | TypeScript |
| A column of another model in a query | TypeScript: `Post.objects.filter(User.email.eq("x"))` |
| A write to an instance | `u.name = "B"` |
| A row that can be missing, used as present | `Post.objects[0]` is `Post \| None` |
| A field that the query did not load | TypeScript: `load(Post.title)` gives rows without `body`. Python: the type is the full model, and the read raises `NotLoaded` at run time |
| A prepared query without its parameters, or with values of the wrong type | TypeScript: `param()` placeholders |

Expressions are typed too: aggregates, `CASE`, window functions, subqueries and `outer()` references, CTEs, JSON paths and full-text search.
For example, in Python `select(func.max(Post.views)).scalar()` is `int | None`, and in TypeScript `sum()` of a text column is an error.

## Compared with Django

Django has no type hints of its own.
The third-party `django-stubs` package adds them.
Lookups are keyword arguments such as `filter(email__lt=1)`, so the type checker does not check the value type against the column.
Such errors show at run time.

## Compared with SQLAlchemy 2

SQLAlchemy 2 types the mapped attributes (`Mapped[int]`) and the rows of `select()`, as MonOrm does.
Its comparison operators take any value, so `User.email < 1` passes the type check.
`update().values(...)` takes keyword arguments that the type checker does not check against the model.
A relationship that a query did not load has the same type as a loaded one.

## Compared with Prisma

Prisma types the CRUD API well.
`where`, `data`, `select` and `include` are typed, and the result type follows `select` and `include`.
MonOrm gives the same for TypeScript: typed inserts and updates, typed filters, and rows that hold only the loaded fields and relations.

The difference is the SQL outside CRUD, and it depends on the Prisma version:

* Prisma ORM 7 (the version in our benchmark): Prisma Client has no query API for window functions or CTEs.
  The docs send you to raw SQL: `$queryRaw<T>`, where Prisma does not check `T` against the query, or TypedSQL, where `prisma generate --sql` reads the types of a `.sql` file from a database.
* Prisma ORM 8 (a release candidate in October 2026, and what `npm install prisma` gives): a typed SQL query builder adds joins, computed columns, grouped top-N and subqueries.
  A CTE still needs raw SQL (`db.raw.sql`), where you declare the row type with `.returnsRow()` and Prisma does not check the SQL text.

MonOrm types CTEs, window functions and subqueries in the query builder, in the same code as the other queries.

## Not compared

We did not compare Drizzle, Tortoise ORM or Prisma Client Python here.
