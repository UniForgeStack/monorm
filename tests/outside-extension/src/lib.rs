// `pass-only` builds no hook, no state and no `init`.
#![cfg_attr(feature = "pass-only", allow(dead_code, unused_macros))]
use monorm::{abi::{Arg, Context, Value}, extension::{capture_storage, Declaration, FieldStorage, ModelView, OwnerLink, ResultCheck, RowFilter}, ir::{ColType, FieldIr, ModelIr, OnDelete, RelKind, RelationIr, SchemaIr}};
use std::sync::atomic::{AtomicU64, Ordering};

const ATTRIBUTE: &str = "outside.tag";

fn tag(ir: &mut SchemaIr) -> Result<(), String> {
    let declarations: Vec<Declaration> = ir.behavior.declarations.iter().filter(|d| !d.lowered && d.attribute == ATTRIBUTE).cloned().collect();
    for d in declarations {
        let label = d.positional.first().and_then(|v| v.as_str()).filter(|s| !s.is_empty())
            .ok_or_else(|| format!("@@{ATTRIBUTE} on {}: the label must be a non-empty string", d.model))?;
        // Static passes may already have captured storage before this library runs.
        let models = ir.models.iter_mut().chain(ir.behavior.storage.iter_mut().flat_map(|s| s.models.iter_mut()));
        let mut found = false;
        for model in models.filter(|m| m.name == d.model) {
            found = true;
            if model.fields.iter().any(|f| f.name == "tag") { return Err(format!("{} already has a field tag", d.model)); }
            model.fields.push(FieldIr { nullable: false, default: Some(label.into()), ..FieldIr::plain("tag", ColType::Text) });
        }
        if !found { return Err(format!("unknown model {}", d.model)); }
    }
    Ok(())
}

fn link(name: &str, target: &str, key: &str, foreign_key: bool) -> RelationIr {
    RelationIr {
        name: name.into(), kind: RelKind::One, target: target.into(), from: vec![key.into()], to: vec![key.into()],
        foreign_key, on_delete: foreign_key.then_some(OnDelete::Cascade), on_update: None, deferrable: None, fk_name: None, through: None, label: None, join_label: None,
    }
}

/// `@@outside.base(parent: "A", parentRef: "a", childRef: "b")` stores the fields of `A` in the table of `A` and the other fields in its own table; both share one key.
fn base(ir: &mut SchemaIr) -> Result<(), String> {
    let declarations: Vec<Declaration> = ir.behavior.declarations.iter().filter(|d| !d.lowered && d.attribute == "outside.base").cloned().collect();
    if declarations.is_empty() { return Ok(()); }
    capture_storage(ir)?;
    for d in declarations {
        let argument = |key: &str| d.arguments.get(key).and_then(|v| v.as_str()).filter(|s| !s.is_empty()).map(str::to_owned).ok_or_else(|| format!("@@outside.base on {}: {key} must be a non-empty string", d.model));
        let (parent_name, parent_ref, child_ref) = (argument("parent")?, argument("parentRef")?, argument("childRef")?);
        let at = |ir: &SchemaIr, name: &str| ir.models.iter().position(|m| m.name == name).ok_or_else(|| format!("unknown model {name}"));
        let (child_at, parent_at) = (at(ir, &d.model)?, at(ir, &parent_name)?);
        let parent: &ModelIr = &ir.models[parent_at];
        let keys: Vec<&FieldIr> = parent.fields.iter().filter(|f| f.primary_key).collect();
        let [root] = keys.as_slice() else { return Err(format!("{parent_name} needs one primary key")) };
        if ir.models[child_at].fields.iter().any(|f| f.primary_key) { return Err(format!("{} takes its key from {parent_name}", d.model)); }
        let mut key = (*root).clone();
        (key.auto_increment, key.default, key.default_now, key.default_sql, key.unique, key.index) = (false, None, false, None, false, false);
        let inherited: Vec<FieldIr> = parent.fields.iter().filter(|f| !f.primary_key).cloned().collect();
        for f in &inherited {
            let owner = ir.behavior.field_storage.iter().find(|s| s.model == parent_name && s.field == f.name).cloned();
            ir.behavior.field_storage.push(FieldStorage {
                model: d.model.clone(), field: f.name.clone(),
                owner: owner.as_ref().map_or_else(|| parent_name.clone(), |s| s.owner.clone()),
                column: owner.map_or_else(|| f.column.clone(), |s| s.column),
            });
        }
        let child = &mut ir.models[child_at];
        child.fields.insert(0, key.clone());
        child.fields.extend(inherited);
        child.relations.push(link(&parent_ref, &parent_name, &key.name, true));
        ir.models[parent_at].relations.push(link(&child_ref, &d.model, &key.name, false));
        let physical = &mut ir.behavior.storage.as_mut().ok_or("outside.base needs the captured storage")?.models;
        physical.iter_mut().find(|m| m.name == parent_name).ok_or("the parent has no physical owner")?.relations.push(link(&child_ref, &d.model, &key.name, false));
        let own = physical.iter_mut().find(|m| m.name == d.model).ok_or("the child has no physical owner")?;
        own.fields.insert(0, key.clone());
        own.relations.push(link(&parent_ref, &parent_name, &key.name, true));
        ir.behavior.owner_links.push(OwnerLink { child: d.model.clone(), parent: parent_name.clone(), child_key: key.name.clone(), parent_key: key.name });
    }
    Ok(())
}

