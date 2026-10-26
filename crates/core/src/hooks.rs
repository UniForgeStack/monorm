//! The change-set hooks of a model: what runs on a value before it goes into a write.
//! Two producers fill a `HookPlan`: a library that `define()` opens (`HookRef::Abi`) and the functions
//! that the artifact compiles in (`HookRef::Static`). The engine runs both through one dispatch.
use crate::loader::State;
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

#[derive(Clone)]
pub enum HookRef {
    /// An export of a loaded library, with the state of its `init`.
    Abi { state: Arc<State>, export: String, reads_context: bool },
    Static(StaticHook),
}

impl HookRef {
    pub fn reads_context(&self) -> bool { matches!(self, Self::Abi { reads_context: true, .. }) }

    /// Rewrites `value` in place. A library hook gets it as the one argument `value`.
    pub fn rewrite(&self, value: &mut Value, context: &Context) -> Result<(), String> {
        match self {
            Self::Static(StaticHook::Field(f)) => f(value),
            Self::Static(_) => Err("a record or computed function cannot rewrite a field".into()),
            Self::Abi { state, export, .. } => {
                let arg = Arg { name: VALUE.to_owned(), value: std::mem::replace(value, Value::Null) };
                *value = state.call(export, std::slice::from_ref(&arg), context)?;
                Ok(())
            }
        }
    }

    /// Checks `value` and leaves it as it is.
    pub fn check(&self, value: &Value, context: &Context) -> Result<(), String> {
        match self {
            Self::Static(StaticHook::Field(f)) => f(&mut value.clone()),
            Self::Static(_) => Err("a record or computed function cannot check a field".into()),
            Self::Abi { state, export, .. } => {
                let arg = Arg { name: VALUE.to_owned(), value: value.clone() };
                state.call(export, std::slice::from_ref(&arg), context).map(drop)
            }
        }
    }

    /// Checks a record: one argument for each dependency.
    pub fn check_record(&self, args: &[Arg], context: &Context) -> Result<(), String> {
        match self {
            Self::Static(StaticHook::Record(f)) => f(args),
            Self::Static(_) => Err("a field or computed function cannot check a record".into()),
            Self::Abi { state, export, .. } => state.call(export, args, context).map(drop),
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
    /// Positions of fields that must be supplied on insert, and cannot be a database expression.
    pub required: Vec<usize>,
    /// The model rejects an upsert, because a conflict outcome skips the hooks.
    pub no_upsert: bool,
}

impl HookPlan {
    /// Fills `required` and `no_upsert` from the hooks; `None` for a plan without any change-set hook.
    pub fn finish(mut self) -> Option<Box<HookPlan>> {
        if self.transforms.is_empty() && self.validators.is_empty() && self.records.is_empty() { return None; }
        let mut required: Vec<usize> = self.transforms.iter().chain(&self.validators).map(|(p, _)| *p).chain(self.records.iter().flat_map(|r| r.positions.iter().copied())).collect();
        required.sort_unstable();
        required.dedup();
        self.required = required;
        self.no_upsert = true;
        Some(Box::new(self))
    }

    /// True when a transform or validator watches `position`.
    pub fn watches(&self, position: usize) -> bool {
        self.transforms.iter().chain(&self.validators).any(|(p, _)| *p == position)
    }
}

/// Opens the library of each package that has change-set bindings (points 1 to 4), calls its `init` with
/// `extension_data[id]`, and resolves each binding into the `HookPlan` of its model. `packages` maps an extension id
/// to the directory of its installed package. Any failure fails `define()`.
pub fn bind(models: &mut [Model], index: &HashMap<String, usize>, r: &Requirements, packages: &BTreeMap<String, PathBuf>) -> Result<(), String> {
    let mut states: BTreeMap<&str, (Arc<State>, Vec<String>)> = BTreeMap::new();
    let mut plans: BTreeMap<usize, HookPlan> = BTreeMap::new();
    for binding in r.hooks.iter().filter(|b| (1..=3).contains(&b.point) && r.packages.contains_key(&b.source)) {
        let id = binding.source.as_str();
        if !states.contains_key(id) {
            let dir = packages.get(id).ok_or_else(|| format!("extension {id}: the schema binds its hooks, but no package directory was given; install the package and pass it to `define()`"))?;
            let library = crate::loader::open_package(id, dir)?;
            let names: Vec<String> = library.hook_names()?.into_iter().map(str::to_owned).collect();
            let data = r.extension_data.get(id).unwrap_or(&serde_json::Value::Null);
            let state = library.init(data)?;
            states.insert(id, (Arc::new(state), names));
        }
        let (state, names) = &states[id];
        let at = match &binding.field { Some(f) => format!("{}.{f}", binding.model), None => binding.model.clone() };
        if !names.contains(&binding.export) {
            return Err(format!("{at}: extension {id} has no hook {}; fix the binding or rebuild the package", binding.export));
        }
        let m = *index.get(&binding.model).ok_or_else(|| format!("hook {} is bound to unknown model {}", binding.export, binding.model))?;
        let position = |name: &str| models[m].field_pos(name).map_err(|e| format!("{at}: hook {}: {e}", binding.export));
        let hook = HookRef::Abi { state: state.clone(), export: binding.export.clone(), reads_context: binding.reads_context };
        let plan = plans.entry(m).or_default();
        match (binding.point, &binding.field) {
            (1, Some(f)) => plan.transforms.push((position(f)?, hook)),
            (2, Some(f)) => plan.validators.push((position(f)?, hook)),
            (3, None) => {
                if binding.dependencies.is_empty() { return Err(format!("{at}: record validator {} names no dependencies", binding.export)); }
                let positions = binding.dependencies.iter().map(|d| position(d)).collect::<Result<Vec<_>, _>>()?;
                plan.records.push(RecordHook { positions, hook });
            }
            _ => return Err(format!("{at}: hook {} has point {} and a field that do not fit", binding.export, binding.point)),
        }
    }
    for (m, mut plan) in plans {
        // A model can already have static hooks of the artifact; both producers share one plan.
        if let Some(existing) = models[m].hooks.take() {
            let existing = *existing;
            plan.transforms.splice(0..0, existing.transforms);
            plan.validators.splice(0..0, existing.validators);
            plan.records.splice(0..0, existing.records);
        }
        models[m].hooks = plan.finish();
    }
    Ok(())
}
