#![cfg(feature = "postgres")]
use orm_core::{dsl, migrate};
use orm_engine::{db, introspect};

const SOURCE: &str = r#"
enum Kind {
 A @map("a")
 B @map("b")
 C @map("c")
}
model Reward {
 id Int @default(autoincrement())
 kind Kind
 amount Int?
 @@id([id, kind])
 @@partition(list: [kind])
 @@index([amount])
 @@check("id > 0")
 @@trigger(parent_touch, before: [update], body: "BEGIN RETURN NEW; END;")
}
partition reward_a {
of = Reward
values = [A]
@@check("amount IS NOT NULL AND amount > 0", name: "positive_amount")
@@unique([amount])
@@index([id], name: "local_id")
@@comment("positive rewards")
@@trigger(local_touch, before: [insert], body: "BEGIN RETURN NEW; END;")
}
partition reward_other {
of = Reward
default = true
}
"#;
fn schema(source: &str) -> orm_core::schema::Schema {
    dsl::check(dsl::compile(source, None).unwrap()).unwrap().1
}

#[test]
fn postgres_partition_routing_constraints_migrations_pull_and_shadow() {
    let Ok(url) = std::env::var("ORM_RUST_TEST_POSTGRES_URL") else { return };
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let conn = db::connect(&url, 1).await.unwrap();
        conn.batch("DROP SCHEMA IF EXISTS partition_acceptance CASCADE; CREATE SCHEMA partition_acceptance; SET search_path TO partition_acceptance, public".into()).await.unwrap();
        let s = schema(SOURCE);
        for sql in migrate::create_statements(&migrate::snapshot(&s).unwrap()) { conn.batch(sql).await.unwrap(); }
        conn.batch("INSERT INTO reward (kind, amount) VALUES ('a', 5), ('b', NULL), ('b', 7)".into()).await.unwrap();
        let rows = conn.query_text("SELECT tableoid::regclass::text FROM reward ORDER BY id".into()).await.unwrap();
        assert_eq!(rows.iter().map(|r| r[0].as_deref().unwrap()).collect::<Vec<_>>(), vec!["reward_a", "reward_other", "reward_other"]);
        assert!(conn.batch("INSERT INTO reward (kind, amount) VALUES ('a', NULL)".into()).await.is_err());
        assert!(conn.batch("INSERT INTO reward (kind, amount) VALUES ('a', -1)".into()).await.is_err());
        assert!(conn.batch("UPDATE reward SET kind = 'a' WHERE amount IS NULL".into()).await.is_err());
        conn.batch("UPDATE reward SET kind = 'a' WHERE amount = 7".into()).await.unwrap();
        assert_eq!(conn.query_text("SELECT tableoid::regclass::text FROM reward WHERE amount = 7".into()).await.unwrap()[0][0].as_deref(), Some("reward_a"));
        let snapshot = migrate::snapshot(&s).unwrap();
        let live = introspect::introspect(&*conn).await.unwrap();
        assert!(live.gaps.is_empty(), "{:?}", live.gaps);
        let shadow = introspect::materialize(&*conn, &snapshot).await.unwrap();
        let drift = introspect::differences(&live.schema, &shadow.schema);
        assert!(drift.is_empty(), "{drift:?}");
        let pulled = migrate::pull::pull(&live.schema, &live.gaps);
        assert!(pulled.gaps.is_empty(), "{:?}", pulled.gaps);
        let pulled_snapshot = migrate::snapshot(&schema(&pulled.schema)).unwrap();
        let roundtrip = introspect::materialize(&*conn, &pulled_snapshot).await.unwrap();
        let drift = introspect::differences(&live.schema, &roundtrip.schema);
        assert!(drift.is_empty(), "{drift:?}\n{}", pulled.schema);

        // Rebound preserves the table and its local objects. Existing rows must satisfy the new bound.
        conn.batch("DELETE FROM reward WHERE kind = 'a'".into()).await.unwrap();
        let rebound = schema(&SOURCE.replace("values = [A]", "values = [C]"));
        let plan = migrate::plan(&rebound, &snapshot).unwrap();
        for step in &plan.up { conn.batch(step.sql.clone()).await.unwrap(); }
        conn.batch("INSERT INTO reward (kind, amount) VALUES ('c', 11)".into()).await.unwrap();
        assert_eq!(conn.query_text("SELECT tableoid::regclass::text FROM reward WHERE amount = 11".into()).await.unwrap()[0][0].as_deref(), Some("reward_a"));
        // Remove and restore a partition with real DDL.
        let removed_source = SOURCE.split("partition reward_a").next().unwrap().to_owned() + "partition reward_other { of = Reward default = true }";
        let removed = schema(&removed_source);
        let remove = migrate::plan(&removed, &plan.snapshot).unwrap();
        assert!(remove.up.iter().any(|s| s.warning.is_some()));
        for step in &remove.up { conn.batch(step.sql.clone()).await.unwrap(); }
        let add = migrate::plan(&s, &remove.snapshot).unwrap();
        for step in &add.up { conn.batch(step.sql.clone()).await.unwrap(); }

        // Range without proxies or local constraints; timestamp bounds survive deparsing.
        let range = schema(r#"model Event {
 id Int @default(autoincrement())
 created DateTime
 @@id([id, created])
 @@partition(range: [created])
}
partition event_q3 { of = Event from = ["2026-07-01"] to = ["2026-10-01"] }
partition event_rest { of = Event default = true }
"#);
        let range_snapshot = migrate::snapshot(&range).unwrap();
        for sql in migrate::create_statements(&range_snapshot) { conn.batch(sql).await.unwrap(); }
        conn.batch("INSERT INTO event (created) VALUES ('2026-07-02'), ('2027-01-01')".into()).await.unwrap();
        assert_eq!(conn.query_text("SELECT tableoid::regclass::text FROM event ORDER BY id".into()).await.unwrap()[0][0].as_deref(), Some("event_q3"));
        let live = introspect::introspect(&*conn).await.unwrap();
        let pulled = migrate::pull::pull(&live.schema, &live.gaps);
        let roundtrip = introspect::materialize(&*conn, &migrate::snapshot(&schema(&pulled.schema)).unwrap()).await.unwrap();
        assert!(introspect::differences(&live.schema, &roundtrip.schema).is_empty());
        // Hash subpartitions and expression partition keys (without a parent unique key).
        let hash = schema("model H {\n id Int @id\n @@partition(range: [id])\n}\npartition h_r { of = H from = [MINVALUE] to = [MAXVALUE] @@partition(hash: [id]) }\npartition h_0 { of = h_r modulus = 1 remainder = 0 }");
        let snapshot = migrate::snapshot(&hash).unwrap();
        let shadow = introspect::materialize(&*conn, &snapshot).await.unwrap();
        assert!(shadow.gaps.is_empty(), "{:?}", shadow.gaps);
        let pulled = migrate::pull::pull(&shadow.schema, &[]);
        let roundtrip = introspect::materialize(&*conn, &migrate::snapshot(&schema(&pulled.schema)).unwrap()).await.unwrap();
        assert!(introspect::differences(&shadow.schema, &roundtrip.schema).is_empty());
        conn.batch("SET search_path TO public; DROP SCHEMA partition_acceptance CASCADE".into()).await.unwrap();
        conn.close().await;
    });
}

