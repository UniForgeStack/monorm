//! Model specializations emitted as Rust source; execution never reads these specs.
use orm_contracts::{extension::{ExportKind, Manifest, Specialization}, ir::{ColType, SchemaIr}};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

pub use orm_contracts::native::{FieldRule, Computed, RecordRule, Method, NativeSpec};

pub struct Sources {
    pub core: String,
    pub engine: String,
    pub specializations: Vec<Specialization>,
    pub python: String,
    pub node: String,
}

/// A proxy takes the spec of its nearest ancestor that has one, then adds its own entries.
/// An own entry with the same field, computed or method name replaces the inherited one.
/// A field rule or computed field on a field the proxy does not keep is left out.
/// An inherited record rule that needs a dropped field fails the build, unless an own record rule of the proxy has the same export.
fn inherited_specs(schema: &SchemaIr, specs: &[NativeSpec]) -> Result<Vec<NativeSpec>, String> {
    let own: BTreeMap<_, _> = specs.iter().map(|s| (s.model.as_str(), s)).collect();
    let mut merged = vec![];
    for proxy in &schema.behavior.model_views {
        let source = schema.behavior.ancestors(&proxy.model).unwrap_or_default().into_iter().find_map(|a| own.get(a).copied());
        let own_spec = own.get(proxy.model.as_str()).copied();
        let Some(model) = schema.models.iter().find(|m| m.name == proxy.model) else {
            merged.extend(own_spec.cloned());
            continue;
        };
        let Some(source) = source.or(own_spec) else { continue };
        let kept = |name: &String| model.fields.iter().any(|f| f.name == *name);
        let inherited = source.model != proxy.model;
        let mut spec = NativeSpec { model: proxy.model.clone(), fields: vec![], computed: vec![], records: vec![], methods: vec![] };
        if inherited {
            spec.fields = source.fields.iter().filter(|r| kept(&r.field)).cloned().collect();
            spec.computed = source.computed.iter().filter(|c| kept(&c.dependency)).cloned().collect();
            let replaced = |export: &String| own_spec.is_some_and(|o| o.records.iter().any(|r| r.export == *export));
            for rule in &source.records {
                if replaced(&rule.export) { continue; }
                match rule.dependencies.iter().find(|d| !kept(d)) {
                    None => spec.records.push(rule.clone()),
                    Some(dropped) => return Err(format!(
                        "{}: record rule {} (inherited from {}) needs field {dropped}, which the proxy drops; keep {dropped} in the proxy, or declare an own record rule of the same export {}",
                        proxy.model, rule.export, source.model, rule.export,
                    )),
                }
            }
            spec.methods = source.methods.clone();
        }
        if let Some(own_spec) = own_spec {
            for rule in &own_spec.fields { spec.fields.retain(|r| r.field != rule.field); spec.fields.push(rule.clone()); }
            for computed in &own_spec.computed { spec.computed.retain(|c| c.field != computed.field); spec.computed.push(computed.clone()); }
            for method in &own_spec.methods { spec.methods.retain(|m| m.name != method.name); spec.methods.push(method.clone()); }
            spec.records.extend(own_spec.records.iter().cloned());
        }
        if own_spec.is_some() || !(spec.fields.is_empty() && spec.computed.is_empty() && spec.records.is_empty() && spec.methods.is_empty()) { merged.push(spec); }
    }
    Ok(merged)
}

/// Resolve names and types at build time. The generated hot path uses field positions.
pub fn generate(schema: &mut SchemaIr, specs: &[NativeSpec], manifests: &[Manifest]) -> Result<Sources, String> {
    generate_with_sources(schema, specs, manifests, &BTreeMap::new())
}

