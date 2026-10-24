use super::*;
use crate::dsl::compile;

const SCHEMA: &str = r#"
enum Role {
  user
  admin
}

enum Level {
  low  @value(1)
  high @value(10)
  @@storage(int)
}

model Post {
  id      BigInt   @id
  a       Int
  b       Int
  c       Int
  title   String
  note    String?
  price   Decimal
  created DateTime
  active  Boolean
  role    Role
  level   Level
  author_id BigInt
  author  Author   @relation(fields: [author_id], references: [id])
}

model Author {
  id     BigInt  @id
  name   String
  active Boolean
  posts  Post[]
}
"#;

const SQLITE: &str = r#"
datasource db {
  provider = "sqlite"
}

model Post {
  id    BigInt @id
  title String
}
"#;

fn ir() -> SchemaIr {
    compile(SCHEMA, None).unwrap_or_else(|e| panic!("{e}"))
}

fn shown(context: Context, text: &str) -> String {
    match check_text(&ir(), "Post", context, text) {
        Ok(checked) => checked.root.to_string(),
        Err(e) => panic!("{text}: {e}"),
    }
}

fn error_in(ir: &SchemaIr, model: &str, context: Context, text: &str) -> Error {
    match check_text(ir, model, context, text) {
        Ok(checked) => panic!("{text}: expected an error, got {}", checked.root),
        Err(e) => e,
    }
}

fn error(context: Context, text: &str) -> Error {
    error_in(&ir(), "Post", context, text)
}

fn error_on(model: &str, context: Context, text: &str) -> Error {
    error_in(&ir(), model, context, text)
}

fn sqlite_error(text: &str) -> Error {
    error_in(&compile(SQLITE, None).unwrap_or_else(|e| panic!("{e}")), "Post", Context::Check, text)
}

#[test]
fn comparison_binds_tighter_than_and_and_and_tighter_than_or() {
    assert_eq!(shown(Context::Check, "a == 1 & b == 2 | c == 3"), "(((a == 1) & (b == 2)) | (c == 3))");
}

#[test]
fn an_unknown_field_names_its_span() {
    let e = error(Context::Check, "a == 1 & missing == 2");
    assert_eq!(e.span, Span { line: 1, column: 10, len: 7 });
    assert_eq!(e.message, "unknown field `missing` on model `Post`");
}

#[test]
fn a_parse_error_names_line_and_column() {
    let e = error(Context::Check, "a == ");
    assert_eq!((e.span.line, e.span.column), (1, 6));
    assert!(e.message.contains("expected a value"), "{e}");
}

