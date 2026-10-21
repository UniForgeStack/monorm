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
        ("model Item {\n  id BigInt @id\n  @@trigger(t, after: [insert], for_each: statement, body: \"SELECT 1\")\n}", "StatementTriggers"),
        ("model Item {\n  id BigInt @id\n  @@trigger(t, after: [insert, update], body: \"SELECT 1\")\n}", "one insert/update/delete event"),
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
  @@trigger(audit, after: [insert, delete], for_each: statement, function: audit_row, args: ["rooms"])
  @@trigger(touch, before: [update], update_of: [title], when: "OLD.label <> NEW.label", body: "BEGIN RETURN NEW; END;")
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
    let dir = std::env::temp_dir().join(format!("orm-dsl-test-{}", std::process::id()));
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
        ("model A {\n  id BigInt @id @prim\n}".into(), "unknown attribute @prim"),
        ("model A {\n  x Int\n}".into(), "model A has no @id field"),
        (format!("model A {{\n  id BigInt @id\n  b B @relation(fields: [b_id], references: [id])\n}}\n{b}"), "no field b_id"),
        (
            format!("model A {{\n  id BigInt @id\n  b_id BigInt?\n  b B @relation(fields: [b_id], references: [id])\n}}\n{b}"),
            "so the relation type is `B?`",
        ),
        (format!("model A {{\n  id BigInt @id\n  b_id BigInt\n  b B @relation(fields: [b_id])\n}}\n{b}"), "needs references:"),
        (format!("model A {{\n  id BigInt @id\n  bs B[]\n}}\n{b}"), "B has no relation to A"),
        (
            format!("model A {{\n  id BigInt @id\n  b_id BigInt\n  b B @relation(fields: [b_id], references: [id], onDelete: cascade)\n}}\n{b}"),
            "unknown referential action cascade",
        ),
        ("model A {\n  id BigInt @id\n  @@index([nope])\n}".into(), "nope"),
        ("model A {\n  id BigInt @id\n  @@index([id], wher: \"x\")\n}".into(), "unknown argument `wher`"),
        ("model A {\n  id BigInt @id\n  @@index([id(sort: desc)])\n}".into(), "sort must be Asc or Desc"),
        ("model A {\n  id BigInt @id\n  @@trigger(t, body: \"x\")\n}".into(), "say when it fires"),
        ("model A {\n  id BigInt @id\n  e Unsupported(\"vector\")\n}".into(), "needs argument \"dims\""),
        ("model A {\n  id BigInt @id\n  e Unsupported(\"nope(1)\")\n}".into(), "unknown type nope"),
        ("model A {\n  id BigInt @id\n  e String @db.Nope\n}".into(), "unknown native type @db.Nope"),
        ("model A {\n  id BigInt @id @default(foo())\n}".into(), "not foo"),
        ("enum E {\n  a\n  b\n}\nmodel A {\n  id BigInt @id\n  e E(a)\n}".into(), "A.e: an enum subset E(...) is only allowed on a field of a proxy model"),
        ("model A {\n  id String @id @default(cuid())\n}".into(), "A.id: cuid() is not supported; use @client_default(uuid())"),
        ("model A {\n  id String @id @default(uuid(5))\n}".into(), "uuid() takes no argument, 4 or 7"),
        ("model A {\n  id BigInt @id @client_default(autoincrement())\n}".into(), "A.id: only the database can make autoincrement(); use @default"),
        ("model A {\n  id String @id @client_default(dbgenerated(\"x\"))\n}".into(), "only the database can make dbgenerated()"),
        ("model A {\n  id String @id @client_default(cuid())\n}".into(), "@client_default takes a literal, uuid(), uuid7() or now(), not cuid()"),
        ("model A {\n  id String @id @client_default(uuid(7))\n}".into(), "uuid() takes no arguments"),
        ("model A {\n  id BigInt @id @client_default(uuid())\n}".into(), "A.id: @client_default uuid does not fit a bigint field"),
        ("model A {\n  id BigInt @id\n  at String @client_default(now())\n}".into(), "A.at: @client_default now does not fit"),
        ("model A {\n  id BigInt @id\n  n Int @client_default(\"x\")\n}".into(), "A.n: @client_default requires int32"),
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
        ("view V {\n  a Int\n}".into(), "`view` blocks aren't supported yet"),
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
        ("model A {\n  id BigInt @id\n  @@trigger(t, after: [insert],\n    function: f)\n}".into(), "<schema>:3:3: model A: an attribute must be on one line"),
        (
            "model A {\n  id BigInt @id\n  @@trigger(t, before: [update], body: \"\"\"BEGIN RETURN NEW; END;\"\"\")\n}".into(),
            "only go in top-level blocks",
        ),
    ];
    for (src, expected) in cases {
        let e = fails(&src);
        assert!(e.contains(expected), "{src:?}\n  got: {e}\n  expected: {expected}");
    }
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
    let dir = std::env::temp_dir().join(format!("orm-imports-{}", std::process::id()));
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
    assert!(compile_file(&root).unwrap_err().contains("imported more than once"));
    std::fs::write(&root, "import \"missing.prisma\"\n").unwrap();
    assert!(compile_file(&root).unwrap_err().contains("import \"missing.prisma\""));
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn imported_models_use_main_extension_definitions() {
    let dir = std::env::temp_dir().join(format!("orm-import-types-{}", std::process::id()));
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
        "id": "test", "version": "1", "host_contract": 1, "schema_contract": 2, "languages": [], "databases": [],
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
fn enum_subsets_lower_only_on_proxy_fields() {
    let lowered = |field: &str| compile(&format!("enum E {{\n  a\n  b\n}}\nmodel M {{\n  id Int @id\n  e E\n}}\nmodel V {{\n  {field}\n  @@proxy.of(M)\n}}"), None);
    for (field, message) in [("e E(c)", "V.e: E has no value c"), ("e E(a, a)", "V.e: a is listed twice"), ("e E(\"a\")", "V.e: an enum subset lists member names")] {
        let error = lowered(field).unwrap_err();
        assert!(error.contains(message), "{error}");
    }
    let mut ir = ok("enum E {\n  a\n  b\n}\nmodel M {\n  id Int @id\n  e E\n}");
    ir.models[0].fields[1].enum_subset = Some(vec!["a".into()]);
    assert!(check(ir).err().unwrap().contains("M.e: an enum subset is only allowed on a field of a proxy model"));
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

#[test]
fn the_generic_pass_owns_generic_and_reverse_fields() {
    let manifest: crate::behavior::Manifest = serde_json::from_value(serde_json::json!({
        "id": "orm.generic", "version": "1", "host_contract": 1, "schema_contract": 2, "languages": [], "databases": [],
        "attributes": [{"name": "generic.relation", "target": "field", "arguments": {"targets": {"kind": "list", "required": true}}},
                       {"name": "generic.reverse", "target": "field"}]
    })).unwrap();
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
        ("@generic.relation(targets: [\"Post\"])", "@generic.relation(targets: [\"Post\"]) @unique", "Tag.target: @unique is not allowed on a Generic field; use the explicit @@generic.relation form"),
        ("@generic.relation(targets: [\"Post\"])", "@map(\"zz\") @generic.relation(targets: [\"Post\"])", "Tag.target: @map is not allowed on a Generic field; use the explicit @@generic.relation form"),
        ("@generic.reverse", "@generic.reverse @map(\"zz\")", "Post.tags: @map is not allowed on a @generic.reverse field; remove it"),
        ("@generic.reverse", "@generic.reverse @relation(\"x\")", "Post.tags: @relation is not allowed on a @generic.reverse field; remove it"),
    ] {
        let mut items = syntax::parse(&source.replace(from, to)).unwrap();
        let error = super::lower::behavior_declarations(&mut items, "t.prisma", std::slice::from_ref(&manifest)).unwrap_err().msg;
        assert!(error.contains(message), "{error}");
    }
    // A Generic field without @generic.relation is not the pass's member.
    let mut items = syntax::parse("model Tag {\n id Int @id\n x Generic\n}").unwrap();
    super::lower::behavior_declarations(&mut items, "t.prisma", std::slice::from_ref(&manifest)).unwrap();
    assert_eq!(members(&items, 0)[1].1, "Generic");
    // Without a compiled generic.relation, Generic stays an unknown type.
    let mut items = syntax::parse(source).unwrap();
    super::lower::behavior_declarations(&mut items, "t.prisma", &[]).unwrap();
    assert_eq!(members(&items, 0)[1].1, "Generic");
    let error = compile("model Tag {\n id Int @id\n target Generic?\n}", None).unwrap_err();
    assert!(error.contains("unknown type Generic; a Generic field needs @generic.relation"), "{error}");
}

#[test]
fn protected_write_is_a_model_flag_without_ddl() {
    let source = "model Post {\n id Int @id\n @@protected_write\n}";
    let ir = compile(source, None).unwrap();
    assert!(ir.models[0].protected_write);
    let snapshot = |source: &str| crate::migrate::snapshot(&check(compile(source, None).unwrap()).unwrap().1).unwrap();
    assert_eq!(snapshot(source), snapshot(&source.replace(" @@protected_write\n", "")));
    let error = compile(&source.replace("@@protected_write", "@@protected_write(true)"), None).unwrap_err();
    assert!(error.contains("@@protected_write takes no arguments"), "{error}");
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
