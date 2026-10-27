//! The change-set hooks of a model: what runs on a value before it goes into a write.
//! Two producers fill a `HookPlan`: a library that `define()` opens (`HookRef::Abi`) and the functions
//! that the artifact compiles in (`HookRef::Static`). The engine runs both through one dispatch.
use crate::loader::{Hook, Library, State};
use crate::schema::Model;
use orm_contracts::abi::{Arg, Context, Value};
use orm_contracts::extension::Requirements;
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::Arc;

/// A function that the artifact compiles in.
#[derive(Clone, Copy)]
pub enum StaticHook {
    /// Checks and rewrites the value of one field.
    Field(fn(&mut Value) -> Result<(), String>),
    /// Checks the supplied values of a record; one argument for each dependency, in order.
    Record(fn(&[Arg]) -> Result<(), String>),
    /// Turns the string of the dependency into the computed string.
    Computed(fn(&str) -> Result<String, String>),
}

/// The name of the argument of a transform or a validator: one hook serves any field.
const VALUE: &str = "value";

/// A hook of a model, from a loaded library or from the artifact. Each method below runs one hook shape;
/// a hook of the wrong kind for the method is an error, not a silent skip.
#[derive(Clone)]
pub enum HookRef {
    /// An export of a loaded library, resolved once by name, with the state of its `init`.
    Abi { state: Arc<State>, hook: Hook, reads_context: bool },
    Static(StaticHook),
}

impl HookRef {
    pub fn reads_context(&self) -> bool { matches!(self, Self::Abi { reads_context: true, .. }) }

    /// Rewrites `value` in place. A library hook gets it as the one argument `value`.
    pub fn rewrite(&self, value: &mut Value, context: &Context) -> Result<(), String> {
        match self {
            Self::Static(StaticHook::Field(f)) => f(value),
            Self::Static(_) => Err("a record or computed function cannot rewrite a field".into()),
            Self::Abi { state, hook, .. } => {
                let arg = Arg { name: VALUE.to_owned(), value: std::mem::replace(value, Value::Null) };
                *value = state.call_hook(hook, std::slice::from_ref(&arg), context)?;
                Ok(())
            }
        }
    }

    /// Checks `value` and leaves it as it is.
    pub fn check(&self, value: &Value, context: &Context) -> Result<(), String> {
        match self {
            Self::Static(StaticHook::Field(f)) => f(&mut value.clone()),
            Self::Static(_) => Err("a record or computed function cannot check a field".into()),
            Self::Abi { state, hook, .. } => {
                let arg = Arg { name: VALUE.to_owned(), value: value.clone() };
                state.call_hook(hook, std::slice::from_ref(&arg), context).map(drop)
            }
        }
    }

    /// The value of a write assignment (point 4); the hook takes no argument.
    pub fn produce(&self, context: &Context) -> Result<Value, String> {
        match self {
            Self::Static(_) => Err("a static function cannot assign a value".into()),
            Self::Abi { state, hook, .. } => state.call_hook(hook, &[], context),
        }
    }

    /// The value of a computed field (point 5): one argument for each dependency, in order.
    pub fn compute(&self, args: &[Arg], context: &Context) -> Result<Value, String> {
        match self {
            Self::Static(_) => Err("a static function cannot compute a field here".into()),
            Self::Abi { state, hook, .. } => state.call_hook(hook, args, context),
        }
    }

    /// The query IR JSON of an expression hook (point 6 or 7); the hook takes no argument.
    pub fn expression(&self, context: &Context) -> Result<serde_json::Value, String> {
        match self {
            Self::Static(_) => Err("a static function cannot give an expression".into()),
            Self::Abi { state, hook, .. } => match state.call_hook(hook, &[], context)? {
                Value::Json(text) => serde_json::from_str(&text).map_err(|e| format!("hook {} returned json that does not parse: {e}", hook.name())),
                other => Err(format!("hook {} must return json (query IR), not {other:?}", hook.name())),
            },
        }
    }

    /// Checks a record: one argument for each dependency.
    pub fn check_record(&self, args: &[Arg], context: &Context) -> Result<(), String> {
        match self {
            Self::Static(StaticHook::Record(f)) => f(args),
            Self::Static(_) => Err("a field or computed function cannot check a record".into()),
            Self::Abi { state, hook, .. } => state.call_hook(hook, args, context).map(drop),
        }
    }
}

/// A record validator and the field positions it reads.
pub struct RecordHook {
    pub positions: Vec<usize>,
    pub hook: HookRef,
}