/// `Money` fields become an amount and a currency column; `outside.note` declarations go into `extension_data`.
fn fields(ir: &mut SchemaIr) -> Result<(), String> {
    let declarations: Vec<Declaration> = ir.behavior.declarations.iter().filter(|d| !d.lowered && d.field.is_some()).cloned().collect();
    for d in declarations {
        let field = d.field.as_deref().unwrap_or_default();
        match d.attribute.as_str() {
            "outside.money" => {
                let models = ir.models.iter_mut().chain(ir.behavior.storage.iter_mut().flat_map(|s| s.models.iter_mut()));
                let mut found = false;
                for model in models.filter(|m| m.name == d.model) {
                    let at = model.fields.iter().position(|f| f.name == field).ok_or_else(|| format!("{}.{field}: the Money placeholder is missing", d.model))?;
                    let nullable = model.fields[at].nullable;
                    model.fields.splice(at..=at, [
                        FieldIr { nullable, ..FieldIr::plain(&format!("{field}_amount"), ColType::Int) },
                        FieldIr { nullable, ..FieldIr::plain(&format!("{field}_currency"), ColType::Text) },
                    ]);
                    found = true;
                }
                if !found { return Err(format!("unknown model {}", d.model)); }
            }
            "outside.note" => {
                let note = d.positional.first().and_then(|v| v.as_str()).ok_or_else(|| format!("{}.{field}: the note must be a string", d.model))?;
                let data = ir.behavior.extension_data.entry("acme.outside".into()).or_insert_with(|| serde_json::json!({}));
                let notes = data.as_object_mut().ok_or("the extension_data of acme.outside must be an object")?.entry("notes").or_insert_with(|| serde_json::json!({}));
                notes[format!("{}.{field}", d.model)] = note.into();
            }
            _ => {}
        }
    }
    Ok(())
}

const WRAPPED: &str = "outside.wrapped";
const WRAPPED_ADAPTER: &str = "acme.outside.wrapped.v1";

/// `@outside.wrapped("prefix")` adapts a Json field: the options of the adapter are the prefix.
fn wrapped(ir: &mut SchemaIr) -> Result<(), String> {
    use monorm::extension::FieldAdapter;
    let declarations: Vec<Declaration> = ir.behavior.declarations.iter().filter(|d| !d.lowered && d.attribute == WRAPPED).cloned().collect();
    for d in declarations {
        let field = d.field.clone().ok_or_else(|| format!("@{WRAPPED} on {}: it needs a field", d.model))?;
        let prefix = d.positional.first().and_then(|v| v.as_str()).ok_or_else(|| format!("{}.{field}: the prefix must be a string", d.model))?;
        ir.behavior.field_adapters.push(FieldAdapter { model: d.model, field, adapter: WRAPPED_ADAPTER.into(), options: serde_json::json!({"prefix": prefix}) });
    }
    monorm::extension::validate_field_adapters(ir)
}

/// The warning code of the view check.
const SHAPE_CODE: &str = "acme.outside.shape";

