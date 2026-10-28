//! Python module generation: `models.py` (runtime) and `models.pyi` (static types).
//!
//! The runtime module embeds the compiled schema IR and builds the model classes from
//! it with `orm.define()`, so every schema detail (indexes, triggers, extension types)
//! reaches the engine without Python code describing it. The stub gives editors and
//! type checkers the per-model classes, relation paths, insert / update shapes and
//! query sets.

use std::collections::BTreeSet;
use std::fmt::Write;

use crate::ir::{ColType, EnumStorage, FieldIr, RelKind, RelationIr, SchemaIr, TypeFieldIr, TypeIr};
use super::{groups, has_server_value_in, sub_has_default, type_requires};
use crate::schema::{Model, Schema};

pub struct Generated {
    pub module: String,
    pub stub: String,
}

const WIDTH: usize = 89;

fn section(name: &str) -> String {
    let head = format!("# -- {name} ");
    format!("{head}{}", "-".repeat(WIDTH.saturating_sub(head.len())))
}

fn field_class(f: &FieldIr) -> &'static str {
    if f.array {
        return "Array";
    }
    if f.enum_name.is_some() {
        return "Enum";
    }
    if f.db_type.is_some() && !f.hints.is_empty() {
        return "Field";
    }
    match f.ty {
        ColType::BigInt => "BigInt",
        ColType::Int => "Integer",
        ColType::Float => "Float",
        ColType::Bool => "Boolean",
        ColType::String => "String",
        ColType::Text => "Text",
        ColType::DateTime => "DateTime",
        ColType::Date => "Date",
        ColType::Uuid => "Uuid",
        ColType::Json => "Json",
        ColType::Decimal => "Decimal",
    }
}

fn base_type(f: &FieldIr) -> String {
    let t = element_type(f);
    if f.array {
        format!("list[{t}]")
    } else {
        t
    }
}

fn element_type(f: &FieldIr) -> String {
    if let Some(h) = f.hints.get("python") {
        return h.clone();
    }
    if let Some(e) = &f.enum_name {
        return e.clone();
    }
    match f.ty {
        ColType::BigInt | ColType::Int => "int",
        ColType::Float => "float",
        ColType::Bool => "bool",
        ColType::String | ColType::Text => "str",
        ColType::DateTime => "datetime",
        ColType::Date => "date",
        ColType::Uuid => "UUID",
        ColType::Json => "Any",
        ColType::Decimal => "Decimal",
    }
    .to_owned()
}

fn value_type(f: &FieldIr) -> String {
    let t = base_type(f);
    if f.nullable {
        format!("{t} | None")
    } else {
        t
    }
}

/// A Python literal of a JSON value (adapter options).
fn py_literal(v: &serde_json::Value) -> String {
    use serde_json::Value;
    match v {
        Value::Null => "None".into(),
        Value::Bool(b) => if *b { "True".into() } else { "False".into() },
        Value::Number(n) => n.to_string(),
        Value::String(t) => serde_json::to_string(t).unwrap_or_default(),
        Value::Array(a) => format!("[{}]", a.iter().map(py_literal).collect::<Vec<_>>().join(", ")),
        Value::Object(o) => format!("{{{}}}", o.iter().map(|(k, v)| format!("{}: {}", serde_json::to_string(k).unwrap_or_default(), py_literal(v))).collect::<Vec<_>>().join(", ")),
    }
}

/// The stub bases of a model: the type models that extend directly into it, descendants first, else `Model`.
fn type_model_bases(type_models: &[crate::ir::TypeModel], name: &str) -> String {
    let bases: Vec<String> = type_models.iter().rev().filter(|t| t.extended_by.iter().any(|c| c == name)).map(|t| format!("{}Fields", t.name)).collect();
    if bases.is_empty() { "Model".into() } else { bases.join(", ") }
}

/// The `(name, stub line)` of the fields of a type model (`own`) or of all its type-model ancestors.
fn type_model_lines(type_models: &[crate::ir::TypeModel], name: &str, own: bool) -> Vec<(String, String)> {
    let line = |f: &FieldIr| (f.name.clone(), format!("{}: f.{}[{}]", f.name, field_class(f), value_type(f)));
    if own {
        return type_models.iter().find(|t| t.name == name).map(|t| t.fields.iter().filter(|f| f.nested.is_none()).map(line).collect()).unwrap_or_default();
    }
    let mut out = Vec::new();
    for t in type_models.iter().filter(|t| t.extended_by.iter().any(|c| c == name)) {
        out.extend(type_model_lines(type_models, &t.name, false));
        out.extend(type_model_lines(type_models, &t.name, true));
    }
    out
}

fn sub_element(sf: &TypeFieldIr) -> String {
    match (&sf.type_name, &sf.leaf) {
        (Some(n), _) => n.clone(),
        (_, Some(l)) => element_type(l),
        _ => "Any".into(),
    }
}

fn or_none(t: String, nullable: bool) -> String {
    if nullable { format!("{t} | None") } else { t }
}

