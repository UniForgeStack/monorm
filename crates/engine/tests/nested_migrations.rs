//! Migrations of nested types run against real databases: the rows a change moves or fills
//! (the SQL shape of each change is tested in `monorm-core`).
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};

use monorm_core::{dsl, migrate as ddl};
use monorm_engine::{db::{self, Driver}, migrate};
use serde_json::{json, Value};

fn runtime() -> tokio::runtime::Runtime { tokio::runtime::Runtime::new().unwrap() }

/// The PostgreSQL tests share one set of tables.
static POSTGRES: Mutex<()> = Mutex::new(());

const SQLITE: &str = "datasource db { provider = \"sqlite\" }\n";

struct Dir(PathBuf);

impl Dir {
    fn new(label: &str) -> Dir {
        static NEXT: Mutex<u32> = Mutex::new(0);
        let n = { let mut n = NEXT.lock().unwrap(); *n += 1; *n };
        let path = std::env::temp_dir().join(format!("monorm-nested-migrations-{}-{label}-{n}", std::process::id()));
        std::fs::create_dir_all(&path).unwrap();
        Dir(path)
    }
}

impl Drop for Dir {
    fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); }
}

struct Live {
    db: Arc<dyn Driver>,
    dir: Dir,
    sqlite: bool,
    _lock: Option<MutexGuard<'static, ()>>,
}

fn pg_url() -> String {
    std::env::var("ORM_RUST_TEST_POSTGRES_URL").or_else(|_| std::env::var("ORM_TEST_DATABASE_URL")).unwrap_or_else(|_| "postgres://postgres:postgres@localhost/orm_test".into())
}

async fn live(dialect: &str, label: &str) -> Live {
    let sqlite = dialect == "sqlite";
    let lock = (!sqlite).then(|| POSTGRES.lock().unwrap_or_else(|e| e.into_inner()));
    let db = db::connect(&if sqlite { "sqlite://:memory:".to_string() } else { pg_url() }, 2).await.unwrap();
    db.batch("DROP TABLE IF EXISTS shop; DROP TABLE IF EXISTS monorm_migrations".into()).await.unwrap();
    Live { db, dir: Dir::new(label), sqlite, _lock: lock }
}

impl Live {
    fn source(&self, source: &str) -> String { if self.sqlite { format!("{SQLITE}{source}") } else { source.to_string() } }

    /// Make the migration that leads to `source`, and apply it.
    async fn migrate(&self, source: &str) {
        let schema = dsl::check(dsl::compile(&self.source(source), None).unwrap()).unwrap().1;
        assert!(!ddl::files::make(&self.dir.0, &schema, None, false, false).unwrap().is_empty(), "no change to migrate");
        migrate::upgrade(&*self.db, &self.dir.0, None, true).await.unwrap();
    }

    async fn downgrade(&self) {
        migrate::downgrade(&*self.db, &self.dir.0, migrate::Down::Steps(1), false, true).await.unwrap();
    }

    async fn text(&self, sql: &str) -> Vec<Vec<Option<String>>> { self.db.query_text(sql.into()).await.unwrap() }

    /// `id -> the json document` of the `address` column.
    async fn addresses(&self) -> Vec<(i64, Value)> {
        let mut rows: Vec<_> = self.text("SELECT id, address FROM shop").await.into_iter()
            .map(|r| (r[0].as_ref().unwrap().parse().unwrap(), serde_json::from_str(r[1].as_ref().unwrap()).unwrap())).collect();
        rows.sort_by_key(|r| r.0);
        rows
    }

    async fn close(self) { self.db.close().await; }
}

const V1: &str = "type Geo {\n  lat Float\n  lng Float?\n}\n\ntype Address {\n  city String\n  geo   Geo\n}\n\nmodel Shop {\n  id      Int      @id\n  address Address?\n}\n";
const JSON_V1: &str = "type Geo {\n  lat Float\n}\n\ntype Address {\n  city String\n  geo  Geo\n}\n\nmodel Shop {\n  id      Int     @id\n  address Address @storage(json)\n}\n";

fn json_v2() -> String {
    JSON_V1.replace("  lat Float\n", "  lat Float\n  alt Float @default(0)\n").replace("  city String\n", "  city String\n  zip  String @default(\"0\")\n")
}

