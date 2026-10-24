//! The lowering seam: schema text in, the SQL text of the IR out, per dialect.

use crate::ir::{ConstraintIr, SchemaIr};

const PG: &str = "datasource db {\n  provider = \"postgresql\"\n}\n";
const LITE: &str = "datasource db {\n  provider = \"sqlite\"\n}\n";

fn schema(datasource: &str, attrs: &str) -> String {
    format!(
        "{datasource}enum Status {{\n  open\n  closed\n  @@storage(text)\n}}\n\
         model Item {{\n  id Int @id\n  price Int\n  qty Int\n  name String\n  email String\n  active Boolean\n  note String?\n  status Status\n  \
         total Int @map(\"Total Amount\")\n  starts DateTime\n  ends DateTime\n{attrs}\n}}\n"
    )
}

fn lower(datasource: &str, attrs: &str) -> Result<SchemaIr, String> {
    super::super::compile(&schema(datasource, attrs), None)
}

fn check_sql(datasource: &str, expr: &str) -> Result<String, String> {
    let ir = lower(datasource, &format!("  @@check({expr})"))?;
    match &ir.models[0].constraints[..] {
        [ConstraintIr::Check { expr, .. }] => Ok(expr.clone()),
        other => panic!("one check expected, found {other:?}"),
    }
}

#[test]
fn check_operators_lower_per_dialect() {
    // (expression, Postgres, SQLite)
    let table: &[(&str, &str, &str)] = &[
        ("price > 0", "price > 0", "price > 0"),
        ("price != qty", "price <> qty", "price <> qty"),
        ("price > 0 & qty > 0", "price > 0 AND qty > 0", "price > 0 AND qty > 0"),
        ("(price > 0 | qty > 0) & active", "(price > 0 OR qty > 0) AND active", "(price > 0 OR qty > 0) AND active"),
        ("~active", "NOT active", "NOT active"),
        ("~(price > 0)", "NOT (price > 0)", "NOT (price > 0)"),
        ("active == true", "active = TRUE", "active = 1"),
        ("status == open", "status = 'open'", "status = 'open'"),
        ("note == null", "note IS NULL", "note IS NULL"),
        ("note != null", "note IS NOT NULL", "note IS NOT NULL"),
        ("note.is_null() | name == \"it's\"", "note IS NULL OR name = 'it''s'", "note IS NULL OR name = 'it''s'"),
        ("name.in_([\"a\", \"b\"])", "name IN ('a', 'b')", "name IN ('a', 'b')"),
        ("price.not_in([1, 2])", "price NOT IN (1, 2)", "price NOT IN (1, 2)"),
        ("price.between(1, 10)", "price BETWEEN 1 AND 10", "price BETWEEN 1 AND 10"),
        ("total > 0", "\"Total Amount\" > 0", "\"Total Amount\" > 0"),
        ("price - qty * 2 > 0", "price - qty * 2 > 0", "price - qty * 2 > 0"),
        ("(price - qty) * 2 > 0", "(price - qty) * 2 > 0", "(price - qty) * 2 > 0"),
        ("price - (qty - 1) > 0", "price - (qty - 1) > 0", "price - (qty - 1) > 0"),
        ("-price < 0", "-price < 0", "-price < 0"),
        ("lower(email) == email", "lower(email) = email", "lower(email) = email"),
        ("char_length(name) > 2", "char_length(name) > 2", "length(name) > 2"),
        ("coalesce(note, name) != \"\"", "coalesce(note, name) <> ''", "coalesce(note, name) <> ''"),
        ("name.contains(\"x\")", "strpos(name, 'x') > 0", "instr(name, 'x') > 0"),
        ("name.startswith(\"x\")", "substr(name, 1, length('x')) = 'x'", "substr(name, 1, length('x')) = 'x'"),
        ("name.endswith(\"x\")", "right(name, length('x')) = 'x'", "substr(name, length(name) - length('x') + 1) = 'x'"),
        ("name.endswith(\"\")", "right(name, length('')) = ''", "substr(name, length(name) - length('') + 1) = ''"),
        ("name.ilike(\"a%\")", "name ILIKE 'a%'", "LOWER(name) LIKE LOWER('a%') ESCAPE '\\'"),
        ("name.icontains(\"a_b\")", "name ILIKE '%a\\_b%'", "LOWER(name) LIKE LOWER('%a\\_b%') ESCAPE '\\'"),
        (
            "name.icontains(email)",
            "name ILIKE '%' || replace(replace(replace(email, '\\', '\\\\'), '%', '\\%'), '_', '\\_') || '%'",
            "LOWER(name) LIKE LOWER('%' || replace(replace(replace(email, '\\', '\\\\'), '%', '\\%'), '_', '\\_') || '%') ESCAPE '\\'",
        ),
        ("name.like(\"a%\")", "name LIKE 'a%'", ""),
        ("sql(\"price > 0\")", "price > 0", "price > 0"),
        ("raw(\"price > 0\")", "price > 0", "price > 0"),
        ("\"price > 0\"", "price > 0", "price > 0"),
    ];
    for (expr, postgres, sqlite) in table {
        assert_eq!(check_sql(PG, expr).as_deref(), Ok(*postgres), "postgres: {expr}");
        if sqlite.is_empty() {
            let message = check_sql(LITE, expr).unwrap_err();
            assert!(message.contains("sql(\"...\")"), "{message}");
        } else {
            assert_eq!(check_sql(LITE, expr).as_deref(), Ok(*sqlite), "sqlite: {expr}");
        }
    }
}

