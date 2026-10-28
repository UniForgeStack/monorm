use orm_core::{dialect::{Dialect, Target}, ir::Operation, schema::Schema};
use orm_engine::{params::NoParams, plan::{Output, Plan, Planner}};
use serde_json::json;

fn schema(defaults: bool) -> serde_json::Value {
    let mut schema = json!({"models":[{"name":"Selected","table":"selected","fields":[
        {"name":"value","column":"value","type":"text"},
        {"name":"id","column":"id","type":"int","primary_key":true},
        {"name":"active","column":"active","type":"bool"}
    ]}]});
    if defaults { schema["behavior"] = json!({"schema_contract":5,"read_defaults":[{"source":"orm.query-defaults","name":"query","model":"Selected","filter":{"t":"col","path":[],"name":"active"},"fields":["value"]}]}); }
    schema
}

#[test]
fn explicit_shape_retains_private_identity() {
    let schema = Schema::from_ir(serde_json::from_value(schema(false)).unwrap()).unwrap();
    for dialect in [Dialect::Postgres, Dialect::Sqlite] {
        let operation: Operation = serde_json::from_value(json!({"op":"select","model":"Selected","model_fields":["value"]})).unwrap();
        let Plan::Select(plan) = Planner::plan(&schema, Target::new(dialect), &operation, &NoParams).unwrap() else { panic!("select") };
        let Output::Instances { shape: Some(shape), .. } = plan.output else { panic!("partial shape") };
        assert_eq!(shape.fields.len(), 2);
        assert!(shape.fields[0].public); assert!(!shape.fields[1].public);
        assert_eq!(shape.fields[1].field.position, 1); assert_eq!(shape.fields[1].physical, Some(1));
    }
}

#[test]
fn duplicate_or_unknown_selection_rejected() {
    let schema = Schema::from_ir(serde_json::from_value(schema(false)).unwrap()).unwrap();
    for fields in [vec!["value", "value"], vec!["missing"]] {
        let operation = serde_json::from_value(json!({"op":"select","model":"Selected","model_fields":fields})).unwrap();
        assert!(Planner::plan(&schema, Target::new(Dialect::Sqlite), &operation, &NoParams).is_err());
    }
}

#[test]
fn neutral_defaults_are_prepared_without_a_producer_feature() {
    let schema = Schema::from_ir(serde_json::from_value(schema(true)).unwrap()).unwrap();
    assert!(schema.models[0].read_flags.filtered);
    assert_eq!(schema.models[0].read_flags.fields.as_deref().unwrap(), ["value"]);
}

#[test]
fn enabled_policy_shape_is_prepared() {
    let schema = Schema::from_ir(serde_json::from_value(schema(true)).unwrap()).unwrap();
    assert!(schema.models[0].read_defaults.filter.is_some());
    let operation = serde_json::from_value(json!({"op":"select","model":"Selected"})).unwrap();
    let Plan::Select(plan) = Planner::plan(&schema, Target::new(Dialect::Sqlite), &operation, &NoParams).unwrap() else { panic!("select") };
    assert!(matches!(plan.output, Output::Instances { shape: Some(_), .. }));
}

#[test]
fn policies_reject_bad_fields_and_recursive_loading() {
    let mut invalid = schema(true);
    invalid["behavior"]["read_defaults"][0]["fields"] = json!(["missing"]);
    assert!(Schema::from_ir(serde_json::from_value(invalid).unwrap()).is_err());
    let mut recursive = schema(true);
    recursive["models"][0]["relations"] = json!([{"name":"self_ref","kind":"one","target":"Selected","from":"id","to":"id"}]);
    recursive["behavior"]["read_defaults"][0]["load"] = json!([["self_ref"]]);
    let error = Schema::from_ir(serde_json::from_value(recursive).unwrap()).err().unwrap();
    assert!(error.contains("recursive default loading"));
}

#[test]
fn generated_types_distinguish_default_missing_fields_and_filtered_targets() {
    let mut raw = schema(true);
    raw["models"][0]["relations"] = json!([{"name":"target","kind":"one","target":"Selected","from":"id","to":"id","foreign_key":true}]);
    let ir = serde_json::from_value(raw.clone()).unwrap();
    let schema = Schema::from_ir(serde_json::from_value(raw).unwrap()).unwrap();
    let py = orm_core::codegen::python::generate(&ir, &schema, "selection-test").unwrap();
    assert!(py.stub.contains("f.BelongsTo[Selected | None"));
    let ts = orm_core::codegen::typescript::generate(&ir, &schema, "selection-test", "orm").unwrap();
    assert!(ts.contains("readonly active?: boolean"));
    assert!(ts.contains("readonly value: string"));
    assert!(ts.contains("Hop<\"target\", \"opt\""));
    // Select-out never changes insert requirements.
    assert!(ts.contains("  active: In<boolean>;"));
}

