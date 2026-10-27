//! Validated, indexed form of [`SchemaIr`] used by the planner.

use std::collections::HashMap;

use crate::ir::{ColType, EnumIr, EnumStorage, ExtensionIr, FieldIr, FunctionIr, ModelIr, RelKind, RelationIr, SchemaIr};

pub type Result<T> = std::result::Result<T, String>;

/// Static facts about a model's read defaults, set when the schema is prepared.
#[derive(Default, Clone, Debug, PartialEq)]
pub struct ReadFlags {
    /// Some default carries a filter, so a to-one reference to the model can be absent.
    pub filtered: bool,
    /// The default field list of the model, if one is set.
    pub fields: Option<Vec<String>>,
    /// The relation paths a default read loads with it.
    pub related: Vec<Vec<String>>,
}

pub struct Model {
    pub ir: ModelIr,
    /// The first primary key field's position.
    pub pk: usize,
    /// Every primary key field's position, in key order.
    pub key: Vec<usize>,
    /// Prepared `@client_default`s by field position.
    pub client_defaults: Vec<(usize, crate::client_default::ClientDefault)>,
    pub file_fields: Vec<crate::file_storage::PreparedFileField>,
    pub read_defaults: crate::selection::PreparedDefaults,
    /// What read defaults mean for this model, for codegen and bindings; empty without any default.
    pub read_flags: ReadFlags,
    /// Columns the ORM assigns on writes, from the neutral write records.
    pub assignments: Vec<crate::writes::PreparedAssignment>,
    /// The delete rewrite of this model, if an extension asked for one.
    pub delete_rewrite: Option<crate::writes::PreparedDeleteRewrite>,
    /// Field positions an instance write must match against the loaded value.
    pub guards: Vec<usize>,
    /// The row filter and insert table when this model is a view over a shared table.
    pub view: Option<crate::view::PreparedView>,
    /// The change-set hooks of this model; `None` when it has no binding.
    pub hooks: Option<Box<crate::hooks::HookPlan>>,
    #[cfg(feature = "composition")]
    pub native: crate::behavior::NativeModel,
    #[cfg(feature = "composition")]
    pub resolved_fields: Vec<crate::behavior::ResolvedField>,
    #[cfg(feature = "composition")]
    pub owner: crate::behavior::OwnerId,
    field_index: HashMap<String, usize>,
    /// relation name -> (index into `ir.relations`, target model index)
    relation_index: HashMap<String, (usize, usize)>,
}

