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
    #[cfg(feature = "composition")]
    pub native: crate::behavior::NativeModel,
    pub resolved_fields: Vec<crate::behavior::ResolvedField>,
    pub owner: crate::behavior::OwnerId,
    /// Some field lives in another table than the model's own, or the model's table joins a parent table.
    pub has_foreign_storage: bool,
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
            assignments: vec![], delete_rewrite: None, guards: vec![],
            #[cfg(feature = "composition")] native: crate::behavior::NativeModel::None,
            owner: crate::behavior::OwnerId(0), resolved_fields: vec![], has_foreign_storage: false })
    }

    pub fn table(&self) -> &str {
        &self.ir.table
    }

    /// The field position a native field reads its stored value from.
    pub fn native_dependency(&self, position: usize) -> Option<usize> {
        #[cfg(feature = "composition")]
        { self.native.dependency(position) }
        #[cfg(not(feature = "composition"))]
        { let _ = position; None }
    }

    /// Positions of the fields a native extension computes on read.
    pub fn native_computed(&self) -> &[usize] {
        #[cfg(feature = "composition")]
        { self.native.computed() }
        #[cfg(not(feature = "composition"))]
        { &[] }
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
    #[cfg(feature = "proxy-models")]
    pub proxy_models: Vec<crate::proxy::PreparedProxy>,
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
    pub owner_links: Vec<crate::behavior::PreparedOwnerLink>,
}

impl Schema {
    pub fn from_ir(ir: SchemaIr) -> Result<Self> { Self::from_ir_impl(ir, true) }

    fn from_ir_impl(mut ir: SchemaIr, behavioral: bool) -> Result<Self> {
        crate::identity::validate(&ir)?;
        if behavioral { crate::behavior::prepare(&mut ir, None)?; }
        crate::partition::validate(&ir)?;
        #[cfg(feature = "composition")]
        let native_models = if behavioral { crate::behavior::bind(&mut ir)? } else { vec![crate::behavior::NativeModel::None; ir.models.len()] };
        let storage = match ir.behavior.storage.take() {
            Some(storage) => {
                let physical = serde_json::from_value(serde_json::json!({
                    "models": storage.models, "enums": ir.enums, "types": ir.types, "dialect": ir.dialect,
                    "native_expression_slots": ir.native_expression_slots,
                    "extensions": ir.extensions, "catalog": ir.catalog, "functions": ir.functions, "identities": ir.identities, "partitions": ir.partitions,
                })).map_err(|e| e.to_string())?;
                Some(Box::new(Self::from_ir_impl(physical, false)?))
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
                return Err(format!("{}.{}: an enum subset is only allowed on a field of a proxy model", m.name, f.name));
            }
            let client_defaults = crate::client_default::prepare(&m.name, &m.fields, &enums)?;
            models.push(Model {
                file_fields: file_fields.next().expect("prepared file model"),
                ir: m, pk, key, field_index, relation_index, client_defaults,
                read_defaults: Default::default(),
                read_flags: ReadFlags::default(),
                assignments: vec![], delete_rewrite: None, guards: vec![],
                #[cfg(feature = "composition")] native: native.next().expect("prepared model"),
                owner: crate::behavior::OwnerId(0), resolved_fields: vec![], has_foreign_storage: false });
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
        let owner_links = crate::ownership::resolve(&mut models, storage.as_deref(), &ir.behavior.field_storage, &ir.behavior.owner_links)?;
        crate::writes::reject_multi_table(&models, storage.as_ref().map(|s| s.models.as_slice()).unwrap_or(&models), &owner_links, &ir.behavior)?;
        #[cfg(feature = "proxy-models")]
        let proxy_models = {
            let mut seen = std::collections::BTreeSet::new();
            for proxy in &ir.behavior.proxy_models {
                if !seen.insert(&proxy.model) || !model_index.contains_key(&proxy.model) || !model_index.contains_key(&proxy.parent) {
                    return Err(format!("{}: duplicate or unresolved proxy metadata; rebuild schema", proxy.model));
                }
                let ancestors = ir.behavior.ancestors(&proxy.model).ok_or("cyclic proxy metadata; rebuild schema")?;
                let parent = ancestors.last().copied().unwrap_or(&proxy.parent);
                let model = &models[model_index[&proxy.model]];
                if proxy.storage_owner != parent { return Err(format!("{}: stale proxy storage owner; rebuild schema", proxy.model)); }
                let physical = storage.as_ref().ok_or("proxy models require explicit physical storage")?;
                if model.table() != models[model_index[parent]].table() || !physical.models.iter().any(|m| m.ir.name == parent && m.table() == model.table()) {
                    return Err(format!("{}: proxy must share its root storage owner", proxy.model));
                }
            }
            models.iter().map(|m| crate::proxy::prepare(&m.ir.name, m.fields(), &enums,
                ir.behavior.proxy_models.iter().find(|p| p.model == m.ir.name))).collect::<Result<Vec<_>>>()?
        };
        Ok(Schema {
            native_expression_slots: ir.native_expression_slots,
            #[cfg(feature = "proxy-models")] proxy_models,
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
            owner_links,
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