/// `@@outside.view(Source, "open")` makes the model a view of `Source` that sees the rows with `status = 'open'`.
/// Each field that the view declares non-null and the source stores as nullable gets a result check.
fn view(ir: &mut SchemaIr) -> Result<(), String> {
    let declarations: Vec<Declaration> = ir.behavior.declarations.iter().filter(|d| !d.lowered && d.attribute == "outside.view").cloned().collect();
    if !declarations.is_empty() { capture_storage(ir)?; }
    for d in declarations {
        let source = d.positional.first().and_then(|v| v.as_str()).ok_or_else(|| format!("@@outside.view on {}: the source must be a model name", d.model))?;
        let status = d.positional.get(1).and_then(|v| v.as_str()).ok_or_else(|| format!("@@outside.view on {}: the status must be a string", d.model))?;
        let stored = ir.models.iter().find(|m| m.name == source).ok_or_else(|| format!("@@outside.view on {}: unknown model {source}", d.model))?;
        if !stored.fields.iter().any(|f| f.name == "status") { return Err(format!("{source} has no field status")); }
        let (root, table) = (stored.name.clone(), stored.table.clone());
        let nullable: Vec<String> = stored.fields.iter().filter(|f| f.nullable).map(|f| f.name.clone()).collect();
        let model = ir.models.iter_mut().find(|m| m.name == d.model).ok_or_else(|| format!("unknown model {}", d.model))?;
        model.table = table;
        let checked: Vec<String> = model.fields.iter().filter(|f| !f.nullable && nullable.contains(&f.name)).map(|f| f.name.clone()).collect();
        ir.behavior.model_views.push(ModelView {
            model: d.model.clone(), source: source.into(), root,
            row_filter: Some(RowFilter { field: "status".into(), values: vec![status.into()] }), insert_table: None,
        });
        ir.behavior.result_checks.extend(checked.into_iter().map(|field| ResultCheck { model: d.model.clone(), field, non_null: true, allowed: None, code: SHAPE_CODE.into() }));
        // The view owns no table.
        if let Some(storage) = ir.behavior.storage.as_mut() { storage.models.retain(|m| m.name != d.model); }
    }
    Ok(())
}

const GUARD: &str = "outside.guard";
const SOURCE: &str = "acme.outside";