/// The hooks of one model. A model without any hook has no plan (`Model.hooks` is `None`).
#[derive(Default)]
pub struct HookPlan {
    /// By field position. A transform runs before the validators of the same position.
    pub transforms: Vec<(usize, HookRef)>,
    pub validators: Vec<(usize, HookRef)>,
    pub records: Vec<RecordHook>,
    /// The hook that gives the value of a field on update, by field position (point 4).
    pub assign: Vec<(usize, HookRef)>,
    /// The hook of each computed field (point 5): the field position, the hook and the positions of its dependencies.
    pub computed: Vec<(usize, HookRef, Vec<usize>)>,
    /// The hook that gives the expression of a field on update (point 6), by field position.
    pub expr_assign: Vec<(usize, HookRef)>,
    /// The hook that gives the expression of an omitted insert value (point 7), by field position.
    pub expr_default: Vec<(usize, HookRef)>,
    /// Positions that a transform or a validator watches, sorted.
    pub watched: Vec<usize>,
    /// Positions of fields that must be supplied on insert, and cannot be a database expression.
    pub required: Vec<usize>,
    /// The model rejects an upsert, because a conflict outcome skips the hooks.
    pub no_upsert: bool,
}

impl HookPlan {
    /// Fills `watched`, `required` and `no_upsert` from the hooks; `None` for a plan without any hook.
    pub fn finish(mut self) -> Option<Box<HookPlan>> {
        let change_set = self.changes_set();
        if !change_set && self.assign.is_empty() && self.computed.is_empty() && self.expr_assign.is_empty() && self.expr_default.is_empty() { return None; }
        let mut watched: Vec<usize> = self.transforms.iter().chain(&self.validators).map(|(p, _)| *p).collect();
        watched.sort_unstable();
        watched.dedup();
        let mut required: Vec<usize> = watched.iter().copied().chain(self.records.iter().flat_map(|r| r.positions.iter().copied())).collect();
        required.sort_unstable();
        required.dedup();
        self.watched = watched;
        self.required = required;
        self.no_upsert = change_set;
        Some(Box::new(self))
    }

    /// True when a transform, a validator or a record rule runs on the change set of a write (hooks 1 to 3).
    pub fn changes_set(&self) -> bool { !(self.transforms.is_empty() && self.validators.is_empty() && self.records.is_empty()) }

    /// True when a hook of the plan reads the request context.
    pub fn reads_context(&self) -> bool {
        self.transforms.iter().chain(&self.validators).chain(&self.assign).chain(&self.expr_assign).chain(&self.expr_default).any(|(_, h)| h.reads_context())
            || self.records.iter().any(|r| r.hook.reads_context())
            || self.computed.iter().any(|(_, h, _)| h.reads_context())
    }

    /// The hook that assigns `position`.
    pub fn assigner(&self, position: usize) -> Option<&HookRef> { self.assign.iter().find(|(p, _)| *p == position).map(|(_, h)| h) }

    /// The expression hook of a write assignment (point 6) at `position`.
    pub fn expr_assigner(&self, position: usize) -> Option<&HookRef> { self.expr_assign.iter().find(|(p, _)| *p == position).map(|(_, h)| h) }

    /// The expression hook of an insert default (point 7) at `position`.
    pub fn expr_defaulter(&self, position: usize) -> Option<&HookRef> { self.expr_default.iter().find(|(p, _)| *p == position).map(|(_, h)| h) }

    /// The hook of the computed field at `position` and the positions of its dependencies.
    pub fn computed_at(&self, position: usize) -> Option<(&HookRef, &[usize])> {
        self.computed.iter().find(|(p, _, _)| *p == position).map(|(_, h, d)| (h, d.as_slice()))
    }

    /// True when a transform or validator watches `position`.
    pub fn watches(&self, position: usize) -> bool { self.watched.binary_search(&position).is_ok() }
}

/// A library that `bind` opened, with the state of its `init`.
struct Opened { library: Library, state: Arc<State> }