#[cfg(feature = "proxy-models")]
#[test]
fn proxy_partition_plans_keep_storage_predicate_and_insert_into_owned_table() {
    use orm_core::{dialect::{Dialect, Target}, ir::Operation};
    use orm_engine::{params::NoParams, plan::{self, Plan}};
    use serde_json::json;
    let source = r#"
model Reward {
 id Int @default(autoincrement())
 kind String
 shop Int
 percent Int?
 @@id([id, kind])
 @@partition(list: [kind])
}
model PercentageReward {
 @@proxy.of(Reward)
 @@partition.values(["percent"])
 @@map("partition_proxy_percent")
}
model Child {
 @@proxy.of(PercentageReward)
}
"#;
    let s = schema(source);
    let target = Target::new(Dialect::Postgres);
    for model in ["PercentageReward", "Child"] {
        for op in ["select", "count", "delete"] {
            let q: Operation = serde_json::from_value(json!({"op":op,"model":model,"without_defaults":true})).unwrap();
            let sql = match plan::Planner::plan(&s, target, &q, &NoParams).unwrap() {
                Plan::Select(p) => p.stmt.to_string(sea_query::PostgresQueryBuilder),
                Plan::Count(p) => p.to_string(sea_query::PostgresQueryBuilder),
                Plan::Delete(p, _) => p.to_string(sea_query::PostgresQueryBuilder),
                _ => panic!("unexpected plan"),
            };
            assert!(sql.contains(" IN ('percent')"), "{sql}");
        }
        let (insert, _) = plan::plan_insert(&s, target, model, &["shop".into(), "percent".into()],
            vec![vec![Some(1.into()), Some(10.into())]], None, &NoParams, true, &Default::default()).unwrap();
        let sql = insert.to_string(sea_query::PostgresQueryBuilder);
        assert!(sql.starts_with("INSERT INTO \"partition_proxy_percent\""), "{sql}");
        assert!(sql.contains("'percent'"), "{sql}");
    }
}

