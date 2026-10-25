use orm_contracts::ir::{ModelIr, SchemaIr};
use serde_json::{json, Value};

fn field(name: &str, ty: &str) -> Value {
    json!({"name": name, "column": name, "type": ty})
}
fn key(name: &str) -> Value {
    json!({"name": name, "column": name, "type": "int", "primary_key": true, "auto_increment": true})
}
fn model(name: &str, fields: Vec<Value>) -> Value {
    json!({"name": name, "table": name.to_lowercase(), "fields": fields})
}
fn relation(name: &str, kind: &str, target: &str, from: &str, to: &str, fk: bool) -> Value {
    json!({"name": name, "kind": kind, "target": target, "from": from, "to": to, "foreign_key": fk})
}
fn declare(attribute: &str, model: &str, positional: Vec<Value>) -> Value {
    json!({"attribute": attribute, "model": model, "field": null, "arguments": {}, "positional": positional,
        "location": {"file": "schema.prisma", "line": 3, "column": 1}})
}
fn schema(models: Vec<Value>, declarations: Vec<Value>) -> SchemaIr {
    serde_json::from_value(json!({"models": models, "behavior": {"declarations": declarations}})).unwrap()
}
fn lower(ir: &mut SchemaIr) -> Result<(), String> {
    orm_inherit::expand(ir)
}
fn rejected(ir: SchemaIr) -> String {
    let mut ir = ir;
    let before = serde_json::to_value(&ir).unwrap();
    let message = lower(&mut ir).unwrap_err();
    assert_eq!(serde_json::to_value(&ir).unwrap(), before, "a rejected pass must leave the schema unchanged");
    message
}
fn names(m: &ModelIr) -> Vec<&str> {
    m.fields.iter().map(|f| f.name.as_str()).collect()
}
fn get<'a>(ir: &'a SchemaIr, name: &str) -> &'a ModelIr {
    ir.models.iter().find(|m| m.name == name).unwrap_or_else(|| panic!("no model {name}"))
}

fn stamped() -> SchemaIr {
    schema(
        vec![
            model("Stamped", vec![field("created", "string"), field("note", "string")]),
            model("Post", vec![key("id"), field("title", "string")]),
            model("Page", vec![key("id")]),
        ],
        vec![
            declare("inherit.abstract", "Stamped", vec![]),
            declare("inherit.extends", "Post", vec![json!("Stamped")]),
            declare("inherit.extends", "Page", vec![json!(["Stamped"])]),
        ],
    )
}

/// Declared effects `models.inherit` and `storage.inherit`, plus the two neutral records.
#[test]
fn lowering_changes_only_the_declared_sections_once() {
    let effects = orm_contracts::extension::pass_effects(&stamped(), orm_inherit::expand).unwrap();
    assert_eq!(
        effects.into_iter().collect::<Vec<_>>(),
        ["behavior.declarations", "behavior.storage", "behavior.type_models", "models"]
    );
}

#[test]
fn extends_copies_base_fields_first_and_removes_the_abstract_model() {
    let mut ir = stamped();
    lower(&mut ir).unwrap();
    assert_eq!(ir.models.iter().map(|m| m.name.as_str()).collect::<Vec<_>>(), ["Post", "Page"]);
    assert_eq!(names(get(&ir, "Post")), ["created", "note", "id", "title"]);
    assert_eq!(get(&ir, "Post").table, "post");
    let storage = ir.behavior.storage.as_ref().unwrap();
    assert_eq!(storage.models.iter().map(|m| m.name.as_str()).collect::<Vec<_>>(), ["Post", "Page"]);
    assert_eq!(names(&storage.models[0]), ["created", "note", "id", "title"]);
    assert_eq!(ir.behavior.type_models.len(), 1);
    assert_eq!(ir.behavior.type_models[0].name, "Stamped");
    assert_eq!(ir.behavior.type_models[0].fields.len(), 2);
    assert!(ir.behavior.declarations.iter().filter(|d| d.attribute.starts_with("inherit.")).all(|d| d.lowered));
}

#[test]
fn a_second_run_changes_nothing() {
    let mut ir = stamped();
    lower(&mut ir).unwrap();
    let before = serde_json::to_value(&ir).unwrap();
    lower(&mut ir).unwrap();
    assert_eq!(serde_json::to_value(&ir).unwrap(), before);
}

#[test]
fn the_child_wins_a_redeclared_field() {
    let mut ir = schema(
        vec![model("Base", vec![field("note", "string")]), model("Item", vec![key("id"), field("note", "int")])],
        vec![declare("inherit.abstract", "Base", vec![]), declare("inherit.extends", "Item", vec![json!("Base")])],
    );
    lower(&mut ir).unwrap();
    assert_eq!(names(get(&ir, "Item")), ["id", "note"]);
}

#[test]
fn several_bases_merge_in_base_order() {
    let mut ir = schema(
        vec![
            model("A", vec![field("a", "string")]),
            model("B", vec![field("b", "string")]),
            model("Item", vec![key("id")]),
        ],
        vec![
            declare("inherit.abstract", "A", vec![]),
            declare("inherit.abstract", "B", vec![]),
            declare("inherit.extends", "Item", vec![json!(["B", "A"])]),
        ],
    );
    lower(&mut ir).unwrap();
    assert_eq!(names(get(&ir, "Item")), ["b", "a", "id"]);
}