pub fn generate_with_sources(schema: &mut SchemaIr, specs: &[NativeSpec], manifests: &[Manifest], sources: &BTreeMap<String, String>) -> Result<Sources, String> {
    let merged = inherited_specs(schema, specs)?;
    let mut specs: Vec<_> = specs.iter().filter(|s| !schema.behavior.model_views.iter().any(|v| v.model == s.model)).cloned().collect();
    specs.extend(merged);
    specs.sort_by(|a, b| a.model.cmp(&b.model));
    for spec in &mut specs { spec.methods.sort_by(|a, b| a.name.cmp(&b.name)); }
    let exports: BTreeMap<_, _> = manifests.iter().flat_map(|m| m.exports.iter().filter_map(|e| e.string_form().map(|f| (e.id.as_str(), f)))).collect();
    let mut core = String::from("#[derive(Clone, Copy, Debug, PartialEq, Eq)]\npub enum NativeModel { None,\n");
    for i in 0..specs.len() { core.push_str(&format!("S{i},\n")); }
    core.push_str("}\nimpl NativeModel { pub fn computed(self) -> &'static [usize] { match self { Self::None => &[],\n");
    let mut engine = String::from("use orm_core::behavior::NativeModel;\n");
    let mut hook_helpers = String::new();
    let mut hook_arms = String::new();
    let mut dependency_arms = String::new();
    let mut computation_arms = String::new();
    let mut bind_arms = String::new();
    let mut specializations = vec![];
    let mut python = String::from("#[allow(unused_imports)] use pyo3::prelude::*;\n");
    let mut node = String::from("#[allow(unused_imports)] use napi_derive::napi;\n");
    let mut registration = String::new();
    let mut seen_models = std::collections::BTreeSet::new();
    for (i, spec) in specs.iter().enumerate() {
        if !seen_models.insert(&spec.model) { return Err(format!("duplicate native model {}", spec.model)); }
        let model = schema.models.iter_mut().find(|m| m.name == spec.model).ok_or_else(|| format!("unknown native model {}", spec.model))?;
        let mut ids = vec![];
        let mut plan = String::new();
        let mut seen_fields = std::collections::BTreeSet::new();
        for rule in &spec.fields {
            if !seen_fields.insert(&rule.field) { return Err(format!("{}.{}: duplicate native field rule", spec.model, rule.field)); }
            let pos = model.fields.iter().position(|f| f.name == rule.field).ok_or_else(|| format!("{}.{}: unknown field", spec.model, rule.field))?;
            let field = &model.fields[pos];
            if !matches!(field.ty, ColType::String | ColType::Text) || field.array || field.enum_name.is_some() {
                return Err(format!("{}.{}: borrowed string exports require a scalar string", spec.model, rule.field));
            }
            let mut body = format!("fn field_s{i}_{pos}(value: &mut orm_contracts::abi::Value) -> Result<(), String> {{\nuse orm_contracts::abi::Value;\n");
            body.push_str(&format!("let v = match value {{ Value::Text(v) => v, Value::Null if {} => return Ok(()), _ => return Err({:?}.into()) }};\n", rule.allow_null, format!("{}.{}: validator requires a non-null string", spec.model, rule.field)));
            for (export, kind) in rule.transforms.iter().map(|s| (s, ExportKind::StringTransform)).chain(rule.validators.iter().map(|s| (s, ExportKind::StringValidator))) {
                let e = exports.get(export.as_str()).ok_or_else(|| format!("missing export {export}; rebuild"))?;
                if e.kind != kind || !matches!(e.input, ColType::String | ColType::Text) || (kind == ExportKind::StringValidator && e.output.is_some()) { return Err(format!("wrong export type for {export}")); }
                ids.push(export.clone());
                if kind == ExportKind::StringTransform {
                    if !matches!(e.output, Some(ColType::String | ColType::Text)) { return Err(format!("wrong transform output for {export}")); }
                    body.push_str(&format!("*v = {}(v.as_str()).map_err(String::from)?;\n", e.rust));
                } else {
                    body.push_str(&format!("let () = {}(v.as_str()).map_err(String::from)?;\n", e.rust));
                }
            }
            body.push_str("Ok(()) }\n");
            hook_helpers.push_str(&body);
            plan.push_str(&format!("plan.transforms.push(({pos}, HookRef::Static(StaticHook::Field(field_s{i}_{pos}))));\n"));
        }
        for (r, rule) in spec.records.iter().enumerate() {
            if rule.dependencies.is_empty() { return Err(format!("{}: record validator must declare dependencies", spec.model)); }
            let export = exports.get(rule.export.as_str()).ok_or_else(|| format!("missing record export {}", rule.export))?;
            if export.kind != ExportKind::StringRecordValidator || !matches!(export.input, ColType::String | ColType::Text) || export.output.is_some() {
                return Err(format!("wrong record export type for {}", rule.export));
            }
            ids.push(rule.export.clone());
            let mut arguments = vec![];
            let dependencies: Vec<_> = rule.dependencies.iter().map(|name| model.fields.iter().position(|f| f.name == *name).ok_or_else(|| format!("missing record dependency {name}"))).collect::<Result<_, _>>()?;
            for (k, dependency) in rule.dependencies.iter().enumerate() {
                let field = model.fields.iter().find(|f| f.name == *dependency).ok_or_else(|| format!("missing record dependency {dependency}"))?;
                if !matches!(field.ty, ColType::String | ColType::Text) || field.array || field.enum_name.is_some() {
                    return Err(format!("record dependency {dependency} requires a scalar string"));
                }
                arguments.push(format!("match args.get({k}).map(|a| &a.value) {{ Some(orm_contracts::abi::Value::Text(v)) => v.as_str(), _ => return Err(\"record validation requires all non-null supplied dependencies; use an explicit transaction for partial records\".into()) }}"));
            }
            hook_helpers.push_str(&format!("fn record_s{i}_{r}(args: &[orm_contracts::abi::Arg]) -> Result<(), String> {{\nlet args = [{}];\nlet () = {}(&args).map_err(String::from)?;\nOk(()) }}\n", arguments.join(", "), export.rust));
            plan.push_str(&format!("plan.records.push(RecordHook {{ positions: vec!{dependencies:?}, hook: HookRef::Static(StaticHook::Record(record_s{i}_{r})) }});\n"));
        }
        hook_arms.push_str(&format!("Self::S{i} => {{ let mut plan = HookPlan::default();\n{plan}plan.finish() }},\n"));
        let mut positions = vec![];
        let mut results = vec![];
        for computed in &spec.computed {
            if model.fields.iter().any(|f| f.name == computed.field) { return Err(format!("{}.{}: computed field collides with a field", spec.model, computed.field)); }
            if model.relations.iter().any(|r| r.name == computed.field) { return Err(format!("{}.{}: computed field collides with a relation", spec.model, computed.field)); }
            if spec.computed.iter().any(|other| other.field == computed.dependency) {
                return Err(format!("{}.{}: nested/cyclic native computed dependencies require an expanded specialization; rebuild with direct stored dependencies", spec.model, computed.field));
            }
            let source = model.fields.iter().find(|f| f.name == computed.dependency).ok_or_else(|| format!("missing computed dependency {}", computed.dependency))?;
            if !matches!(source.ty, ColType::String | ColType::Text) || source.array || source.enum_name.is_some() {
                return Err(format!("{}.{}: computed proof requires a scalar string dependency", spec.model, computed.field));
            }
            let e = exports.get(computed.export.as_str()).ok_or_else(|| format!("missing computed export {}", computed.export))?;
            if e.kind != ExportKind::StringComputed || !matches!(e.input, ColType::String | ColType::Text) || !matches!(e.output, Some(ColType::String | ColType::Text)) {
                return Err(format!("wrong computed export type for {}", computed.export));
            }
            ids.push(computed.export.clone());
            results.push(orm_contracts::extension::ResultDeclaration { model: spec.model.clone(), field: computed.field.clone(), dependencies: vec![computed.dependency.clone()] });
            let pos = model.fields.len();
            let dependency_position = model.fields.iter().position(|f| f.name == computed.dependency).expect("resolved dependency");
            dependency_arms.push_str(&format!("(Self::S{i}, {pos}) => Some({dependency_position}),\n"));
            positions.push(pos);
            let mut field = source.clone();
            field.name = computed.field.clone();
            field.ty = e.output.expect("validated computed output");
            field.max_length = None; field.hints.clear();
            field.primary_key = false; field.auto_increment = false; field.unique = false; field.index = false;
            field.default = None; field.default_sql = None; field.default_now = false; field.renamed_from = None;
            model.fields.push(field);
            computation_arms.push_str(&format!(r#"(NativeModel::S{i}, {pos}) => {{
    let mut values = Vec::with_capacity(rows.len());
    for row in 0..rows.len() {{
        values.push(match rows.cell(row, column, ty)? {{
            crate::db::Cell::Text(value) => Some({path}(value).map_err(crate::db::DbError::other)?),
            crate::db::Cell::Null => None,
            _ => return Err(crate::db::DbError::other("computed dependency is not a string")),
        }});
    }}
    Ok(values)
}},
"#, path=e.rust));
        }
        core.push_str(&format!("Self::S{i} => &{positions:?},\n"));
        let mut methods = vec![];
        let mut seen_methods = std::collections::BTreeSet::new();
        for (j, method) in spec.methods.iter().enumerate() {
            crate::rust_path(&method.name)?;
            if ["__debug__", "False", "None", "True", "and", "as", "assert", "async", "await", "break", "class", "continue", "def", "del", "elif", "else", "except", "finally", "for", "from", "global", "if", "import", "in", "is", "lambda", "nonlocal", "not", "or", "pass", "raise", "return", "try", "while", "with", "yield"].contains(&method.name.as_str()) {
                return Err(format!("{}.{}: model method is a Python keyword", spec.model, method.name));
            }
            if method.name.contains("::") || !seen_methods.insert(&method.name) { return Err(format!("invalid or duplicate model method {}", method.name)); }
            if ["pk", "objects", "qs", "_meta", "DoesNotExist", "MultipleObjectsReturned", "update", "delete", "refresh", "toJSON", "constructor", "then", "toString"].contains(&method.name.as_str()) || model.fields.iter().any(|f| f.name == method.name) || model.relations.iter().any(|r| r.name == method.name) {
                return Err(format!("{}.{}: model method collision", spec.model, method.name));
            }
            let e = exports.get(method.export.as_str()).ok_or_else(|| format!("missing method export {}", method.export))?;
            if !matches!(e.input, ColType::String | ColType::Text) || !matches!(e.kind, ExportKind::StringValidator | ExportKind::StringTransform | ExportKind::StringComputed) {
                return Err(format!("{}.{}: model methods require a scalar string export", spec.model, method.name));
            }
            if (e.kind == ExportKind::StringValidator && e.output.is_some()) || (e.kind != ExportKind::StringValidator && !matches!(e.output, Some(ColType::String | ColType::Text))) { return Err(format!("wrong method output type for {}", method.export)); }
            let rust_type = if e.output.is_some() { "String" } else { "()" };
            let symbol = format!("orm_native_method_{i}_{j}");
            python.push_str(&format!("#[pyfunction] fn {symbol}(value: &str) -> PyResult<{rust_type}> {{ {}(value).map_err(crate::errors::query_err) }}\n", e.rust));
            registration.push_str(&format!("m.add_function(pyo3::wrap_pyfunction!({symbol}, m)?)?;\n"));
            node.push_str(&format!("#[napi(js_name = {symbol:?})] pub fn {symbol}(value: String) -> napi::Result<{rust_type}> {{ {}(&value).map_err(crate::query_err) }}\n", e.rust));
            methods.push(orm_contracts::extension::MethodDeclaration { model: spec.model.clone(), name: method.name.clone(), native_function: symbol, input: e.input, output: e.output });
            ids.push(method.export.clone());
        }
        let methods_json = serde_json::to_string(&methods).map_err(|e| e.to_string())?;
        let model_json = serde_json::to_string(model).map_err(|e| e.to_string())?;
        let enum_names: std::collections::BTreeSet<_> = model.fields.iter().filter_map(|f| f.enum_name.as_deref()).collect();
        let relevant_enums: Vec<_> = schema.enums.iter().filter(|e| enum_names.contains(e.name.as_str())).collect();
        let enums = serde_json::to_string(&relevant_enums).map_err(|e| e.to_string())?;
        let enum_names_json = serde_json::to_string(&enum_names).map_err(|e| e.to_string())?;
        let computed_names: Vec<_> = spec.computed.iter().map(|f| f.field.clone()).collect();
        let computed_names_json = serde_json::to_string(&computed_names).map_err(|e| e.to_string())?;
        let physical = orm_contracts::extension::specialization_storage(model, &schema.behavior, &computed_names)?;
        let physical_json = serde_json::to_string(&physical).map_err(|e| e.to_string())?;
        let ownership: Vec<_> = schema.behavior.field_storage.iter().filter(|f| f.model == model.name).collect();
        let owner_names: std::collections::BTreeSet<_> = physical.iter().map(|m| m.name.as_str()).collect();
        let links: Vec<_> = schema.behavior.owner_links.iter().filter(|l| owner_names.contains(l.child.as_str())).collect();
        let ownership_json = serde_json::to_string(&ownership).map_err(|e| e.to_string())?;
        let links_json = serde_json::to_string(&links).map_err(|e| e.to_string())?;
        let field_adapters: Vec<_> = schema.behavior.field_adapters.iter().filter(|f| f.model == model.name).collect();
        let field_adapters_json = serde_json::to_string(&field_adapters).map_err(|e| e.to_string())?;
        let identities_json = serde_json::to_string(&schema.identities).map_err(|e| e.to_string())?;
        let extension_data_json = serde_json::to_string(&schema.behavior.extension_data).map_err(|e| e.to_string())?;
        let policies: Vec<_> = schema.behavior.read_defaults.iter().filter(|d| d.model == model.name).collect();
        let policies_json = serde_json::to_string(&policies).map_err(|e| e.to_string())?;
        let versions: BTreeMap<_, _> = manifests.iter().map(|m| (&m.id, &m.version)).collect();
        let fingerprint = format!("{:x}", Sha256::digest(serde_json::to_vec(&serde_json::json!({"model": model, "identities": schema.identities, "extension_data": schema.behavior.extension_data, "enums": relevant_enums, "physical_schema": physical, "field_storage": ownership, "owner_links": links, "field_adapters": field_adapters, "read_defaults": policies, "dialect": schema.dialect, "configuration": spec, "extensions": versions, "composition": manifests, "sources": sources, "host_contract": orm_contracts::extension::HOST_CONTRACT})).map_err(|e| e.to_string())?));
        ids.sort(); ids.dedup();
        let specialization = Specialization { model: spec.model.clone(), fingerprint, exports: ids };
        let specialization_json = serde_json::to_string(&specialization).map_err(|e| e.to_string())?;
        let results_json = serde_json::to_string(&results).map_err(|e| e.to_string())?;
        let added_fields = serde_json::to_string(&model.fields[model.fields.len()-spec.computed.len()..]).map_err(|e| e.to_string())?;
        bind_arms.push_str(&format!(r#"{name:?} => {{
    let additions: Vec<crate::ir::FieldIr> = serde_json::from_str({added_fields:?}).expect("generated result fields");
    for field in additions {{
        if !m.fields.iter().any(|f| f.name == field.name) {{ m.fields.push(field); }}
    }}
    if serde_json::to_string(&ir.identities).map_err(|e| e.to_string())? != {identities_json:?} || serde_json::to_string(&ir.behavior.extension_data).map_err(|e| e.to_string())? != {extension_data_json:?} {{
        return Err(format!("{{}}: stale native identity/extension data specialization; rebuild", m.name));
    }}
    let enum_names: std::collections::BTreeSet<String> = serde_json::from_str({enum_names_json:?}).expect("generated enum dependencies");
    let relevant_enums: Vec<_> = ir.enums.iter().filter(|e| enum_names.contains(&e.name)).collect();
    if serde_json::to_string(m).map_err(|e| e.to_string())? != {model_json:?} || serde_json::to_string(&relevant_enums).map_err(|e| e.to_string())? != {enums:?} || ir.dialect.name() != {dialect:?} {{
        return Err(format!("{{}}: stale native specialization; rebuild with the normalized schema", m.name));
    }}
    let computed_names: Vec<String> = serde_json::from_str({computed_names_json:?}).expect("generated computed names");
    let physical = crate::behavior::specialization_storage(m, &ir.behavior, &computed_names)?;
    let ownership: Vec<_> = ir.behavior.field_storage.iter().filter(|f| f.model == m.name).collect();
    let owner_names: std::collections::BTreeSet<_> = physical.iter().map(|m| m.name.as_str()).collect();
    let links: Vec<_> = ir.behavior.owner_links.iter().filter(|l| owner_names.contains(l.child.as_str())).collect();
    if serde_json::to_string(&physical).map_err(|e| e.to_string())? != {physical_json:?} || serde_json::to_string(&ownership).map_err(|e| e.to_string())? != {ownership_json:?} || serde_json::to_string(&links).map_err(|e| e.to_string())? != {links_json:?} {{
        return Err(format!("{{}}: stale native storage specialization; rebuild", m.name));
    }}
    let adapters: Vec<_> = ir.behavior.field_adapters.iter().filter(|f| f.model == m.name).collect();
    if serde_json::to_string(&adapters).map_err(|e| e.to_string())? != {field_adapters_json:?} {{
        return Err(format!("{{}}: stale native field adapter specialization; rebuild", m.name));
    }}
    let policies: Vec<_> = ir.behavior.read_defaults.iter().filter(|d| d.model == m.name).collect();
    if serde_json::to_string(&policies).map_err(|e| e.to_string())? != {policies_json:?} {{ return Err(format!("{{}}: stale native read-default specialization; rebuild", m.name)); }}
    let requirement = serde_json::from_str({specialization_json:?}).expect("generated specialization");
    if !ir.behavior.specializations.contains(&requirement) {{ ir.behavior.specializations.push(requirement); }}
    let methods: Vec<crate::behavior::MethodDeclaration> = serde_json::from_str({methods_json:?}).expect("generated model methods");
    if ir.behavior.methods.iter().any(|method| method.model == m.name && !methods.contains(method)) {{ return Err(format!("{{}}: stale native method adapter; rebuild", m.name)); }}
    for method in methods {{ if !ir.behavior.methods.contains(&method) {{ ir.behavior.methods.push(method); }} }}
    let results: Vec<crate::behavior::ResultDeclaration> = serde_json::from_str({results_json:?}).expect("generated result dependencies");
    for result in results {{ if !ir.behavior.result_fields.contains(&result) {{ ir.behavior.result_fields.push(result); }} }}
    out.push(NativeModel::S{i});
}},
"#, name=spec.model, dialect=schema.dialect.name()));
        specializations.push(specialization);
    }
    core.push_str("} } }\npub fn bind(ir: &mut SchemaIr) -> Result<Vec<NativeModel>, String> { let mut out = Vec::with_capacity(ir.models.len()); for m in &mut ir.models { match m.name.as_str() {\n");
    core.push_str(&bind_arms);
    core.push_str("_ => out.push(NativeModel::None),\n} } Ok(out) }\n");
    core.push_str(&format!("impl NativeModel {{ pub fn dependency(self, field: usize) -> Option<usize> {{ match (self, field) {{ {dependency_arms} _ => None }} }} }}\n"));
    core.push_str(&hook_helpers);
    core.push_str(&format!("impl NativeModel {{ pub fn static_hooks(self) -> Option<Box<crate::hooks::HookPlan>> {{\n#[allow(unused_imports)] use crate::hooks::{{HookPlan, HookRef, RecordHook, StaticHook}};\nmatch self {{ Self::None => None, {hook_arms} }} }} }}\n"));
    engine.push_str("pub fn compute(kind: NativeModel, position: usize, rows: &dyn crate::db::RowSet, column: usize, ty: orm_core::ir::ValueType) -> crate::db::DbResult<Vec<Option<String>>> { match (kind, position) {\n");
    engine.push_str(&computation_arms);
    engine.push_str("_ => Err(crate::db::DbError::other(\"missing compiled computation\")),\n} }\n");
    let module = if registration.is_empty() { "_m" } else { "m" };
    python.push_str(&format!("pub fn register({module}: &Bound<'_, PyModule>) -> PyResult<()> {{ {registration} Ok(()) }}\n"));
    Ok(Sources { core, engine, specializations, python, node })
}