#[cfg(feature = "proxy-models")]
#[test]
fn proxy_partition_upsert_reads_the_existing_row_through_the_partition_table() {
    use orm_core::dialect::{Dialect, Target};
    use orm_engine::{db, params::NoParams, plan::{self, OnConflict}};
    let source = "model Reward {\n id Int @default(autoincrement())\n kind String\n shop Int\n @@id([id, kind])\n @@partition(list: [kind])\n}\nmodel PercentageReward {\n @@proxy.of(Reward)\n @@partition.values([\"percent\"])\n @@map(\"partition_proxy_percent\")\n}\n";
    let (mut ir, _) = dsl::check(dsl::compile(source, None).unwrap()).unwrap();
    ir.behavior.read_defaults.push(serde_json::from_value(serde_json::json!({
        "source":"orm.query-defaults","name":"shop","model":"PercentageReward",
        "filter":{"t":"cmp","op":"gt","l":{"t":"col","path":[],"name":"shop"},"r":{"t":"int","value":0}}
    })).unwrap());
    let s = orm_core::schema::Schema::from_ir(ir).unwrap();
    let target = Target::new(Dialect::Postgres);
    let (stmt, _) = plan::plan_insert(&s, target, "PercentageReward", &["id".into(), "shop".into()],
        vec![vec![Some(1.into()), Some(10.into())]],
        Some(OnConflict::Update(vec!["id".into(), "kind".into()], None, vec!["shop".into()], vec![])), &NoParams, false, &Default::default()).unwrap();
    let sql = db::build(target.dialect, &stmt).0;
    let (_, action) = sql.split_once(" DO UPDATE SET ").unwrap();
    assert!(action.contains("WHERE \"partition_proxy_percent\".\"shop\" > "), "{sql}");
}

#[cfg(feature = "proxy-models")]
#[test]
fn proxy_storage_predicates_skip_virtual_cte_models() {
    use orm_core::{dialect::{Dialect, Target}, ir::Operation};
    use orm_engine::{params::NoParams, plan::Planner};
    use serde_json::json;
    let s = schema("model Item {\n id Int @id\n}");
    let q: Operation = serde_json::from_value(json!({
        "op":"select","model":"Item",
        "with":[{"name":"ids","query":{"model":"Item","columns":[{"t":"expr","expr":{"t":"col","path":[],"name":"id"},"name":"id"}]}}],
        "filters":[{"t":"in_select","item":{"t":"col","path":[],"name":"id"},"select":{"model":"ids","columns":[{"t":"expr","expr":{"t":"cte_col","cte":"ids","name":"id"},"name":"id"}]}}]
    })).unwrap();
    Planner::plan(&s, Target::new(Dialect::Postgres), &q, &NoParams).unwrap();
}
