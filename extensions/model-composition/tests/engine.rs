//! Shared-primary-key writes of the composition package on SQLite, and on PostgreSQL when `ORM_TEST_DATABASE_URL` is set.
use monorm::ir::SchemaIr;
use std::sync::Arc;
use monorm_core::{
    dialect::{Dialect, Target},
    migrate,
    schema::Schema,
};
use monorm_engine::{
    multi_table,
    db::{self, Cell},
    exec::{self, Outcome},
    NoParams,
};

/// Person <- Employee <- Manager, and Person <- Customer, written the way the DSL lowers `@@composition.model`.
fn composed(dialect: Dialect) -> SchemaIr {
    let declare = |model: &str, parent: &str, parent_ref: &str, child_ref: &str| serde_json::json!({
        "attribute": "composition.model", "model": model, "field": null, "positional": [],
        "arguments": {"parent": parent, "parentRef": parent_ref, "childRef": child_ref},
        "location": {"file": "composition.prisma", "line": 1, "column": 1}});
    let mut ir: SchemaIr = serde_json::from_value(serde_json::json!({
        "models": [
            {"name": "Person", "table": "person", "fields": [
                {"name": "id", "column": "id", "type": "int", "primary_key": true, "auto_increment": true},
                {"name": "name", "column": "name", "type": "string", "check": "name <> 'BADROOT'"}]},
            {"name": "Employee", "table": "employee", "fields": [
                {"name": "salary", "column": "salary", "type": "int", "check": "salary > 0"}]},
            {"name": "Manager", "table": "manager", "fields": [
                {"name": "level", "column": "level", "type": "int", "check": "level > 0"}]},
            {"name": "Customer", "table": "customer", "fields": [
                {"name": "points", "column": "points", "type": "int"}]}],
        "dialect": dialect,
        "behavior": {"declarations": [
            declare("Employee", "Person", "person", "employee"),
            declare("Manager", "Employee", "employee", "manager"),
            declare("Customer", "Person", "person", "customer")]}})).unwrap();
    monorm_composition::lower(&mut ir).unwrap();
    // The loader marks the declarations of a package pass lowered; the host then needs no manifest for them.
    for declaration in &mut ir.behavior.declarations { declaration.lowered = true; }
    ir.behavior.schema_contract = monorm::extension::SCHEMA_CONTRACT;
    ir.behavior.packages.insert("monorm.composition".into(), monorm::extension::PackageStamp { version: "0.1.0".into(), attributes: vec!["composition.model".into()], languages: vec!["python".into(), "typescript".into()], databases: vec!["postgres".into(), "sqlite".into()], ..Default::default() });
    ir
}
async fn check(url: &str) {
    let dialect = if url.starts_with("sqlite:") {
        Dialect::Sqlite
    } else {
        Dialect::Postgres
    };
    let ir = composed(dialect);
    let encoded=serde_json::to_string(&ir).unwrap();
    let schema = Schema::from_ir(ir).unwrap();
    // child, own fields and whether the root key fills itself, derived from the owner links and field storage
    let derived: Vec<_> = schema.models.iter().map(|m| (m.ir.name.as_str(), schema.attach_fields(m).map(|p| p.into_iter().map(|p| m.fields()[p].name.as_str()).collect::<Vec<_>>()), schema.key_fills_itself(m))).collect();
    assert_eq!(derived, [
        ("Person", None, false),
        ("Employee", Some(vec!["salary"]), true),
        ("Manager", Some(vec!["level"]), true),
        ("Customer", Some(vec!["points"]), true),
    ]);
    assert!(schema.models.iter().all(|m| m.fields().iter().all(|f| !f.hints.keys().any(|k| k.starts_with("composition.")))));
    let ir=serde_json::from_str(&encoded).unwrap();
    let py=monorm_core::codegen::python::generate(&ir,&schema,"composition.prisma").unwrap();
    let ts=monorm_core::codegen::typescript::generate(&ir,&schema,"composition.prisma","@ufstack/monorm").unwrap();
    assert!(py.stub.contains("class ManagerAttach(TypedDict):\n    level: int"));
    assert!(py.stub.contains("async def attach(self, parent_id: int, values: ManagerAttach) -> Manager"));
    assert!(ts.contains("export interface ManagerAttach {\n  level: In<number>;\n}"));
    assert!(ts.contains("readonly attach: ManagerAttach;"));
    let target = Target::new(dialect);
    let db = db::connect(url, 1).await.unwrap();
    for statement in migrate::create_all(&schema).unwrap() {
        db.batch(statement).await.unwrap();
    }
    let plan = exec::plan_insert(
        &schema,
        target,
        "Manager",
        &["name".into(), "salary".into(), "level".into()],
        vec![vec![
            Some("Alice".into()),
            Some(20i32.into()),
            Some(3i32.into()),
        ]],
        None,
        &NoParams,
        &Default::default(),
    )
    .unwrap();
    let Outcome::Rows { rows, types, .. } = exec::run(db.as_ref(), target, plan).await.unwrap()
    else {
        panic!()
    };
    assert_eq!(rows.len(), 1);
    assert_eq!(rows.cell(0, 0, types[0]).unwrap(), Cell::Int(1));
    assert_eq!(rows.cell(0, 3, types[3]).unwrap(), Cell::Text("Alice"));
    let update = monorm_engine::parse_op(r#"{"op":"update","model":"Manager","filters":[{"t":"cmp","op":"eq","l":{"t":"col","path":[],"name":"salary"},"r":{"t":"int","value":20}}],"set":[{"field":"name","value":{"t":"func","name":"upper","args":[{"t":"col","path":[],"name":"name"}]}},{"field":"salary","value":{"t":"arith","op":"add","l":{"t":"col","path":[],"name":"salary"},"r":{"t":"int","value":1}}}],"returning":true}"#).unwrap();
    let plan = monorm_engine::plan::Planner::plan(&schema, target, &update, &NoParams).unwrap();
    let Outcome::Rows { rows, types, .. } = exec::run(db.as_ref(), target, plan).await.unwrap()
    else {
        panic!()
    };
    assert_eq!(rows.cell(0, 2, types[2]).unwrap(), Cell::Int(21));
    assert_eq!(rows.cell(0, 3, types[3]).unwrap(), Cell::Text("ALICE"));
    let customer = multi_table::prepare_attach(
        &schema,
        target,
        "Customer",
        1i32.into(),
        &["points".into()],
        vec![vec![Some(10i32.into())]],
    )
    .unwrap();
    let Outcome::Rows { rows, types, .. } = multi_table::run_insert(db.as_ref(), target, customer, true)
        .await
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(rows.cell(0, 2, types[2]).unwrap(), Cell::Text("ALICE"));
    let related = monorm_engine::parse_op(
        r#"{"op":"select","model":"Person","select_related":[["employee"],["customer"]]}"#,
    )
    .unwrap();
    let plan = monorm_engine::plan::Planner::plan(&schema, target, &related, &NoParams).unwrap();
    let Outcome::Select(result) = exec::run(db.as_ref(), target, plan).await.unwrap() else {
        panic!()
    };
    assert_eq!(result.rows.len(), 1);
    // Attach touches no parent values and is protected by the ordinary FK/PK.
    let attach = multi_table::prepare_attach(
        &schema,
        target,
        "Employee",
        1i32.into(),
        &["salary".into()],
        vec![vec![Some(99i32.into())]],
    )
    .unwrap();
    assert!(multi_table::run_insert(db.as_ref(), target, attach, true)
        .await
        .is_err());
    assert!(multi_table::prepare_attach(
        &schema,
        target,
        "Employee",
        1i32.into(),
        &["name".into()],
        vec![vec![Some("overwrite".into())]]
    )
    .is_err());
    // A second attach fails on the shared PK; an attach to a missing parent fails on the FK.
    for parent in [1i32, 999] {
        let again = multi_table::prepare_attach(
            &schema,
            target,
            "Customer",
            parent.into(),
            &["points".into()],
            vec![vec![Some(5i32.into())]],
        )
        .unwrap();
        assert!(multi_table::run_insert(db.as_ref(), target, again, true)
            .await
            .is_err());
    }
    // A failure at the root, middle, or leaf insert rolls back every table.
    for (name, salary, level) in [("BADROOT", 5i32, 1i32), ("middle", 0, 1), ("leaf", 5, 0)] {
        let plan = exec::plan_insert(
            &schema,
            target,
            "Manager",
            &["name".into(), "salary".into(), "level".into()],
            vec![vec![Some(name.into()), Some(salary.into()), Some(level.into())]],
            None,
            &NoParams,
            &Default::default(),
        )
        .unwrap();
        assert!(exec::run(db.as_ref(), target, plan).await.is_err());
    }
    for table in ["person", "employee", "manager", "customer"] {
        assert_eq!(
            db.query(format!("SELECT COUNT(*) FROM {table}"), vec![])
                .await
                .unwrap()
                .get_i64(0, 0)
                .unwrap(),
            1,
            "{table}"
        );
    }
    let bad = exec::plan_insert(
        &schema,
        target,
        "Manager",
        &["name".into(), "salary".into(), "level".into()],
        vec![vec![
            Some("rollback".into()),
            Some(22i32.into()),
            Some(0i32.into()),
        ]],
        None,
        &NoParams,
        &Default::default(),
    )
    .unwrap();
    let outer = db.begin().await.unwrap();
    assert!(exec::run(outer.as_ref(), target, bad).await.is_err());
    assert_eq!(
        outer
            .query("SELECT COUNT(*) FROM person".into(), vec![])
            .await
            .unwrap()
            .get_i64(0, 0)
            .unwrap(),
        1
    );
    outer.rollback().await.unwrap();
    let bad=monorm_engine::parse_op(r#"{"op":"update","model":"Manager","set":[{"field":"salary","value":{"t":"int","value":30}},{"field":"level","value":{"t":"int","value":0}}]}"#).unwrap();
    let plan = monorm_engine::plan::Planner::plan(&schema, target, &bad, &NoParams).unwrap();
    assert!(exec::run(db.as_ref(), target, plan).await.is_err());
    assert_eq!(
        db.query("SELECT CAST(level AS BIGINT) FROM manager".into(), vec![])
            .await
            .unwrap()
            .get_i64(0, 0)
            .unwrap(),
        3
    );
    assert_eq!(
        db.query("SELECT CAST(salary AS BIGINT) FROM employee".into(), vec![])
            .await
            .unwrap()
            .get_i64(0, 0)
            .unwrap(),
        21
    );
    // A filter on an inherited field reports the count of logical rows.
    let update = monorm_engine::parse_op(r#"{"op":"update","model":"Manager","filters":[{"t":"cmp","op":"eq","l":{"t":"col","path":[],"name":"salary"},"r":{"t":"int","value":21}}],"set":[{"field":"level","value":{"t":"int","value":4}}]}"#).unwrap();
    let plan = monorm_engine::plan::Planner::plan(&schema, target, &update, &NoParams).unwrap();
    let Outcome::Affected(count) = exec::run(db.as_ref(), target, plan).await.unwrap() else {
        panic!()
    };
    assert_eq!(count, 1);
    let explicit = exec::plan_insert(
        &schema,
        target,
        "Manager",
        &["id".into(), "name".into(), "salary".into(), "level".into()],
        vec![vec![
            Some(42i32.into()),
            Some("Explicit".into()),
            Some(22i32.into()),
            Some(1i32.into()),
        ]],
        None,
        &NoParams,
        &Default::default(),
    )
    .unwrap();
    let Outcome::Rows { rows, types, .. } = exec::run(db.as_ref(), target, explicit).await.unwrap()
    else {
        panic!()
    };
    assert_eq!(rows.cell(0, 0, types[0]).unwrap(), Cell::Int(42));
    let delete =
        monorm_engine::parse_op(r#"{"op":"delete","model":"Employee","returning":true}"#).unwrap();
    let plan = monorm_engine::plan::Planner::plan(&schema, target, &delete, &NoParams).unwrap();
    let Outcome::Rows { rows, .. } = exec::run(db.as_ref(), target, plan).await.unwrap() else {
        panic!()
    };
    assert_eq!(rows.len(), 2);
    // delete() removes every level: the root rows go, and the other children of those rows cascade.
    assert_eq!(
        db.query("SELECT COUNT(*) FROM person".into(), vec![])
            .await
            .unwrap()
            .get_i64(0, 0)
            .unwrap(),
        0
    );
    assert_eq!(
        db.query("SELECT COUNT(*) FROM manager".into(), vec![])
            .await
            .unwrap()
            .get_i64(0, 0)
            .unwrap(),
        0
    );
    assert_eq!(
        db.query("SELECT COUNT(*) FROM employee".into(), vec![])
            .await
            .unwrap()
            .get_i64(0, 0)
            .unwrap(),
        0
    );
    assert_eq!(
        db.query("SELECT COUNT(*) FROM customer".into(), vec![])
            .await
            .unwrap()
            .get_i64(0, 0)
            .unwrap(),
        0
    );
    db.batch("INSERT INTO person (id, name) VALUES (100, 'A'), (101, 'B')".into()).await.unwrap();
    db.batch("INSERT INTO customer (id, points) VALUES (100, 1)".into()).await.unwrap();
    let parent_delete = monorm_engine::parse_op(r#"{"op":"delete","model":"Person"}"#).unwrap();
    let plan = monorm_engine::plan::Planner::plan(&schema, target, &parent_delete, &NoParams).unwrap();
    let Outcome::Affected(count) = exec::run(db.as_ref(), target, plan).await.unwrap() else {
        panic!()
    };
    assert_eq!(count, 2);
    assert_eq!(
        db.query("SELECT COUNT(*) FROM customer".into(), vec![])
            .await
            .unwrap()
            .get_i64(0, 0)
            .unwrap(),
        0
    );
    for statement in migrate::drop_all(&schema).unwrap() {
        db.batch(statement).await.unwrap();
    }
    db.close().await;
}
#[test]
fn composed_models_write_read_and_roll_back_together() {
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        check("sqlite://:memory:").await;
        check_missing_cascade("sqlite://:memory:").await;
        check_cascade_cache("sqlite://:memory:").await;
        check_root_delete("sqlite://:memory:").await;
        check_sqlite_cascade_names("sqlite://:memory:").await;
        if let Ok(url) = std::env::var("ORM_TEST_DATABASE_URL") {
            check(&url).await;
            check_missing_cascade(&url).await;
            check_cascade_cache(&url).await;
            check_root_delete(&url).await;
        }
    });
}

/// Tables made by hand or by Django have no `ON DELETE CASCADE` on a level key.
async fn check_missing_cascade(url: &str) {
    let dialect = if url.starts_with("sqlite:") { Dialect::Sqlite } else { Dialect::Postgres };
    let schema = Schema::from_ir(composed(dialect)).unwrap();
    let target = Target::new(dialect);
    let db = db::connect(url, 1).await.unwrap();
    for statement in migrate::drop_all(&schema).unwrap() {
        db.batch(statement).await.unwrap();
    }
    let id = if dialect == Dialect::Sqlite { "INTEGER PRIMARY KEY AUTOINCREMENT" } else { "BIGSERIAL PRIMARY KEY" };
    db.batch(format!("CREATE TABLE person (id {id}, name TEXT NOT NULL)")).await.unwrap();
    // Employee has the cascade; Manager and Customer do not (Customer has no foreign key at all).
    db.batch("CREATE TABLE employee (id BIGINT PRIMARY KEY REFERENCES person (id) ON DELETE CASCADE, salary BIGINT NOT NULL)".into()).await.unwrap();
    db.batch("CREATE TABLE manager (id BIGINT PRIMARY KEY REFERENCES employee (id), level BIGINT NOT NULL)".into()).await.unwrap();
    db.batch("CREATE TABLE customer (id BIGINT PRIMARY KEY, points BIGINT NOT NULL)".into()).await.unwrap();
    db.batch("INSERT INTO person (id, name) VALUES (1, 'A'); INSERT INTO employee VALUES (1, 5); INSERT INTO manager VALUES (1, 1); INSERT INTO customer VALUES (1, 2)".into()).await.unwrap();
    let count = |table: &'static str| {
        let db = &db;
        async move { db.query(format!("SELECT COUNT(*) FROM {table}"), vec![]).await.unwrap().get_i64(0, 0).unwrap() }
    };
    for op in [r#"{"op":"delete","model":"Manager"}"#, r#"{"op":"delete","model":"Manager","hard":true}"#] {
        let op = monorm_engine::parse_op(op).unwrap();
        let plan = monorm_engine::plan::Planner::plan(&schema, target, &op, &NoParams).unwrap();
        let Err(error) = exec::run(db.as_ref(), target, plan).await else { panic!("the delete must fail") };
        let error = error.to_string();
        assert!(error.contains("ON DELETE CASCADE") && error.contains("manager.id") && error.contains("customer.id"), "{error}");
        assert!(!error.contains("employee.id"), "{error}");
    }
    assert_eq!([count("person").await, count("employee").await, count("manager").await, count("customer").await], [1, 1, 1, 1]);
    db.batch("DROP TABLE customer; DROP TABLE manager; DROP TABLE employee; DROP TABLE person".into()).await.unwrap();
    db.close().await;
}

/// Runs `delete` of `model` (all rows) on `exec`; the tables of the cache and name tests are empty.
async fn delete_all(exec: &dyn db::Executor, schema: &Schema, target: Target, model: &str) -> monorm_engine::Result<Outcome> {
    let op = monorm_engine::parse_op(&format!(r#"{{"op":"delete","model":"{model}"}}"#)).unwrap();
    let plan = monorm_engine::plan::Planner::plan(schema, target, &op, &NoParams).unwrap();
    exec::run(exec, target, plan).await
}

/// The tables of a composed `Person <- Employee <- Manager` and `Person <- Customer`, with each key written by `key`.
async fn make_tables(db: &dyn db::Driver, schema: &Schema, dialect: Dialect, manager: &str) {
    for statement in migrate::drop_all(schema).unwrap() {
        db.batch(statement).await.unwrap();
    }
    let id = if dialect == Dialect::Sqlite { "INTEGER PRIMARY KEY AUTOINCREMENT" } else { "BIGSERIAL PRIMARY KEY" };
    db.batch(format!("CREATE TABLE person (id {id}, name TEXT NOT NULL)")).await.unwrap();
    db.batch("CREATE TABLE employee (id BIGINT PRIMARY KEY REFERENCES person (id) ON DELETE CASCADE, salary BIGINT NOT NULL)".into()).await.unwrap();
    db.batch("CREATE TABLE customer (id BIGINT PRIMARY KEY REFERENCES person (id) ON DELETE CASCADE, points BIGINT NOT NULL)".into()).await.unwrap();
    db.batch(format!("CREATE TABLE manager (id BIGINT PRIMARY KEY REFERENCES employee (id){manager}, level BIGINT NOT NULL)")).await.unwrap();
}

/// A failed check is not cached, a passed check is cached for the connection, its transactions and the settings
/// wrappers, and a schema script clears it.
async fn check_cascade_cache(url: &str) {
    let dialect = if url.starts_with("sqlite:") { Dialect::Sqlite } else { Dialect::Postgres };
    let schema = Schema::from_ir(composed(dialect)).unwrap();
    let target = Target::new(dialect);
    let db = db::connect(url, 1).await.unwrap();
    make_tables(db.as_ref(), &schema, dialect, "").await;
    let failed = delete_all(db.as_ref(), &schema, target, "Manager").await;
    assert!(failed.is_err_and(|e| e.to_string().contains("manager.id")));
    // The cascade is added: the next delete passes, so the failure was not cached.
    make_tables(db.as_ref(), &schema, dialect, " ON DELETE CASCADE").await;
    delete_all(db.as_ref(), &schema, target, "Manager").await.unwrap();
    // The cascade is dropped behind the cache: the cached pass still holds on the connection, a transaction and a scoped connection.
    make_tables(db.as_ref(), &schema, dialect, "").await;
    delete_all(db.as_ref(), &schema, target, "Manager").await.unwrap();
    let tx = db.begin().await.unwrap();
    delete_all(tx.as_ref(), &schema, target, "Customer").await.unwrap();
    delete_all(tx.as_ref(), &schema, target, "Manager").await.unwrap();
    tx.rollback().await.unwrap();
    if dialect == Dialect::Postgres {
        let scoped = db::WithSettings::new(db.clone(), vec![("app.role".into(), "x".into())]);
        delete_all(&scoped, &schema, target, "Manager").await.unwrap();
    }
    // A schema script forgets the checks.
    exec::run_script(db.clone(), vec![], false).await.unwrap();
    let failed = delete_all(db.as_ref(), &schema, target, "Manager").await;
    assert!(failed.is_err_and(|e| e.to_string().contains("manager.id")));
    for statement in migrate::drop_all(&schema).unwrap() {
        db.batch(statement).await.unwrap();
    }
    db.batch("DROP TABLE IF EXISTS manager; DROP TABLE IF EXISTS employee; DROP TABLE IF EXISTS customer; DROP TABLE IF EXISTS person".into()).await.unwrap();
    db.close().await;
}

/// SQLite compares table and column names without regard to case, and ignores a cascade while `foreign_keys` is off.
async fn check_sqlite_cascade_names(url: &str) {
    let schema = Schema::from_ir(composed(Dialect::Sqlite)).unwrap();
    let target = Target::new(Dialect::Sqlite);
    let db = db::connect(url, 1).await.unwrap();
    make_tables(db.as_ref(), &schema, Dialect::Sqlite, "").await;
    db.batch("DROP TABLE manager".into()).await.unwrap();
    db.batch("CREATE TABLE manager (ID BIGINT PRIMARY KEY REFERENCES EMPLOYEE (ID) ON DELETE CASCADE, level BIGINT NOT NULL)".into()).await.unwrap();
    delete_all(db.as_ref(), &schema, target, "Manager").await.unwrap();
    // The cascade is in the schema, but SQLite does not run it while `foreign_keys` is off.
    let off = db::connect(url, 1).await.unwrap();
    make_tables(off.as_ref(), &schema, Dialect::Sqlite, " ON DELETE CASCADE").await;
    off.batch("PRAGMA foreign_keys = OFF".into()).await.unwrap();
    let failed = delete_all(off.as_ref(), &schema, target, "Manager").await;
    assert!(failed.is_err_and(|e| e.to_string().contains("PRAGMA foreign_keys = ON")));
    off.close().await;
    db.close().await;
}

/// A delete of the root model checks the level keys too, on the plain path and on the prepared one.
async fn check_root_delete(url: &str) {
    let dialect = if url.starts_with("sqlite:") { Dialect::Sqlite } else { Dialect::Postgres };
    let schema = Schema::from_ir(composed(dialect)).unwrap();
    let target = Target::new(dialect);
    let db = db::connect(url, 1).await.unwrap();
    let op = monorm_engine::parse_op(r#"{"op":"delete","model":"Person"}"#).unwrap();
    let prepared = monorm_engine::prepared_write::PreparedWrite::prepare(&schema, target, &op, 0, &[]).unwrap();
    let run_prepared = |db: &Arc<dyn db::Driver>| {
        let (db, prepared) = (db.clone(), &prepared);
        async move { prepared.run(db.as_ref(), prepared.call(&NoParams).unwrap()).await }
    };
    make_tables(db.as_ref(), &schema, dialect, "").await;
    let failed = delete_all(db.as_ref(), &schema, target, "Person").await;
    assert!(failed.is_err_and(|e| e.to_string().contains("manager.id") && !e.to_string().contains("employee.id")));
    let failed = run_prepared(&db).await;
    assert!(failed.is_err_and(|e| e.to_string().contains("manager.id")));
    make_tables(db.as_ref(), &schema, dialect, " ON DELETE CASCADE").await;
    delete_all(db.as_ref(), &schema, target, "Person").await.unwrap();
    run_prepared(&db).await.unwrap();
    for statement in migrate::drop_all(&schema).unwrap() {
        db.batch(statement).await.unwrap();
    }
    db.batch("DROP TABLE IF EXISTS manager; DROP TABLE IF EXISTS employee; DROP TABLE IF EXISTS customer; DROP TABLE IF EXISTS person".into()).await.unwrap();
    db.close().await;
}