#[test]
fn an_abstract_model_may_extend_an_abstract_model() {
    let mut ir = schema(
        vec![
            model("Root", vec![field("a", "string")]),
            model("Mid", vec![field("b", "string")]),
            model("Item", vec![key("id")]),
        ],
        vec![
            declare("inherit.abstract", "Root", vec![]),
            declare("inherit.abstract", "Mid", vec![]),
            declare("inherit.extends", "Item", vec![json!("Mid")]),
            declare("inherit.extends", "Mid", vec![json!("Root")]),
        ],
    );
    lower(&mut ir).unwrap();
    assert_eq!(names(get(&ir, "Item")), ["a", "b", "id"]);
    assert_eq!(ir.behavior.type_models.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(), ["Root", "Mid"]);
    assert_eq!(ir.behavior.type_models[1].fields.len(), 2);
}

#[test]
fn indexes_a_primary_key_and_declarations_are_copied() {
    let mut base = model("Base", vec![key("id"), field("slug", "string")]);
    base["indexes"] = json!([{"name": null, "columns": [{"name": "slug"}], "unique": true}]);
    let mut ir = schema(
        vec![base, model("Item", vec![field("extra", "string")])],
        vec![
            declare("inherit.abstract", "Base", vec![]),
            declare("inherit.extends", "Item", vec![json!("Base")]),
            json!({"attribute": "other.flag", "model": "Base", "field": "slug", "arguments": {}, "positional": [],
                "location": {"file": "schema.prisma", "line": 4, "column": 1}}),
        ],
    );
    lower(&mut ir).unwrap();
    let item = get(&ir, "Item");
    assert_eq!(names(item), ["id", "slug", "extra"]);
    assert_eq!(item.indexes.len(), 1);
    assert_eq!(item.key_fields(), ["id"]);
    let copied: Vec<_> = ir.behavior.declarations.iter().filter(|d| d.attribute == "other.flag").collect();
    assert_eq!(copied.len(), 1);
    assert_eq!((copied[0].model.as_str(), copied[0].lowered), ("Item", false));
}

#[test]
fn a_composite_primary_key_is_inherited() {
    let mut base = model("Base", vec![field("a", "int"), field("b", "int")]);
    base["primary_key"] = json!(["a", "b"]);
    let mut ir = schema(
        vec![base, model("Item", vec![field("x", "int")])],
        vec![declare("inherit.abstract", "Base", vec![]), declare("inherit.extends", "Item", vec![json!("Base")])],
    );
    lower(&mut ir).unwrap();
    assert_eq!(get(&ir, "Item").key_fields(), ["a", "b"]);
}

#[test]
fn query_set_and_model_methods_travel_together_with_classes_from() {
    let mut base = model("Base", vec![field("a", "string")]);
    base["query_set"] = json!("BaseQuery");
    base["model_methods"] = json!("BaseMethods");
    let mut ir = schema(
        vec![base, model("Item", vec![key("id")])],
        vec![declare("inherit.abstract", "Base", vec![]), declare("inherit.extends", "Item", vec![json!("Base")])],
    );
    lower(&mut ir).unwrap();
    let item = get(&ir, "Item");
    assert_eq!((item.query_set.as_deref(), item.model_methods.as_deref()), (Some("BaseQuery"), Some("BaseMethods")));
    assert_eq!(item.classes_from.as_deref(), Some("Base"));
}

#[test]
fn a_plain_extends_leaves_classes_from_empty() {
    let mut ir = stamped();
    lower(&mut ir).unwrap();
    assert_eq!(get(&ir, "Post").classes_from, None);
}

fn commented() -> SchemaIr {
    let mut owner = model("Owner", vec![key("id")]);
    owner["relations"] = json!([]);
    let mut base = model("Commentable", vec![]);
    let mut back = relation("comments", "many", "Comment", "", "", false);
    back["label"] = json!("{model}_comments");
    base["relations"] = json!([back]);
    schema(
        vec![owner, base, model("Comment", vec![key("id")]), model("Article", vec![key("id")]), model("Video", vec![key("id")])],
        vec![
            declare("inherit.abstract", "Commentable", vec![]),
            declare("inherit.extends", "Article", vec![json!("Commentable")]),
            declare("inherit.extends", "Video", vec![json!("Commentable")]),
        ],
    )
}

#[test]
fn a_model_placeholder_makes_one_foreign_key_per_child() {
    let mut ir = commented();
    lower(&mut ir).unwrap();
    let comment = get(&ir, "Comment");
    assert_eq!(names(comment), ["id", "article_comments_id", "video_comments_id"]);
    let owning: Vec<_> = comment.relations.iter().map(|r| (r.name.as_str(), r.target.as_str(), r.foreign_key)).collect();
    assert_eq!(owning, [("article_comments", "Article", true), ("video_comments", "Video", true)]);
    let article = get(&ir, "Article");
    let back = article.relations.iter().find(|r| r.name == "comments").unwrap();
    assert_eq!((back.from.as_slice(), back.to.as_slice(), back.foreign_key), (&["id".to_string()][..], &["article_comments_id".to_string()][..], false));
    assert!(back.label.is_none());
    let storage = ir.behavior.storage.as_ref().unwrap();
    let physical = storage.models.iter().find(|m| m.name == "Comment").unwrap();
    assert_eq!(names(physical), ["id", "article_comments_id", "video_comments_id"]);
    assert!(ir.models.iter().flat_map(|m| &m.relations).all(|r| r.label.is_none()));
}

#[test]
fn a_relation_without_a_name_uses_the_default_back_field() {
    let mut ir = commented();
    ir.models[1].relations[0].label = Some(String::new());
    lower(&mut ir).unwrap();
    assert_eq!(names(get(&ir, "Comment")), ["id", "article_comments_id", "video_comments_id"]);
}

#[test]
fn a_name_without_model_for_two_children_collides() {
    let mut ir = commented();
    ir.models[1].relations[0].label = Some("owner".into());
    let message = rejected(ir);
    assert!(message.contains("both generate Comment.owner"), "{message}");
    assert!(message.contains("{model}"), "{message}");
}