/// The type of a subfield on a value class.
fn sub_value(sf: &TypeFieldIr) -> String {
    let t = sub_element(sf);
    or_none(if sf.list { format!("tuple[{t}, ...]") } else { t }, sf.nullable)
}

/// What a caller may give for a subfield; `patch` leaves a list out (it cannot be assigned part by part).
fn sub_input(sf: &TypeFieldIr, patch: bool) -> Option<String> {
    let t = sub_element(sf);
    let one = match &sf.type_name {
        Some(n) => format!("{n} | {n}{}", if patch { "Patch" } else { "Input" }),
        None => t,
    };
    if sf.list {
        return if patch { None } else { Some(or_none(format!("Sequence[{one}]"), sf.nullable)) };
    }
    Some(or_none(one, sf.nullable))
}

/// The value class, input shapes and reference class of one `type` block.
fn write_type(body: &mut String, ir: &SchemaIr, t: &TypeIr) {
    let name = &t.name;
    writeln!(body, "{}\n", section(name)).unwrap();
    writeln!(body, "class {name}(NestedValue):").unwrap();
    for sf in &t.fields {
        writeln!(body, "    @property\n    def {}(self) -> {}: ...", sf.name, sub_value(sf)).unwrap();
    }
    let mut params: Vec<String> = vec![];
    for sf in &t.fields {
        let optional = sf.nullable || sub_has_default(sf) || sf.type_name.as_deref().is_some_and(|n| !type_requires(ir, n));
        let one = sub_input(sf, false).unwrap_or_default();
        params.push(if optional { format!("{}: {one} = ...", sf.name) } else { format!("{}: {one}", sf.name) });
    }
    writeln!(body, "    def __init__(self, *, {}) -> None: ...\n", params.join(", ")).unwrap();
    writeln!(body, "class {name}Input(TypedDict):").unwrap();
    for sf in &t.fields {
        let optional = sf.nullable || sub_has_default(sf) || sf.type_name.as_deref().is_some_and(|n| !type_requires(ir, n));
        let one = sub_input(sf, false).unwrap_or_default();
        writeln!(body, "    {}: {}", sf.name, if optional { format!("NotRequired[{one}]") } else { one }).unwrap();
    }
    writeln!(body, "\nclass {name}Patch(TypedDict, total=False):").unwrap();
    let mut any = false;
    for sf in &t.fields {
        if let Some(one) = sub_input(sf, true) {
            writeln!(body, "    {}: {one}", sf.name).unwrap();
            any = true;
        }
    }
    if !any { writeln!(body, "    pass").unwrap(); }
    writeln!(body, "\nclass _{name}Ref(NestedRef):").unwrap();
    for sf in &t.fields {
        match &sf.type_name {
            Some(n) => writeln!(body, "    {}: _{n}Ref", sf.name).unwrap(),
            None => writeln!(body, "    {}: ColumnRef[{}]", sf.name, sub_value(sf)).unwrap(),
        }
    }
    body.push('\n');
}

/// Generates `models.py` / `models.pyi` for `ir` (already validated as `schema`).
/// `source` names the schema file in the header comment.
pub fn generate(ir: &SchemaIr, schema: &Schema, source: &str) -> Result<Generated, String> {
    generate_with(ir, schema, source, &super::Options::default())
}

