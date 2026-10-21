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
fn mixed_own_and_inherited_class_names_are_rejected() {
    let mut base = model("Base", vec![field("a", "int")]);
    base["query_set"] = json!("BaseQuery");
    let mut item = model("Item", vec![key("id")]);
    item["model_methods"] = json!("ItemMethods");
    let ir = schema(vec![base, item], vec![declare("inherit.abstract", "Base", vec![]), declare("inherit.extends", "Item", vec![json!("Base")])]);
    assert!(rejected(ir).contains("must both come from the bases or both be declared on Item"));
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