async fn insert_addresses(live: &Live, rows: &[(i64, Value)]) {
    let values: Vec<String> = rows.iter().map(|(i, v)| format!("({i}, '{v}')")).collect();
    live.db.batch(format!("INSERT INTO shop (id, address) VALUES {}", values.join(", "))).await.unwrap();
}

const DIALECTS: [&str; 2] = ["postgresql", "sqlite"];

#[test]
fn applying_the_backfill_keeps_present_keys_and_fills_missing_ones() {
    runtime().block_on(async {
        for dialect in DIALECTS {
            let live = live(dialect, "backfill").await;
            live.migrate(JSON_V1).await;
            insert_addresses(&live, &[
                (1, json!({"city": "A", "geo": {"lat": 1}})),
                (2, json!({"city": "B", "zip": "9", "geo": {"lat": 2, "alt": 5}})),
                (3, json!({"city": "C"})),
            ]).await;
            live.migrate(&json_v2()).await;
            assert_eq!(live.addresses().await, [
                (1, json!({"city": "A", "zip": "0", "geo": {"lat": 1, "alt": 0}})),
                (2, json!({"city": "B", "zip": "9", "geo": {"lat": 2, "alt": 5}})),
                (3, json!({"city": "C", "zip": "0"})), // no geo object: nothing to put alt in
            ], "{dialect}");
            live.close().await;
        }
    });
}

#[test]
fn applying_the_backfill_fills_a_key_that_holds_a_json_null() {
    runtime().block_on(async {
        let nullable = JSON_V1.replace("  city String\n", "  city String\n  zip  String?\n");
        let required = JSON_V1.replace("  city String\n", "  city String\n  zip  String @default(\"z\")\n");
        for dialect in DIALECTS {
            let live = live(dialect, "json-null").await;
            live.migrate(&nullable).await;
            let geo = json!({"lat": 1});
            insert_addresses(&live, &[
                (1, json!({"city": "A", "geo": geo})),
                (2, json!({"city": "B", "zip": null, "geo": geo})),
                (3, json!({"city": "C", "zip": "9", "geo": geo})),
            ]).await;
            live.migrate(&required).await;
            let zips: Vec<Value> = live.addresses().await.into_iter().map(|(_, a)| a["zip"].clone()).collect();
            assert_eq!(zips, [json!("z"), json!("z"), json!("9")], "{dialect}");
            live.close().await;
        }
    });
}

#[test]
fn applying_the_backfill_skips_a_null_or_missing_middle_level() {
    runtime().block_on(async {
        let optional = JSON_V1.replace("  geo  Geo\n", "  geo  Geo?\n");
        let deeper = optional.replace("  lat Float\n", "  lat Float\n  alt Float @default(0)\n");
        for dialect in DIALECTS {
            let live = live(dialect, "middle").await;
            live.migrate(&optional).await;
            insert_addresses(&live, &[
                (1, json!({"city": "A", "geo": {"lat": 1}})),
                (2, json!({"city": "B", "geo": null})),
                (3, json!({"city": "C"})),
            ]).await;
            live.migrate(&deeper).await;
            assert_eq!(live.addresses().await, [
                (1, json!({"city": "A", "geo": {"lat": 1, "alt": 0}})),
                (2, json!({"city": "B", "geo": null})),
                (3, json!({"city": "C"})),
            ], "{dialect}");
            live.close().await;
        }
    });
}

#[test]
fn a_downgrade_refills_a_required_leaf_it_re_adds() {
    runtime().block_on(async {
        let source = "type Address {\n  city String\n  zip  String @default(\"z\")\n}\n\nmodel Shop {\n  id      Int      @id\n  address Address?\n}\n";
        let removed = source.replace("  zip  String @default(\"z\")\n", "");
        for dialect in DIALECTS {
            let live = live(dialect, "refill").await;
            live.migrate(source).await;
            live.db.batch("INSERT INTO shop (id, address_city, address_zip) VALUES (1, 'A', 'q'), (2, NULL, NULL)".into()).await.unwrap();
            live.migrate(&removed).await;
            live.downgrade().await;
            let rows = live.text("SELECT id, address_city, address_zip FROM shop ORDER BY id").await;
            let rows: Vec<_> = rows.iter().map(|r| (r[0].clone().unwrap(), r[1].clone(), r[2].clone())).collect();
            assert_eq!(rows, [("1".into(), Some("A".into()), Some("z".into())), ("2".into(), None, None)], "{dialect}");
            live.close().await;
        }
    });
}