/// `@@outside.guard` binds the `acme.outside.*` hooks to a model that has the fields `text`, `start`, `end`, `shop_id` and `updated_by`:
/// trim and non-empty on `text`, `start < end`, the request `user` as `updated_by` on update, and the default filter `shop_id == scope.shop`.
/// A model that also has the text fields `first` and `last` gets the computed field `initials`.
/// Optional text fields get expression records: `slug` (`lower(text)` on update), `stamp` and `editor` (expression hooks on update),
/// `region` and `owner` (expression hooks as insert defaults), `secret` (sealed by hook 1, read back as the computed field `secret_plain`). `shop_id` and `updated_by` default to the request scope on insert.
/// `touched_by` takes the request `user` on update through an expression assignment that reads the request scope.
fn guard(ir: &mut SchemaIr) -> Result<(), String> {
    use monorm::{extension::{HookBinding, ReadDefault}, ir::ClientDefaultIr, writes::{AssignValue, UpsertDefault, WriteAssignment, WriteKind}};
    let declarations: Vec<Declaration> = ir.behavior.declarations.iter().filter(|d| !d.lowered && d.attribute == GUARD).cloned().collect();
    for d in declarations {
        let model = d.model.clone();
        let m = ir.models.iter().find(|m| m.name == model).ok_or_else(|| format!("unknown model {model}"))?;
        for name in ["text", "start", "end", "shop_id", "updated_by"] {
            if !m.fields.iter().any(|f| f.name == name) { return Err(format!("@@{GUARD} on {model}: the model needs a field {name}")); }
        }
        let has_initials = ["first", "last"].iter().all(|n| m.fields.iter().any(|f| f.name == *n));
        let has = |name: &str| m.fields.iter().any(|f| f.name == name);
        let (has_slug, has_stamp, has_editor, has_region, has_owner, has_secret) = (has("slug"), has("stamp"), has("editor"), has("region"), has("owner"), has("secret"));
        let has_touched = has("touched_by");
        let bind = |point: u8, field: Option<&str>, export: &str, dependencies: &[&str], reads_context: bool| HookBinding {
            source: SOURCE.into(), point, model: model.clone(), field: field.map(Into::into), export: export.into(),
            dependencies: dependencies.iter().map(|s| (*s).into()).collect(), reads_context,
        };
        ir.behavior.hooks.extend([
            bind(1, Some("text"), "acme.outside.trim", &[], false),
            bind(2, Some("text"), "acme.outside.nonempty", &[], false),
            bind(3, None, "acme.outside.ordered", &["start", "end"], false),
            bind(4, Some("updated_by"), "acme.outside.actor", &[], true),
        ]);
        // A model with the text fields `first` and `last` also gets the computed field `initials`.
        if has_initials {
            let models = ir.models.iter_mut().chain(ir.behavior.storage.iter_mut().flat_map(|s| s.models.iter_mut()));
            for m in models.filter(|m| m.name == model) { m.fields.push(FieldIr::plain("initials", ColType::Text)); }
            ir.behavior.result_fields.push(monorm::extension::ResultDeclaration { model: model.clone(), field: "initials".into(), dependencies: vec!["first".into(), "last".into()] });
            ir.behavior.hooks.push(bind(5, Some("initials"), "acme.outside.initials", &["first", "last"], false));
        }
        let assign = |field: &str, value: AssignValue| WriteAssignment {
            source: SOURCE.into(), model: model.clone(), field: field.into(), on: vec![WriteKind::Update], value, upsert: UpsertDefault::Drop,
        };
        if has_slug {
            let lower = serde_json::json!({"t": "func", "name": "lower", "args": [{"t": "col", "path": [], "name": "text"}]});
            ir.behavior.write_assignments.push(assign("slug", AssignValue::Expr(lower)));
        }
        for (present, field, export, reads_context) in [(has_stamp, "stamp", "acme.outside.label", false), (has_editor, "editor", "acme.outside.editor", true)] {
            if !present { continue; }
            ir.behavior.hooks.push(bind(6, Some(field), export, &[], reads_context));
            ir.behavior.write_assignments.push(assign(field, AssignValue::ExprHook { export: export.into() }));
        }
        let mut defaults = vec![("shop_id", ClientDefaultIr::Expr(serde_json::json!({"t": "scope", "name": "shop"}))), ("updated_by", ClientDefaultIr::Expr(serde_json::json!({"t": "scope", "name": "user"})))];
        for (present, field, export, reads_context) in [(has_region, "region", "acme.outside.region", false), (has_owner, "owner", "acme.outside.owner", true)] {
            if !present { continue; }
            ir.behavior.hooks.push(bind(7, Some(field), export, &[], reads_context));
            defaults.push((field, ClientDefaultIr::Hook { export: export.into() }));
        }
        let models = ir.models.iter_mut().chain(ir.behavior.storage.iter_mut().flat_map(|s| s.models.iter_mut()));
        for m in models.filter(|m| m.name == model) {
            for (field, default) in &defaults {
                if let Some(f) = m.fields.iter_mut().find(|f| f.name == *field) { f.client_default = Some(default.clone()); }
            }
        }
        if has_secret {
            let models = ir.models.iter_mut().chain(ir.behavior.storage.iter_mut().flat_map(|s| s.models.iter_mut()));
            for m in models.filter(|m| m.name == model) { m.fields.push(FieldIr::plain("secret_plain", ColType::Text)); }
            ir.behavior.result_fields.push(monorm::extension::ResultDeclaration { model: model.clone(), field: "secret_plain".into(), dependencies: vec!["secret".into()] });
            ir.behavior.hooks.extend([
                bind(1, Some("secret"), "acme.outside.seal", &[], false),
                bind(5, Some("secret_plain"), "acme.outside.unseal", &["secret"], false),
            ]);
        }
        ir.behavior.write_assignments.push(WriteAssignment {
            source: SOURCE.into(), model: model.clone(), field: "updated_by".into(), on: vec![WriteKind::Update],
            value: AssignValue::Hook { export: "acme.outside.actor".into() }, upsert: UpsertDefault::Drop,
        });
        // After the hook assignments: a request without a user fails on the hooks first.
        if has_touched {
            ir.behavior.write_assignments.push(assign("touched_by", AssignValue::Expr(serde_json::json!({"t": "scope", "name": "user"}))));
        }
        ir.behavior.read_defaults.push(ReadDefault {
            source: SOURCE.into(), name: "tenant".into(), model,
            filter: Some(serde_json::json!({"t": "cmp", "op": "eq", "l": {"t": "col", "path": [], "name": "shop_id"}, "r": {"t": "scope", "name": "shop"}})),
            ..ReadDefault::default()
        });
    }
    Ok(())
}