#[test]
fn a_many_to_many_path_applies_the_default_filter_of_the_join_model() {
    use orm_engine::{db, plan::plan_prefetch_only};
    let id = json!({"name":"id","column":"id","type":"big_int","primary_key":true});
    let raw = json!({"models":[
        {"name":"Post","table":"posts","fields":[id],
         "relations":[{"name":"tags","kind":"many","target":"Tag","from":"id","to":"id","through":{"model":"PostTag","source":"post_id","target":"tag_id"}}]},
        {"name":"Tag","table":"tags","fields":[id, {"name":"name","column":"name","type":"text"}]},
        {"name":"PostTag","table":"post_tags","fields":[id,
            {"name":"post_id","column":"post_id","type":"big_int"}, {"name":"tag_id","column":"tag_id","type":"big_int"},
            {"name":"live","column":"live","type":"bool"}]}
    ],"behavior":{"schema_contract":5,"read_defaults":[{"source":"orm.query-defaults","name":"query","model":"PostTag","filter":{"t":"col","path":[],"name":"live"}}]}});
    let schema = Schema::from_ir(serde_json::from_value(raw).unwrap()).unwrap();
    let target = Target::new(Dialect::Postgres);
    let filter = json!({"t":"cmp","op":"eq","l":{"t":"col","path":["tags"],"name":"name"},"r":{"t":"text","value":"b"}});
    for (without_defaults, expected) in [(false, true), (true, false)] {
        let op: Operation = serde_json::from_value(json!({"op":"select","model":"Post","filters":[filter],"without_defaults":without_defaults,
            "prefetch":[{"relation":"tags","model":"Tag"}]})).unwrap();
        let Operation::Select(q) = &op else { panic!("select") };
        let Plan::Select(plan) = Planner::plan(&schema, target, &op, &NoParams).unwrap() else { panic!("select") };
        let filtered = db::build(Dialect::Postgres, &plan.stmt).0;
        assert_eq!(filtered.contains(r#""live""#), expected, "{filtered}");
        let prefetch = plan_prefetch_only(&schema, target, q, &NoParams).unwrap();
        let sql = db::build(Dialect::Postgres, &prefetch[0].statement(vec![vec![1.into()]])).0;
        assert_eq!(sql.contains(r#""live""#), expected, "{sql}");
    }
}

mod read_flags {
    use super::*;
    use orm_core::ir::Select;

    fn schema() -> Schema {
        let raw = json!({"models":[
            {"name":"Owner","table":"owner","fields":[
                {"name":"id","column":"id","type":"int","primary_key":true},
                {"name":"active","column":"active","type":"bool"}]},
            {"name":"Plain","table":"plain","fields":[{"name":"id","column":"id","type":"int","primary_key":true}]},
            {"name":"Child","table":"child","fields":[
                {"name":"id","column":"id","type":"int","primary_key":true},
                {"name":"owner_id","column":"owner_id","type":"int"},
                {"name":"plain_id","column":"plain_id","type":"int"}],
             "relations":[
                {"name":"owner","kind":"one","target":"Owner","from":"owner_id","to":"id","foreign_key":true},
                {"name":"plain","kind":"one","target":"Plain","from":"plain_id","to":"id","foreign_key":true}]}],
            "behavior":{"schema_contract":5,"read_defaults":[
                {"source":"orm.query-defaults","name":"query","model":"Owner","filter":{"t":"col","path":[],"name":"active"}},
                {"source":"orm.query-defaults","name":"query","model":"Child","order":[{"field":"id","desc":true}]}]}});
        Schema::from_ir(serde_json::from_value(raw).unwrap()).unwrap()
    }

    fn select(op: serde_json::Value) -> Select {
        let Operation::Select(q) = serde_json::from_value(op).unwrap() else { panic!("select") };
        q
    }

    fn plan(schema: &Schema, op: serde_json::Value) -> orm_engine::plan::SelectPlan {
        let operation = serde_json::from_value(op).unwrap();
        let Plan::Select(plan) = Planner::plan(schema, Target::new(Dialect::Postgres), &operation, &NoParams).unwrap() else { panic!("select") };
        plan
    }

    #[test]
    fn join_and_root_flags_follow_filter_and_bypass() {
        let schema = schema();
        let joined = |bypass: bool| plan(&schema, json!({"op":"select","model":"Child","select_related":[["owner"],["plain"]],"without_defaults":bypass}));
        let Output::Instances { joins, .. } = joined(false).output else { panic!("instances") };
        assert_eq!(joins.iter().map(|j| (j.attr.as_str(), j.filtered)).collect::<Vec<_>>(), [("owner", true), ("plain", false)]);
        let Output::Instances { joins, .. } = joined(true).output else { panic!("instances") };
        assert!(joins.iter().all(|j| !j.filtered));
        assert!(!joined(false).filtered, "Child has an order but no filter");
        assert!(plan(&schema, json!({"op":"select","model":"Owner"})).filtered);
        assert!(!plan(&schema, json!({"op":"select","model":"Owner","without_defaults":true})).filtered);
    }

    #[test]
    fn root_filter_bypass_leaves_join_targets_filtered() {
        let schema = schema();
        assert!(!plan(&schema, json!({"op":"select","model":"Owner","without_root_filter":true})).filtered);
        let joined = plan(&schema, json!({"op":"select","model":"Child","select_related":[["owner"]],"without_root_filter":true}));
        let Output::Instances { joins, .. } = joined.output else { panic!("instances") };
        assert!(joins[0].filtered, "the joined Owner keeps its default");
        let bare = plan(&schema, json!({"op":"select","model":"Child","select_related":[["owner"]],"without_defaults":true}));
        let Output::Instances { joins, .. } = bare.output else { panic!("instances") };
        assert!(!joins[0].filtered);
        for op in ["update", "delete"] {
            let sql = |root_free: bool| {
                let operation = serde_json::from_value(json!({"op":op,"model":"Owner","without_root_filter":root_free,
                    "set":[{"field":"active","value":{"t":"int","value":0}}]})).unwrap();
                match Planner::plan(&schema, Target::new(Dialect::Postgres), &operation, &NoParams).unwrap() {
                    Plan::Update(stmt, _) => orm_engine::db::build(Dialect::Postgres, &stmt).0,
                    Plan::Delete(stmt, _) => orm_engine::db::build(Dialect::Postgres, &stmt).0,
                    _ => panic!("write"),
                }
            };
            assert!(sql(false).contains(r#""active""#) && sql(false).contains("WHERE"), "{op}");
            assert!(!sql(true).contains("WHERE"), "{op}: {}", sql(true));
        }
    }

    #[test]
    fn prepared_flags_and_effective_order() {
        let schema = schema();
        let flags = |name: &str| schema.models.iter().find(|m| m.ir.name == name).unwrap().read_flags.clone();
        assert!(flags("Owner").filtered && !flags("Child").filtered && !flags("Plain").filtered);
        let order = |op| orm_engine::plan::effective_order(&schema, &select(op)).unwrap();
        assert_eq!(order(json!({"op":"select","model":"Child"})).len(), 1);
        assert!(order(json!({"op":"select","model":"Child","without_defaults":true})).is_empty());
        let explicit = json!({"op":"select","model":"Child","order":[{"expr":{"t":"col","path":[],"name":"owner_id"},"desc":false}]});
        assert_eq!(order(explicit.clone()).len(), 1);
        assert!(!order(explicit)[0].desc);
    }
}

mod unset_when_absent {
    use super::*;
    use orm_engine::plan::plan_prefetch_only;

    // `Child.owner` is a to-one that holds the key; `Owner.profile` is a has-one; `Owner.children` is a has-many.
    fn schema(filter_owner: bool) -> Schema {
        let id = json!({"name":"id","column":"id","type":"int","primary_key":true});
        let mut raw = json!({"models":[
            {"name":"Owner","table":"owner","fields":[id, {"name":"active","column":"active","type":"bool"}],
             "relations":[
                {"name":"profile","kind":"one","target":"Child","from":"id","to":"owner_id"},
                {"name":"children","kind":"many","target":"Child","from":"id","to":"owner_id"}]},
            {"name":"Child","table":"child","fields":[id, {"name":"owner_id","column":"owner_id","type":"int"}],
             "relations":[{"name":"owner","kind":"one","target":"Owner","from":"owner_id","to":"id","foreign_key":true}]}]});
        if filter_owner {
            raw["behavior"] = json!({"schema_contract":5,"read_defaults":[{"source":"orm.query-defaults","name":"query","model":"Owner","filter":{"t":"col","path":[],"name":"active"}}]});
        }
        Schema::from_ir(serde_json::from_value(raw).unwrap()).unwrap()
    }

    fn select(op: serde_json::Value) -> orm_core::ir::Select {
        let Operation::Select(q) = serde_json::from_value(op).unwrap() else { panic!("select") };
        q
    }

    fn prefetch(schema: &Schema, model: &str, prefetch: serde_json::Value, bypass: bool) -> Vec<(bool, bool)> {
        let q = select(json!({"op":"select","model":model,"without_defaults":bypass,"prefetch":[prefetch]}));
        plan_prefetch_only(schema, Target::new(Dialect::Postgres), &q, &NoParams).unwrap().iter().map(|p| (p.filtered, p.unset_when_absent)).collect()
    }

    #[test]
    fn one_row_empty_slice_keeps_zero_limit() {
        use orm_engine::db;
        let s = schema(false);
        for dialect in [Dialect::Postgres, Dialect::Sqlite] {
            let q = select(json!({"op":"select","model":"Owner","prefetch":[{
                "relation":"children","model":"Child","attr":"chosen","one":true,"limit":0,"offset":5
            }]}));
            let plans = plan_prefetch_only(&s, Target::new(dialect), &q, &NoParams).unwrap();
            assert!(!plans[0].many);
            let (sql, values) = db::build(dialect, &plans[0].statement(vec![vec![1.into()]]));
            // The upper window bound is offset + 0; PostgreSQL reuses its binding.
            assert!(sql.contains("\"p\".\"_rn\" >") && sql.contains("\"p\".\"_rn\" <="), "{sql}");
            assert_eq!(values.last(), Some(&sea_query::Value::BigUnsigned(Some(5))));
        }
    }

    #[test]
    fn one_row_offset_uses_a_per_parent_window() {
        use orm_engine::db;
        let s = schema(false);
        for dialect in [Dialect::Postgres, Dialect::Sqlite] {
            for desc in [false, true] {
                let q = select(json!({"op":"select","model":"Owner","prefetch":[{
                    "relation":"children","model":"Child","attr":"chosen","one":true,
                    "limit":1,"offset":1,"order":[{"expr":{"t":"col","path":[],"name":"id"},"desc":desc}]
                }]}));
                let plans = plan_prefetch_only(&s, Target::new(dialect), &q, &NoParams).unwrap();
                assert!(!plans[0].many);
                let (sql, values) = db::build(dialect, &plans[0].statement(vec![vec![1.into()], vec![2.into()]]));
                assert_eq!(&values[values.len() - 2..], &[sea_query::Value::BigUnsigned(Some(1)), sea_query::Value::BigUnsigned(Some(2))]);
                assert!(sql.contains("ROW_NUMBER() OVER (PARTITION BY"), "{sql}");
                assert!(sql.contains(if desc { " DESC" } else { " ASC" }), "{sql}");
                assert!(sql.contains("\"p\".\"_rn\" >") && sql.contains("\"p\".\"_rn\" <="), "{sql}");
            }
        }
        for item in [
            json!({"relation":"children","model":"Child","one":true,"limit":1}),
            json!({"relation":"profile","model":"Child","one":true,"attr":"chosen","limit":1}),
            json!({"relation":"children","model":"Child","one":true,"attr":"chosen","limit":2}),
        ] {
            let q = select(json!({"op":"select","model":"Owner","prefetch":[item]}));
            assert!(plan_prefetch_only(&s, Target::new(Dialect::Sqlite), &q, &NoParams).is_err());
        }
    }

    fn join(schema: &Schema, bypass: bool) -> Vec<bool> {
        let operation: Operation = serde_json::from_value(json!({"op":"select","model":"Child","select_related":[["owner"]],"without_defaults":bypass})).unwrap();
        let Plan::Select(plan) = Planner::plan(schema, Target::new(Dialect::Postgres), &operation, &NoParams).unwrap() else { panic!("select") };
        let Output::Instances { joins, .. } = plan.output else { panic!("instances") };
        joins.iter().map(|j| j.unset_when_absent).collect()
    }

    #[test]
    fn prefetch_flag_follows_direction_label_and_cardinality() {
        let s = schema(false);
        assert_eq!(prefetch(&s, "Child", json!({"relation":"owner","model":"Owner"}), false), [(false, true)], "belongs-to");
        assert_eq!(prefetch(&s, "Child", json!({"relation":"owner","model":"Owner","attr":"label"}), false), [(false, false)], "label");
        assert_eq!(prefetch(&s, "Owner", json!({"relation":"profile","model":"Child"}), false), [(false, false)], "has-one");
        assert_eq!(prefetch(&s, "Owner", json!({"relation":"children","model":"Child"}), false), [(false, false)], "has-many");
        assert_eq!(prefetch(&s, "Owner", json!({"relation":"children","model":"Child","attr":"first","one":true}), false), [(false, false)], "one=true");
        assert_eq!(join(&s, false), [true]);
    }

    #[test]
    fn filtered_targets_never_unset_unless_bypassed() {
        let s = schema(true);
        assert_eq!(prefetch(&s, "Child", json!({"relation":"owner","model":"Owner"}), false), [(true, false)], "filtered");
        assert_eq!(prefetch(&s, "Child", json!({"relation":"owner","model":"Owner"}), true), [(false, true)], "bypassed");
        assert_eq!(join(&s, false), [false]);
        assert_eq!(join(&s, true), [true]);
    }

    #[test]
    fn binding_json_wrappers_have_one_shape() {
        let s = schema(true);
        let flags = orm_engine::plan::read_flags_json(&s);
        assert_eq!(flags["Owner"]["filtered"], json!(true));
        assert_eq!(flags["Child"]["filtered"], json!(false));
        assert!(flags["Owner"].get("fields").is_some() && flags["Owner"].get("related").is_some());
        let order = orm_engine::plan::effective_order_json(&s, r#"{"model":"Child","order":[{"expr":{"t":"col","path":[],"name":"owner_id"},"desc":true}]}"#).unwrap();
        // An explicit order is the caller's own; the wrapper reports the default order and the filtered flag.
        assert_eq!(order, json!({"order":[],"filtered":false}));
        let owner = orm_engine::plan::effective_order_json(&s, r#"{"model":"Owner"}"#).unwrap();
        assert_eq!(owner["filtered"], json!(true));
        let bypassed = orm_engine::plan::effective_order_json(&s, r#"{"model":"Owner","without_defaults":true}"#).unwrap();
        assert_eq!(bypassed["filtered"], json!(false));
    }

    #[test]
    fn current_filter_lists_filters_with_their_bypass_state() {
        let s = schema(true);
        let all = orm_engine::plan::current_filter_json(&s, r#"{"model":"Owner"}"#).unwrap();
        let entries = all.as_array().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!((entries[0]["active"].clone(), entries[0]["source"].is_string(), entries[0]["expr"]["t"].is_string()), (json!(true), true, true));
        let name = entries[0]["name"].as_str().unwrap().to_string();
        let named = orm_engine::plan::current_filter_json(&s, &json!({"model":"Owner","without_defaults":[name]}).to_string()).unwrap();
        assert_eq!(named[0]["active"], json!(false));
        let bare = orm_engine::plan::current_filter_json(&s, r#"{"model":"Owner","without_defaults":true}"#).unwrap();
        assert_eq!(bare[0]["active"], json!(false));
        assert_eq!(orm_engine::plan::current_filter_json(&s, r#"{"model":"Child"}"#).unwrap(), json!([]));
        let err = orm_engine::plan::current_filter_json(&s, r#"{"model":"Owner","without_defaults":["nope"]}"#).unwrap_err().to_string();
        assert!(err.contains("unknown default"), "{err}");
    }
}

#[test]
fn upsert_bypass_names_the_defaults_it_skips() {
    use orm_core::ir::Bypass;
    use orm_engine::{db, plan::{plan_insert, OnConflict}};
    let schema = Schema::from_ir(serde_json::from_value(schema(true)).unwrap()).unwrap();
    for dialect in [Dialect::Postgres, Dialect::Sqlite] {
        let sql = |bypass: Bypass, update: bool| {
            let conflict = if update { OnConflict::Update(vec!["id".into()], Default::default(), vec!["value".into()], vec![]) }
                else { OnConflict::Nothing(vec!["id".into()], Default::default()) };
            plan_insert(&schema, Target::new(dialect), "Selected", &["id".into(), "value".into()],
                vec![vec![Some(7.into()), Some("new".into())]], Some(conflict), &NoParams, false, &bypass)
                .map(|(stmt, _)| db::build(dialect, &stmt).0)
        };
        for bypass in [Bypass::All(false), Bypass::Names(vec![])] {
            assert!(sql(bypass, true).unwrap().contains(" DO UPDATE SET \"value\" = \"excluded\".\"value\" WHERE "), "{dialect:?}");
        }
        for bypass in [Bypass::All(true), Bypass::Names(vec!["query".into()])] {
            let sql = sql(bypass, true).unwrap();
            assert!(!sql.contains(" WHERE "), "{sql}");
        }
        let error = sql(Bypass::Names(vec!["nope".into()]), true).unwrap_err().to_string();
        assert!(error.contains("unknown default") && error.contains("query"), "{error}");
        assert!(sql(Bypass::Names(vec!["nope".into()]), false).is_err(), "a name is checked without DO UPDATE too");
    }
}

#[test]
fn neutral_upsert_filters_the_update_action_and_fails_closed() {
    use orm_engine::{db, plan::{plan_insert, OnConflict}};
    let mut raw = schema(true);
    // A second producer must contribute to the same action predicate.
    raw["behavior"]["read_defaults"].as_array_mut().unwrap().push(json!({
        "source":"other-producer","name":"tenant","model":"Selected",
        "filter":{"t":"cmp","op":"eq","l":{"t":"col","path":[],"name":"id"},"r":{"t":"scope","name":"tenant"}}
    }));
    let schema = Schema::from_ir(serde_json::from_value(raw).unwrap()).unwrap();
    struct Tenant;
    impl orm_engine::Params for Tenant {
        fn len(&self) -> usize { 1 }
        fn value(&self, _: usize, _: Option<orm_core::ir::ValueType>) -> orm_engine::Result<sea_query::Value> { Ok(7.into()) }
        fn text(&self, _: usize) -> orm_engine::Result<String> { unreachable!() }
        fn count(&self, _: usize) -> orm_engine::Result<u64> { unreachable!() }
        fn scope(&self, name: &str) -> Option<usize> { (name == "tenant").then_some(0) }
    }
    for dialect in [Dialect::Postgres, Dialect::Sqlite] {
        let insert = |update: bool, params: &dyn orm_engine::Params| {
            let conflict = if update {
                OnConflict::Update(vec!["id".into()], Default::default(), vec!["value".into()], vec![])
            } else { OnConflict::Nothing(vec!["id".into()], Default::default()) };
            plan_insert(&schema, Target::new(dialect), "Selected", &["id".into(), "value".into()],
                vec![vec![Some(7.into()), Some("new".into())]], Some(conflict), params, false, &Default::default())
        };
        let error = insert(true, &NoParams).err().expect("missing scope must fail before execution");
        assert!(error.to_string().contains("scope.tenant"), "{error}");
        let (stmt, _) = insert(true, &Tenant).unwrap();
        let (sql, values) = db::build(dialect, &stmt);
        let (target, action) = sql.split_once(" DO UPDATE SET ").unwrap();
        assert!(!target.contains(" WHERE "), "the default is not an index predicate: {sql}");
        let (_, predicate) = action.split_once(" WHERE ").expect("action predicate");
        assert!(predicate.contains("\"active\"") && predicate.contains("\"id\""), "{sql}");
        assert_eq!(values.last(), Some(&sea_query::Value::Int(Some(7))));
        assert!(insert(false, &NoParams).is_ok(), "DO NOTHING does not read defaults");
    }
}

#[test]
fn neutral_defaults_apply_to_reads_writes_and_returned_shapes() {
    use orm_engine::{db, plan::plan_prefetch_only};
    let mut raw = schema(true);
    raw["models"][0]["relations"] = json!([{"name":"parent","kind":"one","target":"Parent","from":"id","to":"id","foreign_key":true}]);
    raw["models"].as_array_mut().unwrap().push(json!({"name":"Parent","table":"parents","fields":[
        {"name":"id","column":"id","type":"int","primary_key":true},
        {"name":"live","column":"live","type":"bool"},
        {"name":"label","column":"label","type":"text"}
    ]}));
    raw["behavior"]["read_defaults"][0]["source"] = json!("first-producer");
    raw["behavior"]["read_defaults"][0]["name"] = json!("selection");
    raw["behavior"]["read_defaults"][0]["order"] = json!([{"field":"id","desc":true}]);
    raw["behavior"]["read_defaults"][0]["load"] = json!([["parent"]]);
    raw["behavior"]["read_defaults"].as_array_mut().unwrap().push(json!({
        "source":"second-producer","name":"visibility","model":"Parent",
        "filter":{"t":"col","path":[],"name":"live"},"fields":["label"]
    }));
    let schema = Schema::from_ir(serde_json::from_value(raw).unwrap()).unwrap();
    for dialect in [Dialect::Postgres, Dialect::Sqlite] {
        for bypass in [false, true] {
            let operation = serde_json::from_value(json!({"op":"select","model":"Selected","without_defaults":bypass})).unwrap();
            let Plan::Select(plan) = Planner::plan(&schema, Target::new(dialect), &operation, &NoParams).unwrap() else { panic!("select") };
            let sql = db::build(dialect, &plan.stmt).0;
            assert_eq!(sql.contains(" WHERE "), !bypass, "{sql}");
            assert_eq!(sql.contains(" DESC"), !bypass, "{sql}");
            let Output::Instances { shape, joins, .. } = plan.output else { panic!("instances") };
            assert_eq!(shape.is_some(), !bypass);
            assert_eq!(joins.len(), usize::from(!bypass));
            if !bypass { assert!(joins[0].filtered && joins[0].shape.is_some()); }
            for op in ["update", "delete"] {
                let operation = serde_json::from_value(json!({"op":op,"model":"Selected","without_defaults":bypass,
                    "returning":true,"set":[{"field":"value","value":{"t":"text","value":"updated"}}]})).unwrap();
                let plan = Planner::plan(&schema, Target::new(dialect), &operation, &NoParams).unwrap();
                let (sql, shaped) = match plan {
                    Plan::Update(stmt, returned) => (db::build(dialect, &stmt).0, returned.unwrap().2.is_some()),
                    Plan::Delete(stmt, returned) => (db::build(dialect, &stmt).0, returned.unwrap().2.is_some()),
                    _ => panic!("write"),
                };
                assert_eq!(sql.contains(" WHERE "), !bypass, "{sql}");
                assert_eq!(shaped, !bypass);
            }
            // The target's filter also applies inside EXISTS and standalone prefetches.
            let operation = serde_json::from_value(json!({"op":"select","model":"Selected","without_defaults":bypass,
                "filters":[{"t":"cmp","op":"eq","l":{"t":"col","path":["parent"],"name":"label"},"r":{"t":"text","value":"x"}}],
                "prefetch":[{"relation":"parent","model":"Parent"}]})).unwrap();
            let Operation::Select(q) = &operation else { panic!("select") };
            let Plan::Select(plan) = Planner::plan(&schema, Target::new(dialect), &operation, &NoParams).unwrap() else { panic!("select") };
            let sql = db::build(dialect, &plan.stmt).0;
            assert!(sql.contains("EXISTS"), "{sql}");
            assert_eq!(sql.contains("\"live\""), !bypass, "{sql}");
            let prefetch = plan_prefetch_only(&schema, Target::new(dialect), q, &NoParams).unwrap();
            assert_eq!(prefetch[0].filtered, !bypass);
        }
    }
}

#[test]
fn a_many_to_many_join_default_fails_closed_without_scope() {
    struct Tenant;
    impl orm_engine::Params for Tenant {
        fn len(&self) -> usize { 1 }
        fn value(&self, _: usize, _: Option<orm_core::ir::ValueType>) -> orm_engine::Result<sea_query::Value> { Ok(7.into()) }
        fn text(&self, _: usize) -> orm_engine::Result<String> { unreachable!() }
        fn count(&self, _: usize) -> orm_engine::Result<u64> { unreachable!() }
        fn scope(&self, name: &str) -> Option<usize> { (name == "tenant").then_some(0) }
    }
    let id = json!({"name":"id","column":"id","type":"big_int","primary_key":true});
    let raw = json!({"models":[
        {"name":"Post","table":"posts","fields":[id],
         "relations":[{"name":"tags","kind":"many","target":"Tag","from":"id","to":"id","through":{"model":"PostTag","source":"post_id","target":"tag_id"}}]},
        {"name":"Tag","table":"tags","fields":[id, {"name":"name","column":"name","type":"text"}]},
        {"name":"PostTag","table":"post_tags","fields":[id,
            {"name":"post_id","column":"post_id","type":"big_int"}, {"name":"tag_id","column":"tag_id","type":"big_int"},
            {"name":"shop","column":"shop","type":"big_int"}]}
    ],"behavior":{"schema_contract":5,"read_defaults":[{"source":"example.tenancy","name":"tenant","model":"PostTag",
        "filter":{"t":"cmp","op":"eq","l":{"t":"col","path":[],"name":"shop"},"r":{"t":"scope","name":"tenant"}}}]}});
    let schema = Schema::from_ir(serde_json::from_value(raw).unwrap()).unwrap();
    let target = Target::new(Dialect::Postgres);
    let filter = json!({"t":"cmp","op":"eq","l":{"t":"col","path":["tags"],"name":"name"},"r":{"t":"text","value":"b"}});
    let select = |bypass: serde_json::Value| -> Operation {
        serde_json::from_value(json!({"op":"select","model":"Post","filters":[filter],"without_defaults":bypass})).unwrap()
    };
    let error = Planner::plan(&schema, target, &select(json!(false)), &NoParams).err().expect("no scope must fail");
    for expected in ["PostTag", "tenant", "example.tenancy", "scope.tenant", "not set"] { assert!(error.to_string().contains(expected), "{error}"); }
    Planner::plan(&schema, target, &select(json!(false)), &Tenant).expect("a scope allows the join");
    Planner::plan(&schema, target, &select(json!(true)), &NoParams).expect("a bare bypass allows the join");
}

#[test]
fn scope_errors_identify_the_prepared_default_at_each_consumer() {
    use orm_engine::plan::plan_prefetch_only;
    let mut raw = schema(false);
    raw["models"][0]["relations"] = json!([{"name":"children","kind":"many","target":"Child","from":"id","to":"id"}]);
    let mut child = raw["models"][0].clone();
    child["name"] = json!("Child"); child["table"] = json!("child"); child["relations"] = json!([]);
    raw["models"].as_array_mut().unwrap().push(child);
    raw["models"][0]["relations"].as_array_mut().unwrap().push(json!({"name":"child","kind":"one","target":"Child","from":"id","to":"id"}));
    let scoped = json!({"t":"not","item":{"t":"or","items":[
        {"t":"cmp","op":"eq","l":{"t":"col","path":[],"name":"id"},"r":{"t":"scope","name":"tenant"}},
        {"t":"is_null","item":{"t":"scope","name":"tenant"}}
    ]}});
    raw["behavior"] = json!({"schema_contract":5,"read_defaults":[
        {"source":"example.tenancy","name":"tenant_guard","model":"Child","filter":scoped},
        {"source":"example.visibility","name":"visible","model":"Child","filter":{"t":"col","path":[],"name":"active"}},
        {"source":"example.other","name":"unrelated","model":"Selected","filter":scoped}
    ]});
    let schema = Schema::from_ir(serde_json::from_value(raw).unwrap()).unwrap();
    let check = |error: orm_engine::Error| {
        let message = error.to_string();
        for expected in ["Child", "tenant_guard", "example.tenancy", "scope.tenant", "not set", "without_defaults(\"tenant_guard\")"] {
            assert!(message.contains(expected), "missing {expected}: {message}");
        }
        for unrelated in ["unrelated", "example.other", "visible", "example.visibility"] {
            assert!(!message.contains(unrelated), "{message}");
        }
    };
    for dialect in [Dialect::Postgres, Dialect::Sqlite] {
        let target = Target::new(dialect);
        for op in [
            json!({"op":"select","model":"Child"}),
            json!({"op":"count","model":"Child"}),
            json!({"op":"update","model":"Child","set":[{"field":"value","value":{"t":"text","value":"new"}}]}),
            json!({"op":"delete","model":"Child"}),
            json!({"op":"select","model":"Selected","select_related":[["child"]]}),
            json!({"op":"select","model":"Selected","filters":[{"t":"cmp","op":"eq","l":{"t":"col","path":["children"],"name":"value"},"r":{"t":"text","value":"x"}}]}),
        ] {
            let operation: Operation = serde_json::from_value(op).unwrap();
            check(Planner::plan(&schema, target, &operation, &NoParams).err().expect("missing scope"));
        }
        check(orm_engine::plan::plan_insert(&schema, target, "Child", &["id".into()],
            vec![vec![Some(1.into())]], Some(orm_engine::plan::OnConflict::Update(vec!["id".into()], Default::default(), vec!["id".into()], vec![])),
            &NoParams, false, &Default::default()).err().expect("upsert scope"));
        let Operation::Select(q) = serde_json::from_value(json!({"op":"select","model":"Selected","prefetch":[{"relation":"children","model":"Child"}]})).unwrap() else { panic!("select") };
        check(plan_prefetch_only(&schema, target, &q, &NoParams).err().expect("prefetch scope"));
        let bypass = serde_json::from_value(json!({"op":"select","model":"Child","without_defaults":true})).unwrap();
        assert!(Planner::plan(&schema, target, &bypass, &NoParams).is_ok());
    }
}

fn count_children() -> serde_json::Value { json!({"t":"func","name":"count","rel":["children"]}) }

/// `Selected` with a to-many `children`, a to-one `author`, and these defaults.
fn annotated(defaults: serde_json::Value) -> Schema {
    let id = json!({"name":"id","column":"id","type":"int","primary_key":true});
    let raw = json!({"models":[
        {"name":"Selected","table":"selected","fields":[
            {"name":"value","column":"value","type":"text"}, id, {"name":"author_id","column":"author_id","type":"int"}],
         "relations":[{"name":"children","kind":"many","target":"Child","from":"id","to":"selected_id"},
                      {"name":"author","kind":"one","target":"Author","from":"author_id","to":"id","foreign_key":true}]},
        {"name":"Child","table":"child","fields":[id, {"name":"selected_id","column":"selected_id","type":"int"}]},
        {"name":"Author","table":"author","fields":[id, {"name":"name","column":"name","type":"text"}]}
    ],"behavior":{"schema_contract":5,"read_defaults":defaults}});
    Schema::from_ir(serde_json::from_value(raw).unwrap()).unwrap()
}

fn labelled() -> Schema {
    annotated(json!([
        {"source":"example.labels","name":"labels","model":"Selected","annotate":{"n":count_children()},"fields":["value"],"load":[["author"]],"order":[{"field":"id","desc":true}]}
    ]))
}

fn plan_select(schema: &Schema, dialect: Dialect, op: serde_json::Value) -> Result<orm_engine::plan::SelectPlan, String> {
    let operation: Operation = serde_json::from_value(op).unwrap();
    match Planner::plan(schema, Target::new(dialect), &operation, &NoParams).map_err(|e| e.to_string())? {
        Plan::Select(plan) => Ok(plan),
        _ => panic!("select"),
    }
}

#[test]
fn default_annotation_keeps_an_instance_read_with_default_load_and_fields() {
    let schema = labelled();
    for dialect in [Dialect::Postgres, Dialect::Sqlite] {
        let plan = plan_select(&schema, dialect, json!({"op":"select","model":"Selected"})).unwrap();
        let Output::Instances { shape: Some(shape), joins, annotations, .. } = &plan.output else { panic!("instances") };
        // Only the default field `value` is public; the identity and the join key are private.
        assert_eq!(shape.fields.iter().filter(|f| f.public).count(), 1);
        assert_eq!(joins.len(), 1);
        assert_eq!(annotations.iter().map(|a| a.name.as_str()).collect::<Vec<_>>(), ["n"]);
        let (sql, _) = orm_engine::db::build(dialect, &plan.stmt);
        assert!(sql.contains("LEFT JOIN \"author\"") && sql.contains("\"child\""), "{sql}");
        for bypass in [json!(true), json!(["labels"])] {
            let plan = plan_select(&schema, dialect, json!({"op":"select","model":"Selected","without_defaults":bypass})).unwrap();
            let Output::Instances { annotations, .. } = &plan.output else { panic!("instances") };
            assert!(annotations.is_empty());
        }
    }
}

#[test]
fn default_annotation_allows_explicit_load_and_prefetch() {
    let schema = labelled();
    for dialect in [Dialect::Postgres, Dialect::Sqlite] {
        let op = json!({"op":"select","model":"Selected","select_related":[["author"]],"prefetch":[{"relation":"children","model":"Child"}]});
        let plan = plan_select(&schema, dialect, op.clone()).unwrap();
        let Output::Instances { annotations, .. } = &plan.output else { panic!("instances") };
        assert_eq!(annotations.len(), 1);
        let Operation::Select(q) = serde_json::from_value(op).unwrap() else { panic!("select") };
        assert_eq!(orm_engine::plan::plan_prefetch_only(&schema, Target::new(dialect), &q, &NoParams).unwrap().len(), 1);
    }
}

#[test]
fn default_annotations_stay_out_of_aggregate_group_by_distinct_and_subquery_projections() {
    let schema = labelled();
    let id = json!({"t":"col","path":[],"name":"id"});
    let item = |expr: &serde_json::Value| json!({"t":"expr","expr":expr});
    let cases = [
        json!({"op":"select","model":"Selected","columns":[item(&json!({"t":"func","name":"count"}))]}),
        json!({"op":"select","model":"Selected","columns":[item(&id), item(&json!({"t":"func","name":"count"}))],"group_by":[id]}),
        json!({"op":"select","model":"Selected","columns":[item(&id)],"distinct":true}),
        json!({"op":"select","model":"Selected","columns":[item(&id)]}),
        json!({"op":"select","model":"Selected","columns":[item(&id)],"filters":[{"t":"in_select","item":id,"select":{"model":"Selected","columns":[item(&id)]}}]}),
        json!({"op":"select","model":"Selected","columns":[item(&id)],"filters":[{"t":"exists","select":{"model":"Selected"}}]}),
    ];
    for dialect in [Dialect::Postgres, Dialect::Sqlite] {
        for case in &cases {
            let plan = plan_select(&schema, dialect, case.clone()).unwrap();
            let (sql, _) = orm_engine::db::build(dialect, &plan.stmt);
            assert!(!sql.contains("\"child\""), "{sql}");
            match &plan.output {
                Output::Rows { .. } => {}
                Output::Instances { annotations, .. } => assert!(annotations.len() <= 1, "{sql}"),
            }
        }
    }
}

#[test]
fn an_explicit_annotation_wins_over_a_default_of_the_same_name() {
    let schema = labelled();
    for dialect in [Dialect::Postgres, Dialect::Sqlite] {
        let explicit = json!({"name":"n","expr":{"t":"func","name":"count","rel":["children"],"distinct":true,"args":[{"t":"col","path":["children"],"name":"id"}]}});
        let plan = plan_select(&schema, dialect, json!({"op":"select","model":"Selected","annotations":[explicit]})).unwrap();
        let Output::Instances { annotations, .. } = &plan.output else { panic!("instances") };
        assert_eq!(annotations.len(), 1);
        let (sql, _) = orm_engine::db::build(dialect, &plan.stmt);
        assert!(sql.contains("COUNT(DISTINCT"), "{sql}");
    }
}

#[test]
fn scope_errors_identify_the_default_for_an_annotation() {
    let scoped = json!({"t":"func","name":"count","rel":["children"],"filter":{"t":"cmp","op":"eq","l":{"t":"col","path":[],"name":"id"},"r":{"t":"scope","name":"tenant"}}});
    let schema = annotated(json!([
        {"source":"example.labels","name":"tenant_labels","model":"Selected","annotate":{"n":scoped}},
        {"source":"example.other","name":"unrelated","model":"Selected","annotate":{"m":count_children()}}
    ]));
    for dialect in [Dialect::Postgres, Dialect::Sqlite] {
        let error = plan_select(&schema, dialect, json!({"op":"select","model":"Selected"})).err().expect("missing scope");
        for expected in ["Selected", "tenant_labels", "example.labels", "scope.tenant", "not set", "without_defaults(\"tenant_labels\")"] {
            assert!(error.contains(expected), "missing {expected}: {error}");
        }
        for unrelated in ["unrelated", "example.other"] { assert!(!error.contains(unrelated), "{error}"); }
    }
}

#[test]
fn default_annotation_keeps_the_reported_order_equal_to_the_planned_order() {
    let schema = labelled();
    let Operation::Select(q) = serde_json::from_value(json!({"op":"select","model":"Selected"})).unwrap() else { panic!("select") };
    let reported = orm_engine::plan::effective_order(&schema, &q).unwrap();
    assert_eq!(reported.len(), 1);
    assert!(reported[0].desc);
    let plan = plan_select(&schema, Dialect::Postgres, json!({"op":"select","model":"Selected"})).unwrap();
    let (sql, _) = orm_engine::db::build(Dialect::Postgres, &plan.stmt);
    assert!(sql.contains("ORDER BY \"selected\".\"id\" DESC"), "{sql}");
}

fn named_schema() -> Schema {
    let mut raw = schema(false);
    raw["models"][0]["fields"].as_array_mut().unwrap().push(json!({"name":"tenant","column":"tenant","type":"int"}));
    raw["behavior"] = json!({"schema_contract":5,"read_defaults":[
        {"source":"example.active","name":"active","model":"Selected","filter":{"t":"col","path":[],"name":"active"}},
        {"source":"example.tenant","name":"tenant","model":"Selected","filter":{"t":"cmp","op":"eq","l":{"t":"col","path":[],"name":"tenant"},"r":{"t":"int","value":7}}},
        {"source":"example.order","name":"ordered","model":"Selected","order":[{"field":"id","desc":true}],"fields":["value"]}
    ]});
    Schema::from_ir(serde_json::from_value(raw).unwrap()).unwrap()
}

fn select_sql(schema: &Schema, bypass: serde_json::Value) -> String {
    let operation: Operation = serde_json::from_value(json!({"op":"select","model":"Selected","without_defaults":bypass})).unwrap();
    let Plan::Select(plan) = Planner::plan(schema, Target::new(Dialect::Postgres), &operation, &NoParams).unwrap() else { panic!("select") };
    orm_engine::db::build(Dialect::Postgres, &plan.stmt).0
}

#[test]
fn bypass_by_name_drops_only_the_named_defaults() {
    let schema = named_schema();
    let all = select_sql(&schema, json!(false));
    assert!(all.contains("\"active\"") && all.contains("\"tenant\" =") && all.contains("ORDER BY"), "{all}");
    let no_tenant = select_sql(&schema, json!(["tenant"]));
    assert!(no_tenant.contains("\"active\"") && !no_tenant.contains("\"tenant\" =") && no_tenant.contains("ORDER BY"), "{no_tenant}");
    let no_active = select_sql(&schema, json!(["active", "tenant"]));
    assert!(!no_active.contains("WHERE") && no_active.contains("ORDER BY"), "{no_active}");
    let no_order = select_sql(&schema, json!(["ordered"]));
    assert!(no_order.contains("\"tenant\" =") && !no_order.contains("ORDER BY") && no_order.contains("\"id\""), "{no_order}");
    assert!(!select_sql(&schema, json!(true)).contains("WHERE"));
}

#[test]
fn bypass_by_name_reaches_updates_deletes_and_joins() {
    let schema = named_schema();
    for op in ["update", "delete"] {
        let mut raw = json!({"op":op,"model":"Selected","without_defaults":["tenant"]});
        if op == "update" { raw["set"] = json!([{"field":"value","value":{"t":"text","value":"v"}}]); }
        let operation: Operation = serde_json::from_value(raw).unwrap();
        let sql = match Planner::plan(&schema, Target::new(Dialect::Postgres), &operation, &NoParams).unwrap() {
            Plan::Update(stmt, _) => orm_engine::db::build(Dialect::Postgres, &stmt).0,
            Plan::Delete(stmt, _) => orm_engine::db::build(Dialect::Postgres, &stmt).0,
            _ => panic!("write"),
        };
        assert!(sql.contains("\"active\"") && !sql.contains("\"tenant\" ="), "{sql}");
    }
}

#[test]
fn an_unknown_bypass_name_lists_the_model_defaults() {
    let schema = named_schema();
    let operation: Operation = serde_json::from_value(json!({"op":"select","model":"Selected","without_defaults":["tennant"]})).unwrap();
    let err = Planner::plan(&schema, Target::new(Dialect::Postgres), &operation, &NoParams).err().expect("unknown name").to_string();
    assert!(err.contains("tennant") && err.contains("tenant") && err.contains("active"), "{err}");
}