#[test]
fn applying_a_renamed_subfield_keeps_the_data() {
    runtime().block_on(async {
        let renamed = V1.replace("  lng Float?", "  longitude Float? @renamed_from(\"lng\")");
        for dialect in DIALECTS {
            let live = live(dialect, "renamed").await;
            live.migrate(V1).await;
            live.db.batch("INSERT INTO shop (id, address_city, address_geo_lat, address_geo_lng) VALUES (1, 'A', 1.5, 2.5), (2, NULL, NULL, NULL)".into()).await.unwrap();
            live.migrate(&renamed).await;
            let rows = live.text("SELECT id, address_city, address_geo_longitude FROM shop ORDER BY id").await;
            let rows: Vec<_> = rows.iter().map(|r| (r[0].clone().unwrap(), r[1].clone(), r[2].clone())).collect();
            assert_eq!(rows, [("1".into(), Some("A".into()), Some("2.5".into())), ("2".into(), None, None)], "{dialect}");
            live.close().await;
        }
    });
}

const UID: &str = "6f1f4e0a-3c2b-4f6e-9d57-1a2b3c4d5e6f";

fn storage(dialect: &str, where_: &str) -> String {
    let dec = if dialect == "sqlite" { "" } else { "  dec   Decimal\n" }; // SQLite has no exact decimal
    let storage = if where_ == "json" { "@storage(json)" } else { "" };
    format!("type Geo {{\n  lat Float\n  lng Float?\n}}\n\ntype Everything {{\n  s     String\n  i     Int\n  big   BigInt\n  f     Float\n  b     Boolean\n{dec}  at    DateTime\n  day   DateTime @db.Date\n  uid   String @db.Uuid\n  extra Json?\n  geo   Geo?\n}}\n\nmodel Shop {{\n  id   Int         @id\n  data Everything? {storage}\n}}\n")
}

fn leaves(dialect: &str) -> Vec<&'static str> {
    ["s", "i", "big", "f", "b", "dec", "at", "day", "uid", "extra", "geo_lat", "geo_lng"].into_iter().filter(|l| dialect != "sqlite" || *l != "dec").collect()
}

/// The JSON documents the leaf rows of `insert_columns` read back as.
fn expected(dialect: &str) -> Vec<(i64, Value)> {
    let mut one = json!({
        "s": "x", "i": 7, "big": "9007199254740993", "f": 1.5, "b": true, "dec": "1234.56",
        "at": "2026-01-02T03:04:05.123456Z", "day": "2026-01-03", "uid": UID,
        "extra": {"k": [1, 2]}, "geo": {"lat": 1.5, "lng": 2.5},
    });
    let mut two = json!({
        "s": "y", "i": -1, "big": "-9007199254740993", "f": 0.25, "b": false, "dec": "0.10",
        "at": "2026-12-31T23:59:59.000000Z", "day": "2026-12-31", "uid": UID,
        "extra": null, "geo": null,
    });
    if dialect == "sqlite" {
        one.as_object_mut().unwrap().remove("dec");
        two.as_object_mut().unwrap().remove("dec");
    }
    vec![(1, one), (2, two)]
}

async fn insert_columns(live: &Live, dialect: &str) {
    let moment = |text: &str| if dialect == "sqlite" { text.to_string() } else { format!("{text}+00") };
    let (dec, dec2) = if dialect == "sqlite" { ("", "") } else { ("1234.56, ", "0.10, ") };
    let one = format!("'x', 7, 9007199254740993, 1.5, TRUE, {dec}'{}', '2026-01-03', '{UID}', '{{\"k\":[1,2]}}', 1.5, 2.5", moment("2026-01-02 03:04:05.123456"));
    let two = format!("'y', -1, -9007199254740993, 0.25, FALSE, {dec2}'{}', '2026-12-31', '{UID}', NULL, NULL, NULL", moment("2026-12-31 23:59:59"));
    let three = vec!["NULL"; leaves(dialect).len()].join(", ");
    let cols: Vec<String> = leaves(dialect).iter().map(|l| format!("data_{l}")).collect();
    live.db.batch(format!("INSERT INTO shop (id, {}) VALUES (1, {one}), (2, {two}), (3, {three})", cols.join(", "))).await.unwrap();
}

