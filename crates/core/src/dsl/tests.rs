use serde_json::json;

use super::*;

fn ok(src: &str) -> SchemaIr {
    compile(src, None).unwrap_or_else(|e| panic!("{e}"))
}

fn fails(src: &str) -> String {
    match compile(src, None).and_then(check) {
        Ok(_) => panic!("expected an error"),
        Err(e) => e,
    }
}

#[test]
fn bypass_roles_name_the_database_roles_that_skip_triggers() {
    let src = |roles: &str| format!("datasource db {{\n  provider = \"postgresql\"\n  {roles}\n}}\nmodel A {{\n  id BigInt @id\n}}\n");
    assert!(ok(&src("")).bypass_roles.is_empty());
    assert_eq!(ok(&src(r#"bypass_roles = ["app_admin", "ops", "ops"]"#)).bypass_roles, ["app_admin", "ops"]);
    assert!(fails(&src(r#"bypass_roles = "app_admin""#)).contains("bypass_roles takes a list of role names"));
    assert!(fails(&src(r#"bypass_roles = [""]"#)).contains("a bypass role is a non-empty string"));
}

#[test]
fn the_scope_keys_of_the_datasource_name_the_settings_of_scope() {
    let src = |keys: &str| format!("datasource db {{\n  provider = \"postgresql\"\n  {keys}\n}}\nmodel A {{\n  id BigInt @id\n}}\n");
    assert!(ok(&src("")).scope_settings.is_default());
    let ir = ok(&src("scope_prefix = \"tenant\"\n  scope_bypass = { setting = \"tenant.bypass\", value = \"on\" }\n  scope_role_check = true"));
    let s = &ir.scope_settings;
    assert_eq!((s.key_setting("shop").as_str(), s.bypass_setting(), s.bypass_value(), s.role_check, s.list_form()), ("tenant.shop", "tenant.bypass", Some("on"), true, true));
    let colon = ok(&src("scope_bypass = { setting: \"b.x\", value: \"1\" }"));
    assert_eq!((colon.scope_settings.bypass_setting(), colon.scope_settings.bypass_value()), ("b.x", Some("1")));
    assert!(ok(&src("scope_role_check = false")).scope_settings.is_default());
    for (keys, message) in [
        ("scope_prefix = \"\"", "scope_prefix is a non-empty string"),
        ("scope_prefix = 3", "scope_prefix is a non-empty string"),
        ("scope_bypass = \"on\"", "scope_bypass is { setting"),
        ("scope_bypass = { setting = \"b\" }", "scope_bypass is { setting"),
        ("scope_bypass = { setting = \"b\", value = \"\" }", "scope_bypass `value` is a non-empty string"),
        ("scope_bypass = { setting = \"b\", value = \"x\", extra = \"y\" }", "scope_bypass has no key `extra`"),
        ("scope_role_check = \"yes\"", "scope_role_check"),
    ] {
        let err = fails(&src(keys));
        assert!(err.contains(message), "{keys}: {err}");
    }
}

#[test]
fn the_hard_delete_setting_of_the_datasource_is_lowered_and_stamps_the_contract() {
    let src = |keys: &str| format!("datasource db {{\n  provider = \"postgresql\"\n  {keys}\n}}\nmodel A {{\n  id BigInt @id\n}}\n");
    let mut plain = ok(&src(""));
    assert!(plain.hard_delete_setting.is_empty());
    plain.stamp_schema_contract();
    assert!(plain.behavior.schema_contract_minor < 13, "a schema with no key stays at the old contract");
    let mut keyed = ok(&src("hard_delete_setting = \"rules.hard_deletion\""));
    assert_eq!(keyed.hard_delete_setting, "rules.hard_deletion");
    keyed.stamp_schema_contract();
    assert_eq!(keyed.behavior.schema_contract_minor, 13);
    for keys in ["hard_delete_setting = \"\"", "hard_delete_setting = 3"] {
        assert!(fails(&src(keys)).contains("hard_delete_setting is a non-empty string"), "{keys}");
    }
}

#[test]
fn exempt_roles_name_the_database_roles_that_read_every_row() {
    let src = |roles: &str| format!("datasource db {{\n  provider = \"postgresql\"\n  {roles}\n}}\nmodel A {{\n  id BigInt @id\n}}\n");
    assert!(ok(&src("")).exempt_roles.is_empty());
    assert_eq!(ok(&src(r#"exempt_roles = ["reporting", "etl", "etl"]"#)).exempt_roles, ["reporting", "etl"]);
    assert!(fails(&src(r#"exempt_roles = "reporting""#)).contains("exempt_roles takes a list of role names"));
    assert!(fails(&src(r#"exempt_roles = [""]"#)).contains("an exempt role is a non-empty string"));
}

#[test]
fn sqlite_targets_and_feature_checks() {
    let prefix = "datasource db {\n  provider = \"sqlite\"\n}\n";
    let source = format!("{prefix}model Item {{\n  id BigInt @id @default(autoincrement())\n  title String\n}}\n");
    let (ir, schema) = check(ok(&source)).unwrap();
    assert_eq!(ir.dialect, crate::dialect::Dialect::Sqlite);
    let snapshot = crate::migrate::snapshot(&schema).unwrap();
    assert_eq!(snapshot.dialect, ir.dialect);
    assert_eq!(snapshot.version, 2);
    assert_eq!(snapshot.tables[0].columns[0].ty, "INTEGER");
    assert!(crate::migrate::create_all(&schema).unwrap()[0].contains("PRIMARY KEY AUTOINCREMENT"));
    for (body, message) in [
        ("model Item {\n  id BigInt @id\n  values String[]\n}", "Item.values: sqlite does not support Arrays"),
        ("enum Role {\n  user\n}\nmodel Item {\n  id BigInt @id\n  role Role\n}", "choose @@storage(text)"),
        ("model Item {\n  id BigInt @id\n  amount Decimal\n}", "ExactDecimal"),
        ("model Item {\n  id BigInt @id\n  @@trigger(t, when: after, operation: [insert], level: statement, func: \"SELECT 1\")\n}", "StatementTriggers"),
        ("model Item {\n  id BigInt @id\n  @@trigger(t, when: after, operation: [insert, update], func: \"SELECT 1\")\n}", "one insert/update/delete event"),
        ("model Item {\n  id BigInt @id\n  @@index([id], type: Gin)\n}", "IndexMethods"),
        ("model Item {\n  id BigInt @id\n  @@unique([id], deferrable: deferred)\n}", "DeferrableUnique"),
    ] {
        assert!(fails(&format!("{prefix}{body}")).contains(message), "{message}");
    }
    let (_, postgres) = check(ok("model Item {\n  id BigInt @id\n}\n")).unwrap();
    let pg_snapshot = crate::migrate::snapshot(&postgres).unwrap();
    assert!(crate::migrate::plan(&schema, &pg_snapshot).err().unwrap().contains("separate migrations directory"));
    let legacy: crate::ir::SchemaIr = serde_json::from_str(r#"{"models":[]}"#).unwrap();
    assert_eq!(legacy.dialect, crate::dialect::Dialect::Postgres);
}

#[test]
fn blog_schema_compiles_to_the_ir_the_engine_expects() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/blog/schema.prisma");
    let (ir, _) = check(compile_file(&path).unwrap()).unwrap();
    let v = serde_json::to_value(&ir).unwrap();
    let model = |name: &str| v["models"].as_array().unwrap().iter().find(|m| m["name"] == name).unwrap().clone();
    let user = &model("User");
    assert_eq!(user["fields"][1], json!({"name": "email", "column": "email", "type": "string", "unique": true, "max_length": 254}));
    assert_eq!(user["relations"][0], json!({"name": "posts", "kind": "many", "target": "Post", "from": "id", "to": "author_id"}));
    let post = &model("Post");
    assert_eq!(post["table"], "posts");
    assert_eq!(
        post["fields"][0],
        json!({"name": "id", "column": "id", "type": "big_int", "primary_key": true, "auto_increment": true})
    );
    assert_eq!(post["fields"][1], json!({"name": "author_id", "column": "author_id", "type": "big_int", "index": true}));
    assert_eq!(post["fields"][3], json!({"name": "body", "column": "body", "type": "text"}));
    assert_eq!(post["fields"][4], json!({"name": "views", "column": "views", "type": "int", "default": 0}));
    assert_eq!(
        post["relations"][0],
        json!({"name": "author", "kind": "one", "target": "User", "from": "author_id", "to": "id",
               "foreign_key": true, "on_delete": "cascade"})
    );
    assert_eq!(post["relations"][1], json!({"name": "comments", "kind": "many", "target": "Comment", "from": "id", "to": "post_id"}));
    assert_eq!(
        post["indexes"][0],
        json!({"columns": [{"field": "author_id"}, {"field": "created_at", "desc": true}], "where": "published"})
    );
    assert_eq!(post["indexes"][1], json!({"columns": [{"field": "title", "opclass": "gin_trgm_ops"}], "method": "gin"}));
    assert_eq!(post["constraints"][0], json!({"kind": "check", "name": "posts_views_not_negative", "expr": "views >= 0"}));
    assert_eq!(model("Comment")["relations"][1]["on_delete"], "set_null");
    assert_eq!(model("User")["relations"][2], json!({"name": "profile", "kind": "one", "target": "Profile", "from": "id", "to": "user_id"}));
    assert_eq!(post["relations"][2]["through"], json!({"model": "PostTag", "source": "post_id", "target": "tag_id"}));
    assert_eq!(v["enums"][1]["storage"], "int");
}

#[test]
fn every_feature() {
    let ir = ok(r#"
datasource db {
  provider   = "postgresql"
  url        = env("DATABASE_URL")
  extensions = [postgis(schema: "ext", version: "3.4"), uuid_ossp(map: "uuid-ossp")]
}

generator client {
  provider = "prisma-client-js"
}

function audit_row {
returns  = trigger
language = plpgsql
body     = """
BEGIN
    RETURN NULL;
END;
"""
}

model Room {
  id         String                       @id @default(dbgenerated("gen_random_uuid()")) @db.Uuid
  name       String                       @unique @db.Citext
  floor      Int                          @check("floor >= 0") @comment("0 = ground")
  features   Json?                        @default("{\"wifi\": true}")
  embedding  Unsupported("vector(3)")?
  spot       Unsupported("geography(Point)")?
  title      String                       @map("label") @renamed_from("caption") @db.VarChar(80)
  price      Decimal                      @db.Decimal(10, 2)
  born_on    DateTime?                    @db.Date
  small      Int                          @db.SmallInt
  updated_at DateTime                     @default(now())
  bookings   Booking[]

  @@index([embedding(ops: vector_cosine_ops)], type: hnsw, with: { m: 16, ef_construction: 64 })
  @@index([sql("lower(label)", collate: "C"), floor(sort: Desc, nulls: last)], unique: true, include: [name], name: "rooms_x")
  @@unique([floor, title], nulls_not_distinct: true, deferrable: deferred)
  @@map("rooms")
  @@trigger(audit, when: after, operation: [insert, delete], level: statement, func: audit_row, args: ["rooms"])
  @@trigger(touch, when: before, operation: [update_of(title)], condition: "OLD.label <> NEW.label", func: "BEGIN RETURN NEW; END;")
  @@comment("bookable")
}

model Booking {
  id      BigInt   @id @default(autoincrement())
  room_id String   @db.Uuid
  starts  DateTime
  ends    DateTime
  room    Room     @relation(fields: [room_id], references: [id], onDelete: Restrict, onUpdate: Cascade, deferrable: immediate)

  @@exclude([room_id(op: "="), sql("tstzrange(starts, ends)", op: "&&")], where: "starts < ends")
}
"#);
    let (ir, schema) = check(ir).unwrap();
    let v = serde_json::to_value(&ir).unwrap();
    let room = &v["models"][0];
    assert_eq!(room["table"], "rooms");
    assert_eq!(room["comment"], "bookable");
    assert_eq!(room["fields"][0]["type"], "uuid");
    assert_eq!(room["fields"][0]["default_sql"], "gen_random_uuid()");
    assert_eq!(room["fields"][1]["db_type"], "citext");
    assert_eq!(room["fields"][1]["write_sql"], "CAST({} AS citext)");
    assert_eq!(room["fields"][3]["default"], json!({"wifi": true}));
    assert_eq!(room["fields"][4]["db_type"], "vector(3)");
    assert_eq!(room["fields"][4]["hints"]["python"], "list[float]");
    assert_eq!(room["fields"][5]["db_type"], "geography(Point, 4326)");
    assert_eq!(room["fields"][6]["column"], "label");
    assert_eq!(room["fields"][6]["max_length"], 80);
    assert_eq!(room["fields"][6]["renamed_from"], "caption");
    assert_eq!(
        room["fields"][7],
        json!({"name": "price", "column": "price", "type": "decimal", "db_type": "numeric(10, 2)"})
    );
    assert_eq!(room["fields"][8]["type"], "date");
    assert_eq!(room["fields"][9]["db_type"], "smallint");
    assert_eq!(room["relations"][0], json!({"name": "bookings", "kind": "many", "target": "Booking", "from": "id", "to": "room_id"}));
    assert_eq!(room["indexes"][0]["method"], "hnsw");
    assert_eq!(room["indexes"][0]["with"], json!([["m", "16"], ["ef_construction", "64"]]));
    assert_eq!(room["indexes"][1]["name"], "rooms_x");
    assert_eq!(room["triggers"][1]["update_of"], json!(["title"]));
    assert_eq!(room["triggers"][1]["body"], "BEGIN RETURN NEW; END;");
    let booking = &v["models"][1];
    assert_eq!(booking["table"], "booking");
    assert_eq!(booking["relations"][0]["on_update"], "cascade");
    assert_eq!(booking["relations"][0]["deferrable"], "immediate");
    assert_eq!(booking["constraints"][0]["requires"], json!(["btree_gist"]));
    assert_eq!(v["extensions"][0], json!({"name": "postgis", "schema": "ext", "version": "3.4"}));
    assert_eq!(v["extensions"][1], json!({"name": "uuid-ossp"}));
    assert_eq!(v["functions"][0]["body"], "BEGIN\n    RETURN NULL;\nEND;");

    let snap = crate::migrate::snapshot(&schema).unwrap();
    let exts: Vec<_> = snap.extensions.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(exts, ["btree_gist", "citext", "postgis", "uuid-ossp", "vector"]);
}

#[test]
fn named_relations_pair_up() {
    let ir = ok(r#"
model User {
  id      BigInt    @id
  written Message[] @relation("author")
  read    Message[] @relation("reader")
}

model Message {
  id        BigInt @id
  author_id BigInt
  reader_id BigInt
  author    User   @relation("author", fields: [author_id], references: [id])
  reader    User   @relation("reader", fields: [reader_id], references: [id])
}
"#);
    let user = &ir.models[0];
    assert_eq!((user.relations[0].to.as_slice(), user.relations[1].to.as_slice()), (&["author_id".to_string()][..], &["reader_id".to_string()][..]));
    let e = fails(r#"
model User {
  id      BigInt    @id
  written Message[]
}

model Message {
  id        BigInt @id
  author_id BigInt
  reader_id BigInt
  author    User   @relation("author", fields: [author_id], references: [id])
  reader    User   @relation("reader", fields: [reader_id], references: [id])
}
"#);
    assert!(e.contains("relation User.written: Message has no relation to User with fields:"), "{e}");
}

#[test]
fn imported_extension_files_add_types() {
    let dir = std::env::temp_dir().join(format!("monorm-dsl-test-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("ext")).unwrap();
    std::fs::write(
        dir.join("ext/acme.toml"),
        "name = \"acme\"\nfunctions = [\"acme_slug\"]\n[types.money]\nsql = \"numeric({p}, 2)\"\nargs = [\"p\"]\ndefaults = { p = \"12\" }\nvalue = \"text\"\npython = \"str\"\n",
    )
    .unwrap();
    let path = dir.join("schema.prisma");
    std::fs::write(
        &path,
        "import \"ext/acme.toml\"\nmodel Item {\n  id BigInt @id\n  price Unsupported(\"money\")\n  slug String @db.Text @default(dbgenerated(\"acme_slug()\"))\n}",
    )
    .unwrap();
    let (ir, schema) = check(compile_file(&path).unwrap()).unwrap();
    assert_eq!(ir.models[0].fields[1].db_type.as_deref(), Some("numeric(12, 2)"));
    assert_eq!(ir.models[0].fields[1].requires, ["acme"]);
    let snap = crate::migrate::snapshot(&schema).unwrap();
    assert_eq!(snap.extensions.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(), ["acme"]);
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn errors_point_at_the_problem() {
    let b = "model B {\n  id BigInt @id\n}";
    let cases = [
        ("model A {\n  id BigInt @id\n  x Strng\n}".to_owned(), "<schema>:3:5: A.x: unknown type Strng"),
        (
            "model A {\n  id BigInt @id\n  target Generic?\n}".into(),
            "unknown type Generic; expected a model or BigInt, Int, Float, Boolean, String, DateTime, Json, Decimal, Bytes, an enum or Unsupported(\"<extension type>\"); Generic belongs to the built-in generic extension and needs @generic.relation(...)",
        ),
        ("model A {\n  id BigInt @id @prim\n}".into(), "unknown attribute @prim"),
        ("model A {\n  x Int\n}".into(), "model A has no @id field"),
        (format!("model A {{\n  id BigInt @id\n  b B @relation(fields: [b_id], references: [id])\n}}\n{b}"), "no field b_id"),
        (
            format!("model A {{\n  id BigInt @id\n  b_id BigInt?\n  b B @relation(fields: [b_id], references: [id])\n}}\n{b}"),
            "so the relation type is `B?`",
        ),
        (format!("model A {{\n  id BigInt @id\n  b_id BigInt\n  b B @relation(fields: [b_id])\n}}\n{b}"), "needs references:"),
        (format!("model A {{\n  id BigInt @id\n  bs B[]\n}}\n{b}"), "B has no relation to A"),
        (format!("model A {{\n  id BigInt @id\n  bs B[] @relation(\"{{model}}_bs\")\n}}\n{b}"), "{model} in a relation name is only expanded"),
        (
            format!("model A {{\n  id BigInt @id\n  b_id BigInt\n  b B @relation(fields: [b_id], references: [id], onDelete: cascade)\n}}\n{b}"),
            "unknown referential action cascade",
        ),
        ("model A {\n  id BigInt @id\n  @@index([nope])\n}".into(), "nope"),
        ("model A {\n  id BigInt @id\n  @@index([id], wher: \"x\")\n}".into(), "unknown argument `wher`"),
        ("model A {\n  id BigInt @id\n  @@index([id(sort: desc)])\n}".into(), "sort must be Asc or Desc"),
        ("model A {\n  id BigInt @id\n  @@trigger(t, func: \"x\")\n}".into(), "say when it fires"),
        ("model A {\n  id BigInt @id\n  e Unsupported(\"vector\")\n}".into(), "needs argument \"dims\""),
        ("model A {\n  id BigInt @id\n  e Unsupported(\"nope(1)\")\n}".into(), "unknown type nope"),
        ("model A {\n  id BigInt @id\n  e String @db.Nope\n}".into(), "unknown native type @db.Nope"),
        ("model A {\n  id BigInt @id @default(foo())\n}".into(), "not foo"),
        ("enum E {\n  a\n  b\n}\nmodel A {\n  id BigInt @id\n  e E(a)\n}".into(), "A.e: an enum subset E(...) is only allowed on a field of a view model"),
        ("model A {\n  id String @id @default(cuid())\n}".into(), "A.id: cuid() is not supported; use @client_default(uuid())"),
        ("model A {\n  id String @id @default(uuid(5))\n}".into(), "uuid() takes no argument, 4 or 7"),
        ("model A {\n  id BigInt @id @client_default(autoincrement())\n}".into(), "A.id: only the database can make autoincrement(); use @default"),
        ("model A {\n  id String @id @client_default(dbgenerated(\"x\"))\n}".into(), "only the database can make dbgenerated()"),
        ("model A {\n  id String @id @client_default(cuid())\n}".into(), "@client_default takes a literal, uuid(), uuid7() or now(), not cuid()"),
        ("model A {\n  id String @id @client_default(uuid(7))\n}".into(), "uuid() takes no arguments"),
        ("model A {\n  id BigInt @id @client_default(uuid())\n}".into(), "A.id: @client_default uuid does not fit a bigint field"),
        ("model A {\n  id BigInt @id\n  at String @client_default(now())\n}".into(), "A.at: @client_default now does not fit"),
        ("model A {\n  id BigInt @id\n  n Int @client_default(\"x\")\n}".into(), "<schema>:3:9: A.n: @client_default requires int32"),
        ("model A {\n  id BigInt @id\n  n Int @client_default(3000000000)\n}".into(), "<schema>:3:9: A.n: @client_default requires int32"),
        ("enum E {\n  a\n}\nmodel A {\n  id BigInt @id\n  e E @client_default(b)\n}".into(), "A.e: E has no value b"),
        ("model A {\n  id BigInt @id\n  j Json @default(\"{oops\")\n}".into(), "JSON text"),
        (format!("{b}\n{b}"), "declared twice"),
        ("datasource db {\n  provider = \"mysql\"\n}".into(), "provider must be \"postgresql\""),
        ("enum Role {\n  A\n  A\n}".into(), "A is declared twice"),
        ("enum Role {\n  A @value(1)\n}".into(), "@value(n) is for @@storage(int)"),
        ("enum Role {\n  A\n  @@storage(int)\n}".into(), "need @value(n)"),
        ("enum Role {\n  A\n  @@storage(blob)\n}".into(), "native, text or int"),
        ("model A {\n  id BigInt @id\n  r Role @default(B)\n}\nenum Role {\n  A\n}".into(), "Role has no value B"),
        ("model A {\n  id BigInt @id\n  r Role @db.Text\n}\nenum Role {\n  A\n}".into(), "comes from the enum"),
        ("view V {\n  id Int @id\n}".into(), "the SQL body file views/public/V.sql is missing"),
        ("model A {\n  id BigInt[] @id\n}".into(), "an array can't be a primary key"),
        (
            format!("model A {{\n  id BigInt @id\n  b B?\n}}\nmodel B {{\n  id BigInt @id\n  a_id BigInt\n  a A @relation(fields: [a_id], references: [id])\n}}"),
            "B.a_id must be unique for a one-to-one relation",
        ),
        (
            format!("model A {{\n  id BigInt @id\n  b B\n}}\nmodel B {{\n  id BigInt @id\n  a_id BigInt @unique\n  a A @relation(fields: [a_id], references: [id])\n}}"),
            "a one-to-one back relation is optional: B?",
        ),
        (format!("model A {{\n  id BigInt @id\n  bs B[] @relation(through: J)\n}}\n{b}"), "through: J is not a model"),
        (
            format!("model A {{\n  id BigInt @id\n  bs B[] @relation(through: J)\n}}\n{b}\nmodel J {{\n  id BigInt @id\n  a_id BigInt\n  a A @relation(fields: [a_id], references: [id])\n}}"),
            "J has no relation with fields: to B",
        ),
        (
            format!("model A {{\n  id BigInt @id\n  as A[] @relation(through: J)\n}}\nmodel J {{\n  id BigInt @id\n  x BigInt\n  y BigInt\n  a A @relation(\"x\", fields: [x], references: [id])\n  b A @relation(\"y\", fields: [y], references: [id])\n}}"),
            "needs through_fields",
        ),
        ("import \"missing.toml\"".into(), "import \"missing.toml\""),
        ("model A {\n  id BigInt @id\n  @@trigger(t, when: after, operation: [insert],\n    func: f)\n}".into(), "<schema>:3:3: model A: an attribute must be on one line"),
        (
            "model A {\n  id BigInt @id\n  @@trigger(t, when: before, operation: [update], func: \"\"\"BEGIN RETURN NEW; END;\"\"\")\n}".into(),
            "only go in top-level blocks",
        ),
        (
            format!("model A {{\n  id BigInt @id\n  b_id Json\n  b B @relation(fields: [b_id], references: [id])\n}}\n{b}"),
            "relation A.b: A.b_id is Json, and a Json field can't be a reference key",
        ),
        (
            format!("model A {{\n  id BigInt @id\n  b_id BigInt\n  b B @relation(fields: [b_id], references: [key])\n}}\nmodel B {{\n  id BigInt @id\n  key Json\n}}"),
            "relation A.b: B.key is Json, and a Json field can't be a reference key",
        ),
    ];
    for (src, expected) in cases {
        let e = fails(&src);
        assert!(e.contains(expected), "{src:?}\n  got: {e}\n  expected: {expected}");
    }
}

/// Core knows no extension by name, so the `{model}` error names none.
#[test]
fn the_model_placeholder_error_names_no_extension() {
    let e = fails("model A {\n  id BigInt @id\n  bs B[] @relation(\"{model}_bs\")\n}\nmodel B {\n  id BigInt @id\n}");
    assert!(e.contains("{model} in a relation name is only expanded") && !e.to_lowercase().contains("inherit"), "{e}");
}

/// A `{model}` join name on a model with deferred identity waits for a pass, and the join model need not exist.
#[test]
fn a_through_name_with_the_model_placeholder_lowers_on_a_deferred_model_only() {
    let source = "model A {\n  id Int @id\n  bs B[] @relation(through: \"{model}B\")\n}\nmodel B {\n  id Int @id\n}";
    let load = |_: &str| Err("no imports".to_owned());
    let views = std::collections::HashSet::new();
    let lower_with = |deferred: &[&str], source: &str| {
        let deferred = deferred.iter().map(|m| m.to_string()).collect();
        lower::Lowering { load: &load, deferred_identity: &deferred, views: &views, owned_types: &Default::default(), database: None }.lower(syntax::parse(source).unwrap()).map(|(ir, _)| ir).map_err(|e| e.msg)
    };
    let ir = lower_with(&["A"], source).unwrap();
    let r = &ir.models[0].relations[0];
    assert_eq!((r.kind, r.target.as_str(), r.join_label.as_deref(), r.label.as_deref()), (crate::ir::RelKind::Many, "B", Some("{model}B"), Some("")));
    assert!(r.through.is_none() && r.from.is_empty() && ir.models.len() == 2);

    let e = lower_with(&[], source).unwrap_err();
    assert!(e.contains("{model} in a through: name is only expanded in a relation that an extension pass derives") && !e.to_lowercase().contains("inherit"), "{e}");
    let e = lower_with(&["A"], &source.replace("B[]", "B")).unwrap_err();
    assert!(e.contains("a many-to-many relation is a list"), "{e}");
    let e = lower_with(&["A"], &source.replace("through:", "through_fields: [a, b], through:")).unwrap_err();
    assert!(e.contains("through_fields: does not go with"), "{e}");
}

/// The `prisma` code blocks in the docs parse, and the complete ones compile (the
/// "every feature" one imports a file that isn't there). The blog example in
/// prisma-syntax.md is the blog schema.
#[test]
fn documented_schemas_parse() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let blog = std::fs::read_to_string(root.join("examples/blog/schema.prisma")).unwrap();
    let blog = &blog[blog.find("datasource").unwrap()..];
    let mut blog_documented = false;
    for doc in ["docs/schema/prisma-syntax.md", "docs/schema/schema.md"] {
        let text = std::fs::read_to_string(root.join(doc)).unwrap();
        for block in text.split("```prisma\n").skip(1) {
            let src = &block[..block.find("```").unwrap()];
            if let Err(e) = syntax::parse(src) {
                panic!("{doc}: {}: {}\n{src}", e.pos, e.msg);
            }
            blog_documented |= src == blog;
            if !src.contains("import ") && !src.contains("(excerpt)") && src.contains("model ") {
                if let Err(e) = compile(src, None).and_then(check) {
                    panic!("{doc}: {e}\n{src}");
                }
            }
        }
    }
    assert!(blog_documented, "docs/schema/prisma-syntax.md: update the blog example");
}

#[test]
fn negative_numbers_compile_in_enum_values_and_defaults() {
    let ir = ok(r#"
enum Level {
  low  @value(-1)
  high @value(10)
  @@storage(int)
}

model Item {
  id    BigInt @id
  n     Int    @default(-1)
  level Level
}
"#);
    let v = serde_json::to_value(&ir).unwrap();
    assert_eq!(v["enums"][0]["values"][0], json!({"name": "low", "value": -1}));
    assert_eq!(v["models"][0]["fields"][1]["default"], json!(-1));
}

#[test]
fn enums_arrays_decimals() {
    let ir = ok(r#"
enum Role {
  member
  admin  @map("ADMIN")
  @@map("user_role")
}

enum Priority {
  low  @value(1)
  high @value(10)
  @@storage(int)
}

enum Color {
  red
  green
  @@storage(text)
}

model Item {
  id       BigInt     @id
  role     Role       @default(member)
  roles    Role[]     @default([member, admin])
  priority Priority?
  color    Color      @default(green)
  tags     String[]   @default([])
  scores   Int[]
  price    Decimal    @default(0) @db.Decimal(12, 2)
  ratio    Decimal?
}
"#);
    let v = serde_json::to_value(&ir).unwrap();
    assert_eq!(
        v["enums"][0],
        json!({"name": "Role", "db_name": "user_role", "storage": "native",
               "values": [{"name": "member", "value": "member"}, {"name": "admin", "value": "ADMIN"}]})
    );
    assert_eq!(v["enums"][1]["values"][1], json!({"name": "high", "value": 10}));
    let f = &v["models"][0]["fields"];
    assert_eq!(
        f[1],
        json!({"name": "role", "column": "role", "type": "string", "enum": "Role", "db_type": "\"user_role\"",
               "write_sql": "CAST({} AS \"user_role\")", "default": "member"})
    );
    assert_eq!(f[2]["array"], true);
    assert_eq!(f[2]["write_sql"], "CAST({} AS \"user_role\"[])");
    assert_eq!(f[2]["default"], json!(["member", "ADMIN"]));
    assert_eq!(f[3]["type"], "int");
    assert_eq!(f[4]["type"], "text");
    assert_eq!(f[5], json!({"name": "tags", "column": "tags", "type": "string", "array": true, "default": []}));
    assert_eq!(f[7]["type"], "decimal");

    let (_, schema) = check(ir).unwrap();
    let snap = crate::migrate::snapshot(&schema).unwrap();
    assert_eq!(snap.enums.len(), 1);
    assert_eq!(snap.enums[0].values, ["member", "ADMIN"]);
    let t = &snap.tables[0];
    let ty = |c: &str| t.column(c).unwrap().ty.clone();
    assert_eq!(ty("role"), "\"user_role\"");
    assert_eq!(ty("roles"), "\"user_role\"[]");
    assert_eq!(ty("priority"), "integer");
    assert_eq!(ty("tags"), "text[]");
    assert_eq!(ty("scores"), "integer[]");
    assert_eq!(ty("price"), "numeric(12, 2)");
    assert_eq!(ty("ratio"), "numeric");
    assert_eq!(t.column("roles").unwrap().default.as_deref(), Some("ARRAY['member', 'ADMIN']::\"user_role\"[]"));
    assert_eq!(t.column("tags").unwrap().default.as_deref(), Some("'{}'::text[]"));
    let checks: Vec<(&str, &str)> = t.checks.iter().map(|c| (c.name.as_str(), c.expr.as_str())).collect();
    assert_eq!(
        checks,
        [("item_priority_enum_check", "\"priority\" IN (1, 10)"), ("item_color_enum_check", "\"color\" IN ('red', 'green')")]
    );
}

#[test]
fn has_one_and_many_to_many() {
    let ir = ok(r#"
model User {
  id      BigInt   @id
  profile Profile?
}

model Profile {
  id      BigInt @id
  user_id BigInt @unique
  user    User   @relation(fields: [user_id], references: [id])
}

model Post {
  id        BigInt    @id
  tags      Tag[]     @relation(through: PostTag)
  post_tags PostTag[]
}

model Tag {
  id    BigInt @id
  posts Post[] @relation(through: PostTag)
}

model PostTag {
  id      BigInt @id
  post_id BigInt
  tag_id  BigInt
  post    Post   @relation(fields: [post_id], references: [id])
  tag     Tag    @relation(fields: [tag_id], references: [id])

  @@unique([post_id, tag_id])
}

model Person {
  id        BigInt   @id
  following Person[] @relation(through: Follow, through_fields: [follower, followee])
  followers Person[] @relation(through: Follow, through_fields: [followee, follower])
}

model Follow {
  id          BigInt @id
  follower_id BigInt
  followee_id BigInt
  follower    Person @relation("a", fields: [follower_id], references: [id])
  followee    Person @relation("b", fields: [followee_id], references: [id])
}
"#);
    let v = serde_json::to_value(&ir).unwrap();
    assert_eq!(
        v["models"][0]["relations"][0],
        json!({"name": "profile", "kind": "one", "target": "Profile", "from": "id", "to": "user_id"})
    );
    assert_eq!(
        v["models"][2]["relations"][0],
        json!({"name": "tags", "kind": "many", "target": "Tag", "from": "id", "to": "id",
               "through": {"model": "PostTag", "source": "post_id", "target": "tag_id"}})
    );
    assert_eq!(v["models"][3]["relations"][0]["through"], json!({"model": "PostTag", "source": "tag_id", "target": "post_id"}));
    assert_eq!(v["models"][5]["relations"][1]["through"], json!({"model": "Follow", "source": "followee_id", "target": "follower_id"}));
    let (_, schema) = check(ir).unwrap();
    // neither side of a has-one or many-to-many adds a foreign key of its own
    let snap = crate::migrate::snapshot(&schema).unwrap();
    assert_eq!(snap.tables.iter().map(|t| t.foreign_keys.len()).sum::<usize>(), 5);
}

#[test]
fn schema_imports_share_root_settings_and_preserve_ownership() {
    let dir = std::env::temp_dir().join(format!("monorm-imports-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("billing/nested")).unwrap();
    let root = dir.join("schema.prisma");
    let child = dir.join("billing/schema.prisma");
    std::fs::write(&root, "datasource db {\n provider = \"sqlite\"\n}\nimport \"billing/schema.prisma\" (prefix: \"billing_\")\nmodel User {\n id Int @id\n invoices Invoice[]\n}\n").unwrap();
    std::fs::write(&child, "import \"nested/schema.prisma\" (prefix: \"old_\")\nenum Status {\n open\n @@storage(text)\n}\nmodel Invoice {\n id Int @id\n user_id Int\n user User @relation(fields: [user_id], references: [id])\n status Status\n @@map(\"invoices\")\n @@renamed_from(\"old_invoices\")\n}\n").unwrap();
    std::fs::write(dir.join("billing/nested/schema.prisma"), "model Record {\n id Int @id\n}\n").unwrap();
    let project = compile_project_file(&root).unwrap();
    assert_eq!(project.units.len(), 3);
    assert_eq!(project.units[0].models, ["User"]);
    assert_eq!(project.units[1].models, ["Invoice"]);
    assert_eq!(project.units[1].enums, ["Status"]);
    let (ir, schema) = check(project.ir).unwrap();
    assert_eq!(ir.dialect, crate::dialect::Dialect::Sqlite);
    let invoice = ir.models.iter().find(|m| m.name == "Invoice").unwrap();
    assert_eq!(invoice.table, "billing_invoices");
    assert!(invoice.mapped);
    assert!(!ir.models.iter().find(|m| m.name == "Record").unwrap().mapped);
    assert_eq!(invoice.renamed_from.as_deref(), Some("billing_old_invoices"));
    assert_eq!(ir.models.iter().find(|m| m.name == "Record").unwrap().table, "billing_old_record");
    let sql = crate::migrate::create_all(&schema).unwrap().join("\n");
    assert!(sql.contains("billing_invoices"));
    assert!(sql.contains("REFERENCES \"user\""));
    std::fs::write(&child, "datasource db {\n provider = \"sqlite\"\n}\n").unwrap();
    let error = compile_file(&root).unwrap_err();
    assert!(error.contains("billing/schema.prisma:1:1"), "{error}");
    assert!(error.contains("only allowed in the main schema"));
    std::fs::write(&child, "model Invoice {\n id Strin @id\n}\n").unwrap();
    assert!(compile_file(&root).unwrap_err().contains("billing/schema.prisma:2:"));
    std::fs::write(&child, "import \"../schema.prisma\"\n").unwrap();
    assert!(compile_file(&root).unwrap_err().contains("cyclic schema import"));
    std::fs::write(&child, "model User {\n id Int @id\n}\n").unwrap();
    assert!(compile_file(&root).unwrap_err().contains("declared twice"));
    std::fs::write(&root, "import \"billing/nested/schema.prisma\"\nimport \"billing/nested/../nested/schema.prisma\"\n").unwrap();
    let twice = compile_file(&root).unwrap_err();
    assert!(twice.contains("imported more than once, by import \"billing/nested/schema.prisma\" in "), "{twice}");
    assert!(twice.contains("and by import \"billing/nested/../nested/schema.prisma\" in ") && twice.contains("import a shared app once"), "{twice}");
    std::fs::write(&root, "import \"missing.prisma\"\n").unwrap();
    assert!(compile_file(&root).unwrap_err().contains("import \"missing.prisma\""));
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_warning_in_an_imported_file_names_that_file_and_its_own_line() {
    let dir = std::env::temp_dir().join(format!("monorm-import-warning-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("billing")).unwrap();
    let root = dir.join("schema.prisma");
    std::fs::write(&root, "datasource db {\n provider = \"postgresql\"\n}\nimport \"billing/schema.prisma\"\nmodel User {\n id Int @id\n age Int @check(\"age > 0\")\n}\n").unwrap();
    std::fs::write(dir.join("billing/schema.prisma"), "// one\n// two\nmodel Invoice {\n id Int @id\n total Int\n @@check(\"total >= 0\")\n}\n").unwrap();
    let project = compile_project_file(&root).unwrap();
    let at = |file: &str| project.warnings.iter().filter(|w| w.file.ends_with(file)).collect::<Vec<_>>();
    let (billing, main) = (at("billing/schema.prisma"), at("schema.prisma").into_iter().filter(|w| !w.file.contains("billing")).collect::<Vec<_>>());
    assert_eq!(project.warnings.len(), 2, "{:?}", project.warnings);
    assert_eq!((billing[0].span.line, billing[0].span.column), (6, 10), "{:?}", billing[0]);
    assert_eq!(main[0].span.line, 7, "{:?}", main[0]);
    assert!(billing[0].message.contains("raw("));
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn imported_models_use_main_extension_definitions() {
    let dir = std::env::temp_dir().join(format!("monorm-import-types-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("library")).unwrap();
    std::fs::write(dir.join("acme.toml"), "name = \"acme\"\n[types.money]\nsql = \"numeric(12, 2)\"\nvalue = \"text\"\n").unwrap();
    std::fs::write(dir.join("schema.prisma"), "import \"library/schema.prisma\"\nimport \"acme.toml\"\ndatasource db {\n provider = \"postgresql\"\n extensions = [acme(version: \"1.0\")]\n}\n").unwrap();
    std::fs::write(dir.join("library/schema.prisma"), "model Price {\n id BigInt @id\n amount Unsupported(\"money\")\n}\n").unwrap();
    let (ir, _) = check(compile_file(&dir.join("schema.prisma")).unwrap()).unwrap();
    assert_eq!(ir.models[0].fields[1].requires, ["acme"]);
    assert_eq!(ir.extensions[0].version.as_deref(), Some("1.0"));
    std::fs::write(dir.join("library/schema.prisma"), "import \"../acme.toml\"\n").unwrap();
    assert!(compile_file(&dir.join("schema.prisma")).unwrap_err().contains("extension imports belong in the main schema"));
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn behavioral_arguments_preserve_names_null_and_nested_json() {
    let mut items = syntax::parse(r#"model View {
      @@proxy.of(User)
      @@view.members("state", [ACTIVE, OLD])
      @@view.payload("payload", {enabled: true, labels: ["null", null, 3]})
    }"#).unwrap();
    let declarations = super::lower::behavior_declarations(&mut items, "views.prisma", &[]).unwrap();
    assert_eq!(declarations[0].positional, vec![serde_json::json!("User")]);
    assert_eq!(declarations[1].positional[1], serde_json::json!(["ACTIVE", "OLD"]));
    assert_eq!(declarations[2].positional[1], serde_json::json!({"enabled":true,"labels":["null",null,3]}));
    assert_eq!(declarations[2].location.file, "views.prisma");
    assert!(syntax::parse(r#"model V { @@view.payload("x", {key:1,key:2}) }"#).is_err());
    let mut items = syntax::parse(r#"model V { @@view.payload("x", now()) }"#).unwrap();
    assert!(super::lower::behavior_declarations(&mut items, "v.prisma", &[]).is_err());
}

#[test]
fn behavioral_literal_forms_need_an_argument_of_kind_value() {
    let manifest: crate::behavior::Manifest = serde_json::from_value(serde_json::json!({
        "id": "test", "version": "1", "schema_contract": 5, "languages": [], "databases": [],
        "attributes": [{"name": "test.mark", "target": "model",
            "arguments": {"name": {"kind": "string"}, "tags": {"kind": "list"}, "payload": {"kind": "value"}},
            "positional": [{"kind": "string"}, {"kind": "value"}]}]
    })).unwrap();
    let lower = |source: &str| {
        let mut items = syntax::parse(source).unwrap();
        super::lower::behavior_declarations(&mut items, "t.prisma", std::slice::from_ref(&manifest))
    };
    let ok = lower(r#"model V { @@test.mark("a", Name, name: "x", tags: ["x", 1], payload: {labels: [null, Open]}) }"#).unwrap();
    assert_eq!(ok[0].positional[1], serde_json::json!("Name"));
    assert_eq!(ok[0].arguments["payload"], serde_json::json!({"labels": [null, "Open"]}));
    for strict in [r#"@@test.mark(Name)"#, r#"@@test.mark(null)"#, r#"@@test.mark(name: Name)"#,
                   r#"@@test.mark(tags: [Open])"#, r#"@@test.mark(tags: ["x", null])"#, r#"@@test.mark(name: {k: 1})"#] {
        let error = lower(&format!("model V {{ {strict} }}")).unwrap_err();
        assert_eq!(error.msg, "expected a literal", "{strict}");
    }
    // An attribute no compiled manifest declares keeps the literal forms; validation names the missing extension.
    assert!(lower(r#"model V { @@other.mark(Name, null, {k: 1}) }"#).is_ok());
}

#[test]
fn model_arguments_resolve_to_identities_and_an_unknown_name_fails_at_the_name() {
    let manifest: crate::behavior::Manifest = serde_json::from_value(serde_json::json!({
        "id": "test", "version": "1", "schema_contract": "5.14", "languages": [], "databases": [],
        "attributes": [{"name": "test.base", "target": "model",
            "arguments": {"parent": {"kind": "model"}, "targets": {"kind": "model_list"}},
            "positional": [{"kind": "model_list"}]}]
    })).unwrap();
    let lower = |source: &str| {
        let mut items = syntax::parse(source).unwrap();
        super::lower::behavior_declarations(&mut items, "t.prisma", std::slice::from_ref(&manifest))
    };
    let schema = |attr: &str| format!("enum Kind {{ A }}\nmodel User {{ id Int @id }}\nmodel Post {{ id Int @id }}\nmodel V {{\n  {attr}\n}}");
    // A bare name, a quoted name, one name and a list all resolve; a list keeps its shape.
    let ok = lower(&schema(r#"@@test.base(User, parent: "Post", targets: [User, "Post"])"#)).unwrap();
    assert_eq!(ok[0].positional[0], serde_json::json!("User"));
    assert_eq!(ok[0].arguments["parent"], serde_json::json!("Post"));
    assert_eq!(ok[0].arguments["targets"], serde_json::json!(["User", "Post"]));
    assert_eq!(lower(&schema("@@test.base([User, Post])")).unwrap()[0].positional[0], serde_json::json!(["User", "Post"]));
    // The error sits at the unknown name: the value of a named argument, or the item of a list.
    for (attr, line, col, msg) in [
        ("@@test.base(Nope)", 5, 15, "unknown model Nope"),
        ("@@test.base(User, parent: \"Nope\")", 5, 29, "unknown model Nope"),
        ("@@test.base(User, targets: [User, Nope])", 5, 37, "unknown model Nope"),
        ("@@test.base(User, parent: Kind)", 5, 29, "unknown model Kind"),
        ("@@test.base(User, parent: 3)", 5, 29, "expects a model name"),
    ] {
        let error = lower(&schema(attr)).unwrap_err();
        assert_eq!((error.pos.line, error.pos.col, error.msg.as_str()), (line, col, msg), "{attr}");
    }
}

#[test]
fn client_defaults_stay_out_of_the_database_schema() {
    let src = "enum Status {\n  ACTIVE @map(\"active\")\n  OLD @map(\"old\")\n}\nmodel A {\n  id String @id @client_default(uuid7()) @db.Uuid\n  token String @client_default(uuid())\n  \
               status Status @default(OLD) @client_default(ACTIVE)\n  tags Status[] @client_default([ACTIVE, OLD])\n  \
               at DateTime @client_default(now())\n  day DateTime @db.Date @client_default(now())\n  \
               meta Json @client_default(\"{\\\"a\\\": [1]}\")\n  n Int @client_default(3)\n  ok Boolean @client_default(true)\n  \
               prisma String @default(uuid())\n  prisma7 String @default(uuid(7))\n  prisma4 String @default(uuid(4))\n}";
    let ir = ok(src);
    let defaults: Vec<_> = ir.models[0].fields.iter().map(|f| serde_json::to_value(&f.client_default).unwrap()).collect();
    assert_eq!(defaults, [json!({"call": "uuid7"}), json!({"call": "uuid"}), json!({"value": "active"}), json!({"value": ["active", "old"]}),
        json!({"call": "now"}), json!({"call": "now"}), json!({"value": {"a": [1]}}), json!({"value": 3}), json!({"value": true}),
        json!({"call": "uuid"}), json!({"call": "uuid7"}), json!({"call": "uuid"})]);
    assert_eq!(ir.models[0].fields[2].default, Some(json!("old")));
    assert!(ir.models[0].fields[9].default.is_none() && ir.models[0].fields[9].default_sql.is_none());
    let (_, with) = check(ir).unwrap();
    // Compare the DDL without the client default attributes, up to each matching parenthesis.
    let mut plain = src.replace(" @default(uuid(7))", "").replace(" @default(uuid(4))", "").replace(" @default(uuid())", "");
    while let Some(start) = plain.find(" @client_default(") {
        let mut depth = 0;
        let end = plain[start..].char_indices().find_map(|(i, c)| {
            depth += match c { '(' => 1, ')' => -1, _ => 0 };
            (c == ')' && depth == 0).then_some(start + i + 1)
        }).unwrap();
        plain.replace_range(start..end, "");
    }
    let (_, without) = check(ok(&plain)).unwrap();
    assert_eq!(crate::migrate::create_all(&with).unwrap(), crate::migrate::create_all(&without).unwrap());
    assert!(crate::migrate::plan(&with, &crate::migrate::snapshot(&without).unwrap()).unwrap().up.is_empty());
    assert_eq!(with.models[0].client_defaults.len(), 12);
    let field = |f: &str| compile(&format!("enum Status {{\n  ACTIVE @map(\"active\")\n}}\nmodel A {{\n  id Int @id\n  {f}\n}}"), None);
    for (f, message) in [
        ("t String @client_default(\"a\") @client_default(\"b\")", "A.t: a field takes one client default"),
        ("t String @default(uuid()) @client_default(\"x\")", "A.t: a field takes one client default"),
        ("t String @client_default(\"x\") @default(uuid())", "A.t: a field takes one client default"),
        ("s Status @client_default(\"active\")", "A.s: @client_default on an enum field names a member, such as ACTIVE, not a string"),
    ] {
        let error = field(f).unwrap_err();
        assert!(error.contains(message), "{f}: {error}");
    }
    // A Decimal number keeps every digit; an f64 would store 12345678901234568.
    let ir = field("d Decimal @client_default(12345678901234567.89)").unwrap();
    assert_eq!(serde_json::to_value(&ir.models[0].fields[1].client_default).unwrap(), json!({"value": "12345678901234567.89"}));
    let ir = field("d Decimal[] @client_default([1.5, 123456789012345678901234567890])").unwrap();
    assert_eq!(serde_json::to_value(&ir.models[0].fields[1].client_default).unwrap(), json!({"value": ["1.5", "123456789012345678901234567890"]}));
}

#[test]
fn enum_subsets_lower_only_on_view_model_fields() {
    let load = |_: &str| Err("no imports".to_owned());
    let views = std::collections::HashSet::from(["V".to_owned()]);
    let lowered = |field: &str| {
        let source = format!("enum E {{\n  a\n  b\n}}\nmodel M {{\n  id Int @id\n  e E\n}}\nmodel V {{\n  {field}\n}}");
        lower::Lowering { load: &load, deferred_identity: &Default::default(), views: &views, owned_types: &Default::default(), database: None }.lower(syntax::parse(&source).unwrap()).map(|(ir, _)| ir).map_err(|e| e.msg)
    };
    for (field, message) in [("e E(c)", "V.e: E has no value c"), ("e E(a, a)", "V.e: a is listed twice"), ("e E(\"a\")", "V.e: an enum subset lists member names")] {
        let error = lowered(field).unwrap_err();
        assert!(error.contains(message), "{error}");
    }
    let mut ir = ok("enum E {\n  a\n  b\n}\nmodel M {\n  id Int @id\n  e E\n}");
    ir.models[0].fields[1].enum_subset = Some(vec!["a".into()]);
    assert!(check(ir).err().unwrap().contains("M.e: an enum subset is only allowed on a field of a view model"));
}

#[test]
fn minus_field_is_a_descending_index_key() {
    let model = |keys: &str| format!("model Post {{\n  id BigInt @id\n  author_id BigInt\n  created_at DateTime?\n  {keys}\n}}\n");
    let short = ok(&model("@@index([author_id, -created_at(nulls: last)])"));
    let long = ok(&model("@@index([author_id, created_at(sort: Desc, nulls: last)])"));
    assert_eq!(serde_json::to_value(&short).unwrap(), serde_json::to_value(&long).unwrap());
    let (_, short) = check(short).unwrap();
    let (_, long) = check(long).unwrap();
    let (short, long) = (crate::migrate::snapshot(&short).unwrap(), crate::migrate::snapshot(&long).unwrap());
    assert_eq!(serde_json::to_value(&short).unwrap(), serde_json::to_value(&long).unwrap());
    assert!(short.tables[0].indexes[0].keys[1].desc);
    let unique = ok(&model("@@index([-created_at], unique: true)"));
    assert!(unique.models[0].indexes[0].columns[0].desc);
    for (keys, message) in [
        ("@@index([-created_at(sort: Asc)])", "give `-` or sort:, not both"),
        ("@@index([-sql(\"lower(x)\")])", "an expression key takes sort: Desc"),
        ("@@index([-\"created_at\"])", "`-` goes before a field name"),
        ("@@unique([author_id, -created_at])", "use @@index([...], unique: true)"),
        ("@@map(-posts)", "takes one string"),
    ] {
        assert!(fails(&model(keys)).contains(message), "{keys}: {}", fails(&model(keys)));
    }
}

/// A manifest with the keys of the generic extension, written out: the core reads the keys, not the names.
fn owning_manifest() -> crate::behavior::Manifest {
    serde_json::from_value(serde_json::json!({
        "id": "monorm.generic", "version": "1", "schema_contract": "5.0", "languages": [], "databases": [],
        "attributes": [{"name": "generic.relation", "target": "field", "field_type": "Generic", "identities": true, "arguments": {"targets": {"kind": "list", "required": true}}},
                       {"name": "generic.reverse", "target": "field", "member": false, "identities": true},
                       {"name": "generic.relation", "target": "model", "identities": true}]
    })).unwrap()
}

#[test]
fn a_manifest_attribute_owns_a_field_type_or_drops_a_field() {
    let manifest = owning_manifest();
    let source = r#"model Tag {
      id     Int      @id
      target Generic? @generic.relation(targets: ["Post"])
      note   String
    }
    model Post {
      id   Int   @id
      tags Tag[] @generic.reverse
    }"#;
    let mut items = syntax::parse(source).unwrap();
    let declarations = super::lower::behavior_declarations(&mut items, "t.prisma", std::slice::from_ref(&manifest)).unwrap();
    assert_eq!(declarations[0].field_type.as_deref(), Some("Generic?"));
    assert_eq!(declarations[1].field_type.as_deref(), Some("Tag[]"));
    let members = |items: &[syntax::Item], i: usize| match &items[i] { syntax::Item::Model(m) => m.members.iter().map(|f| (f.name.clone(), f.ty.name.clone(), f.ty.optional)).collect::<Vec<_>>(), _ => unreachable!() };
    assert_eq!(members(&items, 0)[1], ("target".to_string(), "Int".to_string(), true));
    assert_eq!(members(&items, 1).len(), 1);
    // The pass builds these members, so an ordinary attribute on one has nowhere to go.
    for (from, to, message) in [
        ("@generic.relation(targets: [\"Post\"])", "@generic.relation(targets: [\"Post\"]) @unique", "Tag.target: @unique is not allowed on a Generic field; remove it (see the generic.* docs)"),
        ("@generic.relation(targets: [\"Post\"])", "@map(\"zz\") @generic.relation(targets: [\"Post\"])", "Tag.target: @map is not allowed on a Generic field; remove it (see the generic.* docs)"),
        ("@generic.reverse", "@generic.reverse @map(\"zz\")", "Post.tags: @map is not allowed on a @generic.reverse field; remove it (see the generic.* docs)"),
        ("@generic.reverse", "@generic.reverse @relation(\"x\")", "Post.tags: @relation is not allowed on a @generic.reverse field; remove it (see the generic.* docs)"),
    ] {
        let mut items = syntax::parse(&source.replace(from, to)).unwrap();
        let error = super::lower::behavior_declarations(&mut items, "t.prisma", std::slice::from_ref(&manifest)).unwrap_err().msg;
        assert!(error.contains(message), "{error}");
    }
    // An owned type needs its attribute; the lowering error names the type, the attribute and the namespace.
    let mut items = syntax::parse("model Tag {\n id Int @id\n x Generic\n}").unwrap();
    super::lower::behavior_declarations(&mut items, "t.prisma", std::slice::from_ref(&manifest)).unwrap();
    assert_eq!(members(&items, 0)[1].1, "Generic");
    let owned = super::lower::owned_types(std::slice::from_ref(&manifest));
    assert_eq!(owned.get("Generic").map(String::as_str), Some("generic.relation"));
    let load = |_: &str| Err("no imports".to_owned());
    let (deferred, views) = Default::default();
    let error = lower::Lowering { load: &load, deferred_identity: &deferred, views: &views, owned_types: &owned, database: None }.lower(items).unwrap_err().msg;
    assert!(error.contains("unknown type Generic; a Generic field needs @generic.relation(...) (namespace generic.*)"), "{error}");
    // Without an owner, the type is plain unknown.
    let error = compile("model Tag {\n id Int @id\n target Generic?\n}", None).unwrap_err();
    assert!(error.contains("unknown type Generic; expected a model or") && !error.contains("a Generic field needs @"), "{error}");
    let error = compile("model Tag {\n id Int @id\n}\nmodel Post {\n id Int @id\n tags Tag[] @generic.reverse\n}", None).unwrap_err();
    assert!(error.contains("@generic.reverse: not compiled into this artifact and no installed extension owns it") && error.contains("(namespace @generic.*"), "{error}");
}

#[test]
fn a_model_or_enum_cannot_share_the_name_of_an_owned_type() {
    let manifest = owning_manifest();
    for declaration in ["model Generic {\n id Int @id\n}", "enum Generic {\n A\n}"] {
        let source = format!("{declaration}\nmodel Tag {{\n id Int @id\n target Generic? @generic.relation(targets: [\"Tag\"])\n}}");
        let mut items = syntax::parse(&source).unwrap();
        let error = super::lower::behavior_declarations(&mut items, "t.prisma", std::slice::from_ref(&manifest)).unwrap_err().msg;
        assert!(error.contains("Tag.target: the type Generic is owned by @generic.relation, and a model or enum has that name; rename it"), "{error}");
    }
}

#[test]
fn an_attribute_asks_for_identities_in_its_manifest() {
    let manifest = owning_manifest();
    for (attribute, field) in [("generic.reverse", true), ("generic.relation", true), ("generic.relation", false)] {
        assert!(super::lower::declared(std::slice::from_ref(&manifest), attribute, field).is_some_and(|a| a.identities), "{attribute} {field}");
    }
    let mut plain = manifest;
    plain.attributes.iter_mut().for_each(|a| a.identities = false);
    assert!(!super::lower::declared(std::slice::from_ref(&plain), "generic.reverse", true).unwrap().identities);
}

#[test]
fn removed_protected_write_names_its_replacement() {
    let source = "model Post {\n id Int @id\n @@protected_write\n}";
    let error = compile(source, None).unwrap_err();
    assert!(error.contains("@@write_rules.protect(layer: app)"), "{error}");
}

#[test]
fn query_set_and_model_methods_name_classes_in_the_ir_without_ddl() {
    let source = "model Post {\n id Int @id\n @@query_set(\"PostQueries\")\n @@model_methods(\"PostMethods\")\n}";
    let ir = compile(source, None).unwrap();
    assert_eq!((ir.models[0].query_set.as_deref(), ir.models[0].model_methods.as_deref()), (Some("PostQueries"), Some("PostMethods")));
    let snapshot = |source: &str| crate::migrate::snapshot(&check(compile(source, None).unwrap()).unwrap().1).unwrap();
    let plain = "model Post {\n id Int @id\n}";
    assert_eq!(snapshot(source), snapshot(plain));
    assert!(!serde_json::to_string(&compile(plain, None).unwrap()).unwrap().contains("query_set"));
    for attribute in ["query_set", "model_methods"] {
        let error = compile(&source.replace(&format!("@@{attribute}(\"Post{}\")", if attribute == "query_set" { "Queries" } else { "Methods" }), &format!("@@{attribute}")), None).unwrap_err();
        assert!(error.contains(&format!("@@{attribute}(\"...\") takes one string")), "{error}");
    }
}

const COMPOSITE: &str = r#"
model Item {
  shop  BigInt
  id    BigInt @default(autoincrement())
  title String
  lines Line[]

  @@id([shop, id])
}
model Line {
  id      BigInt @id @default(autoincrement())
  shop    BigInt
  item_id BigInt
  item    Item   @relation(fields: [shop, item_id], references: [shop, id])
}
"#;

#[test]
fn composite_keys_lower_to_one_primary_key_and_foreign_key() {
    let (ir, schema) = check(ok(COMPOSITE)).unwrap();
    let item = &ir.models[0];
    assert_eq!(item.primary_key, ["shop", "id"]);
    assert_eq!(item.key_fields(), ["shop", "id"]);
    assert_eq!(schema.model(0).key, [0, 1]);
    let line = &ir.models[1].relations[0];
    assert_eq!((line.from.as_slice(), line.to.as_slice()), (&["shop".to_owned(), "item_id".to_owned()][..], &["shop".to_owned(), "id".to_owned()][..]));
    // One column stays a string in the IR; several are a list.
    let json = serde_json::to_value(&ir).unwrap();
    assert_eq!(json["models"][1]["relations"][0]["from"], json!(["shop", "item_id"]));
    assert_eq!(json["models"][0]["relations"][0]["from"], json!(["shop", "id"]));
    let up = crate::migrate::create_all(&schema).unwrap().join("\n");
    assert!(up.contains("CONSTRAINT \"item_pkey\" PRIMARY KEY (\"shop\", \"id\")"), "{up}");
    assert!(up.contains("CONSTRAINT \"line_shop_item_id_fkey\" FOREIGN KEY (\"shop\", \"item_id\") REFERENCES \"item\" (\"shop\", \"id\")"), "{up}");
    let named = ok(&COMPOSITE.replace("@@id([shop, id])", "@@id([shop, id], map: \"item_key\")"));
    assert_eq!(named.models[0].pk_name.as_deref(), Some("item_key"));
}

#[test]
fn composite_key_errors() {
    for (src, message) in [
        ("model A {\n  a BigInt @id\n  b BigInt\n  @@id([a, b])\n}", "@@id"),
        ("model A {\n  a BigInt\n  b BigInt?\n  @@id([a, b])\n}", "optional"),
        ("model A {\n  a BigInt\n  @@id([a])\n}", "two fields or more"),
        ("model A {\n  a BigInt\n  b BigInt\n  @@id([a, c])\n}", "c"),
        ("model A {\n  a BigInt\n  b BigInt\n  @@id([a, b])\n  @@id([b, a])\n}", "@@id"),
        (&COMPOSITE.replace("references: [shop, id]", "references: [id, shop]"), "must be the primary key or a unique constraint of Item, in that order"),
        (&COMPOSITE.replace("fields: [shop, item_id], references: [shop, id]", "fields: [item_id], references: [shop, id]"), "references"),
    ] {
        let e = fails(src);
        assert!(e.contains(message), "{src}: {e}");
    }
    let sqlite = format!("datasource db {{\n  provider = \"sqlite\"\n}}\n{COMPOSITE}");
    assert!(fails(&sqlite).contains("SQLite has no autoincrement in a primary key of several columns"));
    let (_, schema) = check(ok(&sqlite.replace("id    BigInt @default(autoincrement())", "id    BigInt"))).unwrap();
    assert!(crate::migrate::create_all(&schema).unwrap().join("\n").contains("PRIMARY KEY (\"shop\", \"id\")"));
}

#[test]
fn a_changed_primary_key_rebuilds_the_foreign_keys_on_it() {
    let (_, before) = check(ok(COMPOSITE)).unwrap();
    let old = crate::migrate::snapshot(&before).unwrap();
    let reordered = COMPOSITE.replace("@@id([shop, id])", "@@id([id, shop])").replace("fields: [shop, item_id], references: [shop, id]", "fields: [item_id, shop], references: [id, shop]");
    let (_, reordered) = check(ok(&reordered)).unwrap();
    let up: Vec<String> = crate::migrate::plan(&reordered, &old).unwrap().up.into_iter().map(|s| s.sql).collect();
    let pos = |needle: &str| up.iter().position(|s| s.contains(needle)).unwrap_or_else(|| panic!("{needle}: {up:#?}"));
    assert!(pos("DROP CONSTRAINT \"line_shop_item_id_fkey\"") < pos("DROP CONSTRAINT \"item_pkey\""), "{up:#?}");
    assert!(pos("ADD CONSTRAINT \"item_pkey\" PRIMARY KEY (\"id\", \"shop\")") < pos("ADD CONSTRAINT \"line_item_id_shop_fkey\""), "{up:#?}");
}

const NESTED: &str = "type Geo {\n  lat Float\n  lng Float?\n}\n\ntype Address {\n  city String\n  geo Geo\n}\n\nmodel Shop {\n  id Int @id\n  address Address\n}\n";

fn nested_sql(src: &str) -> String {
    let (_, schema) = check(ok(src)).unwrap();
    crate::migrate::create_all(&schema).unwrap().join("\n")
}

fn nested_with(block: &str) -> String {
    NESTED.trim_end().trim_end_matches('}').to_owned() + block + "}\n"
}

#[test]
fn a_type_lowers_to_one_column_per_scalar_leaf() {
    let ir = ok(NESTED);
    let names: Vec<_> = ir.models[0].fields.iter().map(|f| (f.name.as_str(), f.column.as_str(), f.nullable)).collect();
    assert_eq!(
        names,
        [("id", "id", false), ("address_city", "address_city", false), ("address_geo_lat", "address_geo_lat", false), ("address_geo_lng", "address_geo_lng", true)]
    );
    let n = ir.models[0].fields[2].nested.as_ref().unwrap();
    assert_eq!((n.ty.as_str(), n.field.as_str(), n.path.as_slice()), ("Address", "address", &["geo".to_owned(), "lat".to_owned()][..]));
    assert_eq!(ir.types.len(), 2);
}

#[test]
fn prefix_replaces_the_field_segment_of_the_column_names() {
    let ir = ok(&NESTED.replace("address Address", "address Address @storage(columns, prefix: \"addr_\")"));
    let cols: Vec<_> = ir.models[0].fields.iter().map(|f| f.column.as_str()).collect();
    assert_eq!(cols, ["id", "addr_city", "addr_geo_lat", "addr_geo_lng"]);
}

#[test]
fn json_storage_lowers_to_one_json_field() {
    let ir = ok(&NESTED.replace("address Address", "address Address? @storage(json)"));
    let f = &ir.models[0].fields[1];
    assert!(matches!(f.ty, crate::ir::ColType::Json) && f.nullable && f.nested.as_ref().unwrap().path.is_empty());
    assert_eq!(ir.models[0].fields.len(), 2);
    assert!(fails(&NESTED.replace("address Address", "address Address @storage(json, prefix: \"a_\")")).contains("prefix:"));
}

#[test]
fn json_lowering_marks_a_list_of_a_type() {
    let list = |src: &str| ok(src).models[0].fields[1].nested.clone().unwrap().list;
    assert!(list(&NESTED.replace("address Address", "address Address[] @storage(json)")));
    assert!(!list(&NESTED.replace("address Address", "address Address @storage(json)")));
    assert!(!ok(&NESTED).models[0].fields[1].nested.clone().unwrap().list);
}

#[test]
fn a_nullable_field_gets_a_generated_all_null_or_required_set_check() {
    let sql = nested_sql(&NESTED.replace("address Address", "address Address?"));
    assert!(
        sql.contains("CHECK ((\"address_city\" IS NULL AND \"address_geo_lat\" IS NULL AND \"address_geo_lng\" IS NULL) OR (\"address_city\" IS NOT NULL AND \"address_geo_lat\" IS NOT NULL))"),
        "{sql}"
    );
    assert!(!nested_sql(NESTED).contains("IS NULL AND"));
}

#[test]
fn a_nullable_inner_level_gets_its_own_check() {
    let src = NESTED.replace("geo Geo", "geo Geo?");
    let sql = nested_sql(&src);
    assert!(sql.contains("(\"address_geo_lat\" IS NULL AND \"address_geo_lng\" IS NULL) OR (\"address_geo_lat\" IS NOT NULL)"), "{sql}");
    let sql = nested_sql(&src.replace("address Address", "address Address?"));
    assert!(sql.contains("(\"address_city\" IS NULL AND \"address_geo_lat\" IS NULL AND \"address_geo_lng\" IS NULL) OR (\"address_city\" IS NOT NULL)"), "{sql}");
}

#[test]
fn a_nullable_type_without_a_required_subfield_is_an_error_naming_the_fixes() {
    let src = "type Opt {\n  a Int?\n  b Int?\n}\n\nmodel M {\n  id Int @id\n  x Opt?\n  y Opt?\n}\n";
    let e = fails(src);
    for part in ["Opt", "M.x, M.y", "make a subfield of Opt required", "make the field non-null", "@storage(json)"] {
        assert!(e.to_lowercase().contains(&part.to_lowercase()), "{part}: {e}");
    }
    ok(&src.replace("a Int?", "a Int").replace("x Opt?", "x Opt").replace("y Opt?", "y Opt @storage(json)"));
    // the error comes back after an edit of the type
    let inner = "type In {\n  a Int?\n}\n\ntype Out {\n  r Int\n  i In?\n}\n\nmodel M {\n  id Int @id\n  o Out\n}\n";
    assert!(fails(inner).contains("In"));
    ok(&inner.replace("a Int?", "a Int"));
}

#[test]
fn nested_type_errors() {
    for (src, message) in [
        ("type Geo {\n  lat Float\n  g Geo?\n}\n\nmodel Shop {\n  id Int @id\n  n Geo\n}\n", "contains itself"),
        ("type A {\n  b B\n}\n\ntype B {\n  a A\n}\n\nmodel Shop {\n  id Int @id\n  n A\n}\n", "contains itself"),
        ("type T {\n  id Int @id\n}\n\nmodel Shop {\n  id Int @id\n  t T\n}\n", "no @id"),
        ("type T {\n  a Int\n}\n\nmodel T {\n  id Int @id\n}\n", "declared twice"),
        ("type T {\n  a Int\n}\n\nmodel Shop {\n  id Int @id\n  t T[]\n}\n", "@storage(json)"),
        ("type T {\n  a Int\n}\n\nmodel Shop {\n  id Int @id\n  t_a Int\n  t T\n}\n", "collides"),
        ("type T {\n  a Int\n}\n\nmodel Shop {\n  id Int @id\n  t T @unique\n}\n", "takes @storage"),
        ("type T {\n  a Int\n  @@index([a])\n}\n\nmodel Shop {\n  id Int @id\n  t T\n}\n", "unknown attribute @@index"),
        ("model Other {\n  id Int @id\n}\n\ntype T {\n  o Other\n}\n\nmodel Shop {\n  id Int @id\n  t T\n}\n", "holds no relation"),
    ] {
        let e = fails(src);
        assert!(e.contains(message), "{src}: {e}");
    }
    ok("type T {\n  a Int\n}\n\nmodel Shop {\n  id Int @id\n  t T[] @storage(json)\n}\n");
}

const COLLIDE: &str = "type T {\n  a Int\n}\n\n";

#[test]
fn a_field_that_collides_with_a_group_column_is_an_error() {
    let model = |fields: &str| format!("{COLLIDE}model Shop {{\n  id Int @id\n{fields}}}\n");
    let e = fails(&model("  t T\n  x Int @map(\"t_a\")\n"));
    assert!(e.contains("8:3") && e.contains("column t_a") && e.contains("@map"), "{e}");
    let e = fails(&model("  t T\n  t_a Int\n"));
    assert!(e.contains("8:3") && e.contains("rename a field"), "{e}");
    let geo = "type Geo {\n  lat Float\n}\n\nmodel Shop {\n  id Int @id\n  geo_lat Float @map(\"gl\")\n  geo Geo\n}\n";
    let e = fails(geo);
    assert!(e.contains("8:3") && e.contains("rename a field") && !e.contains("prefix"), "{e}");
}

#[test]
fn a_map_on_a_group_subfield_replaces_its_segment() {
    let ir = ok("type G {\n  lat Float\n}\n\ntype A {\n  geo G @map(\"g\")\n}\n\nmodel Shop {\n  id Int @id\n  a A\n}\n");
    assert_eq!(ir.models[0].fields[1].column, "a_g_lat");
    assert_eq!(ir.models[0].fields[1].name, "a_geo_lat");
    let e = fails("type G {\n  lat Float\n}\n\ntype A {\n  geo G @unique\n}\n\nmodel Shop {\n  id Int @id\n  a A\n}\n");
    assert!(e.contains("takes only @map"), "{e}");
}

#[test]
fn the_null_rule_error_points_at_the_subfield() {
    let src = "type Geo {\n  lat Float?\n}\n\ntype Address {\n  city String\n  geo Geo?\n}\n\nmodel Shop {\n  id Int @id\n  address Address\n}\n";
    let e = fails(src);
    assert!(e.contains("7:3") && e.contains("Shop.address.geo"), "{e}");
}

#[test]
fn a_nullable_level_with_one_leaf_has_no_check() {
    let sql = nested_sql("type A {\n  x Int\n}\n\nmodel Shop {\n  id Int @id\n  a A?\n}\n");
    assert!(sql.contains("\"a_x\" integer"), "{sql}");
    assert!(!sql.contains("CHECK"), "{sql}");
}

#[test]
fn a_leaf_name_joins_three_type_levels() {
    let ir = ok("type C {\n  z Int\n}\n\ntype B {\n  c C\n}\n\ntype A {\n  b B\n}\n\nmodel Shop {\n  id Int @id\n  a A\n}\n");
    let f = &ir.models[0].fields[1];
    assert_eq!((f.name.as_str(), f.column.as_str()), ("a_b_c_z", "a_b_c_z"));
    assert_eq!(f.nested.as_ref().unwrap().path, ["b", "c", "z"]);
}

#[test]
fn an_enum_in_a_type_lowers_to_an_enum_leaf() {
    let ir = ok("enum Kind {\n  home\n  work\n  @@storage(text)\n}\n\ntype A {\n  kind Kind\n}\n\nmodel Shop {\n  id Int @id\n  a A\n}\n");
    let f = &ir.models[0].fields[1];
    assert_eq!((f.name.as_str(), f.enum_name.as_deref()), ("a_kind", Some("Kind")));
}

#[test]
fn autoincrement_inside_a_type_is_an_error() {
    let e = fails("type A {\n  n Int @default(autoincrement())\n}\n\nmodel Shop {\n  id Int @id\n  a A\n}\n");
    assert!(e.contains("autoincrement"), "{e}");
}

#[test]
fn index_and_unique_take_subfield_paths() {
    let sql = nested_sql(&nested_with("  @@index([address.city])\n  @@unique([address.geo.lat, id])\n"));
    assert!(sql.contains("(\"address_city\")"), "{sql}");
    assert!(sql.contains("UNIQUE (\"address_geo_lat\", \"id\")"), "{sql}");
    for (path, message) in [("address.geo", "nested type"), ("address.nope", "no field address.nope")] {
        let e = fails(&nested_with(&format!("  @@index([{path}])\n")));
        assert!(e.contains(message), "{path}: {e}");
    }
}

#[test]
fn a_subfield_check_names_its_own_column() {
    let src = NESTED.replace("lat Float\n", "lat Float @check(\"{} >= -90\")\n");
    assert!(nested_sql(&src).contains("\"address_geo_lat\" >= -90"));
}

#[test]
fn a_subfield_check_without_a_column_placeholder_is_an_error() {
    let src = NESTED.replace("lat Float\n", "lat Float @check(\"x > 0\")\n");
    let e = fails(&src);
    assert!(e.contains("{}"), "{e}");
}

#[test]
fn a_default_under_a_nullable_level_has_no_ddl_default() {
    let src = "type A {\n  street String @default(\"x\")\n  city String?\n}\n\nmodel S {\n  id Int @id\n  a A?\n  b A\n}\n";
    let sql = nested_sql(src);
    assert!(!sql.contains("\"a_street\" varchar DEFAULT"), "{sql}");
    assert!(sql.contains("\"b_street\" varchar DEFAULT 'x' NOT NULL"), "{sql}");
    let ir = ok(src);
    let street = ir.models[0].fields.iter().find(|f| f.column == "a_street").unwrap();
    assert!(street.default.is_some() && street.nested.as_ref().unwrap().under_null);
}

#[test]
fn root_required_marks_the_leaves_of_a_non_null_columns_group() {
    let src = "type Geo {\n  lat Float\n}\n\ntype Wrap {\n  inner Geo?\n}\n\nmodel S {\n  id Int @id\n  w Wrap\n  g Geo\n  o Geo?\n  j Geo @storage(json)\n}\n";
    let ir = ok(src);
    let required = |field: &str| -> Vec<bool> {
        ir.models[0].fields.iter().filter_map(|f| f.nested.as_ref()).filter(|n| n.field == field).map(|n| n.root_required).collect()
    };
    assert_eq!(required("w"), [true]);
    assert_eq!(required("g"), [true]);
    assert_eq!(required("o"), [false]);
    assert_eq!(required("j"), [false]);
    let w = ir.models[0].fields.iter().find(|f| f.column == "w_inner_lat").unwrap();
    assert!(w.nested.as_ref().unwrap().under_null, "the middle level is nullable");
    let json = serde_json::to_value(&ir.models).unwrap().to_string();
    assert_eq!(json.matches("\"root_required\":true").count(), 2, "{json}");
    assert!(!json.contains("\"root_required\":false"));
}

#[test]
fn a_list_of_scalars_in_a_type_stays_one_column_and_sqlite_rejects_it() {
    let src = "type T {\n  tags String[]\n  n Int\n}\n\nmodel Shop {\n  id Int @id\n  t T\n}\n";
    assert!(nested_sql(src).contains("\"t_tags\" text[]"));
    let sqlite = format!("datasource db {{\n  provider = \"sqlite\"\n}}\n{src}");
    assert!(fails(&sqlite).contains("sqlite does not support Arrays"));
}

fn json_index(prefix: &str, block: &str) -> Vec<(Option<String>, Option<String>, bool)> {
    let src = format!("{prefix}{}", nested_with(block).replace("address Address", "address Address @storage(json)"));
    let ir = ok(&src);
    let model = &ir.models[0];
    model.indexes.iter().flat_map(|i| i.columns.iter()).map(|c| (c.field.clone(), c.expr.clone(), c.desc)).collect()
}

#[test]
fn an_index_on_a_json_subfield_is_an_expression_key_with_the_planner_text() {
    let keys = json_index("", "  @@index([address.geo.lat(sort: Desc)])\n  @@unique([address.city])\n");
    assert_eq!(
        keys,
        [
            (None, Some("(((\"address\" -> 'geo') ->> 'lat')::double precision)".to_owned()), true),
            (None, Some("((\"address\") ->> 'city')".to_owned()), false),
        ]
    );
    let sqlite = json_index("datasource db {\n  provider = \"sqlite\"\n}\n", "  @@index([address.geo.lat])\n");
    assert_eq!(sqlite, [(None, Some("json_extract(\"address\", '$.\"geo\".\"lat\"')".to_owned()), false)]);
}

#[test]
fn a_json_subfield_index_builds_a_create_index_on_the_expression() {
    let src = nested_with("  @@index([address.city])\n  @@unique([address.floor])\n").replace("address Address", "address Address @storage(json)");
    let sql = nested_sql(&src.replace("  city String\n", "  city String\n  floor Int\n"));
    assert!(sql.contains("CREATE INDEX") && sql.contains("((\"address\") ->> 'city')"), "{sql}");
    assert!(sql.contains("CREATE UNIQUE INDEX") && sql.contains("::integer"), "{sql}");
}

#[test]
fn a_rename_hint_names_the_old_column_through_parent_and_field_renames() {
    let old_columns = |src: &str| -> Vec<(String, Option<String>)> {
        ok(src).models[0].fields.iter().filter(|f| f.nested.is_some()).map(|f| (f.column.clone(), f.renamed_from.clone())).collect()
    };
    let src = "type Geo {\n  lat Float @renamed_from(\"y\")\n  lng Float\n}\n\ntype A {\n  city String\n  pos Geo @renamed_from(\"geo\")\n}\n\nmodel S {\n  id Int @id\n  addr A @renamed_from(\"address\")\n}\n";
    assert_eq!(
        old_columns(src),
        [
            ("addr_city".to_owned(), Some("address_city".to_owned())),
            ("addr_pos_lat".to_owned(), Some("address_geo_y".to_owned())),
            ("addr_pos_lng".to_owned(), Some("address_geo_lng".to_owned())),
        ]
    );
    // a new prefix with a renamed field: the old columns had the default joined names
    let with_prefix = src.replace("addr A @renamed_from(\"address\")", "addr A @storage(columns, prefix: \"a_\") @renamed_from(\"address\")");
    assert_eq!(old_columns(&with_prefix)[0], ("a_city".to_owned(), Some("address_city".to_owned())));
    // no hint, no rename
    let plain = "type Geo {\n  lat Float\n}\n\nmodel S {\n  id Int @id\n  g Geo\n}\n";
    assert_eq!(old_columns(plain), [("g_lat".to_owned(), None)]);
    // a json field renames its one column
    let json = "type Geo {\n  lat Float\n}\n\nmodel S {\n  id Int @id\n  place Geo @storage(json) @renamed_from(\"g\")\n}\n";
    assert_eq!(ok(json).models[0].fields[1].renamed_from.as_deref(), Some("g"));
}

#[test]
fn a_rename_hint_on_a_json_subfield_is_kept_for_the_migration() {
    let leaf = "type Geo {\n  lat Float @renamed_from(\"y\")\n}\n\nmodel S {\n  id Int @id\n  g Geo @storage(json)\n}\n";
    let schema = ok(leaf);
    let geo = schema.types.iter().find(|t| t.name == "Geo").unwrap();
    assert_eq!(geo.fields[0].leaf.as_ref().unwrap().renamed_from.as_deref(), Some("y"));
    let group = "type Geo {\n  lat Float\n}\n\ntype A {\n  pos Geo @renamed_from(\"geo\")\n}\n\nmodel S {\n  id Int @id\n  a A @storage(json)\n}\n";
    let schema = ok(group);
    let a = schema.types.iter().find(|t| t.name == "A").unwrap();
    assert_eq!(a.fields[0].renamed_from.as_deref(), Some("geo"));
    assert!(ok(&leaf.replace("@storage(json)", "@storage(columns)")).models[0].fields.len() == 2);
}

#[test]
fn app_imports_read_the_schema_an_installed_app_declares() {
    use crate::codegen::lookup::{target, ClassKind, Language};
    let dir = std::env::temp_dir().join(format!("monorm-app-imports-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    // a Python app (a package with __init__.py) that imports an npm-style app, as a project would see them
    let auth = dir.join("site-packages/monorm_auth");
    let billing = dir.join("project/node_modules/monorm-billing");
    std::fs::create_dir_all(&auth).unwrap();
    std::fs::create_dir_all(&billing).unwrap();
    std::fs::write(auth.join("__init__.py"), "").unwrap();
    std::fs::write(auth.join("schema.prisma"), "import \"app:monorm-billing\" (prefix: \"b_\")\nmodel User {\n id Int @id\n invoices Invoice[]\n @@query_set(\"UserQueries\")\n}\n").unwrap();
    std::fs::write(billing.join("package.json"), "{\"name\": \"monorm-billing\"}").unwrap();
    std::fs::write(billing.join("schema.prisma"), "model Invoice {\n id Int @id\n user_id Int\n user User @relation(fields: [user_id], references: [id])\n @@query_set(\"InvoiceQueries\")\n}\n").unwrap();
    let root = dir.join("project/schema.prisma");
    std::fs::write(&root, "datasource db {\n provider = \"sqlite\"\n}\nimport \"app:monorm-auth\" (prefix: \"auth_\")\nmodel Post {\n id Int @id\n}\n").unwrap();
    let installed: Installed = [("monorm-auth".to_owned(), auth.join("schema.prisma")), ("monorm-billing".to_owned(), billing.join("schema.prisma"))].into();
    let project = compile_project_file_with(&root, &installed).unwrap();
    let tables: Vec<_> = project.ir.models.iter().map(|m| (m.name.as_str(), m.table.as_str())).collect();
    assert_eq!(tables, [("Invoice", "auth_b_invoice"), ("User", "auth_user"), ("Post", "post")]);
    assert!(check(project.ir).is_ok());
    // the module lookup starts from the resolved schema path, not from the importing project
    let units = &project.units;
    assert_eq!(units[1].path, auth.join("schema.prisma").canonicalize().unwrap());
    let out = dir.join("project");
    assert_eq!(target(Language::Python, &units[1].path, &out, ClassKind::QuerySet, "UserQueries").unwrap(), "monorm_auth.queries:UserQueries");
    assert_eq!(target(Language::TypeScript, &units[2].path, &out, ClassKind::QuerySet, "InvoiceQueries").unwrap(), "monorm-billing/queries.js#InvoiceQueries");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn an_app_import_without_an_installed_app_names_the_package() {
    let dir = std::env::temp_dir().join(format!("monorm-app-missing-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let root = dir.join("schema.prisma");
    std::fs::write(&root, "model A {\n id Int @id\n}\nimport \"app:monorm-auth\"\n").unwrap();
    let error = compile_file(&root).unwrap_err();
    assert!(error.contains("schema.prisma:4:") && error.contains("app:monorm-auth") && error.contains("install the `monorm-auth` package"), "{error}");
    let installed: Installed = [("monorm-auth".to_owned(), dir.join("gone.prisma"))].into();
    assert!(compile_file_with(&root, &installed).unwrap_err().contains("gone.prisma"));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_generator_block_lists_extension_ids_and_ignores_other_keys() {
    let mut items = super::syntax::parse("generator client {\n provider = \"monorm\"\n extensions = [\"acme.audit\", \"acme.tag\"]\n}\nmodel A {\n id Int @id\n @@acme.tag(\"x\")\n}\n").unwrap();
    assert_eq!(super::lower::generator_extensions(&items).unwrap(), ["acme.audit", "acme.tag"]);
    assert_eq!(super::lower::used_namespaces(&items).into_iter().collect::<Vec<_>>(), ["acme"]);
    items = super::syntax::parse("generator client {\n extensions = \"acme.audit\"\n}\n").unwrap();
    assert!(super::lower::generator_extensions(&items).unwrap_err().msg.contains("list of extension ids"));
    items = super::syntax::parse("generator client {\n extensions = [acme]\n}\n").unwrap();
    assert!(super::lower::generator_extensions(&items).unwrap_err().msg.contains("strings"));
}

#[test]
fn a_generator_block_compiles_when_it_names_no_extension() {
    assert!(compile("generator client {\n provider = \"monorm\"\n}\nmodel A {\n id Int @id\n}\n", None).is_ok());
}

#[test]
fn model_mapping_records_explicitness_even_when_it_matches_the_default() {
    let ir = compile("model A {\n id Int @id\n @@map(\"a\")\n}\nmodel B {\n id Int @id\n}\n", None).unwrap();
    assert!(ir.models[0].mapped);
    assert!(!ir.models[1].mapped);
    let encoded = serde_json::to_string(&ir).unwrap();
    let decoded: monorm::ir::SchemaIr = serde_json::from_str(&encoded).unwrap();
    assert!(decoded.models[0].mapped);
}

#[test]
fn json_relation_keys_are_rejected_from_either_side_at_load() {
    for (local_type, target_type, bad_key) in [("Json", "BigInt", "Child.parent_key"), ("BigInt", "Json", "Parent.key")] {
        for reverse_type in ["Child[]", "Child?"] {
            let source = format!("model Parent {{\n id BigInt @id\n key {target_type} @unique\n children {reverse_type}\n}}\nmodel Child {{\n id BigInt @id\n parent_key {local_type} @unique\n parent Parent @relation(fields: [parent_key], references: [key])\n}}");
            let error = compile(&source, None).unwrap_err();
            assert!(error.contains(&format!("relation Parent.children: {bad_key} is Json")), "{error}");
        }
    }
}

fn generated_model(field: &str) -> String {
    format!("model Line {{\n  id Int @id\n  quantity Int\n  price Int\n  {field}\n}}\n")
}

#[test]
fn a_generated_field_keeps_its_expression_and_storage() {
    let ir = ok(&generated_model("total Int @generated(quantity * price)"));
    let g = ir.models[0].fields.iter().find(|f| f.name == "total").unwrap().generated.clone().unwrap();
    assert!(g.stored);
    assert!(g.sql.contains("quantity") && g.sql.contains('*'), "{}", g.sql);
    let ir = ok(&generated_model("total Int @generated(quantity * price, stored: false)"));
    assert!(!ir.models[0].fields.iter().find(|f| f.name == "total").unwrap().generated.as_ref().unwrap().stored);
}

#[test]
fn a_generated_column_refuses_a_volatile_or_stable_function() {
    for f in ["now()"] {
        let e = fails(&generated_model(&format!("total Float @generated({f})")));
        assert!(e.contains("not immutable"), "{f}: {e}");
    }
}

#[test]
fn a_generated_field_refuses_other_value_sources_and_bad_arguments() {
    for (field, needle) in [
        ("total Int @generated(quantity * price) @default(1)", "@default"),
        ("total Int @generated(quantity * price) @check(\"total > 0\")", "@check"),
        ("total Int @generated(total + 1)", "cannot read another"),
        ("total Int @generated(quantity * price, stored: 1)", "true or false"),
        ("total Int @generated", "generated"),
        ("total Int @generated(quantity) @generated(price)", "generated"),
    ] {
        let e = fails(&generated_model(field));
        assert!(e.contains(needle), "{field}: {e}");
    }
    let e = fails("model Line {\n  id Int @id @generated(1)\n}\n");
    assert!(e.contains("@id"), "{e}");
}

fn views(source: &str, bodies: &[(&str, &str)]) -> Result<SchemaIr, String> {
    let overrides = bodies.iter().map(|(path, text)| (PathBuf::from(path), text.to_string())).collect();
    compile_with_overrides(source, None, &overrides).and_then(|ir| check(ir).map(|(ir, _)| ir))
}

#[test]
fn a_view_block_takes_its_body_from_a_sidecar_file() {
    let source = "model User {\n  id Int @id\n}\nview Active {\n  id Int @id\n  @@materialized\n  @@depends_on([Base])\n  @@index([id])\n}\nview Base {\n  id Int @id\n}\n";
    let ir = views(source, &[("views/public/Active.sql", "SELECT id FROM base;\n"), ("views/public/Base.sql", "SELECT id FROM \"user\"")]).unwrap();
    let active = ir.models.iter().find(|m| m.name == "Active").unwrap().sql_view.clone().unwrap();
    assert_eq!((active.body.as_str(), active.materialized, active.depends_on.as_slice()), ("SELECT id FROM base", true, &["Base".to_string()][..]));
    let user = ir.models.iter().find(|m| m.name == "User").unwrap();
    assert!(user.sql_view.is_none());
    let base = ir.models.iter().find(|m| m.name == "Base").unwrap().sql_view.clone().unwrap();
    assert!(!base.materialized);
}

#[test]
fn sqlite_reads_the_main_directory() {
    let source = "datasource db {\n  provider = \"sqlite\"\n}\nview V {\n  id Int @id\n}\n";
    assert!(views(source, &[("views/main/V.sql", "SELECT 1 AS id")]).is_ok());
    assert!(views(source, &[("views/public/V.sql", "SELECT 1 AS id")]).unwrap_err().contains("views/main/V.sql is missing"));
}

#[test]
fn a_view_is_checked_at_compile_time() {
    let one = |body: &str, sql: &str| views(&format!("{body}\n"), &[("views/public/V.sql", sql), ("views/public/W.sql", "SELECT 1 AS id")]).unwrap_err();
    assert!(one("view V {\n  id Int @id\n}", "  ;\n").contains("the SQL body file views/public/V.sql is empty"));
    assert!(one("view V {\n  id Int\n}", "SELECT 1").contains("model V has no @id field"));
    assert!(one("view V {\n  id Int @id\n  @@index([id])\n}", "SELECT 1").contains("an index belongs on a materialized view"));
    assert!(one("view V {\n  id Int @id @unique\n}", "SELECT 1").contains("an index belongs on a materialized view"));
    assert!(one("view V {\n  id Int @id\n  @@depends_on([Nope])\n}", "SELECT 1").contains("which is not a view of this schema"));
    assert!(one("view V {\n  id Int @id\n  @@depends_on([V])\n}", "SELECT 1").contains("names the view itself"));
    assert!(one("view V {\n  id Int @id\n  @@depends_on([W])\n}\nview W {\n  id Int @id\n  @@depends_on([V])\n}", "SELECT 1").contains("makes a cycle"));
    assert!(one("model V {\n  id Int @id\n  @@materialized\n}", "SELECT 1").contains("belongs on a `view` block"));
    assert!(one("view V {\n  id Int @id\n  @@materialized(1)\n}", "SELECT 1").contains("takes no arguments"));
    let sqlite = "datasource db {\n  provider = \"sqlite\"\n}\nview V {\n  id Int @id\n  @@materialized\n}\n";
    assert!(views(sqlite, &[("views/main/V.sql", "SELECT 1")]).unwrap_err().contains("@@materialized requires PostgreSQL"));
}

#[test]
fn a_model_view_cannot_take_a_sql_view_as_its_root() {
    let source = "view V {\n  id Int @id\n}\nmodel M {\n  id Int @id\n}\n";
    let overrides = [(PathBuf::from("views/public/V.sql"), "SELECT 1 AS id".to_string())].into();
    let mut ir = compile_with_overrides(source, None, &overrides).unwrap();
    ir.behavior.model_views.push(monorm::extension::ModelView { model: "M".into(), source: "V".into(), root: "V".into(), row_filter: None, insert_table: None });
    let error = crate::schema::validate_sql_views(&ir).unwrap_err();
    assert!(error.contains("cannot have the SQL view V as its source or root"), "{error}");
}

fn deprecated_source(legacy_line: &str, with_objects: bool) -> String {
    let objects = if with_objects { "  @@index([legacy, title])\n  @@unique([legacy, title])\n" } else { "" };
    format!("model Item {{\n  id BigInt @id @default(autoincrement())\n  title String\n{legacy_line}{objects}}}\n")
}

fn deprecated_plan(source: &str, previous: &crate::migrate::DbSchema) -> crate::migrate::MigrationPlan {
    let (_, schema) = check(ok(source)).unwrap();
    crate::migrate::plan(&schema, previous).unwrap()
}

#[test]
fn deprecated_leaves_the_logical_model_and_stays_in_the_physical_schema() {
    let source = deprecated_source("  legacy String @default(\"none\") @deprecated\n", true);
    let (ir, schema) = check(ok(&source)).unwrap();
    let names = |m: &crate::ir::ModelIr| m.fields.iter().map(|f| f.name.clone()).collect::<Vec<_>>();
    assert_eq!(names(&ir.models[0]), ["id", "title"]);
    assert_eq!(names(&schema.physical().models[0].ir), ["id", "title", "legacy"]);
    assert!(ir.models[0].indexes.is_empty() && ir.models[0].constraints.is_empty(), "objects over the column are physical only");
    let physical = &schema.physical().models[0].ir;
    assert_eq!((physical.indexes.len(), physical.constraints.len()), (1, 1));
    let create = crate::migrate::create_all(&schema).unwrap().join("\n");
    assert!(create.contains("\"legacy\" varchar DEFAULT 'none' NOT NULL") && create.contains("item_legacy_title_idx"), "{create}");
}

#[test]
fn deprecated_attribute_added_or_removed_makes_no_migration_and_the_line_deleted_drops_the_column() {
    let plain = deprecated_source("  legacy String @default(\"none\")\n", true);
    let hidden = deprecated_source("  legacy String @default(\"none\") @deprecated\n", true);
    let gone = deprecated_source("", false);
    let snapshot = deprecated_plan(&plain, &Default::default()).snapshot;
    assert!(deprecated_plan(&hidden, &snapshot).up.is_empty(), "attribute added");
    let hidden_snapshot = deprecated_plan(&hidden, &Default::default()).snapshot;
    assert_eq!(serde_json::to_value(&hidden_snapshot).unwrap(), serde_json::to_value(&snapshot).unwrap());
    assert!(deprecated_plan(&plain, &hidden_snapshot).up.is_empty(), "attribute removed");
    let drop = deprecated_plan(&gone, &hidden_snapshot);
    let columns: Vec<_> = drop.up.iter().filter(|s| s.sql.contains("DROP COLUMN \"legacy\"")).collect();
    assert_eq!(columns.len(), 1, "{:?}", drop.up);
    assert!(!columns[0].destructive.is_empty(), "the drop carries the destructive marker");
}

fn deprecated_sqlite(legacy_line: &str) -> String {
    format!("datasource db {{\n  provider = \"sqlite\"\n}}\n{}", deprecated_source(legacy_line, true))
}

#[test]
fn deprecated_attribute_makes_no_sqlite_migration() {
    let plain = deprecated_sqlite("  legacy String @default(\"none\")\n");
    let hidden = deprecated_sqlite("  legacy String @default(\"none\") @deprecated\n");
    let snapshot = deprecated_plan(&plain, &Default::default()).snapshot;
    assert!(deprecated_plan(&hidden, &snapshot).up.is_empty(), "attribute added");
    let hidden_snapshot = deprecated_plan(&hidden, &Default::default()).snapshot;
    assert_eq!(serde_json::to_value(&hidden_snapshot).unwrap(), serde_json::to_value(&snapshot).unwrap());
    assert!(deprecated_plan(&plain, &hidden_snapshot).up.is_empty(), "attribute removed");
}

#[test]
fn deprecated_takes_exclusions_triggers_and_partial_objects_with_it() {
    let source = "model Item {
  id BigInt @id @default(autoincrement())
  title String
  legacy String @default(\"x\") @deprecated
  @@exclude([legacy(op: \"=\")], where: \"id > 0\")
  @@trigger(touch, when: before, operation: [update_of(legacy)], func: \"BEGIN RETURN NEW; END;\")
  @@index([title], where: \"legacy <> 'x'\")
  @@index([title], name: \"item_other_idx\", where: \"legacy_other IS NULL\")
  @@index([title], name: \"item_partial_uq\", unique: true, where: \"\\\"legacy\\\" = 'y'\")
}
";
    let (ir, schema) = check(ok(source)).unwrap();
    let item = &ir.models[0];
    let kept: Vec<_> = item.indexes.iter().filter_map(|i| i.name.clone()).collect();
    assert_eq!(kept, ["item_other_idx"], "an index whose WHERE names the column leaves the model; a longer name stays");
    assert!(item.constraints.is_empty() && item.triggers.is_empty(), "{:?}", item.constraints);
    let physical = &schema.physical().models[0].ir;
    assert_eq!((physical.indexes.len(), physical.constraints.len(), physical.triggers.len()), (3, 1, 1));
}

#[test]
fn deprecated_nested_field_hides_all_its_columns_and_keeps_them_in_storage() {
    let source = "type Geo {\n  lat Float\n  lng Float?\n}\n\nmodel Shop {\n  id Int @id\n  geo Geo? @deprecated\n  name String?\n}\n";
    let (ir, schema) = check(ok(source)).unwrap();
    let names = |m: &crate::ir::ModelIr| m.fields.iter().map(|f| f.name.clone()).collect::<Vec<_>>();
    assert_eq!(names(&ir.models[0]), ["id", "name"]);
    assert_eq!(names(&schema.physical().models[0].ir).len(), 4);
    assert!(deprecated_plan(source, &crate::migrate::plan(&schema, &Default::default()).unwrap().snapshot).up.is_empty());
}

#[test]
fn deprecated_generated_column_needs_no_default() {
    let source = deprecated_source("  total Int @generated(\"id * 2\") @deprecated\n", false);
    let (ir, schema) = check(ok(&source)).unwrap();
    assert_eq!(ir.models[0].fields.len(), 2);
    assert!(schema.physical().models[0].ir.fields.iter().any(|f| f.name == "total"));
}

#[test]
fn deprecated_rejects_a_partition_key_and_a_view_field() {
    let partitioned = "model Event {\n  id BigInt @id\n  kind String @default(\"a\") @deprecated\n  @@partition(list: [kind])\n}\n";
    assert!(fails(partitioned).contains("Event: a partition key field cannot be deprecated"), "{}", fails(partitioned));
    let view = views("view V {\n  id Int @id\n  legacy Int @deprecated\n}\n", &[("views/public/V.sql", "SELECT 1 AS id")]).unwrap_err();
    assert!(view.contains("@deprecated goes on a field of a table model, not of a view"), "{view}");
}

#[test]
fn deprecated_errors() {
    let model = |field: &str| format!("model Item {{\n  id BigInt @id @default(autoincrement())\n  title String\n  {field}\n}}\n");
    for (source, message) in [
        (model("legacy String @default(\"x\") @deprecated(\"later\")"), "Item.legacy: @deprecated takes no arguments"),
        (model("legacy String @default(\"x\") @deprecated @client_default(\"y\")"), "Item.legacy: @deprecated cannot go with @client_default"),
        (model("legacy String @default(\"x\") @deprecated @renamed_from(\"old\")"), "Item.legacy: @deprecated cannot go with @renamed_from"),
        (model("legacy String @deprecated"), "Item.legacy: a NOT NULL field without a database default cannot be deprecated, because an insert would fail"),
        ("model Item {\n  id BigInt @id @deprecated\n}\n".into(), "Item.id: the primary key cannot be deprecated"),
        ("model Item {\n  id BigInt\n  part Int @default(1) @deprecated\n  @@id([id, part])\n}\n".into(), "Item.part: the primary key cannot be deprecated"),
        (format!("model Item {{\n  id BigInt @id\n  other_id BigInt? @deprecated\n  other Other? @relation(fields: [other_id], references: [id])\n}}\nmodel Other {{\n  id BigInt @id\n}}\n"), "Item.other_id: relation other uses it as its key"),
        (format!("model Item {{\n  id BigInt @id @default(autoincrement())\n  code String @unique @default(\"c\") @deprecated\n}}\nmodel Other {{\n  id BigInt @id\n  item_code String\n  item Item @relation(fields: [item_code], references: [code])\n}}\n"), "Item.code: relation Other.item references it"),
        (format!("model Item {{\n  id BigInt @id\n  other Other? @deprecated\n}}\nmodel Other {{\n  id BigInt @id\n  item_id BigInt? @unique\n  item Item? @relation(fields: [item_id], references: [id])\n}}\n"), "@deprecated goes on a scalar, enum or nested field, not on a relation"),
    ] {
        assert!(fails(&source).contains(message), "{message}: {}", fails(&source));
    }
}

// ---- Arguments of kind `expression` ----------------------------------------------------------

const FILTER_MODELS: &str = "model Owner {\n  id BigInt @id\n  name String\n}\nmodel Item {\n  id BigInt @id\n  a Int\n  b Boolean\n  c Boolean\n  active Boolean\n  tenant Int\n  title String\n  owner_id BigInt\n  owner Owner @relation(fields: [owner_id], references: [id])\n}\n";

fn filter_manifest() -> crate::behavior::Manifest {
    serde_json::from_value(json!({
        "id": "test", "version": "1", "schema_contract": "5.2", "languages": [], "databases": [],
        "attributes": [{"name": "test.filter", "target": "model",
            "arguments": {"filter": {"kind": "expression"}},
            "positional": [{"kind": "string"}, {"kind": "expression"}]}]
    })).unwrap()
}

/// Resolves `@@test.filter(<argument>)` on `Item`; returns the resolved argument, the warnings and the declaration.
fn resolved(argument: &str) -> Result<(serde_json::Value, Vec<expr::Warning>), String> {
    resolved_in(FILTER_MODELS, argument)
}

/// Like `resolved`, on the models in `models`.
fn resolved_in(models: &str, argument: &str) -> Result<(serde_json::Value, Vec<expr::Warning>), String> {
    let mut ir = ok(models);
    let manifest = filter_manifest();
    let source = format!("{models}\nmodel Item2 {{\n  @@test.filter({argument})\n}}\n");
    let mut items = syntax::parse(&source).unwrap();
    let mut declarations = super::lower::behavior_declarations(
        &mut items,
        "t.prisma",
        std::slice::from_ref(&manifest),
    )
    .map_err(|e| e.msg)?;
    for d in &mut declarations {
        d.model = "Item".into();
        expr_arg::stamp(d, "t.prisma", 1);
    }
    ir.behavior.declarations = declarations;
    let mut warnings = vec![];
    expr_arg::resolve(&mut ir, std::slice::from_ref(&manifest), &mut warnings)?;
    let d = &ir.behavior.declarations[0];
    Ok((
        d.positional
            .get(1)
            .cloned()
            .unwrap_or_else(|| d.arguments["filter"].clone()),
        warnings,
    ))
}

/// Resolves `@@test.filter(<argument>)` once for each name in `models`, on the final shape of `ir`.
fn resolved_each(
    ir: &mut SchemaIr,
    models: &[&str],
    argument: &str,
) -> Result<Vec<serde_json::Value>, String> {
    let manifest = filter_manifest();
    let source = format!("{FILTER_MODELS}\nmodel Item2 {{\n  @@test.filter({argument})\n}}\n");
    let mut items = syntax::parse(&source).unwrap();
    let declared = super::lower::behavior_declarations(
        &mut items,
        "t.prisma",
        std::slice::from_ref(&manifest),
    )
    .map_err(|e| e.msg)?;
    ir.behavior.declarations = models
        .iter()
        .map(|m| {
            let mut d = declared[0].clone();
            d.model = (*m).into();
            expr_arg::stamp(&mut d, "t.prisma", 1);
            d
        })
        .collect();
    expr_arg::resolve(ir, std::slice::from_ref(&manifest), &mut vec![])?;
    Ok(ir
        .behavior
        .declarations
        .iter()
        .map(|d| d.positional[1].clone())
        .collect())
}

const SHAPED_MODELS: &str = "model Full {\n  id BigInt @id\n  a Int\n  b Boolean\n}\nmodel Lean {\n  id BigInt @id\n  b Boolean\n}\n";

#[test]
fn one_filter_of_an_abstract_model_resolves_against_each_concrete_model() {
    let mut ir = ok(SHAPED_MODELS);
    let trees = resolved_each(&mut ir, &["Full", "Lean"], "\"x\", b").unwrap();
    assert_eq!(trees[0], trees[1]);
    let error = resolved_each(&mut ir, &["Full", "Lean"], "\"x\", a > 1").unwrap_err();
    assert!(
        error.contains("a") && error.starts_with("t.prisma:"),
        "{error}"
    );
}

#[test]
fn a_filter_resolves_against_a_field_that_an_earlier_pass_added() {
    let mut ir = ok(SHAPED_MODELS);
    assert!(resolved_each(&mut ir, &["Lean"], "\"x\", a > 1").is_err());
    let mut added = ir
        .models
        .iter()
        .find(|m| m.name == "Full")
        .unwrap()
        .fields
        .iter()
        .find(|f| f.name == "a")
        .unwrap()
        .clone();
    added.nullable = false;
    ir.models
        .iter_mut()
        .find(|m| m.name == "Lean")
        .unwrap()
        .fields
        .push(added);
    let trees = resolved_each(&mut ir, &["Lean"], "\"x\", a > 1").unwrap();
    assert_eq!(trees[0]["l"], json!({"t":"col","path":[],"name":"a"}));
}

#[test]
fn a_proxy_filter_that_names_an_omitted_field_fails_and_a_kept_field_resolves() {
    let mut ir = ok(SHAPED_MODELS);
    let lean = resolved_each(&mut ir, &["Lean"], "\"x\", b").unwrap();
    assert_eq!(lean[0], json!({"t":"col","path":[],"name":"b"}));
    let error = resolved_each(&mut ir, &["Lean"], "\"x\", a == 1").unwrap_err();
    assert!(
        error.contains(": @@test.filter: ") && error.contains("a"),
        "{error}"
    );
}

#[test]
fn a_composed_model_filter_names_a_field_that_it_inherits() {
    let mut ir = ok(SHAPED_MODELS);
    let full = ir
        .models
        .iter()
        .find(|m| m.name == "Full")
        .unwrap()
        .fields
        .iter()
        .find(|f| f.name == "a")
        .unwrap()
        .clone();
    ir.models
        .iter_mut()
        .find(|m| m.name == "Lean")
        .unwrap()
        .fields
        .push(full);
    let trees = resolved_each(&mut ir, &["Lean"], "\"x\", a > 1 & b").unwrap();
    assert_eq!(
        trees[0]["items"][0]["l"],
        json!({"t":"col","path":[],"name":"a"})
    );
}

#[test]
fn an_expression_argument_keeps_the_json_of_every_old_form() {
    let col = |n: &str| json!({"t":"col","path":[],"name":n});
    let cases = [
        ("tenant == 1", json!({"t":"cmp","op":"eq","l":col("tenant"),"r":{"t":"int","value":1}})),
        ("title != \\\"x\\\"", json!({"t":"cmp","op":"ne","l":col("title"),"r":{"t":"text","value":"x"}})),
        ("a < 2", json!({"t":"cmp","op":"lt","l":col("a"),"r":{"t":"int","value":2}})),
        ("a <= 2", json!({"t":"cmp","op":"le","l":col("a"),"r":{"t":"int","value":2}})),
        ("a > -2", json!({"t":"cmp","op":"gt","l":col("a"),"r":{"t":"int","value":-2}})),
        ("a >= 2", json!({"t":"cmp","op":"ge","l":col("a"),"r":{"t":"int","value":2}})),
        ("active == true", json!({"t":"cmp","op":"eq","l":col("active"),"r":{"t":"const","value":true}})),
        ("tenant == scope.tenant", json!({"t":"cmp","op":"eq","l":col("tenant"),"r":{"t":"scope","name":"tenant"}})),
        ("a == null", json!({"t":"is_null","item":col("a"),"neg":false})),
        ("a != null", json!({"t":"is_null","item":col("a"),"neg":true})),
        ("not b", json!({"t":"not","item":col("b")})),
        ("b and c", json!({"t":"and","items":[col("b"),col("c")]})),
        ("b or c and active", json!({"t":"or","items":[col("b"),{"t":"and","items":[col("c"),col("active")]}]})),
    ];
    for (source, wire) in cases {
        let (tree, warnings) = resolved(&format!("\"x\", \"{source}\"")).unwrap_or_else(|e| panic!("{source}: {e}"));
        assert_eq!(tree.to_string(), wire.to_string(), "{source}");
        let last = warnings.last().unwrap_or_else(|| panic!("{source}: no warning"));
        assert!(last.message.starts_with("this filter is a string; write the expression: "), "{}", last.message);
        assert_eq!((last.file.as_str(), last.span.line), ("t.prisma", 18), "{source}");
    }
}

#[test]
fn a_native_filter_rejects_the_words_and_or_not_and_names_the_operators() {
    for (word, operator) in [("b and c", "&"), ("b or c", "|"), ("not b", "~")] {
        let source =
            format!("{FILTER_MODELS}\nmodel Item2 {{\n  @@test.filter(\"x\", {word})\n}}\n");
        let error = syntax::parse(&source).expect_err(word);
        assert!(
            error.msg.contains(&format!("use `{operator}`")),
            "{word}: {}",
            error.msg
        );
    }
}

#[test]
fn a_native_expression_and_its_string_give_the_same_tree() {
    for (native, string) in [
        ("tenant == scope.tenant & active", "tenant == scope.tenant and active"),
        ("a > 1 | b", "a > 1 or b"),
        ("~b", "not b"),
    ] {
        let (a, none) = resolved(&format!("\"x\", {native}")).unwrap_or_else(|e| panic!("{native}: {e}"));
        let (b, warned) = resolved(&format!("\"x\", \"{string}\"")).unwrap();
        assert_eq!(a, b, "{native}");
        assert!(none.is_empty(), "{native}: {none:?}");
        assert_eq!(warned.len(), 1, "{string}");
        // The text of the warning is a native expression: it gives the same tree.
        let advice = warned[0].message.strip_prefix("this filter is a string; write the expression: ").unwrap();
        let (round, again) = resolved(&format!("\"x\", {advice}")).unwrap_or_else(|e| panic!("{advice}: {e}"));
        assert_eq!(round, b, "{advice}");
        assert!(again.is_empty());
    }
}

#[test]
fn none_clears_a_filter_without_a_warning() {
    let (tree, warnings) = resolved("\"x\", \"none\"").unwrap();
    assert!(tree.is_null());
    assert!(warnings.is_empty());
}

#[test]
fn an_expression_error_names_file_line_and_column() {
    for (argument, text) in [
        ("\"x\", nope == 1", "nope"),
        ("\"x\", \"nope == 1\"", "nope"),
        ("\"x\", title == 1", "cannot compare String with Int"),
        ("\"x\", \"a or_null\"", "or_null() is not available"),
        ("\"x\", sql(\"a = 1\")", "sql"),
        ("\"x\", \"a == scope\"", "scope"),
    ] {
        let error = resolved(argument).unwrap_err();
        assert!(error.starts_with("t.prisma:"), "{argument}: {error}");
        assert!(error.contains(": @@test.filter: "), "{argument}: {error}");
        assert!(error.contains(text), "{argument}: {error}");
    }
}

#[test]
fn fmt_native_writes_the_native_form_of_an_expression_string() {
    let ir = ok(FILTER_MODELS);
    let manifests = [filter_manifest()];
    let model = |attrs: &str| FILTER_MODELS.replace("  title String\n", &format!("  title String\n  {attrs}\n"));
    let source = model("@@test.filter(\"s\", \"a > 1 and title == \\\"q\\\"\") // keep\n  @@test.filter(\"n\", \"none\")\n  @@test.filter(\"t\", a > 1)\n  @@test.filter(filter: \"tenant == 1 or not b\")");
    let (out, reports) = super::native::convert_source_with(&source, &ir, &manifests).unwrap();
    assert!(reports.is_empty(), "{reports:?}");
    assert_eq!(out, model("@@test.filter(\"s\", (a > 1) & (title == \"q\")) // keep\n  @@test.filter(\"n\", \"none\")\n  @@test.filter(\"t\", a > 1)\n  @@test.filter(filter: (tenant == 1) | ~b)"));
    let (again, reports) = super::native::convert_source_with(&out, &ir, &manifests).unwrap();
    assert_eq!((again.as_str(), reports.len()), (out.as_str(), 0));
    // An extension with no manifest, and a string argument of another kind, stay as written.
    assert_eq!(super::native::convert_source_with(&source, &ir, &[]).unwrap().0, source);
    let (same, _) = super::native::convert_source_with(&model("@@test.other(\"a > 1\")"), &ir, &manifests).unwrap();
    assert_eq!(same, model("@@test.other(\"a > 1\")"));
}

#[test]
fn fmt_native_reports_a_filter_string_it_cannot_read_with_its_location() {
    let ir = ok(FILTER_MODELS);
    let manifests = [filter_manifest()];
    let source = FILTER_MODELS.replace("  title String\n", "  title String\n  @@test.filter(\"x\", \"nope == 1\")\n  @@test.filter(\"y\", \"a ?? 1\")\n");
    let (out, reports) = super::native::convert_source_with(&source, &ir, &manifests).unwrap();
    assert_eq!(out, source);
    assert_eq!(reports.len(), 2, "{reports:?}");
    assert!(reports[0].starts_with("13:") && reports[0].contains("nope"), "{reports:?}");
    assert!(reports[1].starts_with("14:"), "{reports:?}");
}

#[test]
fn a_resolved_tree_is_kept_when_the_step_runs_again() {
    let mut ir = ok(FILTER_MODELS);
    let manifest = filter_manifest();
    let mut items = syntax::parse(&format!("{FILTER_MODELS}\nmodel Item2 {{\n  @@test.filter(\"x\", a > 1)\n}}\n")).unwrap();
    let mut declarations = super::lower::behavior_declarations(&mut items, "t.prisma", std::slice::from_ref(&manifest)).unwrap();
    declarations[0].model = "Item".into();
    ir.behavior.declarations = declarations;
    let mut warnings = vec![];
    expr_arg::resolve(&mut ir, std::slice::from_ref(&manifest), &mut warnings).unwrap();
    let once = ir.behavior.declarations[0].positional[1].clone();
    assert_eq!(once["t"], "cmp");
    expr_arg::resolve(&mut ir, std::slice::from_ref(&manifest), &mut warnings).unwrap();
    assert_eq!(ir.behavior.declarations[0].positional[1], once);
}

#[test]
fn the_parent_filter_is_a_placeholder_until_an_extension_replaces_it() {
    let (tree, _) = resolved("\"x\", \"parent.default_filter and active\"").unwrap();
    assert_eq!(tree["items"][0], json!({"t": monorm::extension::PARENT_PLACEHOLDER}));
    let mut ir = ok(FILTER_MODELS);
    ir.behavior.read_defaults.push(monorm::extension::ReadDefault { model: "Item".into(), filter: Some(tree), ..Default::default() });
    let error = expr_arg::reject_placeholders(&ir).unwrap_err();
    assert!(error.contains("parent.default_filter has no inherited filter"), "{error}");
}

#[test]
fn a_filter_through_a_to_one_relation_and_the_native_text_functions_resolve() {
    let (tree, warnings) = resolved("\"x\", owner.name == \"x\"").unwrap();
    assert_eq!(tree["l"], json!({"t":"col","path":["owner"],"name":"name"}));
    assert!(warnings.is_empty());
    let (tree, _) = resolved("\"x\", title.startswith(\"ab_\")").unwrap();
    assert_eq!(tree, json!({"t":"like","item":{"t":"col","path":[],"name":"title"},"pattern":{"t":"text","value":"ab\\_%"},"ci":false,"neg":false}));
    let (tree, _) = resolved("\"x\", a.between(1, 3)").unwrap();
    assert_eq!(tree["t"], "and");
}

#[test]
fn bytes_is_a_scalar_with_a_narrow_set_of_forms() {
    let model = |field: &str| format!("model A {{\n  id BigInt @id\n  {field}\n}}");
    let ir = ok(&model("raw Bytes?"));
    assert_eq!(ir.models[0].fields[1].ty, crate::ir::ColType::Bytes);
    assert!(ir.models[0].fields[1].nullable);
    assert!(fails(&model("raw Bytes[]")).contains("Bytes[] is not supported"));
    assert!(fails(&model("raw Bytes @default(\"x\")")).contains("takes no literal default"));
    assert!(fails("type T {\n  raw Bytes\n}\nmodel A {\n  id BigInt @id\n  t T\n}").contains("a type holds no Bytes field"));
}

#[test]
fn duration_is_a_scalar_with_no_literal_default_and_no_array() {
    let model = |field: &str| format!("model A {{\n  id BigInt @id\n  {field}\n}}");
    let mut ir = ok(&model("timeout Duration?"));
    assert_eq!(ir.models[0].fields[1].ty, crate::ir::ColType::Duration);
    assert!(ir.models[0].fields[1].nullable);
    assert!(fails(&model("timeout Duration[]")).contains("Duration[] is not supported"));
    assert!(fails(&model("timeout Duration @default(\"1\")")).contains("takes no literal default"));
    assert!(fails("type T {\n  d Duration\n}\nmodel A {\n  id BigInt @id\n  t T\n}").contains("a type holds no Duration field"));
    assert!(ok(&model("timeout Duration @default(dbgenerated(\"'1 hour'\"))")).models[0].fields[1].default_sql.is_some());
    // An engine of 5.4 cannot parse the type, so the IR carries the minor 5.
    ir.stamp_schema_contract();
    assert_eq!((ir.behavior.schema_contract, ir.behavior.schema_contract_minor), (monorm::extension::SCHEMA_CONTRACT, 5));
    let mut plain = ok(&model("n Int"));
    plain.stamp_schema_contract();
    assert_eq!(plain.behavior.schema_contract_minor, 0);
}

#[test]
fn a_duration_compares_and_orders_but_is_no_number() {
    const MODELS: &str = "model Item {\n  id BigInt @id\n  t Duration\n  u Duration\n  n Int\n}\n";
    for ok_filter in ["\"x\", t > u", "\"x\", t == u", "\"x\", t != u", "\"x\", t <= u"] {
        resolved_in(MODELS, ok_filter).unwrap_or_else(|e| panic!("{ok_filter}: {e}"));
    }
    for bad in ["\"x\", t > n", "\"x\", t == n", "\"x\", t + u > t"] {
        assert!(resolved_in(MODELS, bad).is_err(), "{bad}");
    }
}

#[test]
fn string_with_byte_a_still_compiles_with_a_deprecation_warning() {
    let project = compile_project("datasource db {\n provider = \"postgresql\"\n}\nmodel A {\n  id BigInt @id\n  raw String @db.ByteA\n}", None).unwrap();
    assert_eq!(project.ir.models[0].fields[1].ty, crate::ir::ColType::String);
    assert!(project.warnings.iter().any(|w| w.message.contains("`String @db.ByteA` is deprecated")), "{:?}", project.warnings);
    let bytes = compile_project("datasource db {\n provider = \"postgresql\"\n}\nmodel A {\n  id BigInt @id\n  raw Bytes\n}", None).unwrap();
    assert!(bytes.warnings.is_empty(), "{:?}", bytes.warnings);
}

const SHAPE_MODELS: &str = "enum Status {\n  ACTIVE\n  ARCHIVED\n}\nmodel Owner {\n  id BigInt @id\n  name String\n  status Status\n}\nmodel Item {\n  id BigInt @id\n  a Int\n  title String\n  status Status\n  owner_id BigInt\n  owner Owner @relation(fields: [owner_id], references: [id])\n}\n";

#[test]
fn an_enum_member_gives_the_same_tree_as_its_string_form_and_names_the_stored_value() {
    let (native, none) = resolved_in(SHAPE_MODELS, "\"x\", status == ACTIVE").unwrap();
    let (string, warned) = resolved_in(SHAPE_MODELS, "\"x\", \"status == ACTIVE\"").unwrap();
    assert_eq!(native, string);
    assert!(none.is_empty() && warned.len() == 1, "{none:?} {warned:?}");
    assert_eq!(native["t"], "cmp");
    assert_eq!(native["l"], json!({"t":"col","path":[],"name":"status"}));
    // The other side holds the stored value of the member, not a source name.
    assert_eq!(native["r"], json!({"t":"text","value":"ACTIVE"}));
    let (path, _) = resolved_in(SHAPE_MODELS, "\"x\", owner.status != ARCHIVED").unwrap();
    assert_eq!(path["l"], json!({"t":"col","path":["owner"],"name":"status"}));
}

#[test]
fn path_method_and_function_shapes_resolve_to_one_pinned_tree() {
    let col = |name: &str| json!({"t":"col","path":[],"name":name});
    let (tree, _) = resolved_in(SHAPE_MODELS, "\"x\", owner.name.in_([\"a\", \"b\"])").unwrap();
    assert_eq!(tree["t"], "in", "{tree}");
    assert_eq!(tree["item"], json!({"t":"col","path":["owner"],"name":"name"}));
    let (tree, _) = resolved_in(SHAPE_MODELS, "\"x\", a.not_in([1, 2])").unwrap();
    assert_eq!(tree["t"], "in", "{tree}");
    assert_eq!(tree["neg"], true);
    let (tree, _) = resolved_in(SHAPE_MODELS, "\"x\", title.like(\"a%\")").unwrap();
    assert_eq!(tree, json!({"t":"like","item":col("title"),"pattern":{"t":"text","value":"a%"},"ci":false,"neg":false}));
    let (tree, _) = resolved_in(SHAPE_MODELS, "\"x\", title.ilike(\"a%\")").unwrap();
    assert_eq!(tree["ci"], true);
    let (tree, _) = resolved_in(SHAPE_MODELS, "\"x\", lower(title) == \"x\"").unwrap();
    assert_eq!(tree["l"]["t"], "func", "{tree}");
    assert_eq!(tree["l"]["args"], json!([col("title")]));
    let (tree, _) = resolved_in(SHAPE_MODELS, "\"x\", a + 1 > 3").unwrap();
    assert_eq!(tree["l"]["t"], "arith", "{tree}");
}

#[test]
fn a_deferrable_unique_on_a_json_subfield_is_refused_by_name() {
    let json = NESTED.replace("address Address", "address Address @storage(json)");
    let with = |args: &str| json.trim_end().trim_end_matches('}').to_owned() + &format!("  @@unique([address.city]{args})\n}}\n");
    assert!(nested_sql(&with("")).contains("UNIQUE INDEX"));
    let e = fails(&with(", deferrable: deferred"));
    assert!(e.contains("@@unique: a key on a json subfield") && e.contains("cannot be deferrable"), "{e}");
}

fn lower_deferred(source: &str) -> (crate::ir::SchemaIr, lower::Deferred) {
    let load = |_: &str| Err("no imports".to_owned());
    let views = std::collections::HashSet::new();
    let none = Default::default();
    let (ir, _, deferred) = lower::Lowering { load: &load, deferred_identity: &none, views: &views, owned_types: &Default::default(), database: None }.lower_deferred(syntax::parse(source).unwrap()).unwrap();
    (ir, deferred)
}

fn late_field(ir: &mut crate::ir::SchemaIr) {
    let mut late = ir.models[0].fields[0].clone();
    late.name = "late".into();
    late.column = "late".into();
    late.primary_key = false;
    ir.models[0].fields.push(late);
}

const LATE: &str = "model A {\n  id Int @id\n  @@index([id], where: late == null)\n  @@check(late != 3)\n}";

/// A field that a pass adds after lowering is known to the native expressions of the second round.
#[test]
fn a_native_expression_waits_for_the_field_that_a_pass_adds() {
    let (mut ir, mut deferred) = lower_deferred(LATE);
    assert!(ir.models[0].indexes[0].where_.as_deref().unwrap().starts_with('\u{1}'));
    late_field(&mut ir);
    deferred.resolve(&mut ir, true).unwrap();
    deferred.finish(&mut ir).unwrap();
    assert!(ir.models[0].indexes[0].where_.as_deref().unwrap().contains("late"));
}

/// A field that no model declares after the passes is the same error as before, and it names the model that wrote the expression.
#[test]
fn a_field_that_no_pass_adds_is_an_error_naming_the_model() {
    let (mut ir, mut deferred) = lower_deferred(LATE);
    let e = deferred.resolve(&mut ir, true).unwrap_err();
    assert!(e.msg.contains("unknown field `late` on model `A`") && e.pos.line == 3, "{} at {}", e.msg, e.pos);
}

/// The slots of the second round are appended to the ones that exist, not assigned over them.
#[test]
fn the_native_slots_append_to_the_existing_slots() {
    let (mut ir, mut deferred) = lower_deferred(LATE);
    ir.native_expression_slots.push(("Other".into(), "check:0".into()));
    late_field(&mut ir);
    deferred.resolve(&mut ir, true).unwrap();
    deferred.finish(&mut ir).unwrap();
    assert_eq!(ir.native_expression_slots, [("Other".to_owned(), "check:0".to_owned()), ("A".into(), "index:0:where".into()), ("A".into(), "check:0".into())]);
}

/// A pass that puts an index before a native one moves the slot with it: the plain index gets no slot.
#[test]
fn a_native_slot_follows_its_index_when_a_pass_inserts_one_before() {
    let (mut ir, mut deferred) = lower_deferred(LATE);
    let mut plain = ir.models[0].indexes[0].clone();
    plain.where_ = None;
    ir.models[0].indexes.insert(0, plain);
    late_field(&mut ir);
    deferred.resolve(&mut ir, true).unwrap();
    deferred.finish(&mut ir).unwrap();
    assert!(ir.native_expression_slots.contains(&("A".to_owned(), "index:1:where".to_owned())), "{:?}", ir.native_expression_slots);
    assert!(!ir.native_expression_slots.iter().any(|(_, p)| p == "index:0:where"), "{:?}", ir.native_expression_slots);
}

/// A pass that leaves a marker inside a longer text is reported, not shipped to the database.
#[test]
fn a_marker_that_a_pass_wrapped_is_an_error() {
    let (mut ir, mut deferred) = lower_deferred(LATE);
    let wrapped = format!("({})", ir.models[0].indexes[0].where_.as_deref().unwrap());
    ir.models[0].indexes[0].where_ = Some(wrapped);
    late_field(&mut ir);
    deferred.resolve(&mut ir, true).unwrap();
    let e = deferred.finish(&mut ir).unwrap_err();
    assert!(e.msg.contains("was not resolved"), "{}", e.msg);
}

fn owning_src(provider: &str, owner_extra: &str, relation: &str) -> String {
    format!(
        "datasource db {{\n  provider = \"{provider}\"\n}}\nmodel Guitar {{\n  id BigInt @id\n  spec_id BigInt\n  spec Spec {relation}\n{owner_extra}}}\nmodel Spec {{\n  id BigInt @id\n  code String @unique\n  guitars Guitar[]\n}}\n"
    )
}

#[test]
fn an_owning_relation_sets_the_flag_and_the_contract_minor() {
    let ir = ok(&owning_src("postgresql", "", "@relation(fields: [spec_id], references: [id], owns: true)"));
    assert!(ir.models.iter().find(|m| m.name == "Guitar").unwrap().relations.iter().find(|r| r.name == "spec").unwrap().owns);
    let mut stamped = ir.clone();
    stamped.stamp_schema_contract();
    assert_eq!(stamped.behavior.schema_contract_minor, 6);
    let plain = ok(&owning_src("postgresql", "", "@relation(fields: [spec_id], references: [id])"));
    assert!(!plain.models.iter().flat_map(|m| &m.relations).any(|r| r.owns));
    let mut stamped = plain;
    stamped.stamp_schema_contract();
    assert!(stamped.behavior.schema_contract_minor < 6);
}

#[test]
fn an_owning_relation_takes_every_on_delete_value() {
    for action in ["Cascade", "NoAction", "Restrict"] {
        let rel = format!("@relation(fields: [spec_id], references: [id], onDelete: {action}, owns: true)");
        check(ok(&owning_src("postgresql", "", &rel))).unwrap_or_else(|e| panic!("{action}: {e}"));
    }
    let nullable = "datasource db {\n  provider = \"postgresql\"\n}\nmodel Guitar {\n  id BigInt @id\n  spec_id BigInt?\n  spec Spec? @relation(fields: [spec_id], references: [id], onDelete: SetNull, owns: true)\n}\nmodel Spec {\n  id BigInt @id\n  guitars Guitar[]\n}\n";
    check(ok(nullable)).unwrap();
}

#[test]
fn owns_goes_on_a_to_one_side_with_fields() {
    let list = fails(&owning_src("postgresql", "", "@relation(fields: [spec_id], references: [id], owns: true)").replace("guitars Guitar[]", "guitars Guitar[] @relation(owns: true)"));
    assert!(list.contains("owns: goes on a to-one side that has fields: and references:, not on Guitar[]"), "{list}");
    let back = "datasource db {\n  provider = \"postgresql\"\n}\nmodel Guitar {\n  id BigInt @id\n  spec_id BigInt @unique\n  spec Spec @relation(fields: [spec_id], references: [id])\n}\nmodel Spec {\n  id BigInt @id\n  guitar Guitar? @relation(owns: true)\n}\n";
    assert!(fails(back).contains("owns: goes on a to-one side"));
    let through = "datasource db {\n  provider = \"postgresql\"\n}\nmodel A {\n  id BigInt @id\n  bs B[] @relation(through: AB, owns: true)\n}\nmodel B {\n  id BigInt @id\n}\nmodel AB {\n  a_id BigInt\n  b_id BigInt\n  a A @relation(fields: [a_id], references: [id])\n  b B @relation(fields: [b_id], references: [id])\n  @@id([a_id, b_id])\n}\n";
    assert!(fails(through).contains("not on a many-to-many relation"));
    assert!(fails(&owning_src("postgresql", "", "@relation(fields: [spec_id], references: [id], owns: 1)")).contains("owns must be true or false"));
}

#[test]
fn an_owning_relation_references_the_primary_key() {
    let e = fails(&owning_src("postgresql", "", "@relation(fields: [spec_id], references: [code], owns: true)").replace("spec_id BigInt", "spec_id String"));
    assert!(e.contains("relation Guitar.spec: owns: references: must be the primary key of Spec"), "{e}");
}

#[test]
fn an_owning_relation_keeps_the_scope_of_the_owned_row() {
    use crate::writes::ScopedField;
    let scoped = |model: &str| ScopedField { source: "x".into(), model: model.into(), field: "id".into(), key: "tenant".into() };
    let mut ir = ok(&owning_src("postgresql", "", "@relation(fields: [spec_id], references: [id], owns: true)"));
    ir.behavior.scoped_fields = vec![scoped("Guitar")];
    ir.stamp_schema_contract();
    let e = check(ir.clone()).err().expect("an error");
    assert!(e.contains("Guitar and Spec must have the same scope keys"), "{e}");
    ir.behavior.scoped_fields = vec![scoped("Guitar"), scoped("Spec")];
    check(ir).unwrap();
}

#[test]
fn a_model_owns_itself_on_postgresql_only() {
    let own = |provider: &str| {
        format!("datasource db {{\n  provider = \"{provider}\"\n}}\nmodel Node {{\n  id BigInt @id\n  next_id BigInt?\n  next Node? @relation(\"next\", fields: [next_id], references: [id], owns: true)\n  prev Node[] @relation(\"next\")\n}}\n")
    };
    check(ok(&own("postgresql"))).unwrap();
    let e = check(ok(&own("sqlite"))).err().expect("an error");
    assert!(e.contains("a model can't own itself on SQLite"), "{e}");
}

#[test]
fn an_owning_cycle_of_any_length_is_refused_on_sqlite_only() {
    let ring = |provider: &str| format!("datasource db {{\n  provider = \"{provider}\"\n}}\n\
        model A {{\n  id BigInt @id\n  b_id BigInt?\n  b B? @relation(\"ab\", fields: [b_id], references: [id], owns: true)\n  from_c C[] @relation(\"ca\")\n}}\n\
        model B {{\n  id BigInt @id\n  c_id BigInt?\n  c C? @relation(\"bc\", fields: [c_id], references: [id], owns: true)\n  from_a A[] @relation(\"ab\")\n}}\n\
        model C {{\n  id BigInt @id\n  a_id BigInt?\n  a A? @relation(\"ca\", fields: [a_id], references: [id], owns: true)\n  from_b B[] @relation(\"bc\")\n}}\n");
    check(ok(&ring("postgresql"))).unwrap();
    let e = check(ok(&ring("sqlite"))).err().expect("an error");
    assert!(e.contains("an owning cycle (") && e.contains("is not allowed on SQLite"), "{e}");
}

const LINKED: &str = "datasource db {\n  provider = \"postgresql\"\n}\n\
model User {\n  id BigInt @id\n  name String\n  posts Post[]\n  comments Comment[]\n  commenters User[] @relation(path: [posts, comments, author])\n}\n\
model Post {\n  id BigInt @id\n  author_id BigInt\n  author User @relation(fields: [author_id], references: [id])\n  comments Comment[]\n}\n\
model Comment {\n  id BigInt @id\n  post_id BigInt\n  author_id BigInt\n  post Post @relation(fields: [post_id], references: [id])\n  author User @relation(fields: [author_id], references: [id])\n}\n";

const CHAIN: &str = "datasource db {\n  provider = \"postgresql\"\n}\n\
model Post {\n  id BigInt @id\n  address_id BigInt\n  address Address @relation(fields: [address_id], references: [id])\n  country Country? @relation(path: [address, city, country])\n}\n\
model Address {\n  id BigInt @id\n  city_id BigInt\n  city City @relation(fields: [city_id], references: [id])\n  posts Post[]\n}\n\
model City {\n  id BigInt @id\n  country_id BigInt\n  country Country @relation(fields: [country_id], references: [id])\n  addresses Address[]\n}\n\
model Country {\n  id BigInt @id\n  cities City[]\n}\n";

fn linked_with(from: &str, to: &str) -> String {
    assert!(LINKED.contains(from), "{from}");
    LINKED.replace(from, to)
}

#[test]
fn a_linked_relation_keeps_its_path_and_sets_the_contract_minor() {
    let ir = ok(LINKED);
    let r = ir.models.iter().find(|m| m.name == "User").unwrap().relations.iter().find(|r| r.name == "commenters").unwrap();
    assert_eq!((r.kind, r.path.clone(), r.foreign_key, r.from.is_empty(), r.constraint), (crate::ir::RelKind::Many, vec!["posts".to_string(), "comments".into(), "author".into()], false, true, true));
    let mut stamped = ir.clone();
    stamped.stamp_schema_contract();
    assert_eq!(stamped.behavior.schema_contract_minor, 7);
    check(ir).unwrap();
    let ir = ok(CHAIN);
    check(ir).unwrap();
}

#[test]
fn a_linked_relation_needs_no_back_side_and_may_use_a_linked_hop() {
    let src = LINKED.replace("  comments Comment[]\n  commenters", "  comments Comment[]\n  also User[] @relation(path: [commenters])\n  commenters");
    check(ok(&src)).unwrap();
}

#[test]
fn a_linked_relation_with_a_wrong_path_is_a_schema_error() {
    let e = fails(&linked_with("[posts, comments, author]", "[posts, nothing, author]"));
    assert!(e.contains("relation User.commenters: path: Post has no relation nothing"), "{e}");
    let e = fails(&linked_with("[posts, comments, author]", "[posts, author, author]"));
    assert!(e.contains("path: author is a relation of Comment, not of User") || e.contains("path: author is a relation of Post"), "{e}");
    let e = fails(&linked_with("[posts, comments, author]", "[posts, comments, post]"));
    assert!(e.contains("path: the last hop ends at Post, not at User"), "{e}");
    let e = fails(&linked_with("[posts, comments, author]", "[commenters]"));
    assert!(e.contains("path: a cycle: User.commenters -> User.commenters"), "{e}");
    let e = fails(&linked_with("commenters User[]", "commenters User?"));
    assert!(e.contains("path: a hop is to-many, so the relation is User[], not User?"), "{e}");
    let e = fails(&linked_with("commenters User[]", "commenters User"));
    assert!(e.contains("a linked relation over to-one hops is optional: User?"), "{e}");
}

#[test]
fn a_linked_relation_has_no_keys_of_its_own() {
    for key in ["fields: [id]", "references: [id]", "through: Comment", "onDelete: Cascade"] {
        let e = fails(&linked_with("path: [posts, comments, author]", &format!("path: [posts, comments, author], {key}")));
        assert!(e.contains(&format!("a linked relation, which has no keys of its own; {}: does not go with it", key.split(':').next().unwrap())), "{e}");
    }
}

#[test]
fn a_cycle_through_two_linked_relations_is_a_schema_error() {
    let src = LINKED.replace("  commenters User[] @relation(path: [posts, comments, author])\n", "  a User[] @relation(path: [b])\n  b User[] @relation(path: [a])\n");
    let e = fails(&src);
    assert!(e.contains("path: a cycle: User.a -> User.b -> User.a"), "{e}");
}

#[test]
fn constraint_false_sets_the_flag_and_refuses_the_actions() {
    let plain = "@relation(fields: [spec_id], references: [id])";
    let ir = ok(&owning_src("postgresql", "", "@relation(fields: [spec_id], references: [id], constraint: false)"));
    let r = ir.models.iter().find(|m| m.name == "Guitar").unwrap().relations.iter().find(|r| r.name == "spec").unwrap();
    assert!(!r.constraint && r.foreign_key);
    let mut stamped = ir.clone();
    stamped.stamp_schema_contract();
    assert_eq!(stamped.behavior.schema_contract_minor, 7);
    let mut stamped = ok(&owning_src("postgresql", "", plain));
    stamped.stamp_schema_contract();
    assert!(stamped.behavior.schema_contract_minor < 7);
    for arg in ["onDelete: Cascade", "onUpdate: Cascade", "deferrable: deferred", "map: \"fk_spec\""] {
        let e = fails(&owning_src("postgresql", "", &format!("@relation(fields: [spec_id], references: [id], constraint: false, {arg})")));
        assert!(e.contains(&format!("{}: does not go with constraint: false", arg.split(':').next().unwrap())), "{e}");
    }
    let e = fails(&owning_src("postgresql", "", "@relation(fields: [spec_id], references: [id], constraint: false, owns: true)"));
    assert!(e.contains("owns: does not go with constraint: false"), "{e}");
    let e = fails(&owning_src("postgresql", "", "@relation(fields: [spec_id], references: [id], constraint: 1)"));
    assert!(e.contains("constraint must be true or false"), "{e}");
}

fn trigger_schema(provider: &str, extra: &str, attrs: &str) -> String {
    format!(
        "datasource db {{\n  provider = \"{provider}\"\n  {extra}\n}}\nfunction f {{\nreturns = trigger\nlanguage = plpgsql\nbody = \"\"\"\nBEGIN RETURN NULL; END;\n\"\"\"\n}}\n\
         model Item {{\n  id BigInt @id\n  title String\n  stock Int\n  updated_at DateTime\n  {attrs}\n}}\n"
    )
}

fn pg_ddl(attrs: &str) -> String {
    let (_, schema) = check(ok(&trigger_schema("postgresql", "", attrs))).unwrap_or_else(|e| panic!("{e}"));
    crate::migrate::create_all(&schema).unwrap().into_iter().find(|s| s.contains("TRIGGER")).unwrap()
}

fn pg_fails(attrs: &str) -> String {
    fails(&trigger_schema("postgresql", "", attrs))
}

#[test]
fn create_tables_drops_a_constraint_trigger_before_it_creates_it() {
    let (_, schema) = check(ok(&trigger_schema("postgresql", "", "@@trigger(t, when: after, operation: [insert], deferrable: deferred, func: f)"))).unwrap();
    let plain = crate::migrate::create_all(&schema).unwrap().into_iter().find(|s| s.contains("TRIGGER")).unwrap();
    assert!(plain.starts_with("DROP TRIGGER IF EXISTS \"t\" ON \"item\";\nCREATE CONSTRAINT TRIGGER \"t\" AFTER INSERT ON \"item\" DEFERRABLE INITIALLY DEFERRED"), "{plain}");
    assert!(!plain.contains("OR REPLACE"), "{plain}");
}

#[test]
fn a_trigger_takes_the_pgtrigger_arguments() {
    let t = |attrs: &str| ok(&trigger_schema("postgresql", "", attrs)).models[0].triggers[0].clone();
    let one = t("@@trigger(t, when: before, operation: [insert, update_of(title, stock), delete], condition: \"true\", func: \"BEGIN RETURN NEW; END;\")");
    assert_eq!(one.timing, crate::ir::TriggerTiming::Before);
    assert_eq!(one.events, [crate::ir::TriggerEvent::Insert, crate::ir::TriggerEvent::Update, crate::ir::TriggerEvent::Delete]);
    assert_eq!(one.update_of, ["title", "stock"]);
    assert_eq!(one.for_each, crate::ir::ForEach::Row);
    assert_eq!(one.when.as_deref(), Some("true"));
    assert_eq!(one.body.as_deref(), Some("BEGIN RETURN NEW; END;"));
    // a single operation needs no list, and a list of conditions is joined with AND
    let two = t("@@trigger(t, when: after, operation: update, level: statement, condition: [\"1 = 1\", raw(\"2 = 2\")], func: f, args: [\"a\"])");
    assert_eq!(two.for_each, crate::ir::ForEach::Statement);
    assert_eq!(two.when.as_deref(), Some("(1 = 1) AND (2 = 2)"));
    assert_eq!((two.function.as_deref(), two.args.as_slice()), (Some("f"), ["a".to_owned()].as_slice()));
    let three = t("@@trigger(t, referencing(old: o, new: n), when: after, operation: [update], level: statement, func: f)");
    assert_eq!((three.old_table.as_deref(), three.new_table.as_deref()), (Some("o"), Some("n")));
    let four = t("@@trigger(t, when: after, operation: [insert], deferrable: deferred, func: f)");
    assert_eq!(four.deferrable, Some(crate::ir::Deferrable::Deferred));
    let five = t("@@trigger(t, when: before, operation: [insert], declare: \"x int;\", func: \"BEGIN RETURN NEW; END;\")");
    assert_eq!(five.declare.as_deref(), Some("x int;"));
    let six = t("@@trigger(t, when: before, operation: [insert], skippable: true, func: \"BEGIN RETURN NEW; END;\")");
    assert_eq!(six.skippable.as_deref(), Some("t"));
    let seven = t("@@trigger(t, when: before, operation: [update], condition: [any_change(title, stock), all_dont_change(exclude: [title], exclude_auto: true)], func: \"BEGIN RETURN NEW; END;\")");
    assert_eq!(seven.changes.len(), 2);
    assert_eq!(seven.changes[0].fields, ["title", "stock"]);
    assert!(seven.changes[1].exclude_auto && seven.changes[1].exclude == ["title"]);
    let mut plain = ok(&trigger_schema("postgresql", "", "@@trigger(t, when: before, operation: [insert], func: \"x\")"));
    plain.stamp_schema_contract();
    assert_eq!(plain.behavior.schema_contract_minor, 0);
    let mut stamped = ok(&trigger_schema("postgresql", "", "@@trigger(t, when: after, operation: [insert], deferrable: immediate, func: f)"));
    stamped.stamp_schema_contract();
    assert_eq!(stamped.behavior.schema_contract_minor, 8);
}

#[test]
fn a_trigger_schema_error_names_the_rule() {
    let body = "func: \"BEGIN RETURN NEW; END;\"";
    let cases = [
        (format!("@@trigger(t, referencing(new: n), when: after, operation: [insert, update], level: statement, {body})"), "referencing needs exactly one event"),
        (format!("@@trigger(t, referencing(new: n), when: before, operation: [insert], level: statement, {body})"), "referencing needs when: after"),
        (format!("@@trigger(t, referencing(new: n), when: after, operation: [update_of(title)], level: statement, {body})"), "referencing allows no update_of"),
        (format!("@@trigger(t, referencing(new: n), when: after, operation: [insert], deferrable: deferred, {body})"), "referencing allows no deferrable"),
        (format!("@@trigger(t, referencing(old: o), when: after, operation: [insert], level: statement, {body})"), "old: needs an update or delete event"),
        (format!("@@trigger(t, referencing(new: n), when: after, operation: [delete], level: statement, {body})"), "new: needs an update or insert event"),
        (format!("@@trigger(t, when: before, operation: [insert], deferrable: deferred, {body})"), "deferrable: needs when: after and level: row"),
        (format!("@@trigger(t, when: after, operation: [insert], level: statement, deferrable: deferred, {body})"), "deferrable: needs when: after and level: row"),
        (format!("@@trigger(t, when: instead_of, operation: [insert], level: statement, {body})"), "instead_of needs level: row"),
        (format!("@@trigger(t, when: instead_of, operation: [insert], condition: \"true\", {body})"), "instead_of allows no condition:"),
        (format!("@@trigger(t, when: instead_of, operation: [update_of(title)], {body})"), "instead_of allows no update_of"),
        (format!("@@trigger(t, when: instead_of, operation: [insert], skippable: true, {body})"), "instead_of allows no skippable: true"),
        (format!("@@trigger(t, when: after, operation: [truncate], {body})"), "truncate needs level: statement"),
        (format!("@@trigger(t, when: before, operation: [update], level: statement, condition: any_change(title), {body})"), "needs level: row"),
        (format!("@@trigger(t, when: before, operation: [insert, update], condition: any_change(title), {body})"), "update as the only event"),
        (format!("@@trigger(t, when: before, operation: [update], level: statement, condition: \"NEW.stock > 0\", {body})"), "a statement-level condition cannot read OLD or NEW"),
        (format!("@@trigger(t, when: before, operation: [insert], condition: \"OLD.stock > 0\", {body})"), "a condition of an insert trigger cannot read OLD"),
        (format!("@@trigger(t, when: before, operation: [delete], condition: \"new.stock > 0\", {body})"), "a condition of a delete trigger cannot read NEW"),
        (format!("@@trigger(t, when: before, operation: [update], condition: any_change(nope), {body})"), "nope"),
        (format!("@@trigger(t, when: before, operation: [update, update_of(title)], {body})"), "update"),
        (format!("@@trigger(t, when: before, operation: [insert, insert], {body})"), "twice"),
        (format!("@@trigger(t, operation: [insert], {body})"), "say when it fires"),
        (format!("@@trigger(t, when: before, {body})"), "operation"),
        (format!("@@trigger(t, when: before, operation: [insert], declare: \"x int;\", func: f)"), "declare"),
        (format!("@@trigger(t, when: before, operation: [insert], func: f, body: \"x\")"), "`body:` is removed"),
        (format!("@@trigger(t, when: before, operation: [update], condition: any_change(title, exclude: [title]), {body})"), "exclude"),
        (format!("@@trigger(t, when: before, operation: [update], condition: any_change(exclude: [title, stock, id, updated_at]), {body})"), "none"),
    ];
    for (attrs, expected) in cases {
        let e = match compile(&trigger_schema("postgresql", "", &attrs), None).and_then(check) {
            Ok(_) => panic!("{attrs}: expected an error"),
            Err(e) => e,
        };
        assert!(e.contains(expected), "{attrs}\n  got: {e}\n  expected: {expected}");
    }
}

#[test]
fn a_row_word_inside_a_string_literal_is_no_row_read() {
    let body = "func: \"BEGIN RETURN NEW; END;\"";
    let attrs = |cond: &str| format!("@@trigger(t, when: before, operation: [insert], condition: \"{cond}\", {body})");
    let accepted = |cond: &str| compile(&trigger_schema("postgresql", "", &attrs(cond)), None).and_then(check).map(|_| ());
    accepted("NEW.title <> 'old.x'").unwrap();
    accepted("NEW.title <> 'it''s old.x'").unwrap();
    let e = accepted("NEW.title <> 'x' AND OLD.stock > 0").unwrap_err();
    assert!(e.contains("a condition of an insert trigger cannot read OLD"), "{e}");
}

#[test]
fn an_old_trigger_argument_names_the_new_one() {
    for (attrs, expected) in [
        ("@@trigger(t, before: [insert], func: f)", "`before:` is removed; use `when: before"),
        ("@@trigger(t, after: [insert], func: f)", "`after:` is removed; use `when: after"),
        ("@@trigger(t, instead_of: [insert], func: f)", "`instead_of:` is removed; use `when: instead_of"),
        ("@@trigger(t, when: before, operation: [insert], for_each: statement, func: f)", "`for_each:` is removed; use `level:"),
        ("@@trigger(t, when: before, operation: [update], update_of: [title], func: f)", "`update_of:` is removed; use `operation: [update_of(a, b)]`"),
        ("@@trigger(t, when: before, operation: [insert], body: \"x\")", "`body:` is removed; use `func: \"...\"`"),
        ("@@trigger(t, when: before, operation: [insert], function: f)", "`function:` is removed; use `func: <function block name>`"),
        ("@@trigger(t, when: \"NEW.stock > 0\", operation: [insert], func: f)", "`condition: \"...\"`"),
    ] {
        let e = pg_fails(attrs);
        assert!(e.contains(expected), "{attrs}\n  got: {e}\n  expected: {expected}");
    }
}

#[test]
fn trigger_ddl_has_deferral_transition_tables_and_helpers() {
    assert_eq!(
        pg_ddl("@@trigger(t, when: after, operation: [insert, update], deferrable: deferred, func: f)"),
        "DROP TRIGGER IF EXISTS \"t\" ON \"item\";\nCREATE CONSTRAINT TRIGGER \"t\" AFTER INSERT OR UPDATE ON \"item\" DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION \"f\"()"
    );
    assert_eq!(
        pg_ddl("@@trigger(t, when: after, operation: [insert], deferrable: immediate, func: f)"),
        "DROP TRIGGER IF EXISTS \"t\" ON \"item\";\nCREATE CONSTRAINT TRIGGER \"t\" AFTER INSERT ON \"item\" DEFERRABLE INITIALLY IMMEDIATE FOR EACH ROW EXECUTE FUNCTION \"f\"()"
    );
    assert_eq!(
        pg_ddl("@@trigger(t, referencing(old: o, new: n), when: after, operation: [update], level: statement, func: f)"),
        "CREATE OR REPLACE TRIGGER \"t\" AFTER UPDATE ON \"item\" REFERENCING OLD TABLE AS \"o\" NEW TABLE AS \"n\" FOR EACH STATEMENT EXECUTE FUNCTION \"f\"()"
    );
    assert!(pg_ddl("@@trigger(t, referencing(new: n), when: after, operation: [insert], level: statement, func: f)").contains("REFERENCING NEW TABLE AS \"n\" FOR EACH STATEMENT"));
    let helper = |h: &str| pg_ddl(&format!("@@trigger(t, when: before, operation: [update], condition: {h}, func: f)"));
    assert!(helper("any_change(title, stock)").contains("WHEN ((OLD.\"title\" IS DISTINCT FROM NEW.\"title\" OR OLD.\"stock\" IS DISTINCT FROM NEW.\"stock\")) EXECUTE"));
    assert!(helper("all_change(title, stock)").contains("WHEN ((OLD.\"title\" IS DISTINCT FROM NEW.\"title\" AND OLD.\"stock\" IS DISTINCT FROM NEW.\"stock\")) EXECUTE"));
    assert!(helper("any_dont_change(title, stock)").contains("WHEN ((OLD.\"title\" IS NOT DISTINCT FROM NEW.\"title\" OR OLD.\"stock\" IS NOT DISTINCT FROM NEW.\"stock\")) EXECUTE"));
    assert!(helper("all_dont_change(title, stock)").contains("WHEN ((OLD.\"title\" IS NOT DISTINCT FROM NEW.\"title\" AND OLD.\"stock\" IS NOT DISTINCT FROM NEW.\"stock\")) EXECUTE"));
    let all = helper("any_change(exclude: [title])");
    assert!(all.contains("OLD.\"id\"") && all.contains("OLD.\"stock\"") && all.contains("OLD.\"updated_at\"") && !all.contains("OLD.\"title\""), "{all}");
    let both = helper("[any_change(title), \"NEW.stock > 0\"]");
    assert!(both.contains("WHEN ((NEW.stock > 0) AND (OLD.\"title\" IS DISTINCT FROM NEW.\"title\"))"), "{both}");
}

#[test]
fn exclude_auto_drops_the_fields_a_write_assignment_sets() {
    use monorm::writes::{AssignValue, UpsertDefault, WriteAssignment, WriteKind};
    let mut ir = ok(&trigger_schema("postgresql", "", "@@trigger(t, when: before, operation: [update], condition: any_change(exclude_auto: true), func: f)"));
    ir.behavior.write_assignments.push(WriteAssignment {
        source: "acme.touch".into(), model: "Item".into(), field: "updated_at".into(), on: vec![WriteKind::Update], value: AssignValue::Now, upsert: UpsertDefault::default(),
    });
    ir.stamp_schema_contract();
    let (_, schema) = check(ir).unwrap();
    let ddl = crate::migrate::create_all(&schema).unwrap().into_iter().find(|s| s.contains("TRIGGER")).unwrap();
    assert!(ddl.contains("OLD.\"title\"") && !ddl.contains("OLD.\"updated_at\""), "{ddl}");
}

#[test]
fn a_skippable_trigger_gets_the_skip_guard_in_when() {
    let attrs = "@@trigger(guarded, when: before, operation: [insert], condition: \"NEW.stock > 0\", skippable: true, func: \"BEGIN RETURN NEW; END;\")";
    let with_roles = {
        let (ir, schema) = check(ok(&trigger_schema("postgresql", "bypass_roles = [\"ops\"]", attrs))).unwrap();
        assert_eq!(ir.models[0].triggers[0].skippable.as_deref(), Some("guarded"));
        assert_eq!(schema.skippable_triggers().into_iter().collect::<Vec<_>>(), ["guarded"]);
        crate::migrate::create_all(&schema).unwrap().into_iter().find(|s| s.contains("TRIGGER \"guarded\"")).unwrap()
    };
    assert!(with_roles.contains("WHEN ((NEW.stock > 0) AND (NOT ('guarded' = ANY(") && with_roles.contains("pg_has_role(session_user, 'ops', 'member')"), "{with_roles}");
    // with no bypass_roles nobody can skip, so the trigger has no skip test
    assert!(pg_ddl(attrs).contains("WHEN (NEW.stock > 0) EXECUTE"));
    // a trigger without skippable: true has no guard
    let plain = {
        let (_, schema) = check(ok(&trigger_schema("postgresql", "bypass_roles = [\"ops\"]", "@@trigger(plain, when: before, operation: [insert], func: \"BEGIN RETURN NEW; END;\")"))).unwrap();
        assert!(schema.skippable_triggers().is_empty());
        crate::migrate::create_all(&schema).unwrap().into_iter().find(|s| s.contains("TRIGGER \"plain\"")).unwrap()
    };
    assert!(!plain.contains("WHEN"), "{plain}");
}

#[test]
fn a_declared_variable_goes_in_front_of_the_body() {
    let (_, schema) = check(ok(&trigger_schema("postgresql", "", "@@trigger(t, when: before, operation: [insert], declare: \"n int;\", func: \"BEGIN n := 1; RETURN NEW; END;\")"))).unwrap();
    let all = crate::migrate::create_all(&schema).unwrap().join("\n");
    assert!(all.contains("DECLARE n int; BEGIN n := 1; RETURN NEW; END;"), "{all}");
}

#[test]
fn sqlite_refuses_the_postgresql_trigger_features() {
    let sqlite = |attrs: &str| fails(&trigger_schema("sqlite", "", attrs).replace("function f {\nreturns = trigger\nlanguage = plpgsql\nbody = \"\"\"\nBEGIN RETURN NULL; END;\n\"\"\"\n}\n", ""));
    let body = "func: \"SELECT 1\"";
    for (attrs, expected) in [
        (format!("@@trigger(t, referencing(new: n), when: after, operation: [update], {body})"), "`referencing` needs PostgreSQL; sqlite has no such trigger feature"),
        (format!("@@trigger(t, when: after, operation: [insert], deferrable: deferred, {body})"), "`deferrable` needs PostgreSQL; sqlite has no such trigger feature"),
        (format!("@@trigger(t, when: after, operation: [insert], declare: \"x int;\", {body})"), "`declare` needs PostgreSQL; sqlite has no such trigger feature"),
    ] {
        let e = sqlite(&attrs);
        assert!(e.contains(expected), "{attrs}: {e}");
    }
    // a change helper and skippable work on SQLite: the helper is the SQLite null-safe test
    let src = trigger_schema("sqlite", "", "@@trigger(t, when: before, operation: [update], condition: any_change(title), skippable: true, func: \"SELECT 1\")")
        .replace("function f {\nreturns = trigger\nlanguage = plpgsql\nbody = \"\"\"\nBEGIN RETURN NULL; END;\n\"\"\"\n}\n", "");
    let (_, schema) = check(ok(&src)).unwrap();
    let ddl = crate::migrate::create_all(&schema).unwrap().join("\n");
    assert!(ddl.contains("WHEN (OLD.\"title\" IS NOT NEW.\"title\")") && !ddl.contains("pg_has_role"), "{ddl}");
}

#[test]
fn outermost_managed_import_option_wins() {
    let dir = std::env::temp_dir().join(format!("monorm-managed-imports-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let root = dir.join("schema.prisma");
    let app = dir.join("app.prisma");
    let leaf = dir.join("leaf.prisma");
    std::fs::write(&leaf, "model Leaf {\n id Int @id\n @@managed(false)\n}").unwrap();
    let mut installed = Installed::new();
    installed.apps.insert("example".into(), app.clone());
    for (inner, options, expected) in [
        (false, "", vec!["plain"]),
        (true, " (managed: false)", vec![]),
        (false, " (managed: true)", vec!["plain", "leaf"]),
    ] {
        std::fs::write(&app, format!("import \"leaf.prisma\" (managed: {inner})\nmodel Plain {{ id Int @id }}")).unwrap();
        let source = format!("import \"app:example\"{options}");
        let (_, schema) = check(compile_project_with(&source, Some(&root), &installed).unwrap().ir).unwrap();
        let sql = crate::migrate::create_all(&schema).unwrap().join("\n");
        for table in ["plain", "leaf"] {
            assert_eq!(sql.contains(&format!("\"{table}\"")), expected.contains(&table), "{sql}");
        }
    }
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn managed_options_and_attributes_report_each_misuse() {
    let model = |attr: &str| format!("model A {{\n id Int @id\n {attr}\n}}");
    for (attr, expected) in [
        ("@@managed", "@@managed takes true or false"),
        ("@@managed(false, true)", "@@managed takes true or false"),
        ("@@managed(value: false)", "@@managed takes true or false"),
        ("@@managed(\"no\")", "@@managed takes true or false"),
        ("@@managed(false)\n @@managed(false)", "@@managed is given twice"),
        ("@@managed(true)\n @@managed(true)", "@@managed is given twice"),
        ("@@managed(true)\n @@managed(false)", "@@managed is given twice"),
        ("@@managed(false)\n @@managed(true)", "@@managed is given twice"),
    ] {
        let error = fails(&model(attr));
        assert!(error.contains(expected), "{attr}: {error}");
    }
    let view = views("view V {\n id Int @id\n @@managed(false)\n}\n", &[("views/public/V.sql", "SELECT 1 AS id")]).unwrap_err();
    assert!(view.contains("@@managed does not apply to a view"), "{view}");

    for (options, expected) in [
        ("(managed: 1)", "schema import managed must be true or false"),
        ("(managed: \"no\")", "schema import managed must be true or false"),
        ("(shadow: true)", "unknown schema import option shadow"),
        ("(prefix: 1)", "schema import prefix must be a string"),
        ("()", "schema import takes named prefix and managed options"),
        ("(true)", "schema import takes named prefix and managed options"),
    ] {
        let error = fails(&format!("import \"other.prisma\" {options}"));
        assert!(error.contains(expected), "{options}: {error}");
    }
}

#[test]
fn managed_true_equals_the_default() {
    let model = |attr: &str| format!("model A {{\n id Int @id\n {attr}\n}}");
    let plain = ok(&model(""));
    let explicit = ok(&model("@@managed(true)"));
    assert!(explicit.models.iter().all(|m| m.managed));
    assert_eq!(serde_json::to_value(&plain).unwrap(), serde_json::to_value(&explicit).unwrap());
    let (_, schema) = check(explicit).unwrap();
    let sql = crate::migrate::create_all(&schema).unwrap().join("\n");
    assert!(sql.contains("CREATE TABLE IF NOT EXISTS \"a\""), "{sql}");
}

#[test]
fn managed_is_refused_on_an_extension_import() {
    let dir = std::env::temp_dir().join(format!("monorm-managed-extension-import-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("acme.toml"), "name = \"acme\"\n[types.money]\nsql = \"numeric(12, 2)\"\nvalue = \"text\"\n").unwrap();
    std::fs::write(dir.join("schema.prisma"), "import \"acme.toml\" (managed: false)\nmodel A {\n id Int @id\n}\n").unwrap();
    let error = compile_file(&dir.join("schema.prisma")).unwrap_err();
    assert!(error.contains("managed is only allowed on schema imports"), "{error}");
    std::fs::remove_dir_all(dir).unwrap();
}

fn database_project(name: &str, toml: &str, schema: &str) -> (Installed, PathBuf, PathBuf) {
    let dir = std::env::temp_dir().join(format!("monorm-database-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("monorm.toml"), toml).unwrap();
    std::fs::write(dir.join("schema.prisma"), schema).unwrap();
    let installed = Installed { database: crate::settings::Settings::load(&dir).unwrap().database().unwrap(), ..Default::default() };
    (installed, dir.join("schema.prisma"), dir)
}

#[test]
fn the_database_table_fills_the_ir_like_a_datasource_block() {
    let model = "model Post {\n id Int @id\n tenant_id Int\n}\n";
    let keys = "bypass_roles = [\"app_admin\"]\nexempt_roles = [\"reporting\"]\nscope_prefix = \"tenant\"\nscope_bypass = { setting = \"tenant.bypass\", value = \"on\" }\nscope_role_check = true\nhard_delete_setting = \"rules.hard_deletion\"\nextensions = [\"citext\", { name = \"postgis\", schema = \"ext\", version = \"3.4\" }]\n";
    let (installed, file, dir) = database_project("fills", &format!("[database]\nprovider = \"postgresql\"\n{keys}"), model);
    let from_table = compile_project_file_with(&file, &installed).unwrap().ir;
    std::fs::write(&file, format!("datasource db {{\n provider = \"postgresql\"\n bypass_roles = [\"app_admin\"]\n exempt_roles = [\"reporting\"]\n scope_prefix = \"tenant\"\n scope_bypass = {{ setting = \"tenant.bypass\", value = \"on\" }}\n scope_role_check = true\n hard_delete_setting = \"rules.hard_deletion\"\n extensions = [citext, postgis(schema: \"ext\", version: \"3.4\")]\n}}\n{model}")).unwrap();
    let from_block = compile_project_file(&file).unwrap().ir;
    assert_eq!(serde_json::to_value(&from_table).unwrap(), serde_json::to_value(&from_block).unwrap());
    assert_eq!((from_table.bypass_roles.clone(), from_table.scope_settings.prefix.as_str(), from_table.extensions.len()), (vec!["app_admin".to_owned()], "tenant", 2));
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn the_database_provider_sets_the_dialect_and_the_view_body_folder() {
    let schema = "view Totals {\n id Int @id\n}\n";
    let (installed, file, dir) = database_project("dialect", "[database]\nprovider = \"sqlite\"\n", schema);
    std::fs::create_dir_all(dir.join("views/main")).unwrap();
    std::fs::write(dir.join("views/main/Totals.sql"), "SELECT 1 AS id").unwrap();
    let ir = compile_project_file_with(&file, &installed).unwrap().ir;
    assert_eq!(ir.dialect, crate::dialect::Dialect::Sqlite);
    assert_eq!(ir.models[0].sql_view.as_ref().unwrap().body, "SELECT 1 AS id");
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_datasource_block_with_a_database_table_is_an_error_that_names_the_file() {
    let (installed, file, dir) = database_project("both", "[database]\nprovider = \"sqlite\"\n", "datasource db {\n provider = \"sqlite\"\n}\nmodel A {\n id Int @id\n}\n");
    let error = compile_project_file_with(&file, &installed).err().unwrap();
    assert!(error.contains("a datasource block is not allowed when monorm.toml has a [database] table"), "{error}");
    assert!(error.contains("schema.prisma:1:"), "{error}");
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn without_a_database_table_the_datasource_block_and_the_defaults_stay() {
    let (installed, file, dir) = database_project("none", "migrations = \"db\"\n", "datasource db {\n provider = \"sqlite\"\n}\nmodel A {\n id Int @id\n}\n");
    assert_eq!(compile_project_file_with(&file, &installed).unwrap().ir.dialect, crate::dialect::Dialect::Sqlite);
    std::fs::write(&file, "model A {\n id Int @id\n}\n").unwrap();
    assert_eq!(compile_project_file_with(&file, &installed).unwrap().ir.dialect, crate::dialect::Dialect::Postgres);
    std::fs::remove_dir_all(dir).unwrap();
}