#[test]
fn a_name_without_model_for_one_child_works() {
    let mut ir = commented();
    ir.models[1].relations[0].label = Some("owner".into());
    ir.models.retain(|m| m.name != "Video");
    ir.behavior.declarations.retain(|d| d.model != "Video");
    lower(&mut ir).unwrap();
    assert_eq!(names(get(&ir, "Comment")), ["id", "owner_id"]);
}

#[test]
fn an_unconsumed_placeholder_is_an_error_in_core_not_here() {
    let ir = commented();
    let json = serde_json::to_value(&ir).unwrap();
    assert_eq!(json["models"][1]["relations"][0]["label"], "{model}_comments");
}

fn parent() -> SchemaIr {
    let mut user = model("User", vec![key("id"), field("name", "string")]);
    user["relations"] = json!([relation("posts", "many", "Post", "id", "author_id", false)]);
    let mut post = model("Post", vec![key("id"), field("author_id", "int")]);
    post["relations"] = json!([relation("author", "one", "User", "author_id", "id", true)]);
    schema(
        vec![user, post, model("MyUser", vec![field("nick", "string")])],
        vec![declare("inherit.override", "MyUser", vec![json!("User")])],
    )
}

#[test]
fn override_replaces_the_base_and_keeps_its_table() {
    let mut ir = parent();
    lower(&mut ir).unwrap();
    assert_eq!(ir.models.iter().map(|m| m.name.as_str()).collect::<Vec<_>>(), ["Post", "MyUser"]);
    let my = get(&ir, "MyUser");
    assert_eq!(my.table, "user");
    assert_eq!(names(my), ["id", "name", "nick"]);
    assert_eq!(my.relations[0].target, "Post");
    assert_eq!(get(&ir, "Post").relations[0].target, "MyUser");
    assert_eq!(ir.behavior.model_overrides.get("User").map(String::as_str), Some("MyUser"));
    assert_eq!(my.renamed_from, None);
    let storage = ir.behavior.storage.as_ref().unwrap();
    assert!(storage.models.iter().all(|m| m.name != "User"));
    assert_eq!(storage.models.iter().find(|m| m.name == "MyUser").unwrap().table, "user");
    assert_eq!(storage.models.iter().find(|m| m.name == "Post").unwrap().relations[0].target, "MyUser");
}

#[test]
fn override_effects_are_declared_once() {
    let effects = orm_contracts::extension::pass_effects(&parent(), orm_inherit::expand).unwrap();
    assert_eq!(effects.into_iter().collect::<Vec<_>>(), ["behavior.declarations", "behavior.model_overrides", "behavior.storage", "models"]);
}

#[test]
fn override_with_an_explicit_map_records_the_rename() {
    let mut ir = parent();
    ir.models[2].table = "custom_user".into();
    ir.models[2].mapped = true;
    lower(&mut ir).unwrap();
    let my = get(&ir, "MyUser");
    assert_eq!((my.table.as_str(), my.renamed_from.as_deref()), ("custom_user", Some("user")));
}

#[test]
fn override_follows_an_import_prefix_table() {
    let mut ir = parent();
    ir.models[0].table = "app_user".into();
    ir.models[2].table = "app_myuser".into();
    lower(&mut ir).unwrap();
    assert_eq!(get(&ir, "MyUser").table, "app_user");
}

#[test]
fn abstract_with_a_map_is_rejected() {
    let mut ir = stamped();
    ir.models[0].table = "elsewhere".into();
    ir.models[0].mapped = true;
    let message = rejected(ir);
    assert!(message.starts_with("schema.prisma:3:1: @@inherit.abstract: "), "{message}");
    assert!(message.contains("@@map"), "{message}");
}

#[test]
fn abstract_with_a_rename_hint_is_rejected() {
    let mut ir = stamped();
    ir.models[0].renamed_from = Some("old".into());
    assert!(rejected(ir).contains("@@renamed_from"));
}

#[test]
fn a_relation_to_an_abstract_model_is_rejected() {
    let mut ir = stamped();
    ir.models[1].relations.push(serde_json::from_value(relation("stamp", "one", "Stamped", "id", "id", true)).unwrap());
    let message = rejected(ir);
    assert!(message.contains("Post.stamp") && message.contains("Stamped is abstract"), "{message}");
}

#[test]
fn extending_a_concrete_model_names_the_override_attribute() {
    let ir = schema(
        vec![model("Base", vec![key("id")]), model("Item", vec![key("id")])],
        vec![declare("inherit.extends", "Item", vec![json!("Base")])],
    );
    assert!(rejected(ir).contains("`Base` is not abstract; use `@@inherit.override(\"Base\")`"));
}

#[test]
fn overriding_an_abstract_model_names_the_extends_attribute() {
    let ir = schema(
        vec![model("Base", vec![field("a", "int")]), model("Item", vec![key("id")])],
        vec![declare("inherit.abstract", "Base", vec![]), declare("inherit.override", "Item", vec![json!("Base")])],
    );
    assert!(rejected(ir).contains("`Base` is abstract; use `@@inherit.extends`"));
}

#[test]
fn an_unknown_base_is_rejected() {
    let ir = schema(vec![model("Item", vec![key("id")])], vec![declare("inherit.extends", "Item", vec![json!("Nope")])]);
    assert!(rejected(ir).contains("unknown model Nope"));
    let ir = schema(vec![model("Item", vec![key("id")])], vec![declare("inherit.override", "Item", vec![json!("Nope")])]);
    assert!(rejected(ir).contains("unknown model Nope"));
}