/// The count of states that `drop_state` has dropped, which `outside.drops` returns.
static DROPS: AtomicU64 = AtomicU64::new(0);

struct State { prefix: String }

impl Drop for State {
    fn drop(&mut self) { DROPS.fetch_add(1, Ordering::SeqCst); }
}

fn init(data: &serde_json::Value) -> Result<State, String> {
    let prefix = match data.get("prefix") {
        Some(serde_json::Value::String(prefix)) => prefix.clone(),
        None | Some(serde_json::Value::Null) => String::new(),
        Some(other) => return Err(format!("prefix must be a string, got {other}")),
    };
    if data.get("fail").is_some() { return Err("init asked to fail".into()); }
    if data.get("panic").is_some() { panic!("init asked to panic"); }
    Ok(State { prefix })
}

fn one<'a>(args: &'a [Arg], name: &str) -> Result<&'a Value, String> {
    args.iter().find(|a| a.name == name).map(|a| &a.value).ok_or_else(|| format!("missing argument {name}"))
}

fn text<'a>(args: &'a [Arg], name: &str) -> Result<&'a str, String> {
    match one(args, name)? { Value::Text(s) => Ok(s), other => Err(format!("{name} must be text, got {other:?}")) }
}

fn prefix(state: &State, args: &[Arg], _: &Context) -> Result<Value, String> { Ok(Value::Text(format!("{}{}", state.prefix, text(args, "value")?))) }
fn audit(_: &State, args: &[Arg], _: &Context) -> Result<Value, String> { text(args, "value").map(|_| Value::Null) }
fn context(_: &State, _: &[Arg], ctx: &Context) -> Result<Value, String> {
    let tenant = match ctx.get("tenant") { Some(Value::Text(t)) => serde_json::Value::String(t.clone()), _ => serde_json::Value::Null };
    Ok(Value::Json(serde_json::json!({"tenant": tenant, "n": ctx.values.len()}).to_string()))
}
fn trim(_: &State, args: &[Arg], _: &Context) -> Result<Value, String> { Ok(Value::Text(text(args, "value")?.trim().to_owned())) }
fn nonempty(_: &State, args: &[Arg], _: &Context) -> Result<Value, String> {
    if text(args, "value")?.is_empty() { Err("must not be empty".into()) } else { Ok(Value::Null) }
}
/// A record rule over the fields `start` and `end`.
fn ordered(_: &State, args: &[Arg], _: &Context) -> Result<Value, String> {
    match (one(args, "start")?, one(args, "end")?) {
        (Value::Int(s), Value::Int(e)) if s < e => Ok(Value::Null),
        (Value::Int(s), Value::Int(e)) => Err(format!("start {s} must be before end {e}")),
        (s, e) => Err(format!("start and end must be integers, got {s:?} and {e:?}")),
    }
}
/// A write value: the `user` of the request scope.
fn actor(_: &State, _: &[Arg], ctx: &Context) -> Result<Value, String> {
    match ctx.get("user") { Some(Value::Text(user)) => Ok(Value::Text(user.clone())), _ => Err("no user in the request scope".into()) }
}
/// A computed field: the capital letters of two text fields.
fn initials(_: &State, args: &[Arg], _: &Context) -> Result<Value, String> {
    let letter = |name| text(args, name).map(|s| s.chars().next().map(|c| c.to_uppercase().to_string()).unwrap_or_default());
    Ok(Value::Text(letter("first")? + &letter("last")?))
}
/// Expression hooks (points 6 and 7): each returns the query IR of one text value.
fn text_expr(value: &str) -> Value { Value::Json(serde_json::json!({"t": "text", "value": value}).to_string()) }
fn label(_: &State, _: &[Arg], _: &Context) -> Result<Value, String> { Ok(text_expr("guarded")) }
fn region(_: &State, _: &[Arg], _: &Context) -> Result<Value, String> { Ok(text_expr("eu")) }
fn by_user(_: &State, _: &[Arg], ctx: &Context) -> Result<Value, String> {
    match ctx.get("user") { Some(Value::Text(user)) => Ok(text_expr(&format!("by:{user}"))), _ => Err("no user in the request scope".into()) }
}
/// The stored form of `secret`: `enc:` and the reversed text. The reverse is not encryption; it makes the stored value differ from the plain one.
fn seal(_: &State, args: &[Arg], _: &Context) -> Result<Value, String> { Ok(Value::Text(format!("enc:{}", text(args, "value")?.chars().rev().collect::<String>()))) }
fn unseal(_: &State, args: &[Arg], _: &Context) -> Result<Value, String> {
    let sealed = text(args, "secret")?;
    let body = sealed.strip_prefix("enc:").ok_or_else(|| format!("{sealed:?} is not sealed"))?;
    Ok(Value::Text(body.chars().rev().collect()))
}
fn drops(_: &State, _: &[Arg], _: &Context) -> Result<Value, String> { Ok(Value::Int(DROPS.load(Ordering::SeqCst) as i64)) }
fn fail(_: &State, _: &[Arg], _: &Context) -> Result<Value, String> { Err("the hook failed".into()) }
fn explode(_: &State, _: &[Arg], _: &Context) -> Result<Value, String> { panic!("the hook panicked") }
/// One value of each tag goes in and comes back: the round trip of the typed value.
fn echo(_: &State, args: &[Arg], _: &Context) -> Result<Value, String> { Ok(one(args, "value")?.clone()) }
fn add(_: &State, args: &[Arg], _: &Context) -> Result<Value, String> {
    match (one(args, "a")?, one(args, "b")?) {
        (Value::Int(a), Value::Int(b)) => Ok(Value::Int(a + b)),
        (Value::Float(a), Value::Float(b)) => Ok(Value::Float(a + b)),
        (a, b) => Err(format!("cannot add {a:?} and {b:?}")),
    }
}