#[test]
fn a_type_mismatch_names_both_types_at_the_operator() {
    let e = error(Context::Check, r#"price == "x""#);
    assert_eq!(e.span, Span { line: 1, column: 7, len: 2 });
    assert_eq!(e.message, "type mismatch: cannot compare Decimal with String");
    assert!(error(Context::Check, "title == 1").message.contains("String with Int"));
    assert!(error(Context::Check, "active == 1").message.contains("Boolean with Int"));
    assert!(error(Context::Check, "a & b").message.contains("`&` takes boolean operands, found Int"));
    assert!(error(Context::Check, "role < role").message.contains("`<` does not order Role"));
}

#[test]
fn numbers_compare_across_number_types_and_strings_across_string_types() {
    assert_eq!(shown(Context::Check, "a == price"), "(a == price)");
    assert_eq!(shown(Context::Check, "a < 1.5"), "(a < 1.5)");
    assert_eq!(shown(Context::Check, r#"title >= "m""#), r#"(title >= "m")"#);
    assert_eq!(shown(Context::Check, "~active & a + 1 * 2 >= -3"), "(~active & ((a + (1 * 2)) >= -3))");
}

#[test]
fn null_comparisons_become_null_tests() {
    assert_eq!(shown(Context::Check, "note == null"), "(note is null)");
    assert_eq!(shown(Context::Check, "null != note"), "(note is not null)");
    assert!(check_text(&ir(), "Post", Context::Check, "note == null").unwrap().warnings.is_empty());
    let e = error(Context::Check, "note < null");
    assert_eq!(e.message, "`null` only goes with `==` and `!=`; use `==` or `!=`");
}

#[test]
fn comparing_a_non_null_field_with_null_warns() {
    let checked = check_text(&ir(), "Post", Context::Check, "a == null").unwrap();
    assert_eq!(checked.root.to_string(), "(a is null)");
    assert_eq!(checked.warnings.len(), 1);
    assert_eq!(checked.warnings[0].span, Span { line: 1, column: 1, len: 1 });
    assert_eq!(checked.warnings[0].message, "`a` is not nullable, so `a == null` is always false");
    let checked = check_text(&ir(), "Post", Context::Check, "a != null").unwrap();
    assert_eq!(checked.warnings[0].message, "`a` is not nullable, so `a != null` is always true");
}

#[test]
fn an_enum_member_resolves_against_the_other_side_and_lowers_to_its_stored_value() {
    assert_eq!(shown(Context::Check, "role == admin"), "(role == Role.admin)");
    assert_eq!(shown(Context::Check, "admin != role"), "(Role.admin != role)");
    let checked = check_text(&ir(), "Post", Context::Check, "level == high").unwrap();
    let NodeKind::Binary(_, _, member) = &checked.root.kind else { panic!("{:?}", checked.root) };
    assert_eq!(member.ty, Ty::Enum("Level".into()));
    assert_eq!(
        member.kind,
        NodeKind::EnumMember { enum_name: "Level".into(), member: "high".into(), value: serde_json::json!(10) }
    );
    let checked = check_text(&ir(), "Post", Context::Check, "role == admin").unwrap();
    let NodeKind::Binary(_, _, member) = &checked.root.kind else { panic!("{:?}", checked.root) };
    assert!(matches!(&member.kind, NodeKind::EnumMember { value, .. } if value == "admin"));
}

#[test]
fn a_wrong_enum_member_fails_at_the_name() {
    let e = error(Context::Check, "role == root");
    assert_eq!(e.span, Span { line: 1, column: 9, len: 4 });
    assert_eq!(e.message, "enum `Role` has no member `root`");
    assert_eq!(error(Context::Check, "role == high").message, "enum `Role` has no member `high`");
    assert!(error(Context::Check, "role == level").message.contains("cannot compare Role with Level"));
    assert!(error(Context::Check, r#"role == "admin""#).message.contains("write the member `admin`, not a string"));
    assert_eq!(error(Context::Check, "admin == user").message, "unknown field `admin` on model `Post`");
}

#[test]
fn a_string_compared_with_a_date_time_must_be_rfc_3339() {
    assert_eq!(shown(Context::Check, r#"created > "2024-01-01T00:00:00Z""#), r#"(created > "2024-01-01T00:00:00Z")"#);
    assert_eq!(shown(Context::Check, r#""2024-01-01T00:00:00+02:00" < created"#), r#"("2024-01-01T00:00:00+02:00" < created)"#);
    let e = error(Context::Check, r#"created > "yesterday""#);
    assert_eq!(e.span, Span { line: 1, column: 11, len: 11 });
    assert!(e.message.contains("RFC 3339"), "{e}");
}

#[test]
fn methods_check_their_receiver_and_arguments() {
    assert_eq!(shown(Context::Check, "role.in_([user, admin])"), "role.in_([Role.user, Role.admin])");
    assert_eq!(shown(Context::Check, "a.not_in([1, 2])"), "a.not_in([1, 2])");
    assert_eq!(shown(Context::Check, "a.between(1, 5)"), "a.between(1, 5)");
    assert_eq!(shown(Context::Check, "note.is_null()"), "(note is null)");
    for method in ["like", "ilike", "contains", "icontains", "startswith", "endswith"] {
        let text = format!(r#"title.{method}("a")"#);
        assert_eq!(shown(Context::Check, &text), text);
    }
    let group = r#"(title).startswith("a") & a.in_([1])"#;
    assert_eq!(shown(Context::Check, group), r#"(title.startswith("a") & a.in_([1]))"#);
}

#[test]
fn a_method_mistake_fails_at_the_method() {
    let e = error(Context::Check, r#"a.contains("x")"#);
    assert_eq!(e.span, Span { line: 1, column: 3, len: 8 });
    assert_eq!(e.message, "`contains` takes a string, found Int");
    assert_eq!(error(Context::Check, "title.shout()").message, "unknown method `shout`");
    assert_eq!(error(Context::Check, "a.in_(1)").message, "`in_` takes a list");
    assert!(error(Context::Check, r#"a.in_(["x"])"#).message.contains("cannot compare Int with String"));
    assert_eq!(error(Context::Check, "a.between(1)").message, "`between` takes 2 arguments, found 1");
    assert_eq!(error(Context::Check, "role.in_([root])").message, "enum `Role` has no member `root`");
    let checked = check_text(&ir(), "Post", Context::Check, "a.is_null()").unwrap();
    assert_eq!(checked.warnings[0].message, "`a` is not nullable, so `a.is_null()` is always false");
}

#[test]
fn catalog_functions_check_their_arguments_and_return_a_type() {
    assert_eq!(shown(Context::Check, r#"lower(title) == "a""#), r#"(lower(title) == "a")"#);
    assert_eq!(shown(Context::Check, "char_length(upper(title)) > a"), "(char_length(upper(title)) > a)");
    assert_eq!(shown(Context::Check, r#"coalesce(note, title, "x") != "y""#), r#"(coalesce(note, title, "x") != "y")"#);
    assert_eq!(shown(Context::Check, "abs(a - b) > 1"), "(abs((a - b)) > 1)");
    assert_eq!(shown(Context::Check, "now() > created"), "(now() > created)");
    let checked = check_text(&ir(), "Post", Context::IndexKey, "char_length(title)").unwrap();
    assert_eq!(checked.root.ty, Ty::Scalar(ColType::Int));
    let NodeKind::Key { inner, .. } = &checked.root.kind else { panic!("{:?}", checked.root) };
    let NodeKind::Call { function, .. } = &inner.kind else { panic!("{inner:?}") };
    assert_eq!(function.sql(Dialect::Sqlite), Some("length({0})"));
    assert_eq!(function.sql(Dialect::Postgres), Some("char_length({0})"));
    assert_eq!(function.volatility, Volatility::Immutable);
}

#[test]
fn a_function_mistake_fails_at_the_call() {
    let e = error(Context::Check, "nope(a) == 1");
    assert_eq!(e.span, Span { line: 1, column: 1, len: 4 });
    assert_eq!(e.message, "unknown function `nope`; write `sql(\"...\")` for raw SQL");
    assert_eq!(error(Context::Check, "lower(title, title) == title").message, "`lower` takes 1 argument, found 2");
    assert_eq!(error(Context::Check, "coalesce() == 1").message, "`coalesce` takes at least 1 argument, found 0");
    let e = error(Context::Check, "lower(a) == title");
    assert_eq!(e.span, Span { line: 1, column: 7, len: 1 });
    assert_eq!(e.message, "`lower` takes a string, found Int");
    assert!(error(Context::Check, r#"coalesce(a, "x") == 1"#).message.contains("cannot compare Int with String"));
    assert_eq!(error(Context::Check, "abs(title) == 1").message, "`abs` takes a number, found String");
}

#[test]
fn sql_escape_hatch_matches_every_type() {
    assert_eq!(shown(Context::Check, r#"sql("a % 2") == 0"#), r#"(sql("a % 2") == 0)"#);
    assert_eq!(shown(Context::Check, r#"title == sql("x")"#), r#"(title == sql("x"))"#);
    assert_eq!(error(Context::Check, "sql(1) == 0").message, "`sql` takes one string");
}

#[test]
fn a_function_without_sql_for_the_dialect_fails() {
    let e = sqlite_error("now() == id");
    assert_eq!(e.span, Span { line: 1, column: 1, len: 3 });
    assert_eq!(e.message, "`now` has no SQL for SQLite; write `sql(\"...\")`");
}

#[test]
fn an_index_takes_only_immutable_functions() {
    assert_eq!(shown(Context::IndexFilter, r#"lower(title) == "a""#), r#"(lower(title) == "a")"#);
    let e = error(Context::IndexFilter, "created > now()");
    assert_eq!(e.span, Span { line: 1, column: 11, len: 3 });
    assert_eq!(e.message, "`now` is not immutable, so an index cannot use it");
    assert_eq!(error(Context::IndexKey, "now()").message, "`now` is not immutable, so an index cannot use it");
}

#[test]
fn an_index_key_field_may_carry_options() {
    assert_eq!(shown(Context::IndexKey, "created(sort: Desc)"), "created desc");
    assert_eq!(shown(Context::IndexKey, "lower(title)"), "lower(title)");
    assert!(error(Context::Check, "created(sort: Desc)").message.contains("unknown function `created`"));
}

#[test]
fn a_default_filter_follows_to_one_relations() {
    assert_eq!(shown(Context::DefaultFilter, "author.active"), "author.active");
    assert_eq!(shown(Context::DefaultFilter, r#"author.name == "a" & active"#), r#"((author.name == "a") & active)"#);
    let checked = check_text(&ir(), "Post", Context::DefaultFilter, "author.active").unwrap();
    assert_eq!(
        checked.root.kind,
        NodeKind::Field {
            via: vec!["author".into()],
            model: "Author".into(),
            field: "active".into(),
            spans: vec![Span { line: 1, column: 1, len: 6 }, Span { line: 1, column: 8, len: 6 }],
        }
    );
    assert_eq!(checked.root.span, Span { line: 1, column: 1, len: 13 });
    assert_eq!(shown(Context::DefaultFilter, "author.name.startswith(\"a\")"), "author.name.startswith(\"a\")");
}

#[test]
fn every_name_of_a_path_keeps_its_own_span() {
    let checked = check_text(&ir(), "Post", Context::DefaultFilter, "author . active").unwrap();
    let NodeKind::Field { spans, .. } = &checked.root.kind else { panic!("{:?}", checked.root.kind) };
    assert_eq!(spans, &vec![Span { line: 1, column: 1, len: 6 }, Span { line: 1, column: 10, len: 6 }]);
    assert_eq!(checked.root.span, Span { line: 1, column: 1, len: 15 });
}

#[test]
fn a_relation_path_mistake_names_the_step() {
    let e = error_on("Author", Context::DefaultFilter, "posts.title == \"a\"");
    assert_eq!(e.span, Span { line: 1, column: 1, len: 11 });
    assert_eq!(e.message, "`posts` is a to-many relation; a path follows only to-one relations");
    assert_eq!(error(Context::DefaultFilter, "author.missing").message, "unknown field `missing` on model `Author`");
    assert_eq!(error(Context::DefaultFilter, "author == 1").message, "`author` is a relation; name one of its fields, e.g. `author.id`");
    assert_eq!(error(Context::DefaultFilter, "a.b").message, "`a` is a field, not a relation");
    for context in [Context::Check, Context::IndexFilter, Context::IndexKey] {
        let e = error(context, "author.active");
        assert_eq!(e.message, "`author.active` follows a relation; only a default filter can");
    }
}

#[test]
fn scope_and_parent_belong_to_a_default_filter() {
    let checked = check_text(&ir(), "Post", Context::DefaultFilter, "author_id == scope.owner").unwrap();
    assert_eq!(checked.root.to_string(), "(author_id == scope.owner)");
    let NodeKind::Binary(_, _, scope) = &checked.root.kind else { panic!("{:?}", checked.root) };
    assert_eq!(scope.ty, Ty::Scalar(ColType::BigInt));
    assert_eq!(shown(Context::DefaultFilter, "scope.owner == author_id"), "(scope.owner == author_id)");
    assert_eq!(shown(Context::DefaultFilter, "parent.default_filter & active"), "(parent.default_filter & active)");
    let checked = check_text(&ir(), "Post", Context::DefaultFilter, "parent.default_filter").unwrap();
    assert_eq!(checked.root.ty, BOOL);
    assert_eq!(error(Context::DefaultFilter, "scope.owner").message, "`scope.owner` takes the type of the other side of a comparison");
    assert_eq!(error(Context::DefaultFilter, "parent.title").message, "`parent` has only `default_filter`");
    assert_eq!(error(Context::Check, "a == scope.owner").message, "`scope` is only available in a default filter");
    assert_eq!(error(Context::IndexFilter, "parent.default_filter").message, "`parent.default_filter` is only available in a default filter");
}

#[test]
fn word_operators_name_the_symbol_to_write() {
    let e = error(Context::Check, "a == 1 and b == 2");
    assert_eq!(e.message, "`and` is not an operator; use `&`");
    assert_eq!((e.span.line, e.span.column), (1, 8));
    assert_eq!(error(Context::Check, "a == 1 or b == 2").message, "`or` is not an operator; use `|`");
    assert_eq!(error(Context::Check, "not active").message, "`not` is not an operator; use `~`");
}

#[test]
fn an_expression_stays_on_one_line() {
    let e = error(Context::Check, "a == 1 &\n b == 2");
    assert_eq!((e.span.line, e.span.column), (2, 2));
    assert_eq!(e.message, "an expression must be on one line");
    assert_eq!(shown(Context::Check, "(a == 1 & b == 2)"), "((a == 1) & (b == 2))");
}

#[test]
fn comparisons_do_not_chain() {
    let e = error(Context::Check, "a < b < c");
    assert_eq!(e.message, "comparisons do not chain; join them with `&`");
    assert_eq!((e.span.line, e.span.column), (1, 7));
}

#[test]
fn unary_operators_bind_as_documented() {
    assert_eq!(shown(Context::Check, "~active == true"), "~(active == true)");
    assert_eq!(shown(Context::Check, "-(a + 1) < 0"), "(-(a + 1) < 0)");
    assert_eq!(shown(Context::Check, "a - -b > 0"), "((a - -b) > 0)");
    assert_eq!(shown(Context::Check, "a -5 > 0"), "((a - 5) > 0)");
    assert_eq!(shown(Context::Check, "a == -5"), "(a == -5)");
    assert_eq!(shown(Context::Check, "-a * 2 < 0"), "((-a * 2) < 0)");
    assert_eq!(error(Context::Check, "-title < 0").message, "`-` takes a number, found String");
    assert_eq!(error(Context::Check, "~a").message, "`~` takes boolean operands, found Int");
}

#[test]
fn the_parser_seam_returns_a_value_tree_without_a_schema() {
    let (_, value) = syntax::parse_expression("a == 1 & ~b").unwrap();
    assert!(matches!(value, Value::Binary(BinOp::And, ..)), "{value:?}");
    let e = syntax::parse_expression("a == 1 b").unwrap_err();
    assert_eq!((e.pos.line, e.pos.col), (1, 8));
}

#[test]
fn a_warning_has_line_column_and_length() {
    let checked = check_text(&ir(), "Post", Context::Check, "a == null").unwrap();
    assert_eq!(
        checked.warnings,
        [Diagnostic {
            span: Span { line: 1, column: 1, len: 1 },
            message: "`a` is not nullable, so `a == null` is always false".into()
        }]
    );
    let project = crate::dsl::compile_project(SCHEMA, None).unwrap();
    assert!(project.warnings.is_empty());
}

#[test]
fn a_leading_minus_on_an_index_key_field_means_descending() {
    assert_eq!(shown(Context::IndexKey, "-created"), "created desc");
    assert_eq!(shown(Context::IndexKey, "-b"), "b desc");
    assert_eq!(shown(Context::Check, "-b < 0"), "(-b < 0)");
    assert_eq!(error(Context::IndexKey, "-lower(title)").message, "`-` takes a number, found String");
}

#[test]
fn a_null_test_through_a_relation_gives_no_warning() {
    let checked = check_text(&ir(), "Post", Context::DefaultFilter, "author.active == null").unwrap();
    assert!(checked.warnings.is_empty(), "{:?}", checked.warnings);
}

#[test]
fn a_minus_below_an_index_key_root_is_negation() {
    assert_eq!(shown(Context::IndexKey, "-a * b"), "(-a * b)");
    assert_eq!(shown(Context::IndexKey, "coalesce(-a, 1)"), "coalesce(-a, 1)");
    assert_eq!(error(Context::IndexKey, "lower(-title)").message, "`-` takes a number, found String");
    assert_eq!(shown(Context::Check, "a > -(-1)"), "(a > --1)");
}

#[test]
fn an_index_key_keeps_its_options() {
    assert_eq!(shown(Context::IndexKey, "created(sort: Desc, nulls: last)"), "created desc nulls last");
    assert_eq!(shown(Context::IndexKey, "created(nulls: first)"), "created nulls first");
    assert_eq!(shown(Context::IndexKey, "-created(nulls: last)"), "created desc nulls last");
    assert_eq!(
        shown(Context::IndexKey, r#"title(ops: text_pattern_ops, collate: "C")"#),
        "title ops text_pattern_ops collate C"
    );
    assert_eq!(shown(Context::IndexKey, r#"sql("lower(title)", sort: Desc)"#), r#"sql("lower(title)") desc"#);
    assert_eq!(error(Context::IndexKey, "created(bogus: 1)").message, "index key: unknown argument `bogus`");
    assert_eq!(error(Context::IndexKey, r#"-sql("x")"#).message, "an expression key takes sort: Desc, not `-`");
    assert_eq!(error(Context::IndexKey, "-created(sort: Desc)").message, "index: give `-` or sort:, not both");
}

#[test]
fn the_root_has_the_type_its_context_needs() {
    let e = error(Context::Check, "a + 1");
    assert_eq!(e.message, "a check must be a boolean expression, found Int");
    let e = error(Context::Check, "title");
    assert_eq!(e.message, "a check must be a boolean expression, found String");
    assert_eq!(e.span, Span { line: 1, column: 1, len: 5 });
    assert_eq!(error(Context::IndexFilter, "price").message, "an index filter must be a boolean expression, found Decimal");
    assert_eq!(error(Context::DefaultFilter, "a + 1").message, "a default filter must be a boolean expression, found Int");
    assert_eq!(shown(Context::Check, r#"sql("a > 1")"#), r#"sql("a > 1")"#);
    assert_eq!(shown(Context::IndexKey, "a + 1"), "(a + 1)");
}

#[test]
fn scope_belongs_to_a_comparison() {
    let message = "`scope.x` takes the type of the other side of a comparison";
    assert_eq!(error(Context::DefaultFilter, "a + scope.x > 1").message, message);
    assert_eq!(error(Context::DefaultFilter, "scope.x & active").message, message);
    assert_eq!(shown(Context::DefaultFilter, "a < scope.x"), "(a < scope.x)");
}

#[test]
fn an_expression_never_spans_lines_inside_a_list_a_call_or_a_group() {
    // The error points at the first token on the later line, or at a string that spans lines.
    for (text, at) in [
        ("a.in_([1,\n2])", (2, 1)),
        ("lower(\ntitle) == \"x\"", (2, 1)),
        ("(a == 1\n)", (2, 1)),
        ("sql(\"\"\"a\nb\"\"\") == 1", (1, 5)),
    ] {
        let e = error(Context::Check, text);
        assert_eq!(e.message, "an expression must be on one line", "{text}");
        assert_eq!((e.span.line, e.span.column), at, "{text}");
    }
}

#[test]
fn a_word_operator_on_a_later_line_names_the_symbol() {
    let e = error(Context::Check, "a == 1\nand b == 2");
    assert_eq!(e.message, "`and` is not an operator; use `&`");
    assert_eq!((e.span.line, e.span.column), (2, 1));
}

#[test]
fn spans_cover_the_source_text() {
    let e = error(Context::Check, r#"created == "a\"b""#);
    assert!(e.message.contains("RFC 3339"), "{}", e.message);
    assert_eq!(e.span, Span { line: 1, column: 12, len: 6 });
    let e = error(Context::Check, r#"created == """x""""#);
    assert_eq!(e.span, Span { line: 1, column: 12, len: 7 });
    let checked = check_text(&ir(), "Post", Context::IndexKey, "1_000").unwrap();
    assert_eq!(checked.root.span, Span { line: 1, column: 1, len: 5 });
    assert_eq!(error(Context::Check, "- missing > 1").span, Span { line: 1, column: 3, len: 7 });
    assert_eq!(error(Context::Check, r#"a . contains("x")"#).span, Span { line: 1, column: 5, len: 8 });
}