#[test]
fn an_extends_cycle_is_rejected() {
    let ir = schema(
        vec![model("A", vec![field("a", "int")]), model("B", vec![field("b", "int")])],
        vec![
            declare("inherit.abstract", "A", vec![]),
            declare("inherit.abstract", "B", vec![]),
            declare("inherit.extends", "A", vec![json!("B")]),
            declare("inherit.extends", "B", vec![json!("A")]),
        ],
    );
    assert!(rejected(ir).contains("extends cycle: A -> B -> A"));
}

#[test]
fn extending_itself_is_rejected() {
    let ir = schema(
        vec![model("A", vec![field("a", "int")])],
        vec![declare("inherit.abstract", "A", vec![]), declare("inherit.extends", "A", vec![json!("A")])],
    );
    assert!(rejected(ir).contains("A extends itself"));
}

#[test]
fn two_overrides_of_one_model_name_both() {
    let ir = schema(
        vec![model("Base", vec![key("id")]), model("One", vec![field("a", "int")]), model("Two", vec![field("b", "int")])],
        vec![declare("inherit.override", "One", vec![json!("Base")]), declare("inherit.override", "Two", vec![json!("Base")])],
    );
    let message = rejected(ir);
    assert!(message.contains("One") && message.contains("Two") && message.contains("both override Base"), "{message}");
}

#[test]
fn an_override_chain_is_rejected() {
    let ir = schema(
        vec![model("Base", vec![key("id")]), model("One", vec![field("a", "int")]), model("Two", vec![field("b", "int")])],
        vec![declare("inherit.override", "One", vec![json!("Base")]), declare("inherit.override", "Two", vec![json!("One")])],
    );
    assert!(rejected(ir).contains("One is itself an override"));
}

#[test]
fn the_same_field_from_two_bases_is_a_conflict() {
    let ir = schema(
        vec![model("A", vec![field("x", "int")]), model("B", vec![field("x", "int")]), model("Item", vec![key("id")])],
        vec![
            declare("inherit.abstract", "A", vec![]),
            declare("inherit.abstract", "B", vec![]),
            declare("inherit.extends", "Item", vec![json!(["A", "B"])]),
        ],
    );
    let message = rejected(ir);
    assert!(message.contains("Item.x") && message.contains("both A and B"), "{message}");
}

#[test]
fn different_primary_keys_from_two_bases_are_a_conflict() {
    let ir = schema(
        vec![model("A", vec![key("id")]), model("B", vec![key("uid")]), model("Item", vec![field("x", "int")])],
        vec![
            declare("inherit.abstract", "A", vec![]),
            declare("inherit.abstract", "B", vec![]),
            declare("inherit.extends", "Item", vec![json!(["A", "B"])]),
        ],
    );
    assert!(rejected(ir).contains("primary key of B (uid) differs from the one of A (id)"));
}

#[test]
fn a_child_key_that_differs_from_the_base_key_is_a_conflict() {
    let ir = schema(
        vec![model("A", vec![key("id")]), model("Item", vec![key("uid")])],
        vec![declare("inherit.abstract", "A", vec![]), declare("inherit.extends", "Item", vec![json!("A")])],
    );
    assert!(rejected(ir).contains("differs from the one of Item"));
}

#[test]
fn each_class_name_is_inherited_on_its_own() {
    let mut base = model("Base", vec![field("a", "int")]);
    base["query_set"] = json!("BaseQuery");
    base["model_methods"] = json!("BaseMethods");
    let mut item = model("Item", vec![key("id")]);
    item["model_methods"] = json!("ItemMethods");
    let mut ir = schema(vec![base, item], vec![declare("inherit.abstract", "Base", vec![]), declare("inherit.extends", "Item", vec![json!("Base")])]);
    lower(&mut ir).unwrap();
    let item = get(&ir, "Item");
    assert_eq!((item.query_set.as_deref(), item.model_methods.as_deref()), (Some("BaseQuery"), Some("ItemMethods")));
    assert_eq!(item.classes_from, None);
}

#[test]
fn an_override_may_change_only_its_query_set() {
    let mut user = model("User", vec![key("id")]);
    user["query_set"] = json!("UserQuery");
    user["model_methods"] = json!("UserMethods");
    let mut my = model("MyUser", vec![]);
    my["query_set"] = json!("MyQuery");
    let mut ir = schema(vec![user, my], vec![declare("inherit.override", "MyUser", vec![json!("User")])]);
    lower(&mut ir).unwrap();
    let my = get(&ir, "MyUser");
    assert_eq!((my.query_set.as_deref(), my.model_methods.as_deref()), (Some("MyQuery"), Some("UserMethods")));
}

#[test]
fn a_query_set_in_two_bases_is_a_conflict() {
    let mut a = model("A", vec![field("a", "int")]);
    a["query_set"] = json!("AQuery");
    let mut b = model("B", vec![field("b", "int")]);
    b["query_set"] = json!("BQuery");
    let ir = schema(
        vec![a, b, model("Item", vec![key("id")])],
        vec![declare("inherit.abstract", "A", vec![]), declare("inherit.abstract", "B", vec![]), declare("inherit.extends", "Item", vec![json!(["A", "B"])])],
    );
    let message = rejected(ir);
    assert!(message.contains("@@query_set is in both A and B"), "{message}");
}

#[test]
fn a_generated_back_field_that_collides_is_rejected() {
    let mut ir = commented();
    ir.models[2].fields.push(serde_json::from_value(field("article_comments_id", "int")).unwrap());
    assert!(rejected(ir).contains("Comment.article_comments_id: generated name collides"));
}