impl Model {
    /// The columns of a derived table (a CTE): no table of its own, no relations, no
    /// primary key (`pk` points at the first column).
    pub fn derived(name: &str, fields: Vec<FieldIr>) -> Result<Model> {
        let mut field_index = HashMap::new();
        for (i, f) in fields.iter().enumerate() {
            if field_index.insert(f.name.clone(), i).is_some() {
                return Err(format!("{name} has several columns named {:?}", f.name));
            }
        }
        let ir = ModelIr {
            partition: None,
            name: name.to_owned(),
            table: name.to_owned(),
            mapped: false,
            fields,
            primary_key: vec![],
            pk_name: None,
            relations: vec![],
            indexes: vec![],
            constraints: vec![],
            triggers: vec![],
            renamed_from: None,
            comment: None,
            protected_write: false,
            query_set: None,
            model_methods: None,
            classes_from: None,
        };
        Ok(Model {
            file_fields: vec![],
            ir, pk: 0, key: vec![0], field_index, relation_index: HashMap::new(), client_defaults: vec![],
            read_defaults: Default::default(),
            read_flags: ReadFlags::default(),
            assignments: vec![], delete_rewrite: None, guards: vec![], view: None, hooks: None,
            #[cfg(feature = "composition")] native: crate::behavior::NativeModel::None,
            #[cfg(feature = "composition")] owner: crate::behavior::OwnerId(0),
            #[cfg(feature = "composition")] resolved_fields: vec![] })
    }

    pub fn table(&self) -> &str {
        &self.ir.table
    }

    pub fn fields(&self) -> &[FieldIr] {
        &self.ir.fields
    }

    pub fn field(&self, name: &str) -> Result<&FieldIr> {
        self.field_index
            .get(name)
            .map(|&i| &self.ir.fields[i])
            .ok_or_else(|| format!("model {} has no field {name:?}", self.ir.name))
    }

    /// True if the file-storage extension declares the field.
    pub fn is_file_field(&self, name: &str) -> bool {
        self.file_fields.iter().any(|file| self.fields()[file.position].name == name)
    }

    pub fn field_pos(&self, name: &str) -> Result<usize> {
        self.field_index
            .get(name)
            .copied()
            .ok_or_else(|| format!("model {} has no field {name:?}", self.ir.name))
    }

    pub fn pk_field(&self) -> &FieldIr {
        &self.ir.fields[self.pk]
    }

    /// The primary key fields in key order; `pk_field()` is the first.
    pub fn key_fields(&self) -> Vec<&FieldIr> {
        self.key.iter().map(|&i| &self.ir.fields[i]).collect()
    }

    /// Returns the relation and the index of its target model.
    pub fn relation(&self, name: &str) -> Result<(&RelationIr, usize)> {
        self.relation_index
            .get(name)
            .map(|&(r, t)| (&self.ir.relations[r], t))
            .ok_or_else(|| format!("model {} has no relation {name:?}", self.ir.name))
    }
}

pub struct Schema {
    pub(crate) native_expression_slots: Vec<(String, String)>,
    /// The prepared result checks by model index; empty when no model has one.
    pub result_checks: Vec<Option<crate::result_check::PreparedChecks>>,
    pub dialect: crate::dialect::Dialect,
    pub models: Vec<Model>,
    pub enums: Vec<EnumIr>,
    /// Nested types (`type` blocks), for the shape of a `json` field.
    pub types: Vec<crate::ir::TypeIr>,
    pub identities: Option<crate::identity::IdentityManifest>,
    pub extensions: Vec<ExtensionIr>,
    pub functions: Vec<FunctionIr>,
    pub partitions: Vec<crate::ir::PartitionIr>,
    pub catalog: Vec<ExtensionIr>,
    model_index: HashMap<String, usize>,
    #[cfg(feature = "composition")]
    pub native_models: Vec<crate::behavior::NativeModel>,
    storage: Option<Box<Schema>>,
    #[cfg(feature = "composition")]
    pub owner_links: Vec<crate::behavior::PreparedOwnerLink>,
}

/// The view and check records must name known models, and a view shares the table of its root.
fn validate_views(behavior: &crate::behavior::Requirements, models: &[Model], model_index: &HashMap<String, usize>, physical: Option<&Schema>) -> Result<()> {
    if !behavior.proxy_models.is_empty() && behavior.model_views.is_empty() {
        return Err("the models carry proxy records from an older release and no model views; run `orm generate`".into());
    }
    let mut seen = std::collections::BTreeSet::new();
    for view in &behavior.model_views {
        let known = |name: &str| model_index.get(name).map(|&i| &models[i]);
        let (Some(model), Some(_), Some(root)) = (known(&view.model), known(&view.source), known(&view.root)) else {
            return Err(format!("{}: unresolved model view; rebuild schema", view.model));
        };
        if !seen.insert(&view.model) { return Err(format!("{}: duplicate model view; rebuild schema", view.model)); }
        let ancestors = behavior.ancestors(&view.model).ok_or_else(|| format!("{}: cyclic model view; rebuild schema", view.model))?;
        if ancestors.last().copied() != Some(view.root.as_str()) { return Err(format!("{}: stale view root; rebuild schema", view.model)); }
        if model.table() != root.table() { return Err(format!("{}: a view must share the table of its root {}", view.model, view.root)); }
        if let Some(physical) = physical {
            if !physical.models.iter().any(|m| m.ir.name == view.root && m.table() == model.table()) {
                return Err(format!("{}: a view must share its root storage owner {}", view.model, view.root));
            }
        }
    }
    if let Some(check) = behavior.result_checks.iter().find(|c| !model_index.contains_key(&c.model)) {
        return Err(format!("{}: result check names an unknown model; rebuild schema", check.model));
    }
    Ok(())
}

impl Schema {
    pub fn from_ir(ir: SchemaIr) -> Result<Self> { Self::from_ir_impl(ir, true, Some(&Default::default())) }

    /// Like `from_ir`, and leaves the hook bindings unresolved: code generation, migrations and compile-time checks
    /// need no extension library.
    pub fn from_ir_unbound(ir: SchemaIr) -> Result<Self> { Self::from_ir_impl(ir, true, None) }

    /// Like `from_ir`, and loads the library of each package in `packages` (extension id to package directory)
    /// that has hook bindings in the IR.
    pub fn from_ir_with_packages(ir: SchemaIr, packages: &std::collections::BTreeMap<String, std::path::PathBuf>) -> Result<Self> {
        Self::from_ir_impl(ir, true, Some(packages))
    }

    fn from_ir_impl(mut ir: SchemaIr, behavioral: bool, packages: Option<&std::collections::BTreeMap<String, std::path::PathBuf>>) -> Result<Self> {
        crate::identity::validate(&ir, crate::behavior::manifests())?;
        if behavioral { crate::behavior::prepare(&mut ir, None)?; }
        crate::partition::validate(&ir)?;
        #[cfg(not(feature = "model-composition"))]
        if ir.behavior.extensions.contains_key("composition") {
            return Err("model-composition is absent from this native artifact; rebuild with model-composition enabled".into());
        }
        #[cfg(feature = "composition")]
        let native_models = if behavioral { crate::behavior::bind(&mut ir)? } else { vec![crate::behavior::NativeModel::None; ir.models.len()] };
        let storage = match ir.behavior.storage.take() {
            Some(storage) => {
                let physical = serde_json::from_value(serde_json::json!({
                    "models": storage.models, "enums": ir.enums, "types": ir.types, "dialect": ir.dialect,
                    "native_expression_slots": ir.native_expression_slots,
                    "extensions": ir.extensions, "catalog": ir.catalog, "functions": ir.functions, "identities": ir.identities, "partitions": ir.partitions,
                })).map_err(|e| e.to_string())?;
                Some(Box::new(Self::from_ir_impl(physical, false, None)?))
            }
            None => None,
        };
        crate::features::validate(&ir)?;
        #[cfg(feature = "composition")]
        let mut native = native_models.iter().copied();
        let model_index: HashMap<String, usize> = ir
            .models
            .iter()
            .enumerate()
            .map(|(i, m)| (m.name.clone(), i))
            .collect();
        if model_index.len() != ir.models.len() {
            return Err("duplicate model name in schema".into());
        }
        let mut file_fields = crate::file_storage::prepare(&ir)?.into_iter();
        let enums = ir.enums;
        let types = ir.types;
        let mut models = Vec::with_capacity(ir.models.len());
        for mut m in ir.models {
            for f in &mut m.fields {
                let Some(name) = &f.enum_name else { continue };
                let i = enums
                    .iter()
                    .position(|e| e.name == *name)
                    .ok_or_else(|| format!("{}.{}: unknown enum {name}", m.name, f.name))?;
                let ok = match enums[i].storage {
                    EnumStorage::Native | EnumStorage::Text => matches!(f.ty, ColType::String | ColType::Text),
                    EnumStorage::Int => matches!(f.ty, ColType::Int | ColType::BigInt),
                };
                if !ok {
                    return Err(format!("{}.{}: enum {name} can't be stored as {:?}", m.name, f.name, f.ty));
                }
                f.enum_idx = Some(i as u32);
            }
            let field_index: HashMap<String, usize> =
                m.fields.iter().enumerate().map(|(i, f)| (f.name.clone(), i)).collect();
            if field_index.len() != m.fields.len() { return Err(format!("model {} has duplicate field names", m.name)); }
            let flagged = m.fields.iter().filter(|f| f.primary_key).count();
            let key: Vec<usize> = m.key_fields().iter().filter_map(|f| field_index.get(*f).copied()).collect();
            let pk = match key.as_slice() {
                [pk] if flagged == 1 => *pk,
                [pk, ..] if flagged == key.len() && m.primary_key.len() == key.len() => *pk,
                _ => return Err(format!("model {} must have exactly one primary key: @id on one field or @@id([...])", m.name)),
            };
            let mut relation_index = HashMap::new();
            for (i, r) in m.relations.iter().enumerate() {
                let target = *model_index.get(&r.target).ok_or_else(|| {
                    format!("relation {}.{} targets unknown model {}", m.name, r.name, r.target)
                })?;
                if let Some(missing) = r.from.iter().find(|f| !field_index.contains_key(*f)) {
                    return Err(format!("relation {}.{}: no field {missing:?}", m.name, r.name));
                }
                if r.from.is_empty() || r.from.len() != r.to.len() {
                    return Err(format!("relation {}.{}: from and to need the same number of fields", m.name, r.name));
                }
                if r.through.is_some() && r.from.len() != 1 {
                    return Err(format!("relation {}.{}: a many-to-many relation has one key field on each side", m.name, r.name));
                }
                if field_index.contains_key(&r.name) {
                    return Err(format!("{}.{} is both a field and a relation", m.name, r.name));
                }
                relation_index.insert(r.name.clone(), (i, target));
            }
            if let Some(f) = m.fields.iter().find(|f| f.enum_subset.is_some()) {
                return Err(format!("{}.{}: an enum subset is only allowed on a field of a view model", m.name, f.name));
            }
            let client_defaults = crate::client_default::prepare(&m.name, &m.fields, &enums)?;
            #[cfg(feature = "composition")]
            let kind = native.next().expect("prepared model");
            models.push(Model {
                file_fields: file_fields.next().expect("prepared file model"),
                ir: m, pk, key, field_index, relation_index, client_defaults,
                read_defaults: Default::default(),
                read_flags: ReadFlags::default(),
                assignments: vec![], delete_rewrite: None, guards: vec![], view: None,
                #[cfg(feature = "composition")] hooks: kind.static_hooks(),
                #[cfg(not(feature = "composition"))] hooks: None,
                #[cfg(feature = "composition")] native: kind,
                #[cfg(feature = "composition")] owner: crate::behavior::OwnerId(0),
            #[cfg(feature = "composition")] resolved_fields: vec![] });
        }
        // Validate relation targets now that every model is indexed.
        for m in &models {
            for r in &m.ir.relations {
                let (_, t) = m.relation(&r.name)?;
                let what = |e: String| format!("relation {}.{}: {e}", m.ir.name, r.name);
                for to in &r.to {
                    models[t].field(to).map_err(what)?;
                }
                if let Some(th) = &r.through {
                    if r.kind != RelKind::Many {
                        return Err(what("a relation through a join model is to-many".into()));
                    }
                    let j = *model_index.get(&th.model).ok_or_else(|| what(format!("unknown join model {}", th.model)))?;
                    models[j].field(&th.source).map_err(what)?;
                    models[j].field(&th.target).map_err(what)?;
                }
            }
        }
        crate::selection::prepare(&mut models, &ir.behavior.read_defaults)?;
        crate::writes::prepare(&mut models, &model_index, &ir.behavior)?;
        if let (true, Some(packages)) = (behavioral, packages) { crate::hooks::bind(&mut models, &model_index, &ir.behavior, packages)?; }
        #[cfg(feature = "composition")]
        let owner_links = crate::ownership::resolve(&mut models, storage.as_deref(), &ir.behavior.field_storage, &ir.behavior.owner_links)?;
        #[cfg(feature = "composition")]
        crate::writes::reject_composed(&models, storage.as_ref().map(|s| s.models.as_slice()).unwrap_or(&models), &owner_links, &ir.behavior)?;
        let physical = storage.as_deref();
        validate_views(&ir.behavior, &models, &model_index, physical)?;
        let mut result_checks = Vec::with_capacity(models.len());
        for m in models.iter_mut() {
            let checks: Vec<_> = ir.behavior.result_checks.iter().filter(|c| c.model == m.ir.name).collect();
            let view = crate::view::prepare(&m.ir.name, m.fields(), &enums, ir.behavior.model_views.iter().find(|v| v.model == m.ir.name))?;
            let prepared = crate::result_check::prepare(&m.ir.name, m.fields(), &enums, &checks)?;
            m.view = view;
            result_checks.push(prepared);
        }
        if result_checks.iter().all(Option::is_none) { result_checks.clear(); }
        Ok(Schema {
            native_expression_slots: ir.native_expression_slots,
            result_checks,
            dialect: ir.dialect,
            models,
            enums,
            types,
            identities: ir.identities,
            extensions: ir.extensions,
            functions: ir.functions,
            partitions: ir.partitions,
            catalog: ir.catalog,
            model_index,
            #[cfg(feature = "composition")] native_models,
            storage,
            #[cfg(feature = "composition")] owner_links,
        })
    }

    /// Schema used by physical migrations; logical views retain their storage.
    pub fn physical(&self) -> &Schema {
        if let Some(storage) = &self.storage { return storage; }
        self
    }

    pub fn model_idx(&self, name: &str) -> Result<usize> {
        self.model_index.get(name).copied().ok_or_else(|| format!("unknown model {name:?}"))
    }

    pub fn model(&self, idx: usize) -> &Model {
        &self.models[idx]
    }

    /// Walks `path` (relation names) from `root` and returns the model reached.
    pub fn walk(&self, root: usize, path: &[String]) -> Result<usize> {
        let mut cur = root;
        for hop in path {
            cur = self.models[cur].relation(hop)?.1;
        }
        Ok(cur)
    }
}
