//! The change-set hooks of a model: what runs on a value before it goes into a write.
//! Two producers fill a `HookPlan`: a library that `define()` opens (`HookRef::Abi`) and the functions
//! that the artifact compiles in (`HookRef::Static`). The engine runs both through one dispatch.
use crate::loader::State;
use orm_contracts::abi::{Arg, Context, Value};
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

#[derive(Clone)]
pub enum HookRef {
    /// An export of a loaded library, with the state of its `init`.
    Abi { state: Arc<State>, export: String, reads_context: bool },
    Static(StaticHook),
}

impl HookRef {
    pub fn reads_context(&self) -> bool { matches!(self, Self::Abi { reads_context: true, .. }) }

    /// Rewrites `value` in place. `name` is the field name that the hook sees.
    pub fn rewrite(&self, name: &str, value: &mut Value, context: &Context) -> Result<(), String> {
        match self {
            Self::Static(StaticHook::Field(f)) => f(value),
            Self::Static(_) => Err("a record or computed function cannot rewrite a field".into()),
            Self::Abi { state, export, .. } => {
                let arg = Arg { name: name.to_owned(), value: std::mem::replace(value, Value::Null) };
                *value = state.call(export, std::slice::from_ref(&arg), context)?;
                Ok(())
            }
        }
    }

    /// Checks `value` and leaves it as it is.
    pub fn check(&self, name: &str, value: &Value, context: &Context) -> Result<(), String> {
        match self {
            Self::Static(StaticHook::Field(f)) => f(&mut value.clone()),
            Self::Static(_) => Err("a record or computed function cannot check a field".into()),
            Self::Abi { state, export, .. } => {
                let arg = Arg { name: name.to_owned(), value: value.clone() };
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