#[test]
fn bad_arguments_are_rejected() {
    let cases = [
        ("inherit.abstract", vec![json!("x")], "takes no arguments"),
        ("inherit.extends", vec![json!(3)], "takes a model name or a list"),
        ("inherit.extends", vec![json!([])], "takes a model name or a list"),
        ("inherit.extends", vec![json!(["A", "A"])], "lists A twice"),
        ("inherit.override", vec![json!(["A", "B"])], "takes one model"),
    ];
    for (attribute, positional, expected) in cases {
        let ir = schema(
            vec![model("A", vec![field("a", "int")]), model("B", vec![field("b", "int")]), model("Item", vec![key("id")])],
            vec![declare(attribute, "Item", positional)],
        );
        let message = rejected(ir);
        assert!(message.contains(expected), "{attribute}: {message}");
    }
}

#[test]
fn a_second_extends_on_one_model_is_rejected() {
    let ir = schema(
        vec![model("A", vec![field("a", "int")]), model("B", vec![field("b", "int")]), model("Item", vec![key("id")])],
        vec![
            declare("inherit.abstract", "A", vec![]),
            declare("inherit.abstract", "B", vec![]),
            declare("inherit.extends", "Item", vec![json!("A")]),
            declare("inherit.extends", "Item", vec![json!("B")]),
        ],
    );
    assert!(rejected(ir).contains("second @@inherit.extends"));
}

#[test]
fn a_schema_without_inheritance_is_untouched() {
    let mut ir = schema(vec![model("A", vec![key("id")])], vec![]);
    let before = serde_json::to_value(&ir).unwrap();
    lower(&mut ir).unwrap();
    assert_eq!(serde_json::to_value(&ir).unwrap(), before);
}

#[test]
fn an_override_with_a_map_that_ends_in_its_name_keeps_the_map() {
    let mut ir = parent();
    ir.models[2].table = "legacy_myuser".into();
    ir.models[2].mapped = true;
    lower(&mut ir).unwrap();
    let my = get(&ir, "MyUser");
    assert_eq!((my.table.as_str(), my.renamed_from.as_deref()), ("legacy_myuser", Some("user")));
}

#[test]
fn an_abstract_map_that_ends_in_its_name_is_rejected() {
    let mut ir = stamped();
    ir.models[0].table = "x_stamped".into();
    ir.models[0].mapped = true;
    let message = rejected(ir);
    assert!(message.contains("@@map"), "{message}");
}

#[test]
fn an_abstract_model_with_an_import_prefix_has_no_map() {
    let mut ir = stamped();
    for m in &mut ir.models { m.table = format!("app_{}", m.table); }
    lower(&mut ir).unwrap();
    assert_eq!(get(&ir, "Post").table, "app_post");
}

#[test]
fn an_abstract_partition_is_rejected() {
    let mut ir = stamped();
    ir.models[0].partition = Some(serde_json::from_value(json!({"strategy": "range", "keys": [{"name": "created"}]})).unwrap());
    let message = rejected(ir);
    assert!(message.contains("@@partition"), "{message}");
}

#[test]
fn a_check_constraint_is_copied_to_each_child() {
    let mut ir = stamped();
    ir.models[0].constraints.push(serde_json::from_value(json!({"kind": "check", "expr": "note <> ''"})).unwrap());
    lower(&mut ir).unwrap();
    for name in ["Post", "Page"] {
        let json = serde_json::to_value(&get(&ir, name).constraints).unwrap();
        assert_eq!(json, json!([{"kind": "check", "expr": "note <> ''"}]), "{name}");
    }
}

#[test]
fn an_explicit_index_name_on_an_abstract_model_with_two_children_is_rejected() {
    let mut ir = stamped();
    ir.models[0].indexes.push(serde_json::from_value(json!({"name": "stamped_created", "columns": [{"name": "created"}]})).unwrap());
    let message = rejected(ir);
    assert!(message.contains("\"stamped_created\"") && message.contains("two or more children"), "{message}");
}

#[test]
fn an_explicit_constraint_name_on_an_abstract_model_with_two_children_is_rejected() {
    let mut ir = stamped();
    ir.models[0].constraints.push(serde_json::from_value(json!({"kind": "check", "name": "note_set", "expr": "note <> ''"})).unwrap());
    assert!(rejected(ir).contains("\"note_set\""));
}

#[test]
fn an_explicit_name_is_fine_with_one_child() {
    let mut ir = stamped();
    ir.models[0].indexes.push(serde_json::from_value(json!({"name": "stamped_created", "columns": [{"name": "created"}]})).unwrap());
    ir.models.retain(|m| m.name != "Page");
    ir.behavior.declarations.retain(|d| d.model != "Page");
    lower(&mut ir).unwrap();
    assert_eq!(get(&ir, "Post").indexes[0].name.as_deref(), Some("stamped_created"));
}

#[test]
fn a_placeholder_on_delete_is_applied_to_the_generated_foreign_key() {
    let mut ir = commented();
    ir.models[1].relations[0].on_delete = Some(orm_contracts::ir::OnDelete::SetNull);
    lower(&mut ir).unwrap();
    let comment = get(&ir, "Comment");
    assert!(comment.relations.iter().all(|r| r.on_delete == Some(orm_contracts::ir::OnDelete::SetNull)), "{:?}", comment.relations);
    assert!(get(&ir, "Article").relations.iter().all(|r| r.on_delete.is_none()));
}