/// [`generate`] with generator options (the user's query-set classes).
pub fn generate_with(ir: &SchemaIr, schema: &Schema, source: &str, options: &super::Options) -> Result<Generated, String> {
    let query_sets = options.query_sets(ir, schema, ':')?;
    let model_methods = options.model_methods(ir, schema, ':')?;
    let adapters = super::Adapters::resolve(options, ir, "python")?;
    let field_type = |m: &Model, f: &FieldIr| adapters.value(m, f, "None").unwrap_or_else(|| value_type(f));
    let input_type = |m: &Model, f: &FieldIr| adapters.input(m, f, "None").unwrap_or_else(|| value_type(f));
    // A model view takes the class of its root. That class has the root's `QuerySet[Root]` as a base,
    // which the stub cannot combine with the view's `QuerySet[View]`, so the stub types its methods as `Any`.
    let inherits = |model: &str| query_sets.get(model).is_some_and(|q| q.2);
    // A class that an abstract model declares takes the model as its type parameter (`class UserQueries[M: AbstractUserFields]`).
    let generic = |model: &str| super::declares_generic(ir, &query_sets, model);
    let ir_json = super::embedded_schema_json(ir)?;
    if ir_json.contains("\"\"\"") {
        return Err("schema text contains \"\"\" which can't be embedded in the generated module".into());
    }
    let names: Vec<&str> = schema.models.iter().map(|m| m.ir.name.as_str()).collect();
    let enums: Vec<&str> = schema.enums.iter().map(|e| e.name.as_str()).collect();
    let mut exported: Vec<String> = enums.iter().map(|n| n.to_string()).collect();
    let type_names: Vec<&str> = ir.types.iter().map(|t| t.name.as_str()).collect();
    exported.extend(type_names.iter().flat_map(|n| [n.to_string(), format!("{n}Input"), format!("{n}Patch")]));
    exported.extend(names.iter().map(|n| n.to_string()));
    exported.extend(names.iter().flat_map(|n| [format!("{n}Insert"), format!("{n}Update"), format!("{n}UpdateRow")]));
    exported.extend(names.iter().map(|n| format!("{n}QuerySet")));
    exported.extend(ir.behavior.type_models.iter().map(|t| format!("{}Fields", t.name)));
    let attach_names: Vec<_> = schema.models.iter().filter(|m| schema.attach_fields(m).is_some()).map(|m| &m.ir.name).collect();
    exported.extend(attach_names.iter().map(|n| format!("{n}Attach")));
    let all = exported.iter().map(|n| format!("    \"{n}\",\n")).collect::<String>();

    // -- models.py -------------------------------------------------------------------
    let mut py = String::new();
    writeln!(py, "# Generated from {source} by `python -m orm generate`. Do not edit.").unwrap();
    py.push_str(
        "#\n\
         # Runtime half of the generated module: the model classes are built from the compiled\n\
         # schema below. models.pyi carries the static types (columns, relation paths, typed\n\
         # inserts / updates and query sets) for editors and type checkers.\n\n",
    );
    let type_models = &ir.behavior.type_models;
    writeln!(py, "from orm import {}QuerySet, define{}\n", if type_models.is_empty() { "" } else { "Model, " }, if query_sets.is_empty() { "" } else { ", use_query_set" }).unwrap();
    for (model, (module, class)) in &model_methods {
        writeln!(py, "from {module} import {class} as _{model}Methods").unwrap();
    }
    if !model_methods.is_empty() {
        py.push('\n');
    }
    // the extensions some model uses, with the models that use each, in chain order
    let model_exts: Vec<(&str, Vec<(usize, &super::ExtensionCode)>)> = names.iter().map(|n| (*n, options.model_extensions(ir, n))).filter(|(_, e)| !e.is_empty()).collect();
    let used_exts: BTreeSet<usize> = model_exts.iter().flat_map(|(_, e)| e.iter().map(|(i, _)| *i)).chain(options.extensions.iter().enumerate().filter(|(_, e)| e.runtime_module.is_some() && !e.members.query_set.is_empty()).map(|(i, _)| i)).collect();
    let ext_base = options.ext_import.as_deref().unwrap_or("orm_ext");
    let mut ext_kwargs = String::new();
    if !used_exts.is_empty() {
        for i in &used_exts {
            if let Some(module) = &options.extensions[*i].runtime_module {
                writeln!(py, "import {module} as _ext{i}").unwrap();
            } else {
                writeln!(py, "from {ext_base} import {} as _ext{i}", options.extensions[*i].python_package()).unwrap();
            }
        }
        py.push('\n');
        let modules: Vec<String> = used_exts.iter().map(|i| format!("{:?}: _ext{i}", options.extensions[*i].members.extension)).collect();
        let members: Vec<String> = model_exts.iter().map(|(m, e)| format!("{m:?}: ({},)", e.iter().map(|(_, x)| format!("{:?}", x.members.extension)).collect::<Vec<_>>().join(", "))).collect();
        write!(ext_kwargs, ", extensions={{{}}}, members={{{}}}", modules.join(", "), members.join(", ")).unwrap();
    }
    if !model_methods.is_empty() {
        let classes: Vec<String> = model_methods.keys().map(|m| format!("{m:?}: _{m}Methods")).collect();
        write!(ext_kwargs, ", model_methods={{{}}}", classes.join(", ")).unwrap();
    }
    writeln!(py, "_SCHEMA = r\"\"\"\n{ir_json}\n\"\"\"\n").unwrap();
    #[cfg(feature = "reference-loading")]
    writeln!(py, "_models = define(_SCHEMA, module=__name__, required_capabilities=(\"reference-loading\",){ext_kwargs})").unwrap();
    #[cfg(not(feature = "reference-loading"))]
    writeln!(py, "_models = define(_SCHEMA, module=__name__{ext_kwargs})").unwrap();
    for (i, u) in adapters.uses.iter().enumerate() {
        writeln!(py, "from {} import {} as _fa{i}", u.module("python"), u.runtime.install).unwrap();
        writeln!(py, "_FA{i} = []").unwrap();
        for m in &schema.models {
            let fields: Vec<String> = m.field_adapters.iter().filter(|a| a.adapter == u.id).map(|a| {
                let name = &m.fields()[a.position].name;
                format!("{name:?}: ({}, {})", py_literal(&a.options), if a.nullable { "True" } else { "False" })
            }).collect();
            if fields.is_empty() { continue; }
            writeln!(py, "_FA{i}.append(_fa{i}(_models[{:?}], {{{}}}))", m.ir.name, fields.join(", ")).unwrap();
        }
        for function in &u.runtime.module_functions {
            writeln!(py, "\ndef {}(*args, **kwargs):\n    for adapter in _FA{i}:\n        adapter.{}(*args, **kwargs)", function.name, function.method).unwrap();
        }
        py.push('\n');
    }
    for n in enums.iter().chain(&type_names).chain(&names) {
        writeln!(py, "{n} = _models[\"{n}\"]").unwrap();
    }
    if !type_names.is_empty() {
        let dicts: Vec<String> = type_names.iter().flat_map(|n| [format!("{n}Input"), format!("{n}Patch")]).collect();
        writeln!(py, "{} = dict", dicts.join(" = ")).unwrap();
    }
    if !query_sets.is_empty() {
        py.push_str("\n# The user's query-set classes, imported on first use of `Model.objects`.\n");
        for (model, (module, class, _)) in &query_sets {
            writeln!(py, "use_query_set({model}, \"{module}:{class}\")").unwrap();
        }
    }
    py.push_str("\n# Typed per model in models.pyi; plain aliases at runtime so the names can be imported.\n");
    let qs: Vec<String> = names.iter().map(|n| format!("{n}QuerySet")).collect();
    if !qs.is_empty() { writeln!(py, "{} = QuerySet", qs.join(" = ")).unwrap(); }
    // An abstract model has no runtime model; the name only has to import.
    let type_stubs: Vec<String> = type_models.iter().map(|t| format!("{}Fields", t.name)).collect();
    if !type_stubs.is_empty() { writeln!(py, "{} = Model", type_stubs.join(" = ")).unwrap(); }
    let mut dicts: Vec<String> =
        names.iter().flat_map(|n| [format!("{n}Insert"), format!("{n}Update"), format!("{n}UpdateRow")]).collect();
    dicts.extend(attach_names.iter().map(|n| format!("{n}Attach")));
    if !dicts.is_empty() { writeln!(py, "{} = dict\n", dicts.join(" = ")).unwrap(); }
    writeln!(py, "__all__ = [\n{all}]").unwrap();

    // -- models.pyi ------------------------------------------------------------------
    let mut used: BTreeSet<&str> = BTreeSet::new();
    for m in &schema.models {
        for f in m.fields() {
            let t = base_type(f);
            for (word, import) in
                [("datetime", "datetime"), ("date", "date"), ("UUID", "UUID"), ("Any", "Any"), ("Decimal", "Decimal")]
            {
                if t.split(|c: char| !c.is_alphanumeric() && c != '_').any(|w| w == word) {
                    used.insert(import);
                }
            }
        }
    }
    for f in type_models.iter().flat_map(|t| &t.fields) {
        let t = base_type(f);
        for (word, import) in [("datetime", "datetime"), ("date", "date"), ("UUID", "UUID"), ("Any", "Any"), ("Decimal", "Decimal")] {
            if t.split(|c: char| !c.is_alphanumeric() && c != '_').any(|w| w == word) {
                used.insert(import);
            }
        }
    }
    for t in &ir.types {
        for sf in &t.fields {
            let word = sub_element(sf);
            for (w, import) in [("datetime", "datetime"), ("date", "date"), ("UUID", "UUID"), ("Any", "Any"), ("Decimal", "Decimal")] {
                if word.split(|c: char| !c.is_alphanumeric() && c != '_').any(|x| x == w) {
                    used.insert(import);
                }
            }
        }
    }
    let mut body = String::new();
    for e in &schema.enums {
        writeln!(body, "{}\n", section(&e.name)).unwrap();
        let base = if e.storage == EnumStorage::Int { "IntEnum" } else { "StrEnum" };
        used.insert(if e.storage == EnumStorage::Int { "IntEnum" } else { "StrEnum" });
        writeln!(body, "class {}({base}):", e.name).unwrap();
        for v in &e.values {
            writeln!(body, "    {} = {}", v.name, v.value).unwrap();
        }
        body.push('\n');
    }
    for t in &ir.types {
        write_type(&mut body, ir, t);
    }
    for t in type_models {
        writeln!(body, "{}\n", section(&format!("{}Fields", t.name))).unwrap();
        writeln!(body, "class {}Fields({}):", t.name, type_model_bases(type_models, &t.name)).unwrap();
        let inherited = type_model_lines(type_models, &t.name, false);
        let own = type_model_lines(type_models, &t.name, true);
        for (name, line) in &own {
            let clash = inherited.iter().any(|(n, l)| n == name && l != line);
            writeln!(body, "    {line}{}", if clash { "  # type: ignore[assignment]" } else { "" }).unwrap();
        }
        if own.is_empty() { body.push_str("    ...\n"); }
        body.push('\n');
    }
    for m in &schema.models {
        let name = &m.ir.name;
        let groups = groups(m);
        writeln!(body, "{}\n", section(name)).unwrap();
        let exts = options.model_extensions(ir, name);
        let instances: String = exts.iter().rev().map(|(i, _)| format!("_ext{i}Instance, ")).collect();
        let methods = if model_methods.contains_key(name.as_str()) { format!("_{name}Methods, ") } else { String::new() };
        let bases = type_model_bases(type_models, name);
        writeln!(body, "class {name}({instances}{methods}{}):", if bases == "Model" { bases } else { format!("{bases}, Model") }).unwrap();
        for a in &m.field_adapters {
            let Some((_, u)) = adapters.of(&a.adapter) else { continue };
            for member in &u.runtime.members {
                writeln!(body, "    {}", orm_contracts::extension::expand_adapter_template(member, "python", &m.fields()[a.position].name)).unwrap();
            }
        }
        let inherited = type_model_lines(type_models, name, false);
        for f in m.fields().iter().filter(|f| f.nested.is_none()) {
            let line = format!("{}: f.{}[{}]", f.name, field_class(f), field_type(m, f));
            let clash = inherited.iter().any(|(n, l)| *n == f.name && *l != line);
            writeln!(body, "    {line}{}", if clash { "  # type: ignore[assignment]" } else { "" }).unwrap();
        }
        for g in &groups {
            writeln!(body, "    {}: f.Nested[{}, _{}Ref]", g.name, or_none(g.ty.to_owned(), g.nullable), g.ty).unwrap();
        }
        if !m.ir.relations.is_empty() {
            body.push('\n');
        }
        for r in &m.ir.relations {
            match r.kind {
                RelKind::Many if r.through.is_some() => {
                    writeln!(body, "    {}: f.ManyToMany[{}, _{}Path]", r.name, r.target, r.target).unwrap()
                }
                RelKind::Many => writeln!(body, "    {}: f.HasMany[{}, _{}Path]", r.name, r.target, r.target).unwrap(),
                RelKind::One if !r.foreign_key => {
                    writeln!(body, "    {}: f.HasOne[{} | None, _{}Path]", r.name, r.target, r.target).unwrap()
                }
                RelKind::One => {
                    let nullable = r.from.iter().map(|f| m.field(f).map(|f| f.nullable)).collect::<Result<Vec<_>, _>>()?.contains(&true);
                    let nullable = nullable || super::reference_target_filtered(schema, &r.target);
                    let t = if nullable { format!("{} | None", r.target) } else { r.target.clone() };
                    writeln!(body, "    {}: f.BelongsTo[{t}, _{}Path]", r.name, r.target).unwrap();
                }
            }
        }
        let objects = if !exts.is_empty() || query_sets.contains_key(name.as_str()) { format!("_{name}Objects") } else { format!("{name}QuerySet") };
        writeln!(body, "\n    objects: ClassVar[{objects}]").unwrap();
        let indexes = if super::indexes_free(m, str::to_owned) { crate::migrate::model::unique_index_names(m) } else { vec![] };
        if !indexes.is_empty() {
            writeln!(body, "    indexes: ClassVar[_{name}Indexes]").unwrap();
        }
        body.push('\n');
        if let keys @ [_, _, ..] = m.key_fields().as_slice() {
            let parts: Vec<String> = keys.iter().map(|f| value_type(f)).collect();
            writeln!(body, "    @property\n    def pk(self) -> tuple[{}]: ...\n", parts.join(", ")).unwrap();
        }
        if exts.iter().any(|(i, _)| !options.extensions[*i].members.bound_query_set.is_empty()) {
            writeln!(body, "    def qs(self) -> _{name}BoundObjects: ...  # type: ignore[override]\n").unwrap();
        }
        writeln!(
            body,
            "    async def update(self, **values: Unpack[{name}Update]) -> None: ...  # type: ignore[override]\n"
        )
        .unwrap();

        writeln!(body, "class _{name}Path(RelationPath[{name}]):").unwrap();
        for f in m.fields().iter().filter(|f| f.nested.is_none()) {
            writeln!(body, "    {}: ColumnRef[{}]", f.name, field_type(m, f)).unwrap();
        }
        for g in &groups {
            writeln!(body, "    {}: _{}Ref", g.name, g.ty).unwrap();
        }
        for r in &m.ir.relations {
            writeln!(body, "    {}: _{}Path", r.name, r.target).unwrap();
        }
        writeln!(body, "    @property\n    def objects(self) -> {objects}: ...").unwrap();
        body.push('\n');
        if !indexes.is_empty() {
            writeln!(body, "class _{name}Indexes:").unwrap();
            for index in indexes.iter().filter(|n| super::is_python_ident(n)) {
                writeln!(body, "    {index}: IndexRef").unwrap();
            }
            writeln!(body, "    def __getitem__(self, name: str) -> IndexRef: ...").unwrap();
            body.push('\n');
        }

        // insert / update shapes; a to-one relation can be given instead of its key
        let belongs = |field: &str| {
            m.ir.relations.iter().find(|r| r.kind == RelKind::One && r.foreign_key && r.from.iter().any(|f| f == field))
        };
        // a relation of several key fields is listed once, after its last one
        let last = |r: &RelationIr, field: &str| r.from.last().is_some_and(|f| f == field);
        writeln!(body, "class {name}Insert(TypedDict):").unwrap();
        for f in m.fields().iter().filter(|f| f.nested.is_none()) {
            let t = input_type(m, f);
            match belongs(&f.name) {
                Some(r) => {
                    let target = if f.nullable { format!("{} | None", r.target) } else { r.target.clone() };
                    if !f.nullable && !has_server_value_in(schema, m, f) && f.client_default.is_none() {
                        writeln!(body, "    # One of {} / {} is required (checked at runtime).", f.name, r.name).unwrap();
                    }
                    writeln!(body, "    {}: NotRequired[{t}]", f.name).unwrap();
                    if last(r, &f.name) { writeln!(body, "    {}: NotRequired[{target}]", r.name).unwrap(); }
                }
                None if f.nullable || has_server_value_in(schema, m, f) || f.client_default.is_some() => writeln!(body, "    {}: NotRequired[{t}]", f.name).unwrap(),
                None => writeln!(body, "    {}: {t}", f.name).unwrap(),
            }
        }
        for g in &groups {
            let t = or_none(format!("{} | {}Input", g.ty, g.ty), g.nullable);
            if g.nullable || !type_requires(ir, g.ty) { writeln!(body, "    {}: NotRequired[{t}]", g.name).unwrap(); } else { writeln!(body, "    {}: {t}", g.name).unwrap(); }
        }
        if attach_names.contains(&&m.ir.name) {
            writeln!(body, "\nclass {name}Attach(TypedDict):").unwrap();
            let local: Vec<_> = schema.attach_fields(m).unwrap_or_default().into_iter().map(|p| &m.fields()[p]).collect();
            if local.is_empty() { writeln!(body, "    pass").unwrap(); }
            for field in local {
                let ty = value_type(field);
                let ty = if field.nullable || has_server_value_in(schema, m, field) { format!("NotRequired[{ty}]") } else { ty };
                writeln!(body, "    {}: {ty}", field.name).unwrap();
            }
        }
        writeln!(body, "\nclass {name}Update(TypedDict, total=False):").unwrap();
        for (position, f) in m.fields().iter().enumerate().filter(|(_, f)| f.nested.is_none()) {
            let t = input_type(m, f);
            let expression = !m.hooks.as_ref().is_some_and(|h| h.required.contains(&position));
            let expression = expression && m.adapter_for(&f.name).is_none();
            if expression { writeln!(body, "    {}: {t} | Expression[{t}]", f.name).unwrap(); }
            else { writeln!(body, "    {}: {t}", f.name).unwrap(); }
            if let Some(r) = belongs(&f.name).filter(|r| last(r, &f.name)) {
                let target = if f.nullable { format!("{} | None", r.target) } else { r.target.clone() };
                writeln!(body, "    {}: {target}", r.name).unwrap();
            }
        }
        for g in &groups {
            writeln!(body, "    {}: {}", g.name, or_none(format!("{} | {}Patch", g.ty, g.ty), g.nullable)).unwrap();
        }
        // update_many rows: the primary key plus plain values (no expressions)
        writeln!(body, "\nclass {name}UpdateRow(TypedDict, total=False):").unwrap();
        for f in m.fields().iter().filter(|f| f.nested.is_none()) {
            let t = field_type(m, f);
            if f.primary_key {
                writeln!(body, "    {}: Required[{t}]", f.name).unwrap();
            } else {
                writeln!(body, "    {}: {t}", f.name).unwrap();
            }
            if let Some(r) = belongs(&f.name).filter(|r| last(r, &f.name)) {
                let target = if f.nullable { format!("{} | None", r.target) } else { r.target.clone() };
                writeln!(body, "    {}: {target}", r.name).unwrap();
            }
        }
        for g in &groups {
            writeln!(body, "    {}: {}", g.name, or_none(format!("{} | {}Input", g.ty, g.ty), g.nullable)).unwrap();
        }
        // get_or_insert lookup and defaults: plain values, none required
        writeln!(body, "\nclass {name}Lookup(TypedDict, total=False):").unwrap();
        for f in m.fields().iter().filter(|f| f.nested.is_none()) {
            writeln!(body, "    {}: {}", f.name, field_type(m, f)).unwrap();
            if let Some(r) = belongs(&f.name).filter(|r| last(r, &f.name)) {
                writeln!(body, "    {}: {}", r.name, r.target).unwrap();
            }
        }
        for g in &groups {
            writeln!(body, "    {}: {}", g.name, or_none(format!("{} | {}Input", g.ty, g.ty), g.nullable)).unwrap();
        }
        writeln!(body, "\nclass {name}QuerySet(QuerySet[{name}]):").unwrap();
        let default_names = m.read_defaults.names();
        if !default_names.is_empty() {
            let literal = default_names.iter().map(|n| format!("{n:?}")).collect::<Vec<_>>().join(", ");
            writeln!(body, "    def without_defaults(self, *names: Literal[{literal}]) -> Self: ...  # type: ignore[override]").unwrap();
        }
        if attach_names.contains(&&m.ir.name) {
            let key_type = value_type(m.pk_field());
            writeln!(body, "    async def attach(self, parent_id: {key_type}, values: {name}Attach) -> {name}: ...  # type: ignore[override]").unwrap();
        }
        writeln!(
            body,
            "    def insert(self, **values: Unpack[{name}Insert]) -> InsertOne[{name}]: ...  # type: ignore[override]"
        )
        .unwrap();
        writeln!(
            body,
            "    @overload  # type: ignore[override]\n    def insert_many(self, rows: Iterable[{name}Insert], *, batch_size: int | None = None, \
             copy: Literal[False] = False) -> InsertMany[{name}]: ...\n    @overload\n    def insert_many(self, rows: Iterable[{name}Insert], \
             *, copy: Literal[True]) -> CopyInsert[{name}]: ..."
        )
        .unwrap();
        writeln!(
            body,
            "    async def get_or_insert(self, defaults: {name}Lookup | None = None, **lookup: Unpack[{name}Lookup]) \
             -> tuple[{name}, bool]: ...  # type: ignore[override]"
        )
        .unwrap();
        writeln!(
            body,
            "    def update(self, **values: Unpack[{name}Update]) -> Update[{name}]: ...  # type: ignore[override]"
        )
        .unwrap();
        writeln!(
            body,
            "    def update_many(self, rows: Iterable[{name}UpdateRow], *, batch_size: int | None = None) \
             -> UpdateMany[{name}]: ...  # type: ignore[override]\n"
        )
        .unwrap();
        if !exts.is_empty() || inherits(name) || generic(name) {
            // the user's class first, then the extensions, last in the chain first, as at runtime
            let params = if generic(name) { format!("[{name}]") } else { String::new() };
            let user = if query_sets.contains_key(name.as_str()) && !inherits(name) { format!("_{name}UserObjects{params}, ") } else { String::new() };
            let any = if inherits(name) { "    def __getattr__(self, name: str) -> Any: ...\n" } else { "" };
            let mixins: String = exts.iter().rev().map(|(i, _)| format!("_ext{i}QuerySet[{name}], ")).collect();
            let bound: String = exts
                .iter()
                .rev()
                .filter(|(i, _)| !options.extensions[*i].members.bound_query_set.is_empty())
                .map(|(i, _)| format!("_ext{i}BoundQuerySet[{name}], "))
                .collect();
            if bound.is_empty() && any.is_empty() {
                writeln!(body, "class _{name}Objects({user}{mixins}{name}QuerySet): ...\n").unwrap();
            } else if bound.is_empty() {
                writeln!(body, "class _{name}Objects({user}{mixins}{name}QuerySet):\n{any}").unwrap();
            } else {
                writeln!(body, "class _{name}Objects({user}{mixins}{name}QuerySet):").unwrap();
                body.push_str(any);
                writeln!(body, "    def of(self, objs: Iterable[{name}] | {name}) -> _{name}BoundObjects: ...\n").unwrap();
                writeln!(body, "class _{name}BoundObjects({bound}BoundQuerySet[{name}]): ...\n").unwrap();
            }
        }
    }

    let mut pyi = String::new();
    writeln!(pyi, "# Generated from {source} by `python -m orm generate`. Do not edit.").unwrap();
    pyi.push_str(
        "#\n\
         # Static half of the generated module. Each model gets:\n\
         #   * the model class: column descriptors (`User.email` is a ColumnRef[str] on the class,\n\
         #     a read-only str on an instance), relation descriptors and typed `update()`;\n\
         #   * a path class (`_UserPath`): what a relation to the model evaluates to on the class\n\
         #     side, so `User.posts.created_at` autocompletes and type-checks as ColumnRef[datetime];\n\
         #   * `UserInsert` / `UserUpdate` / `UserUpdateRow` / `UserLookup` TypedDicts: the row\n\
         #     shapes accepted by insert / update / update_many / get_or_insert;\n\
         #   * a query set class (`UserQuerySet`): typed `insert()`, `insert_many()`,\n\
         #     `get_or_insert()`, `update()`, `update_many()`.\n\n\
         from collections.abc import Iterable",
    );
    pyi.push_str(if body.contains("Sequence[") { ", Sequence\n" } else { "\n" });
    let dt: Vec<&str> = ["date", "datetime"].into_iter().filter(|d| used.contains(d)).collect();
    if !dt.is_empty() {
        writeln!(pyi, "from datetime import {}", dt.join(", ")).unwrap();
    }
    if used.contains("Decimal") {
        pyi.push_str("from decimal import Decimal\n");
    }
    let en: Vec<&str> = ["IntEnum", "StrEnum"].into_iter().filter(|d| used.contains(d)).collect();
    if !en.is_empty() {
        writeln!(pyi, "from enum import {}", en.join(", ")).unwrap();
    }
    // `Literal` and `overload` type `insert_many(..., copy=True)`.
    let typing = format!(
        "{}ClassVar, Literal, NotRequired, Required, TypedDict, overload",
        if used.contains("Any") || query_sets.keys().any(|m| inherits(m)) { "Any, " } else { "" },
    );
    writeln!(pyi, "from typing import {typing}").unwrap();
    if used.contains("UUID") {
        pyi.push_str("from uuid import UUID\n");
    }

    pyi.push_str(
        "\nfrom typing_extensions import Self, Unpack\n\n\
         from orm import ColumnRef, CopyInsert, Expression, InsertMany, InsertOne, Model, ",
    );
    if body.contains("-> IndexRef: ...") {
        pyi.push_str("IndexRef, ");
    }
    for name in ["NestedRef", "NestedValue"] {
        if body.contains(&format!("({name})")) {
            write!(pyi, "{name}, ").unwrap();
        }
    }
    pyi.push_str("QuerySet, RelationPath, Update, UpdateMany\nfrom orm import fields as f\n\n");
    for (model, (module, class)) in &model_methods {
        writeln!(pyi, "from {module} import {class} as _{model}Methods").unwrap();
    }
    for (model, (module, class, _)) in query_sets.iter().filter(|(m, _)| !inherits(m)) {
        let alias = if (used_exts.is_empty() || options.model_extensions(ir, model).is_empty()) && !generic(model) { "Objects" } else { "UserObjects" };
        writeln!(pyi, "from {module} import {class} as _{model}{alias}").unwrap();
    }
    if used_exts.iter().any(|i| !options.extensions[*i].members.bound_query_set.is_empty()) {
        pyi.push_str("from orm import BoundQuerySet\n");
    }
    for i in &used_exts {
        let e = &options.extensions[*i];
        if e.members.query_set.is_empty() { continue; }
        let bound = if e.members.bound_query_set.is_empty() { String::new() } else { format!(", {} as _ext{i}BoundQuerySet", e.members.bound_query_set) };
        let module = e.runtime_module.clone().unwrap_or_else(|| format!("{ext_base}.{}", e.python_package()));
        writeln!(pyi, "from {module} import {} as _ext{i}QuerySet, {} as _ext{i}Instance{bound}", e.members.query_set, e.members.instance).unwrap();
    }
    if !query_sets.is_empty() || !used_exts.is_empty() || !model_methods.is_empty() {
        pyi.push('\n');
    }
    for u in &adapters.uses {
        for line in &u.runtime.imports { writeln!(pyi, "{line}").unwrap(); }
    }
    for u in &adapters.uses {
        for function in &u.runtime.module_functions { writeln!(pyi, "\ndef {}{}: ...", function.name, function.stub).unwrap(); }
    }
    if !adapters.uses.is_empty() { pyi.push('\n'); }
    pyi.push_str(&body);
    writeln!(pyi, "__all__ = [\n{all}]").unwrap();
    Ok(Generated { module: py, stub: pyi })
}

