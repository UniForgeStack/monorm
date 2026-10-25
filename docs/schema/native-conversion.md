# Adopt native schema expressions

`orm fmt --native` converts supported SQL checks, index and exclusion predicates,
and expression index keys without options to checked schema expressions.
It accepts the usual schema, migrations-directory and project configuration options.
It works offline and does not create migrations or edit migration history.
It keeps comments and unrelated source text intact. Imported project files own
their declarations. Installed app schemas stay under package control.

The converter parses SQL, resolves physical columns through field maps, and runs
the existing expression type and context checks. It supports literals, local
columns, parentheses, and arithmetic (`+ - * /`). It also supports comparisons,
null tests (`IS NULL`, `IS NOT NULL`), and boolean operators (`NOT`, `AND`, `OR`).
Use `IN`, `NOT IN`, and `BETWEEN` for membership and range tests.
Direct catalog function calls also work.
These calls include `lower`, `upper`, `abs`, `coalesce`, Postgres `char_length`
and `now`, and SQLite `length`. Context restrictions still apply: `now()` cannot
become an immutable index expression.
SQLite accepts the renderer's exact `0`/`1` forms with a checked boolean peer.
The converter preserves operand order and string contents.
It does not prove algebraic identities or reorder boolean terms.

SQL comments, casts, collations, qualified operators, subqueries, unknown functions,
and complex method forms stay as SQL. Temporal and UUID literals stay as SQL if
the native renderer would change their contents. Null tests and literals can
convert if their SQL round trip preserves their contents. SQL `= NULL` stays SQL.
Enum SQL literals stay SQL if the checker requires a named enum member.
The command reports each fallback with its source location and reason.
A second conversion makes no further changes. The command validates each proposed
change and its migration target before it returns files for replacement.

With history, the planner reconciles checked native expressions against the SQL
in the previous snapshot. Both plan directions and the next snapshot retain the
accepted old SQL and names. This also protects manual native replacements and
keeps their old definitions in an unrelated later migration.
A conversion with no rule change writes no migration or snapshot.
Explicit names match by physical name. A generated-name transition requires a
unique match with a previously generated name. All other index or exclusion
properties must agree. Ambiguous matches and real changes use ordinary migrations.
Raw SQL edits do not receive native-adoption reconciliation.
The builder retains the historical space format outside quotes and preserves
literal contents within quotes.

A database built from migration history retains the historical SQL.
Without history, conversion uses canonical SQL and generated names and reports
that distinction. A later rule change still generates forward and reverse SQL.
The converter proves equivalence only within the supported typed subset.

Pull uses the same translator and checker with the final mapped field names.
Unsupported whole predicates use `raw("...")` with the original SQL.
Unsupported keys, keys with options, and exclusion expression keys with `op:`
retain `sql(...)`. Introspected names stay explicit.
Connected pull and drift materialize schemas through the existing shadow path.
Their comparator also reconciles supported typed expressions to ignore redundant
parentheses. Other properties and unsupported expressions retain strict comparison.