/// `AbstractUser` has a placeholder to `Role`; `User` extends it; `MyUser` overrides `User` and extends it again.
fn user_with_roles() -> SchemaIr {
    let mut base = model("AbstractUser", vec![field("name", "string")]);
    let mut roles = relation("roles", "many", "Role", "", "", false);
    roles["label"] = json!("{model}_roles");
    base["relations"] = json!([roles]);
    base["indexes"] = json!([{"name": null, "columns": [{"name": "name"}]}]);
    base["query_set"] = json!("UserQuery");
    schema(
        vec![base, model("User", vec![key("id")]), model("Role", vec![key("id")]), model("MyUser", vec![key("id"), field("nick", "string")])],
        vec![
            declare("inherit.abstract", "AbstractUser", vec![]),
            declare("inherit.extends", "User", vec![json!("AbstractUser")]),
            declare("inherit.extends", "MyUser", vec![json!("AbstractUser")]),
            declare("inherit.override", "MyUser", vec![json!("User")]),
        ],
    )
}

#[test]
fn override_plus_extends_keeps_the_foreign_key_of_the_base() {
    let mut ir = user_with_roles();
    lower(&mut ir).unwrap();
    let role = get(&ir, "Role");
    assert_eq!(names(role), ["id", "user_roles_id"]);
    let owning: Vec<_> = role.relations.iter().map(|r| (r.name.as_str(), r.target.as_str())).collect();
    assert_eq!(owning, [("user_roles", "MyUser")]);
    let my = get(&ir, "MyUser");
    assert_eq!(names(my), ["name", "id", "nick"]);
    let roles = my.relations.iter().filter(|r| r.name == "roles").collect::<Vec<_>>();
    assert_eq!(roles.len(), 1);
    assert_eq!((roles[0].from.as_slice(), roles[0].to.as_slice()), (&["id".to_string()][..], &["user_roles_id".to_string()][..]));
    assert_eq!((my.indexes.len(), my.query_set.as_deref()), (1, Some("UserQuery")));
    let storage = ir.behavior.storage.as_ref().unwrap();
    assert_eq!(names(storage.models.iter().find(|m| m.name == "Role").unwrap()), ["id", "user_roles_id"]);
}

#[test]
fn override_plus_extends_with_a_field_that_differs_from_the_override_base_is_a_conflict() {
    let mut ir = user_with_roles();
    ir.models[1].fields.push(serde_json::from_value(field("name", "int")).unwrap());
    let message = rejected(ir);
    assert!(message.contains("MyUser.name: the field is in both User and AbstractUser"), "{message}");
}

#[test]
fn override_plus_extends_copies_a_check_once() {
    let mut ir = user_with_roles();
    ir.models[0].constraints.push(serde_json::from_value(json!({"kind": "check", "expr": "name <> ''"})).unwrap());
    lower(&mut ir).unwrap();
    assert_eq!(get(&ir, "MyUser").constraints.len(), 1);
}

/// `AbstractUser.roles Role[] @relation(through: "{model}Role")`, extended by `User` and `Admin`.
fn user_roles() -> SchemaIr {
    let mut base = model("AbstractUser", vec![key("id"), field("email", "string")]);
    let mut roles = relation("roles", "many", "Role", "", "", false);
    roles["label"] = json!("");
    roles["join_label"] = json!("{model}Role");
    base["relations"] = json!([roles]);
    schema(
        vec![base, model("User", vec![]), model("Admin", vec![field("level", "int")]), model("Role", vec![key("id"), field("name", "string")])],
        vec![
            declare("inherit.abstract", "AbstractUser", vec![]),
            declare("inherit.extends", "User", vec![json!("AbstractUser")]),
            declare("inherit.extends", "Admin", vec![json!("AbstractUser")]),
        ],
    )
}

fn through_of(m: &ModelIr, relation: &str) -> (String, String, String) {
    let t = m.relations.iter().find(|r| r.name == relation).unwrap().through.clone().unwrap_or_else(|| panic!("{}.{relation} has no through", m.name));
    (t.model, t.source, t.target)
}

#[test]
fn a_join_model_is_named_from_its_own_child() {
    let mut ir = user_roles();
    ir.models.retain(|m| m.name != "User" && m.name != "Admin");
    ir.models.push(serde_json::from_value(model("MyUser", vec![])).unwrap());
    ir.behavior.declarations.retain(|d| d.attribute != "inherit.extends");
    ir.behavior.declarations.push(serde_json::from_value(declare("inherit.extends", "MyUser", vec![json!("AbstractUser")])).unwrap());
    lower(&mut ir).unwrap();
    let mut all: Vec<_> = ir.models.iter().map(|m| m.name.as_str()).collect();
    all.sort();
    assert_eq!(all, ["MyUser", "MyUserRole", "Role"]);
    assert_eq!(get(&ir, "MyUserRole").table, "myuser_roles");
    assert_eq!(through_of(get(&ir, "MyUser"), "roles"), ("MyUserRole".into(), "myuser_id".into(), "role_id".into()));
    assert_eq!(through_of(get(&ir, "Role"), "myuser_roles"), ("MyUserRole".into(), "role_id".into(), "myuser_id".into()));
}