macro_rules! export {
    ($($abi:tt)*) => {
        monorm::extension! {
            $($abi)*
            init = init;
            hooks {
                "outside.prefix" => prefix, "outside.audit" => audit, "outside.context" => context, "outside.drops" => drops,
                "acme.outside.trim" => trim, "acme.outside.nonempty" => nonempty, "acme.outside.ordered" => ordered, "acme.outside.actor" => actor, "acme.outside.initials" => initials,
                "acme.outside.seal" => seal, "acme.outside.unseal" => unseal, "acme.outside.label" => label, "acme.outside.region" => region, "acme.outside.editor" => by_user, "acme.outside.owner" => by_user,
                "outside.fail" => fail, "outside.panic" => explode, "outside.echo" => echo, "outside.add" => add,
            };
            ("acme.outside.base", base),
            ("acme.outside.tag", tag),
            ("acme.outside.fields", fields),
            ("acme.outside.view", view),
            ("acme.outside.guard", guard),
            ("acme.outside.wrapped", wrapped)
        }
    };
}

#[cfg(not(any(feature = "abi-major", feature = "abi-newer", feature = "pass-only")))]
export!();
#[cfg(feature = "abi-major")]
export!(abi = (1, 0););
#[cfg(all(feature = "abi-newer", not(feature = "abi-major")))]
export!(abi = (0, 3););

/// The three symbols of a library from slice 1, written by hand: `extension!` always writes all six.
#[cfg(feature = "pass-only")]
mod slice_one {
    use monorm::abi::{self, Buffer, PassFn};
    use monorm::extension::ContractVersion;

    #[unsafe(no_mangle)]
    pub extern "C" fn monorm_ext_abi_version() -> u32 { ContractVersion::new(0, 1).to_bits() }

    /// # Safety
    /// Called by the ORM loader with readable ranges and a writable `out`.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn monorm_ext_run_pass(id: *const u8, id_len: usize, ir: *const u8, ir_len: usize, out: *mut Buffer) -> i32 {
        let passes: &[(&str, PassFn)] = &[("acme.outside.base", super::base as PassFn), ("acme.outside.tag", super::tag as PassFn), ("acme.outside.fields", super::fields as PassFn), ("acme.outside.view", super::view as PassFn), ("acme.outside.guard", super::guard as PassFn), ("acme.outside.wrapped", super::wrapped as PassFn)];
        // SAFETY: the loader passes the ranges and the `out` that `run_pass` documents.
        unsafe { abi::run_pass(passes, id, id_len, ir, ir_len, out) }
    }

    /// # Safety
    /// Called by the ORM loader with a buffer of this library.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn monorm_ext_free(ptr: *mut u8, len: usize) {
        // SAFETY: the loader passes a buffer that this library returned, once.
        unsafe { abi::free(ptr, len) }
    }
}