#[test]
fn a_placeholder_in_a_literal_stays_text() {
    for datasource in [PG, LITE] {
        assert_eq!(check_sql(datasource, "coalesce(note, \"{0}\") == name").as_deref(), Ok("coalesce(note, '{0}') = name"));
        assert_eq!(check_sql(datasource, "coalesce(note, \"{1} {*}\") == name").as_deref(), Ok("coalesce(note, '{1} {*}') = name"));
    }
}

#[test]
fn a_column_named_like_a_reserved_word_is_quoted() {
    let words = ["current_user", "current_role", "current_catalog", "current_schema", "system_user", "asymmetric"];
    let fields: String = words.iter().map(|w| format!("  {w} Int\n")).collect();
    let checks: String = words.iter().map(|w| format!("  @@check({w} > 0)\n")).collect();
    let expected: Vec<String> = words.iter().map(|w| format!("\"{w}\" > 0")).collect();
    for datasource in [PG, LITE] {
        let ir = super::super::compile(&format!("{datasource}model Word {{\n  id Int @id\n{fields}{checks}}}\n"), None).unwrap();
        let texts: Vec<&str> = ir.models[0]
            .constraints
            .iter()
            .filter_map(|c| match c {
                ConstraintIr::Check { expr, .. } => Some(expr.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(texts, expected);
    }
}

fn typed_sql(datasource: &str, expr: &str) -> Result<String, String> {
    let model = format!("model Ev {{\n  id Int @id\n  seen_at DateTime\n  born DateTime @db.Date\n  uid String @db.Uuid\n  label String\n  @@check({expr})\n}}\n");
    match &super::super::compile(&format!("{datasource}{model}"), None)?.models[0].constraints[..] {
        [ConstraintIr::Check { expr, .. }] => Ok(expr.clone()),
        other => panic!("one check expected, found {other:?}"),
    }
}

#[test]
fn a_typed_literal_takes_the_stored_text_form_on_sqlite() {
    // SQLite compares these columns as text; the engine stores UTC `%Y-%m-%d %H:%M:%S%.f`,
    // `YYYY-MM-DD` and a lower-case hyphenated UUID. Postgres keeps the text as written.
    // (expression, Postgres, SQLite)
    let table: &[(&str, &str, &str)] = &[
        ("seen_at > \"2024-01-01T00:00:00Z\"", "seen_at > '2024-01-01T00:00:00Z'", "seen_at > '2024-01-01 00:00:00'"),
        ("\"2024-01-01T00:00:00Z\" < seen_at", "'2024-01-01T00:00:00Z' < seen_at", "'2024-01-01 00:00:00' < seen_at"),
        (
            "seen_at.between(\"2024-01-01T02:00:00+02:00\", \"2024-01-02T00:00:00.5Z\")",
            "seen_at BETWEEN '2024-01-01T02:00:00+02:00' AND '2024-01-02T00:00:00.5Z'",
            "seen_at BETWEEN '2024-01-01 00:00:00' AND '2024-01-02 00:00:00.500'",
        ),
        ("seen_at.in_([\"2024-01-01T00:00:00Z\"])", "seen_at IN ('2024-01-01T00:00:00Z')", "seen_at IN ('2024-01-01 00:00:00')"),
        (
            "coalesce(seen_at, \"2024-01-01T00:00:00Z\") > \"2024-01-01T00:00:00Z\"",
            "coalesce(seen_at, '2024-01-01T00:00:00Z') > '2024-01-01T00:00:00Z'",
            "coalesce(seen_at, '2024-01-01 00:00:00') > '2024-01-01 00:00:00'",
        ),
        ("born > \"2024-1-5\"", "born > '2024-1-5'", "born > '2024-01-05'"),
        (
            "uid == \"550E8400-E29B-41D4-A716-446655440000\"",
            "uid = '550E8400-E29B-41D4-A716-446655440000'",
            "uid = '550e8400-e29b-41d4-a716-446655440000'",
        ),
        ("uid == \"550e8400e29b41d4a716446655440000\"", "uid = '550e8400e29b41d4a716446655440000'", "uid = '550e8400-e29b-41d4-a716-446655440000'"),
        // A string column keeps every literal as written.
        ("label == \"2024-01-01T00:00:00Z\"", "label = '2024-01-01T00:00:00Z'", "label = '2024-01-01T00:00:00Z'"),
    ];
    for (expr, postgres, sqlite) in table {
        assert_eq!(typed_sql(PG, expr).as_deref(), Ok(*postgres), "postgres: {expr}");
        assert_eq!(typed_sql(LITE, expr).as_deref(), Ok(*sqlite), "sqlite: {expr}");
    }
}

#[test]
fn field_check_and_filters_lower() {
    for (datasource, boolean) in [(PG, "TRUE"), (LITE, "1")] {
        let ir = super::super::compile(
            &format!(
                "{datasource}model Item {{\n  id Int @id\n  price Int @check(price > 0)\n  active Boolean\n  email String\n  \
                 @@index([email], where: active == true)\n  @@index([lower(email), price(sort: Desc)], unique: true)\n}}\n"
            ),
            None,
        )
        .unwrap();
        let model = &ir.models[0];
        assert_eq!(model.fields[1].check.as_deref(), Some("price > 0"));
        assert_eq!(model.indexes[0].where_, Some(format!("active = {boolean}")));
        assert_eq!(model.indexes[1].columns[0].field, None);
        assert_eq!(model.indexes[1].columns[0].expr.as_deref(), Some("lower(email)"));
        assert_eq!(model.indexes[1].columns[1].field.as_deref(), Some("price"));
        assert!(model.indexes[1].columns[1].desc);
    }
}

#[test]
fn exclusion_keys_and_filter_lower() {
    let ir = lower(PG, "  @@exclude([name(op: \"=\"), sql(\"tstzrange(starts, ends)\", op: \"&&\")], where: price > 0)").unwrap();
    let ConstraintIr::Exclude { elements, where_, .. } = &ir.models[0].constraints[0] else { panic!("an exclusion") };
    assert_eq!(where_.as_deref(), Some("price > 0"));
    assert_eq!(elements[1].column.expr.as_deref(), Some("tstzrange(starts, ends)"));
    let message = lower(PG, "  @@exclude([price + qty], where: price > 0)").unwrap_err();
    assert!(message.contains("op") && message.contains("sql("), "{message}");
    let message = lower(PG, "  @@index([price + qty(sort: Desc)])").unwrap_err();
    assert!(message.contains("sql("), "{message}");
}

#[test]
fn native_index_key_and_filter_lower() {
    let ir = lower(PG, "  @@index([lower(email), price + qty], where: active & price > 0)").unwrap();
    let index = &ir.models[0].indexes[0];
    assert_eq!(index.columns[0].expr.as_deref(), Some("lower(email)"));
    assert_eq!(index.columns[1].expr.as_deref(), Some("price + qty"));
    assert_eq!(index.where_.as_deref(), Some("active AND price > 0"));
    let ir = lower(LITE, "  @@index([lower(email)], where: active)").unwrap();
    assert_eq!(ir.models[0].indexes[0].where_.as_deref(), Some("active"));
}

#[test]
fn partition_blocks_keep_text_and_reject_native_expressions() {
    let base = "datasource db {\n  provider = \"postgresql\"\n}\nmodel Ev {\n  id Int\n  kind String\n  amount Int\n  @@id([id, kind])\n  @@partition(list: [kind])\n}\n";
    let with = |check: &str| super::super::compile_project(&format!("{base}partition ev_a {{\n of = Ev\n values = [\"a\"]\n @@check({check})\n}}\n"), None);
    let project = with("\"amount > 0\"").unwrap();
    assert!(project.warnings.is_empty(), "{:?}", project.warnings);
    assert!(with("raw(\"amount > 0\")").unwrap().warnings.is_empty());
    let message = with("amount > 0").err().unwrap();
    assert!(message.contains("check"), "{message}");
}

#[test]
fn a_plain_string_warns_and_raw_does_not() {
    let project = |attrs: &str| super::super::compile_project(&schema(PG, attrs), None).unwrap();
    let plain = project("  @@check(\"price > 0\")\n  @@index([name], where: \"active\")");
    assert_eq!(plain.warnings.len(), 2, "{:?}", plain.warnings);
    assert!(plain.warnings.iter().all(|w| w.message.contains("raw(")), "{:?}", plain.warnings);
    // The check is on schema line 21, the index filter on line 22, and the warning marks the value.
    assert_eq!((plain.warnings[0].span.line, plain.warnings[0].span.column), (21, 11));
    assert_eq!(plain.warnings[0].span.len as usize, "\"price > 0\"".len());
    assert_eq!(plain.warnings[1].span.line, 22);
    let ir = &plain.ir.models[0];
    assert!(matches!(&ir.constraints[0], ConstraintIr::Check { expr, .. } if expr == "price > 0"));
    assert_eq!(ir.indexes[0].where_.as_deref(), Some("active"));
    let raw = project("  @@check(raw(\"price > 0\"))\n  @@index([name], where: raw(\"active\"))");
    assert!(raw.warnings.is_empty(), "{:?}", raw.warnings);
    let native = project("  @@check(price > 0)");
    assert!(native.warnings.is_empty());
}

#[test]
fn a_minus_before_a_function_call_is_negation_and_before_a_field_is_desc() {
    let ir = lower(PG, "  @@index([-abs(qty), -price, -(price + qty)])").unwrap();
    let columns = &ir.models[0].indexes[0].columns;
    assert_eq!(columns[0].expr.as_deref(), Some("-abs(qty)"));
    assert!(!columns[0].desc);
    assert_eq!(columns[1].field.as_deref(), Some("price"));
    assert!(columns[1].desc);
    assert_eq!(columns[2].expr.as_deref(), Some("-(price + qty)"));
    assert!(!columns[2].desc);
}

#[test]
fn the_op_option_belongs_to_exclusions_and_the_messages_say_exclusion() {
    let message = lower(PG, "  @@index([price(op: \"=\")])").unwrap_err();
    assert!(message.contains("op"), "{message}");
    let message = lower(PG, "  @@exclude([price], where: price > 0)").unwrap_err();
    assert!(message.contains("exclusion") && message.contains("op"), "{message}");
    let message = lower(PG, "  @@exclude([price + qty(sort: Desc)])").unwrap_err();
    assert!(message.contains("exclusion") && !message.contains("index"), "{message}");
}

#[test]
fn a_where_argument_warns_at_the_extent_of_its_value() {
    let project = super::super::compile_project(&schema(PG, "  @@index([name], where: \"active\")"), None).unwrap();
    let warning = &project.warnings[0];
    assert_eq!((warning.span.line, warning.span.column, warning.span.len), (21, 26, "\"active\"".len() as u32));
    assert!(!warning.message.contains("::"), "{}", warning.message);
}

#[test]
fn a_type_member_check_stays_text() {
    let src = |check: &str| format!("{PG}type Addr {{\n  zip Int @check({check})\n}}\nmodel Shop {{\n  id Int @id\n  addr Addr\n}}\n");
    let project = super::super::compile_project(&src("\"{} > 0\""), None).unwrap();
    assert!(project.warnings.is_empty(), "{:?}", project.warnings);
    let message = super::super::compile_project(&src("{} > 0"), None).err().unwrap();
    assert!(message.contains("raw("), "{message}");
}