/// The leaf columns as text, one row per id.
async fn column_rows(live: &Live, dialect: &str) -> Vec<Vec<Option<String>>> {
    let cols: Vec<String> = leaves(dialect).iter().map(|l| format!("data_{l}")).collect();
    live.text(&format!("SELECT id, {} FROM shop ORDER BY id", cols.join(", "))).await
}

async fn json_rows(live: &Live) -> Vec<(i64, Value)> {
    let rows = live.text("SELECT id, data FROM shop ORDER BY id").await;
    rows.into_iter().map(|r| (r[0].as_ref().unwrap().parse().unwrap(), r[1].as_deref().map_or(Value::Null, |s| serde_json::from_str(s).unwrap()))).collect()
}

/// A DateTime reads the same with or without a zero fraction.
fn plain(doc: Value) -> Value {
    match doc {
        Value::String(s) if s.ends_with(".000000Z") || s.ends_with(".000000") => Value::String(s.replace(".000000", "")),
        Value::Object(m) => Value::Object(m.into_iter().map(|(k, v)| (k, plain(v))).collect()),
        other => other,
    }
}

#[test]
fn a_columns_to_json_change_copies_every_leaf_type_and_down_reverses_it() {
    runtime().block_on(async {
        for dialect in DIALECTS {
            let live = live(dialect, "to-json").await;
            live.migrate(&storage(dialect, "columns")).await;
            insert_columns(&live, dialect).await;
            let before = column_rows(&live, dialect).await;
            live.migrate(&storage(dialect, "json")).await;
            let mut want = expected(dialect);
            want.push((3, Value::Null));
            let got: Vec<_> = json_rows(&live).await.into_iter().map(|(i, d)| (i, plain(d))).collect();
            assert_eq!(got, want.into_iter().map(|(i, d)| (i, plain(d))).collect::<Vec<_>>(), "{dialect}");
            live.downgrade().await;
            assert_eq!(column_rows(&live, dialect).await, before, "{dialect}");
            live.close().await;
        }
    });
}

#[test]
fn a_json_to_columns_change_copies_every_leaf_type_and_down_reverses_it() {
    runtime().block_on(async {
        for dialect in DIALECTS {
            let live = live(dialect, "to-columns").await;
            live.migrate(&storage(dialect, "json")).await;
            let mut docs = expected(dialect);
            docs.push((3, Value::Null));
            let values: Vec<String> = docs.iter().map(|(i, d)| if d.is_null() { format!("({i}, NULL)") } else { format!("({i}, '{d}')") }).collect();
            live.db.batch(format!("INSERT INTO shop (id, data) VALUES {}", values.join(", "))).await.unwrap();
            live.migrate(&storage(dialect, "columns")).await;
            let rows = column_rows(&live, dialect).await;
            let names = leaves(dialect);
            for (row, (_, doc)) in rows.iter().zip(&docs) {
                let get = |leaf: &str| -> Option<String> {
                    let v = if doc.is_null() { &Value::Null } else if let Some(sub) = leaf.strip_prefix("geo_") {
                        if doc["geo"].is_null() { &Value::Null } else { &doc["geo"][sub] }
                    } else { &doc[leaf] };
                    match v {
                        Value::Null => None,
                        Value::String(s) => Some(s.clone()),
                        other => Some(other.to_string()),
                    }
                };
                for (cell, leaf) in row[1..].iter().zip(&names) {
                    let want = get(leaf);
                    match *leaf {
                        // a column prints its own form of these
                        "b" | "at" | "extra" => assert_eq!(cell.is_some(), want.is_some(), "{dialect} {leaf}: {cell:?}"),
                        _ => assert_eq!(cell, &want, "{dialect} {leaf}"),
                    }
                }
            }
            live.downgrade().await;
            let back: Vec<_> = json_rows(&live).await.into_iter().map(|(i, d)| (i, plain(d))).collect();
            assert_eq!(back, docs.into_iter().map(|(i, d)| (i, plain(d))).collect::<Vec<_>>(), "{dialect}");
            live.close().await;
        }
    });
}
