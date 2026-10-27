// `pass-only` builds no hook, no state and no `init`.
#![cfg_attr(feature = "pass-only", allow(dead_code, unused_macros))]
use orm_contracts::{abi::{Arg, Context, Value}, extension::{capture_storage, Declaration, ModelView, ResultCheck, RowFilter}, ir::{ColType, FieldIr, SchemaIr}};
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
        orm_contracts::extension! {
            $($abi)*
            init = init;
            hooks {
                "outside.prefix" => prefix, "outside.audit" => audit, "outside.context" => context, "outside.drops" => drops,
                "outside.fail" => fail, "outside.panic" => explode, "outside.echo" => echo, "outside.add" => add,
            };
            ("acme.outside.tag", tag),
            ("acme.outside.fields", fields),
            ("acme.outside.view", view)
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
    use orm_contracts::abi::{self, Buffer, PassFn};
    use orm_contracts::extension::ContractVersion;

    #[unsafe(no_mangle)]
    pub extern "C" fn orm_ext_abi_version() -> u32 { ContractVersion::new(0, 1).to_bits() }

    /// # Safety
    /// Called by the ORM loader with readable ranges and a writable `out`.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn orm_ext_run_pass(id: *const u8, id_len: usize, ir: *const u8, ir_len: usize, out: *mut Buffer) -> i32 {
        let passes: &[(&str, PassFn)] = &[("acme.outside.tag", super::tag as PassFn), ("acme.outside.fields", super::fields as PassFn), ("acme.outside.view", super::view as PassFn)];
        // SAFETY: the loader passes the ranges and the `out` that `run_pass` documents.
        unsafe { abi::run_pass(passes, id, id_len, ir, ir_len, out) }
    }

    /// # Safety
    /// Called by the ORM loader with a buffer of this library.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn orm_ext_free(ptr: *mut u8, len: usize) {
        // SAFETY: the loader passes a buffer that this library returned, once.
        unsafe { abi::free(ptr, len) }
    }
}
