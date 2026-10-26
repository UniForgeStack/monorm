use orm_contracts::{abi::{Arg, Context, Value}, extension::Declaration, ir::{ColType, FieldIr, SchemaIr}};
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
            ("acme.outside.tag", tag)
        }
    };
}

#[cfg(not(any(feature = "abi-major", feature = "abi-newer")))]
export!();
#[cfg(feature = "abi-major")]
export!(abi = (1, 0););
#[cfg(all(feature = "abi-newer", not(feature = "abi-major")))]
export!(abi = (0, 3););