#[test]
fn a_through_template_gives_each_child_its_own_join_model() {
    let mut ir = user_roles();
    ir.models[1].table = "auth_user".into();
    ir.models[1].mapped = true;
    lower(&mut ir).unwrap();
    let mut all: Vec<_> = ir.models.iter().map(|m| m.name.as_str()).collect();
    all.sort();
    assert_eq!(all, ["Admin", "AdminRole", "Role", "User", "UserRole"]);
    let join = get(&ir, "UserRole");
    assert_eq!((join.table.as_str(), names(join)), ("auth_user_roles", vec!["id", "user_id", "role_id"]));
    assert_eq!(get(&ir, "AdminRole").table, "admin_roles");
    assert!(join.fields.iter().all(|f| !f.nullable));
    let links: Vec<_> = join.relations.iter().map(|r| (r.name.as_str(), r.target.as_str(), r.from[0].as_str(), r.to[0].as_str(), r.foreign_key, r.on_delete)).collect();
    assert_eq!(links, [
        ("user", "User", "user_id", "id", true, Some(orm_contracts::ir::OnDelete::Cascade)),
        ("role", "Role", "role_id", "id", true, Some(orm_contracts::ir::OnDelete::Cascade)),
    ]);
    // No explicit name: core derives it per table and shortens it to the identifier limit.
    let unique: Vec<_> = [join, get(&ir, "AdminRole")].iter().map(|m| match &m.constraints[..] {
        [orm_contracts::ir::ConstraintIr::Unique { name, fields, .. }] => (name.clone(), fields.clone()),
        other => panic!("{other:?}"),
    }).collect();
    assert_eq!(unique[0], (None, vec!["user_id".to_string(), "role_id".to_string()]));
    assert_eq!(unique[1], (None, vec!["admin_id".to_string(), "role_id".to_string()]));
    assert_eq!(through_of(get(&ir, "User"), "roles"), ("UserRole".into(), "user_id".into(), "role_id".into()));
    assert_eq!(through_of(get(&ir, "Admin"), "roles"), ("AdminRole".into(), "admin_id".into(), "role_id".into()));
    let role = get(&ir, "Role");
    assert_eq!(names(role), ["id", "name"]);
    assert_eq!(through_of(role, "user_roles"), ("UserRole".into(), "role_id".into(), "user_id".into()));
    assert_eq!(through_of(role, "admin_roles"), ("AdminRole".into(), "role_id".into(), "admin_id".into()));
    let back = role.relations.iter().find(|r| r.name == "user_roles").unwrap();
    assert_eq!((back.target.as_str(), back.from.as_slice(), back.to.as_slice(), back.foreign_key), ("User", &["id".to_string()][..], &["id".to_string()][..], false));
    assert!(ir.models.iter().flat_map(|m| &m.relations).all(|r| r.label.is_none() && r.join_label.is_none()));
    let storage = ir.behavior.storage.as_ref().unwrap();
    assert!(["UserRole", "AdminRole"].iter().all(|n| storage.models.iter().any(|m| &m.name == n)));
}

#[test]
fn a_generated_join_model_name_that_exists_is_an_error() {
    let mut ir = user_roles();
    ir.models.push(serde_json::from_value(model("UserRole", vec![key("id")])).unwrap());
    let message = rejected(ir);
    assert!(message.contains("the join model UserRole already exists"), "{message}");
}

#[test]
fn a_plain_model_list_relation_stays_one_to_many_with_a_nullable_key() {
    let mut ir = user_roles();
    ir.models[0].relations[0].join_label = None;
    ir.models[0].relations[0].label = Some("{model}_roles".into());
    lower(&mut ir).unwrap();
    assert!(ir.models.iter().all(|m| m.name != "UserRole"));
    let role = get(&ir, "Role");
    let mut cols = names(role);
    cols.sort();
    assert_eq!(cols, ["admin_roles_id", "id", "name", "user_roles_id"]);
    assert!(role.fields.iter().filter(|f| f.name.ends_with("_roles_id")).all(|f| f.nullable));
    assert!(ir.models.iter().flat_map(|m| &m.relations).all(|r| r.through.is_none()));
}

fn string_key(name: &str) -> Value {
    json!({"name": name, "column": name, "type": "string", "primary_key": true})
}

#[test]
fn a_join_model_follows_a_child_key_type() {
    let mut ir = user_roles();
    ir.models[1].fields.insert(0, serde_json::from_value(string_key("id")).unwrap());
    lower(&mut ir).unwrap();
    let join = get(&ir, "UserRole");
    assert_eq!(join.fields[1].ty, get(&ir, "User").fields.iter().find(|f| f.name == "id").unwrap().ty);
    assert_ne!(join.fields[1].ty, join.fields[2].ty);
}

#[test]
fn a_join_target_that_takes_its_key_from_a_base_still_gets_its_join_model() {
    let mut ir = user_roles();
    // `Admin` sorts before `Role`, so `Admin.roles` is resolved before `Role` has merged its key.
    ir.models.iter_mut().find(|m| m.name == "Role").unwrap().fields.retain(|f| !f.primary_key);
    ir.models.push(serde_json::from_value(model("Identified", vec![key("id")])).unwrap());
    ir.behavior.declarations.push(serde_json::from_value(declare("inherit.abstract", "Identified", vec![])).unwrap());
    ir.behavior.declarations.push(serde_json::from_value(declare("inherit.extends", "Role", vec![json!("Identified")])).unwrap());
    lower(&mut ir).unwrap();
    assert_eq!(names(get(&ir, "AdminRole")), ["id", "admin_id", "role_id"]);
    assert_eq!(names(get(&ir, "Role")), ["id", "name"]);
}

/// `User` extends `AbstractUser` (join model `UserRole`); `MyUser` overrides `User`.
fn overridden_user_roles() -> SchemaIr {
    let mut ir = user_roles();
    ir.models.retain(|m| m.name != "Admin");
    ir.models.push(serde_json::from_value(model("MyUser", vec![field("nick", "string")])).unwrap());
    ir.behavior.declarations.retain(|d| d.model != "Admin");
    ir.behavior.declarations.push(serde_json::from_value(declare("inherit.override", "MyUser", vec![json!("User")])).unwrap());
    ir
}