/// A module exposing only declarations owned by one source file. All modules load
/// the same generated runtime by path, so even circular relations share classes.
pub fn facade(unit: &crate::dsl::SchemaUnit, shared_path: &str, stub_import: &str, schema: &Schema) -> Generated {
    let mut names = unit.enums.clone();
    for name in &unit.types {
        names.extend([name.clone(), format!("{name}Input"), format!("{name}Patch")]);
    }
    for name in &unit.models {
        names.extend([name.clone(), format!("{name}Insert"), format!("{name}Update"), format!("{name}UpdateRow"), format!("{name}QuerySet")]);
        if schema.models.iter().any(|m| m.ir.name == *name && schema.attach_fields(m).is_some()) { names.push(format!("{name}Attach")); }
    }
    let path = serde_json::to_string(shared_path).unwrap();
    let exports = serde_json::to_string(&names).unwrap();
    let mut module = format!(r#"# Generated schema module. Do not edit.
import importlib.util as _util
from pathlib import Path as _Path
import sys as _sys

_path = (_Path(__file__).parent / {path}).resolve()
_key = '_orm_schema:' + str(_path)
if _key not in _sys.modules:
    _spec = _util.spec_from_file_location(_key, _path)
    _module = _util.module_from_spec(_spec)
    _sys.modules[_key] = _module
    try:
        _spec.loader.exec_module(_module)
    except BaseException:
        del _sys.modules[_key]
        raise
_shared = _sys.modules[_key]

__all__ = {exports}
"#);
    let mut stub = String::from("# Generated schema module. Do not edit.\n");
    for name in &names {
        writeln!(module, "{name} = _shared.{name}").unwrap();
        writeln!(stub, "from {stub_import} import {name} as {name}").unwrap();
    }
    writeln!(stub, "\n__all__ = {exports}").unwrap();
    Generated { module, stub }
}