/// Opens the library of each package that has hook bindings (points 1 to 7), calls its `init` with
/// `extension_data[id]`, and resolves each binding into the `HookPlan` of its model. `packages` maps an extension id
/// to the directory of its installed package. Any failure fails `define()`.
pub fn bind(models: &mut [Model], index: &HashMap<String, usize>, r: &Requirements, packages: &BTreeMap<String, PathBuf>) -> Result<(), String> {
    let mut opened: BTreeMap<&str, Opened> = BTreeMap::new();
    let mut plans: BTreeMap<usize, HookPlan> = BTreeMap::new();
    for binding in r.hooks.iter().filter(|b| (1..=7).contains(&b.point)) {
        let id = binding.source.as_str();
        let at = match &binding.field { Some(f) => format!("{}.{f}", binding.model), None => binding.model.clone() };
        if !r.packages.contains_key(id) {
            return Err(format!("{at}: hook {} names extension {id}, which did not run as a library for this schema; regenerate the schema", binding.export));
        }
        if !opened.contains_key(id) {
            let dir = packages.get(id).ok_or_else(|| format!("extension {id}: the schema binds its hooks, but no package directory was given; install the package and pass it to `define()`"))?;
            let library = crate::loader::open_package(id, dir)?;
            let data = r.extension_data.get(id).unwrap_or(&serde_json::Value::Null);
            let state = Arc::new(library.init(data)?);
            opened.insert(id, Opened { library, state });
        }
        let Opened { library, state } = &opened[id];
        let resolved = state.hook(&binding.export).ok_or_else(|| format!("{at}: extension {id} has no hook {}; fix the binding or rebuild the package", binding.export))?;
        let m = *index.get(&binding.model).ok_or_else(|| format!("hook {} is bound to unknown model {}", binding.export, binding.model))?;
        let position = |name: &str| models[m].field_pos(name).map_err(|e| format!("{at}: hook {}: {e}", binding.export));
        let hook = HookRef::Abi { state: state.clone(), hook: resolved, reads_context: binding.reads_context };
        let plan = plans.entry(m).or_default();
        match (binding.point, &binding.field) {
            (1, Some(f)) => plan.transforms.push((position(f)?, hook)),
            (2, Some(f)) => plan.validators.push((position(f)?, hook)),
            (4, Some(f)) => {
                let p = position(f)?;
                let field = &models[m].fields()[p];
                let output = library.export(&binding.export).and_then(|e| e.point.zip(e.output));
                match output {
                    Some((4, ty)) if orm_contracts::extension::same_value_type(ty, field.ty) => {}
                    Some((4, ty)) => return Err(format!("{at}: hook {} returns {ty:?}, but the field is {:?}; rebuild the package or fix the binding", binding.export, field.ty)),
                    _ => return Err(format!("{at}: hook {} is not a point-4 export with an output type; rebuild the package or fix the binding", binding.export)),
                }
                plan.assign.push((p, hook));
            }
            (5, Some(f)) => {
                let p = position(f)?;
                let field = &models[m].fields()[p];
                let output = library.export(&binding.export).and_then(|e| e.point.zip(e.output));
                match output {
                    Some((5, ty)) if orm_contracts::extension::same_value_type(ty, field.ty) => {}
                    Some((5, ty)) => return Err(format!("{at}: hook {} returns {ty:?}, but the field is {:?}; rebuild the package or fix the binding", binding.export, field.ty)),
                    _ => return Err(format!("{at}: hook {} is not a point-5 export with an output type; rebuild the package or fix the binding", binding.export)),
                }
                if binding.dependencies.is_empty() { return Err(format!("{at}: computed field hook {} names no dependencies", binding.export)); }
                let positions = binding.dependencies.iter().map(|d| position(d)).collect::<Result<Vec<_>, _>>()?;
                plan.computed.push((p, hook, positions));
            }
            (6 | 7, Some(f)) => {
                let p = position(f)?;
                let output = library.export(&binding.export).and_then(|e| e.point.zip(e.output));
                if output != Some((binding.point, orm_contracts::ir::ColType::Json)) {
                    return Err(format!("{at}: hook {} is not a point-{} export that returns json (query IR); rebuild the package or fix the binding", binding.export, binding.point));
                }
                if binding.point == 6 { plan.expr_assign.push((p, hook)) } else { plan.expr_default.push((p, hook)) }
            }
            (3, None) => {
                if binding.dependencies.is_empty() { return Err(format!("{at}: record validator {} names no dependencies", binding.export)); }
                let positions = binding.dependencies.iter().map(|d| position(d)).collect::<Result<Vec<_>, _>>()?;
                plan.records.push(RecordHook { positions, hook });
            }
            _ => return Err(format!("{at}: hook {} has point {} and a field that do not fit", binding.export, binding.point)),
        }
    }
    for (m, mut plan) in plans {
        for (p, _, deps) in &plan.computed {
            if let Some(d) = deps.iter().find(|d| plan.computed.iter().any(|(c, _, _)| c == *d)) {
                return Err(format!("{}.{}: a computed field depends on the computed field {}; it may read stored fields only", models[m].ir.name, models[m].fields()[*p].name, models[m].fields()[*d].name));
            }
        }
        // A model can already have static hooks of the artifact; both producers share one plan.
        if let Some(existing) = models[m].hooks.take() {
            let existing = *existing;
            plan.transforms.splice(0..0, existing.transforms);
            plan.validators.splice(0..0, existing.validators);
            plan.records.splice(0..0, existing.records);
            plan.computed.splice(0..0, existing.computed);
        }
        for (p, default) in &models[m].client_defaults {
            let hook = match default {
                crate::client_default::ClientDefault::Hook(_) => true,
                crate::client_default::ClientDefault::Expr(_) => false,
                _ => continue,
            };
            let field = &models[m].fields()[*p].name;
            if plan.watches(*p) { return Err(format!("{}.{field}: a field with an expression default takes no transform or validator", models[m].ir.name)); }
            if hook && plan.expr_defaulter(*p).is_none() {
                return Err(format!("{}.{field}: the expression default runs a hook, but no point-7 binding names it; bind it to the field", models[m].ir.name));
            }
        }
        models[m].hooks = plan.finish();
    }
    Ok(())
}