#[test]
fn an_override_keeps_the_join_model_and_points_it_to_the_child() {
    let mut ir = overridden_user_roles();
    lower(&mut ir).unwrap();
    let mut all: Vec<_> = ir.models.iter().map(|m| m.name.as_str()).collect();
    all.sort();
    assert_eq!(all, ["MyUser", "Role", "UserRole"]);
    let join = get(&ir, "UserRole");
    assert_eq!(join.table, "user_roles");
    assert_eq!(join.relations.iter().map(|r| r.target.as_str()).collect::<Vec<_>>(), ["MyUser", "Role"]);
    assert_eq!(through_of(get(&ir, "MyUser"), "roles"), ("UserRole".into(), "user_id".into(), "role_id".into()));
    let back = get(&ir, "Role").relations.iter().find(|r| r.name == "user_roles").unwrap().clone();
    assert_eq!((back.target.as_str(), back.through.unwrap().model.as_str().to_owned()), ("MyUser", "UserRole".to_owned()));
}

#[test]
fn a_hand_written_through_relation_survives_an_override() {
    let mut user = model("User", vec![key("id")]);
    let mut roles = relation("roles", "many", "Role", "id", "id", false);
    roles["through"] = json!({"model": "UserRole", "source": "user_id", "target": "role_id"});
    user["relations"] = json!([roles]);
    let mut join = model("UserRole", vec![key("id"), field("user_id", "int"), field("role_id", "int")]);
    join["relations"] = json!([relation("user", "one", "User", "user_id", "id", true), relation("role", "one", "Role", "role_id", "id", true)]);
    let mut role = model("Role", vec![key("id")]);
    let mut users = relation("users", "many", "User", "id", "id", false);
    users["through"] = json!({"model": "UserRole", "source": "role_id", "target": "user_id"});
    role["relations"] = json!([users]);
    let mut ir = schema(
        vec![user, role, join, model("MyUser", vec![field("nick", "string")])],
        vec![declare("inherit.override", "MyUser", vec![json!("User")])],
    );
    lower(&mut ir).unwrap();
    assert_eq!(get(&ir, "UserRole").relations[0].target, "MyUser");
    assert_eq!(through_of(get(&ir, "MyUser"), "roles").0, "UserRole");
    assert_eq!(get(&ir, "Role").relations[0].target, "MyUser");
}

#[test]
fn an_override_cannot_change_the_key_type() {
    let mut ir = parent();
    ir.models[2].fields.insert(0, serde_json::from_value(string_key("id")).unwrap());
    let message = rejected(ir);
    assert!(message.contains("`MyUser.id`: an override cannot change the key type of `User`; incoming foreign keys keep it"), "{message}");
    let mut same = parent();
    same.models[2].fields.insert(0, serde_json::from_value(key("id")).unwrap());
    lower(&mut same).unwrap();
}

#[test]
fn an_override_cannot_change_the_key_length() {
    let sized = |length: u32| {
        let mut f = string_key("id");
        f["max_length"] = json!(length);
        serde_json::from_value(f).unwrap()
    };
    let mut ir = parent();
    ir.models[0].fields[0] = sized(36);
    ir.models[2].fields.insert(0, sized(64));
    let message = rejected(ir);
    assert!(message.contains("`MyUser.id`: an override cannot change the key type of `User`; incoming foreign keys keep it"), "{message}");
    let mut same = parent();
    same.models[0].fields[0] = sized(36);
    same.models[2].fields.insert(0, sized(36));
    lower(&mut same).unwrap();
}

#[test]
fn a_child_may_redeclare_the_key_and_change_its_type() {
    let mut ir = user_roles();
    ir.models[1].fields.insert(0, serde_json::from_value(string_key("id")).unwrap());
    lower(&mut ir).unwrap();
    let user = get(&ir, "User");
    assert_eq!(user.fields.iter().filter(|f| f.primary_key).count(), 1);
    let mut cols = names(user);
    cols.sort();
    assert_eq!(cols, ["email", "id"]);
}

#[test]
fn a_key_on_another_field_says_to_redeclare_the_base_key() {
    let mut ir = user_roles();
    ir.models[1].fields.insert(0, serde_json::from_value(string_key("uuid")).unwrap());
    let message = rejected(ir);
    assert!(message.contains("differs from the one of User (uuid)") && message.contains("redeclare `id`"), "{message}");
}

#[test]
fn a_lone_prefixed_abstract_model_is_not_mapped() {
    let mut ir = stamped();
    ir.models[0].table = "base_stamped".into();
    lower(&mut ir).unwrap();
}

#[test]
fn an_explicit_map_equal_to_the_default_is_still_rejected_on_an_abstract() {
    let mut ir = stamped();
    ir.models[0].mapped = true;
    assert!(rejected(ir).contains("@@map"));
}

#[test]
fn an_override_map_equal_to_its_prefixed_default_is_preserved() {
    let mut ir = parent();
    ir.models[0].table = "app_user".into();
    ir.models[2].table = "app_myuser".into();
    ir.models[2].mapped = true;
    lower(&mut ir).unwrap();
    assert_eq!(get(&ir, "MyUser").table, "app_myuser");
    assert_eq!(get(&ir, "MyUser").renamed_from.as_deref(), Some("app_user"));
}

#[test]
fn an_explicit_primary_key_name_on_an_abstract_with_two_children_is_rejected() {
    let mut ir = stamped();
    ir.models[0].pk_name = Some("shared_pkey".into());
    let message = rejected(ir);
    assert!(message.contains("shared_pkey") && message.contains("two or more children"), "{message}");
}

#[test]
fn an_explicit_primary_key_name_with_one_child_is_inherited() {
    let mut ir = stamped();
    ir.models.pop();
    ir.behavior.declarations.pop();
    ir.models[0].pk_name = Some("shared_pkey".into());
    ir.models[0].fields.push(serde_json::from_value(key("id")).unwrap());
    ir.models[1].fields.clear();
    lower(&mut ir).unwrap();
    assert_eq!(get(&ir, "Post").pk_name.as_deref(), Some("shared_pkey"));
}
