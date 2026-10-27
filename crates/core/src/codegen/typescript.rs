//! TypeScript module generation: one `models.ts` with the runtime models and their
//! static types.
//!
//! The module embeds the compiled schema IR and builds the models from it with the
//! runtime's `define()`, like the Python module. Per model it declares:
//!
//! * `User` (instance type): its columns as read-only properties (camelCase), its to-many
//!   relations as query sets (`posts: RelatedSet<PostSpec, ...>`) and `update()` /
//!   `delete()` / `qs()`. To-one relations are absent: a query that loads them
//!   (`load()`) adds them to its rows' type;
//! * `UserInsert` / `UserUpdate` / `UserUpdateRow`: what `insert()`, `update()` and
//!   `updateMany()` take (a required foreign key is "the key or the related row");
//! * `UserFields` / `UserPath`: the columns and relation paths, so `User.posts.createdAt`
//!   is a `Column<Date, "User" | "*many", "createdAt", [Hop<"posts", ...>]>` (the field
//!   name and the hops let `load()` narrow the rows' type);
//! * `UserSpec` tying them together, and the model object `User` (`UserModel`).
//!
//! Field and relation names are camelCase (`author_id` -> `authorId`), the same function
//! as the runtime's `camel()`.

use std::collections::BTreeSet;
use std::fmt::Write;

use crate::ir::{ColType, EnumStorage, FieldIr, RelKind, RelationIr, SchemaIr, TypeFieldIr, TypeIr};
use super::{groups, has_server_value, sub_has_default, type_requires};
use crate::schema::{Model, Schema};

const WIDTH: usize = 89;

fn section(name: &str) -> String {
    let head = format!("// -- {name} ");
    format!("{head}{}", "-".repeat(WIDTH.saturating_sub(head.len())))
}

pub use crate::behavior::camel;

use crate::behavior::RESERVED_MEMBERS as RESERVED;

fn element_type(f: &FieldIr) -> String {
    if let Some(h) = f.hints.get("typescript") {
        return h.clone();
    }
    if let Some(e) = &f.enum_name {
        return e.clone();
    }
    match f.ty {
        ColType::BigInt => "bigint",
        ColType::Int | ColType::Float => "number",
        ColType::Bool => "boolean",
        ColType::String | ColType::Text | ColType::Uuid => "string",
        ColType::DateTime | ColType::Date => "Date",
        ColType::Json => "JsonValue",
        ColType::Decimal => "Decimal",
    }
    .to_owned()
}

/// The type a column reads as.
fn value_type(f: &FieldIr) -> String {
    let t = element_type(f);
    let t = if f.array && f.enum_name.is_some() && f.hints.contains_key("typescript") { format!("({t})[]") }
        else if f.array && !f.hints.contains_key("typescript") { format!("{t}[]") } else { t };
    if f.nullable {
        format!("{t} | null")
    } else {
        t
    }
}

/// The fields of a type model, those of the type models it extends first; a redeclared field replaces the inherited one.
fn type_model_fields<'a>(type_models: &'a [crate::ir::TypeModel], name: &str) -> Vec<&'a FieldIr> {
    let mut out: Vec<&FieldIr> = Vec::new();
    for t in type_models.iter().filter(|t| t.extended_by.iter().any(|c| c == name)) {
        for f in type_model_fields(type_models, &t.name) {
            out.retain(|g| g.name != f.name);
            out.push(f);
        }
    }
    if let Some(t) = type_models.iter().find(|t| t.name == name) {
        for f in t.fields.iter().filter(|f| f.nested.is_none()) {
            out.retain(|g| g.name != f.name);
            out.push(f);
        }
    }
    out
}

/// The type a column is written from (`bigint` columns take numbers, ...).
fn input_type(f: &FieldIr) -> String {
    format!("In<{}>", value_type(f))
}

fn quote(s: &str) -> String {
    serde_json::to_string(s).expect("a string serializes")
}

fn sub_element(sf: &TypeFieldIr) -> String {
    match (&sf.type_name, &sf.leaf) {
        (Some(n), _) => n.clone(),
        (_, Some(l)) => element_type(l),
        _ => "unknown".into(),
    }
}

fn or_null(t: String, nullable: bool) -> String {
    if nullable { format!("{t} | null") } else { t }
}

/// The type of a subfield on a value.
fn sub_value(sf: &TypeFieldIr) -> String {
    let t = sub_element(sf);
    or_null(if sf.list { format!("readonly ({t})[]") } else { t }, sf.nullable)
}

/// What a caller may give for a subfield; `patch` leaves a list out (it cannot be assigned part by part).
fn sub_input(sf: &TypeFieldIr, patch: bool) -> Option<String> {
    let one = match &sf.type_name {
        Some(n) => format!("{n} | {n}{}", if patch { "Patch" } else { "Input" }),
        None => format!("In<{}>", sub_element(sf)),
    };
    if sf.list {
        return if patch { None } else { Some(or_null(format!("readonly ({one})[]"), sf.nullable)) };
    }
    Some(or_null(one, sf.nullable))
}

fn sub_optional(ir: &SchemaIr, sf: &TypeFieldIr) -> bool {
    sf.nullable || sub_has_default(sf) || sf.type_name.as_deref().is_some_and(|n| !type_requires(ir, n))
}

/// The value type, input shapes, reference type and value class of one `type` block.
fn write_type(body: &mut String, ir: &SchemaIr, t: &TypeIr) {
    let name = &t.name;
    writeln!(body, "{}\n", section(name)).unwrap();
    writeln!(body, "/** Immutable `{name}` value, compared with `equals()`. */\nexport interface {name} {{").unwrap();
    for sf in &t.fields {
        writeln!(body, "  readonly {}: {};", sf.name, sub_value(sf)).unwrap();
    }
    writeln!(body, "  equals(other: unknown): boolean;\n  toJSON(): Record<string, unknown>;\n}}\n").unwrap();
    writeln!(body, "export interface {name}Input {{").unwrap();
    for sf in &t.fields {
        let q = if sub_optional(ir, sf) { "?" } else { "" };
        writeln!(body, "  readonly {}{q}: {};", sf.name, sub_input(sf, false).unwrap_or_default()).unwrap();
    }
    writeln!(body, "}}\n").unwrap();
    writeln!(body, "export interface {name}Patch {{").unwrap();
    for sf in &t.fields {
        if let Some(one) = sub_input(sf, true) {
            writeln!(body, "  readonly {}?: {one};", sf.name).unwrap();
        }
    }
    writeln!(body, "}}\n").unwrap();
    writeln!(body, "export interface {name}Class {{\n  new (values: {name}Input): {name};\n}}\n").unwrap();
    writeln!(
        body,
        "/** The columns of a `{name}` member: the whole value, and one column per subfield. */\nexport type {name}Ref<S extends string, H extends readonly Hop[], O extends boolean, N extends string, X> = Column<O extends true ? X | null : X, S, N, H> & {{"
    )
    .unwrap();
    for sf in &t.fields {
        match &sf.type_name {
            Some(n) => writeln!(body, "  readonly {}: {n}Ref<S, H, O, {}, {} | (null extends X ? null : never)>;", sf.name, quote(&sf.name), sub_value(sf)).unwrap(),
            None => {
                let v = sub_value(sf);
                writeln!(body, "  readonly {}: Column<O extends true ? {v} | null : null extends X ? {v} | null : {v}, S, {}, H>;", sf.name, quote(&sf.name)).unwrap()
            }
        }
    }
    writeln!(body, "}};\n").unwrap();
}

/// The to-one relation of `m` whose key holds field `field` (a foreign key on `m`).
fn belongs_to<'a>(m: &'a Model, field: &str) -> Option<&'a RelationIr> {
    m.ir.relations.iter().find(|r| r.kind == RelKind::One && r.foreign_key && r.from.iter().any(|f| f == field))
}

/// Whether `field` is the last key field of `r`; a relation of several fields is listed once.
fn last_key(r: &RelationIr, field: &str) -> bool {
    r.from.last().is_some_and(|f| f == field)
}

/// The hop kind of a relation, for loaded-relation types.
fn hop_kind(schema: &Schema, m: &Model, r: &RelationIr) -> Result<&'static str, String> {
    if r.kind == RelKind::One && super::reference_target_filtered(schema, &r.target) { return Ok("opt"); }
    Ok(match r.kind {
        RelKind::Many if r.through.is_some() => "m2m",
        RelKind::Many => "many",
        RelKind::One if !r.foreign_key => "opt",
        RelKind::One if r.from.iter().map(|f| m.field(f).map(|f| f.nullable)).collect::<Result<Vec<_>, _>>()?.contains(&true) => "opt",
        RelKind::One => "one",
    })
}

/// A to-one relation as a value: the related row (any object with its key).
fn related_ref(schema: &Schema, r: &RelationIr) -> Result<String, String> {
    let target = schema.model(schema.model_idx(&r.target)?);
    let keys = r.to.iter().map(|k| Ok(format!("readonly {}: {}", camel(k), input_type(target.field(k)?)))).collect::<Result<Vec<_>, String>>()?;
    Ok(format!("{{ {} }}", keys.join("; ")))
}

/// Generates `models.ts` for `ir` (already validated as `schema`). `source` names the
/// schema file in the header comment; `runtime` is the module the runtime is imported
/// from (`"orm"`).
pub fn generate(ir: &SchemaIr, schema: &Schema, source: &str, runtime: &str) -> Result<String, String> {
    generate_with(ir, schema, source, runtime, &super::Options::default())
}

/// [`generate`] with generator options (the user's query-set classes).
pub fn generate_with(ir: &SchemaIr, schema: &Schema, source: &str, runtime: &str, options: &super::Options) -> Result<String, String> {
    let query_sets = options.query_sets(ir, schema, '#')?;
    // one namespace import per module, read lazily so an import cycle with it is safe
    let modules: Vec<&str> = query_sets.values().map(|(module, ..)| *module).collect::<BTreeSet<_>>().into_iter().collect();
    let query_set = |model: &str| query_sets.get(model).map(|(module, class, _)| format!("_q{}.{class}", modules.iter().position(|m| m == module).unwrap()));
    let model_methods = options.model_methods(ir, schema, '#')?;
    let adapters = super::Adapters::resolve(options, ir, "typescript")?;
    let field_type = |m: &Model, f: &FieldIr| adapters.value(m, f, "null").unwrap_or_else(|| value_type(f));
    let field_input = |m: &Model, f: &FieldIr| adapters.input(m, f, "null").unwrap_or_else(|| input_type(f));
    let method_modules: Vec<&str> = model_methods.values().map(|(module, _)| *module).collect::<BTreeSet<_>>().into_iter().collect();
    let methods_of = |model: &str| model_methods.get(model).map(|(module, class)| format!("_m{}.{class}", method_modules.iter().position(|m| m == module).unwrap()));
    // A class that an abstract model declares takes the model's spec as its type parameter (`class UserQueries<S extends AbstractUserSpec>`).
    let generic = |model: &str| super::declares_generic(ir, &query_sets, model);
    let query_type = |model: &str, spec: &str| query_set(model).map(|class| if generic(model) { format!("{class}<{spec}>") } else { class });
    let has_queries = |model: &str| query_set(model).is_some() || options.extensions.iter().any(|e| e.applies(ir, model));
    let ext_base = options.ext_import.as_deref().unwrap_or("./orm-ext");
    // names: camelCase must stay unique and clear of the runtime's own
    for m in &schema.models {
        let mut seen = BTreeSet::new();
        let own = groups(m);
        let names = m.fields().iter().filter(|f| f.nested.is_none()).map(|f| f.name.as_str()).chain(own.iter().map(|g| g.name)).chain(m.ir.relations.iter().map(|r| r.name.as_str()));
        for n in names {
            let c = camel(n);
            if RESERVED.contains(&c.as_str()) {
                return Err(format!("{}.{n}: the name {c} is reserved in TypeScript models; rename it", m.ir.name));
            }
            if !seen.insert(c.clone()) {
                return Err(format!("{} has two members named {c} in TypeScript (camelCase)", m.ir.name));
            }
        }
    }
    let ir_json = super::embedded_schema_json(ir)?;
    let mut used: BTreeSet<&str> = BTreeSet::new();
    let mut used_index = false;
    let mut body = String::new();

    for e in &schema.enums {
        writeln!(body, "{}\n", section(&e.name)).unwrap();
        writeln!(body, "export const {} = {{", e.name).unwrap();
        for v in &e.values {
            let value = match e.storage {
                EnumStorage::Int => v.value.to_string(),
                _ => quote(v.value.as_str().unwrap_or(&v.name)),
            };
            writeln!(body, "  {}: {value},", v.name).unwrap();
        }
        writeln!(body, "}} as const;").unwrap();
        writeln!(body, "export type {0} = (typeof {0})[keyof typeof {0}];\n", e.name).unwrap();
    }

    for t in &ir.types {
        write_type(&mut body, ir, t);
        writeln!(body, "export const {0} = nested[{1}] as unknown as {0}Class;\n", t.name, quote(&t.name)).unwrap();
        for sf in t.fields.iter().filter_map(|sf| sf.leaf.as_ref()) {
            for (word, import) in [("Decimal", "Decimal"), ("JsonValue", "JsonValue")] {
                if element_type(sf) == word { used.insert(import); }
            }
        }
    }

    for t in &ir.behavior.type_models {
        let name = &t.name;
        let fields = type_model_fields(&ir.behavior.type_models, name);
        writeln!(body, "{}\n", section(name)).unwrap();
        writeln!(body, "/** The column values of the abstract model {name}; a model that extends it has them. */").unwrap();
        writeln!(body, "export interface {name}Data {{").unwrap();
        for f in &fields {
            let ty = value_type(f);
            for (word, import) in [("Decimal", "Decimal"), ("JsonValue", "JsonValue")] {
                if ty.split(|c: char| !c.is_alphanumeric()).any(|w| w == word) {
                    used.insert(import);
                }
            }
            writeln!(body, "  readonly {}: {ty};", camel(&f.name)).unwrap();
        }
        writeln!(body, "}}\n").unwrap();
        writeln!(body, "/** The bound of a query-set class that the abstract model {name} declares: `class Queries<S extends {name}Spec>`. */").unwrap();
        writeln!(body, "export interface {name}Spec extends ModelSpec {{\n  readonly row: {name}Data;\n  readonly data: {name}Data;\n}}\n").unwrap();
    }

    for m in &schema.models {
        let name = &m.ir.name;
        let pk = m.pk_field();
        let groups = groups(m);
        writeln!(body, "{}\n", section(name)).unwrap();
        for f in m.fields() {
            let t = field_type(m, f);
            for (word, import) in [("Decimal", "Decimal"), ("JsonValue", "JsonValue")] {
                if t.split(|c: char| !c.is_alphanumeric()).any(|w| w == word) {
                    used.insert(import);
                }
            }
        }

        // the column values
        writeln!(body, "/** The column values of a {name} row. */").unwrap();
        writeln!(body, "export interface {name}Data {{").unwrap();
        for f in m.fields().iter().filter(|f| f.nested.is_none()) {
            let optional = m.read_flags.fields.as_ref().is_some_and(|fields| !fields.contains(&f.name));
            writeln!(body, "  readonly {}{}: {};", camel(&f.name), if optional { "?" } else { "" }, field_type(m, f)).unwrap();
        }
        for g in &groups {
            writeln!(body, "  readonly {}: {};", camel(g.name), or_null(g.ty.to_owned(), g.nullable)).unwrap();
        }
        writeln!(body, "}}\n").unwrap();

        // the instance
        writeln!(
            body,
            "/** A {name} row. To-one relations are typed on rows of queries that load them. */"
        )
        .unwrap();
        let exts = options.model_extensions(ir, name);
        let mut instances: String = exts.iter().rev().map(|(i, e)| format!(", _e{i}.{}", e.members.instance)).collect();
        if let Some(class) = methods_of(name) {
            write!(instances, ", {class}").unwrap();
        }
        writeln!(body, "export interface {name} extends {name}Data, Instance<{name}Spec>{instances} {{").unwrap();
        for a in &m.field_adapters {
            let Some((_, u)) = adapters.of(&a.adapter) else { continue };
            for member in &u.runtime.members {
                writeln!(body, "  {}", orm_contracts::extension::expand_adapter_template(member, "typescript", &m.fields()[a.position].name)).unwrap();
            }
        }
        for r in &m.ir.relations {
            match r.kind {
                RelKind::Many if r.through.is_some() => {
                    let queries = if has_queries(&r.target) { format!(" & QueriesOf<{}Spec>", r.target) } else { String::new() };
                    writeln!(body, "  readonly {}: ManyRelatedSet<{}Spec>{queries};", camel(&r.name), r.target).unwrap()
                }
                RelKind::Many => {
                    // the key and the to-one relation back, filled in by insert()
                    let target = schema.model(schema.model_idx(&r.target)?);
                    let mut link: Vec<String> = r.to.iter().map(|k| quote(&camel(k))).collect();
                    if let Some(back) = target
                        .ir
                        .relations
                        .iter()
                        .find(|b| b.kind == RelKind::One && b.foreign_key && b.from == r.to && b.target == *name)
                    {
                        link.push(quote(&camel(&back.name)));
                    }
                    let queries = if has_queries(&r.target) { format!(" & QueriesOf<{}Spec>", r.target) } else { String::new() };
                    writeln!(body, "  readonly {}: RelatedSet<{}Spec, {}>{queries};", camel(&r.name), r.target, link.join(" | ")).unwrap()
                }
                RelKind::One => {}
            }
        }
        writeln!(body, "}}\n").unwrap();

        // insert: optional when the database fills it in; a required foreign key is the
        // key or the related row, exactly one of them
        writeln!(body, "export type {name}Insert = {{").unwrap();
        let mut one_of = vec![];
        for (position, f) in m.fields().iter().enumerate().filter(|(_, f)| f.nested.is_none()) {
            #[cfg(feature = "composition")]
            if m.native.computed().contains(&position) { continue; }
            #[cfg(not(feature = "composition"))]
            let _ = position;
            let optional = f.nullable || has_server_value(f) || f.client_default.is_some();
            match belongs_to(m, &f.name) {
                Some(r) if !optional && r.from.len() == 1 => one_of.push((f, r)),
                Some(r) => {
                    // several key fields: the key or the relation is checked at runtime
                    let null = if f.nullable { " | null" } else { "" };
                    writeln!(body, "  {}?: {};", camel(&f.name), field_input(m, f)).unwrap();
                    if last_key(r, &f.name) {
                        writeln!(body, "  {}?: {}{null};", camel(&r.name), related_ref(schema, r)?).unwrap();
                    }
                }
                None => {
                    let q = if optional { "?" } else { "" };
                    writeln!(body, "  {}{q}: {};", camel(&f.name), field_input(m, f)).unwrap();
                }
            }
        }
        for g in &groups {
            let t = or_null(format!("{} | {}Input", g.ty, g.ty), g.nullable);
            let q = if g.nullable || !type_requires(ir, g.ty) { "?" } else { "" };
            writeln!(body, "  {}{q}: {t};", camel(g.name)).unwrap();
        }
        write!(body, "}}").unwrap();
        for (f, r) in one_of {
            let (key, rel) = (camel(&f.name), camel(&r.name));
            write!(
                body,
                " & (\n  | {{ {key}: {}; {rel}?: never }}\n  | {{ {rel}: {}; {key}?: never }}\n)",
                field_input(m, f),
                related_ref(schema, r)?
            )
            .unwrap();
        }
        writeln!(body, ";\n").unwrap();

        // update: plain values or expressions over the model
        writeln!(body, "export interface {name}Update {{").unwrap();
        for (position, f) in m.fields().iter().enumerate().filter(|(_, f)| f.nested.is_none()) {
            #[cfg(feature = "composition")]
            if m.native.computed().contains(&position) { continue; }
            let t = field_type(m, f);
            let expression = !m.hooks.as_ref().is_some_and(|h| h.required.contains(&position));
            if expression {
                writeln!(body, "  {}?: {} | Expression<Compat<{t}>, \"{name}\" | \"~{name}\", {{}}>;", camel(&f.name), field_input(m, f)).unwrap();
            } else { writeln!(body, "  {}?: {};", camel(&f.name), field_input(m, f)).unwrap(); }
            if let Some(r) = belongs_to(m, &f.name).filter(|r| last_key(r, &f.name)) {
                let null = if f.nullable { " | null" } else { "" };
                writeln!(body, "  {}?: {}{null};", camel(&r.name), related_ref(schema, r)?).unwrap();
            }
        }
        for g in &groups {
            writeln!(body, "  {}?: {};", camel(g.name), or_null(format!("{} | {}Patch", g.ty, g.ty), g.nullable)).unwrap();
        }
        writeln!(body, "}}\n").unwrap();

        // updateMany rows: the primary key plus plain values
        writeln!(body, "export interface {name}UpdateRow {{").unwrap();
        for (position, f) in m.fields().iter().enumerate().filter(|(_, f)| f.nested.is_none()) {
            #[cfg(feature = "composition")]
            if m.native.computed().contains(&position) { continue; }
            #[cfg(not(feature = "composition"))]
            let _ = position;
            let q = if f.primary_key { "" } else { "?" };
            writeln!(body, "  {}{q}: {};", camel(&f.name), field_input(m, f)).unwrap();
            if let Some(r) = belongs_to(m, &f.name).filter(|r| last_key(r, &f.name)) {
                let null = if f.nullable { " | null" } else { "" };
                writeln!(body, "  {}?: {}{null};", camel(&r.name), related_ref(schema, r)?).unwrap();
            }
        }
        for g in &groups {
            writeln!(body, "  {}?: {};", camel(g.name), or_null(format!("{} | {}Input", g.ty, g.ty), g.nullable)).unwrap();
        }
        writeln!(body, "}}\n").unwrap();

        let composed = pk.hints.get("composition.child").is_some_and(|v| v == "true");
        if composed {
            writeln!(body, "export interface {name}Attach {{").unwrap();
            for field in m.fields().iter().filter(|f| f.hints.get("composition.local").is_some_and(|v| v == "true")) {
                let optional = if field.nullable || has_server_value(field) { "?" } else { "" };
                writeln!(body, "  {}{optional}: {};", camel(&field.name), input_type(field)).unwrap();
            }
            writeln!(body, "}}\n").unwrap();
        }
        let spec_name = if exts.is_empty() { format!("{name}Spec") } else { format!("{name}BaseSpec") };
        writeln!(body, "export interface {spec_name} {{").unwrap();
        writeln!(body, "  readonly name: {};", quote(name)).unwrap();
        writeln!(body, "  readonly row: {name};").unwrap();
        writeln!(body, "  readonly data: {name}Data;").unwrap();
        writeln!(body, "  readonly insert: {name}Insert;").unwrap();
        let default_names = m.read_defaults.names();
        if !default_names.is_empty() {
            writeln!(body, "  readonly defaults: {};", default_names.iter().map(|n| quote(n)).collect::<Vec<_>>().join(" | ")).unwrap();
        }
        if composed { writeln!(body, "  readonly attach: {name}Attach;").unwrap(); }
        writeln!(body, "  readonly update: {name}Update;").unwrap();
        writeln!(body, "  readonly updateRow: {name}UpdateRow;").unwrap();
        match m.key_fields().as_slice() {
            [_] => writeln!(body, "  readonly pk: {};", value_type(pk)).unwrap(),
            keys => {
                let keys: Vec<String> = keys.iter().map(|f| format!("readonly {}: {}", camel(&f.name), value_type(f))).collect();
                writeln!(body, "  readonly pk: {{ {} }};", keys.join("; ")).unwrap()
            }
        }
        if exts.is_empty() {
            if let Some(class) = query_type(name, &spec_name) { writeln!(body, "  readonly queries: {class};").unwrap(); }
            writeln!(body, "}}\n").unwrap();
        } else {
            // `queries` cannot name its own spec, so the extension mixins apply to the base spec
            writeln!(body, "}}\n").unwrap();
            let mut ctor = format!("new (...args: any[]) => QuerySet<{name}BaseSpec>");
            for (i, e) in &exts { ctor = format!("ReturnType<typeof _e{i}.{}<{ctor}>>", e.members.query_set); }
            let ext_type = format!("InstanceType<{ctor}>");
            let queries = query_type(name, &spec_name).map_or(ext_type.clone(), |class| format!("{class} & {ext_type}"));
            let mut bound = format!("new (...args: any[]) => BoundQuerySet<{name}BaseSpec>");
            let mut any_bound = false;
            for (i, e) in &exts {
                let export = &e.members.bound_query_set;
                if !export.is_empty() {
                    bound = format!("ReturnType<typeof _e{i}.{export}<{bound}>>");
                    any_bound = true;
                }
            }
            let bound_slot = if any_bound { format!("  readonly boundQueries: InstanceType<{bound}>;\n") } else { String::new() };
            writeln!(body, "export interface {name}Spec extends {name}BaseSpec {{\n  readonly queries: {queries};\n{bound_slot}}}\n").unwrap();
        }

        // columns and relation paths: `S` the scopes a column reads (the root model, plus
        // `*many` through a to-many hop), `H` the hops so far, `O` whether a hop may be null
        writeln!(
            body,
            "export interface {name}Fields<S extends string, H extends readonly Hop[], O extends boolean> {{"
        )
        .unwrap();
        for f in m.fields().iter().filter(|f| f.nested.is_none()) {
            writeln!(body, "  readonly {}: Column<O extends true ? {} | null : {}, S, {}, H>;", camel(&f.name), field_type(m, f), field_type(m, f), quote(&camel(&f.name)))
                .unwrap();
        }
        for g in &groups {
            let x = or_null(g.ty.to_owned(), g.nullable);
            writeln!(body, "  readonly {}: {}Ref<S, H, O, {}, {x}>;", camel(g.name), g.ty, quote(&camel(g.name))).unwrap();
        }
        for r in &m.ir.relations {
            let kind = hop_kind(schema, m, r)?;
            let scope = if matches!(kind, "many" | "m2m") { "S | Many" } else { "S" };
            let opt = if kind == "opt" { "true" } else { "O" };
            writeln!(
                body,
                "  readonly {}: {}Path<{scope}, [...H, Hop<{}, {}, {}Spec>], {opt}>;",
                camel(&r.name),
                r.target,
                quote(&camel(&r.name)),
                quote(kind),
                r.target
            )
            .unwrap();
        }
        writeln!(body, "}}\n").unwrap();
        writeln!(
            body,
            "export interface {name}Path<S extends string, H extends readonly Hop[], O extends boolean>\n  \
             extends RelationPath<{name}Spec, S, H>,\n    {name}Fields<S, H, O> {{\n  \
             /** The relation query set for `load()`: `{name}.objects` bound to this relation. */\n  \
             readonly objects: QuerySetOf<{name}Spec, {name}, {}, {{}}, never, Via<H, undefined, false, S>>;\n}}\n",
            quote(name)
        )
        .unwrap();
        let methods: Vec<_> = ir.behavior.methods.iter().filter(|method| method.model == *name).collect();
        let indexes = if super::indexes_free(m, camel) { crate::migrate::model::unique_index_names(m) } else { vec![] };
        if methods.is_empty() && indexes.is_empty() {
            writeln!(body, "export interface {name}Model extends ModelClass<{name}Spec>, {name}Fields<{}, [], false> {{}}\n", quote(name)).unwrap();
        } else {
            writeln!(body, "export interface {name}Model extends ModelClass<{name}Spec>, {name}Fields<{}, [], false> {{", quote(name)).unwrap();
            if !indexes.is_empty() {
                writeln!(body, "  /** The unique indexes, for `onConflict(columns, {{ index }})`. */\n  readonly indexes: {{").unwrap();
                for index in &indexes {
                    let key = if super::is_js_ident(index) { index.clone() } else { quote(index) };
                    writeln!(body, "    readonly {key}: IndexRef<{}>;", quote(name)).unwrap();
                }
                writeln!(body, "  }};").unwrap();
                used_index = true;
            }
            for method in methods {
                let output = if method.output.is_some() { "string" } else { "void" };
                writeln!(body, "  {}(value: string): {output};", camel(&method.name)).unwrap();
            }
            writeln!(body, "}}\n").unwrap();
        }
        writeln!(body, "export const {name} = models[{}] as unknown as {name}Model;\n", quote(name)).unwrap();
        if let Some(class) = query_set(name) {
            // an inherited class is a `QuerySet` of the root spec, which `useQuerySet` rejects for the proxy
            let inherited = query_sets[name.as_str()].2;
            let class = if generic(name) { format!("{class}<{name}Spec>") } else { class };
            let class = if inherited { format!("(() => {class}) as never") } else { format!("() => {class}") };
            writeln!(body, "useQuerySet({name}, {class});\n").unwrap();
        }
        if let Some(class) = methods_of(name) {
            writeln!(body, "useModelMethods({name}, () => {class});\n").unwrap();
        }
    }

    let mut out = String::new();
    writeln!(out, "// Generated from {source} by `orm generate typescript`. Do not edit.").unwrap();
    out.push_str(
        "//\n\
         // The models are built at runtime from the compiled schema below; the declarations\n\
         // give them their static types. Per model: the row type (`User`), the shapes insert /\n\
         // update / updateMany take, the columns and relation paths, and the model object.\n\n\
         /* eslint-disable */\n",
    );
    let mut types = vec![
        "Column", "Compat", "Expression", "Hop", "In", "Instance", "ManyRelatedSet", "Many", "ModelClass", "RelatedSet",
        "QuerySetOf", "RelationPath", "SchemaIR", "Via",
    ];
    types.extend(used.iter().copied());
    if used_index {
        types.push("IndexRef");
    }
    if !ir.behavior.type_models.is_empty() {
        types.push("ModelSpec");
    }
    let used_exts: BTreeSet<usize> = schema.models.iter().flat_map(|m| options.model_extensions(ir, &m.ir.name).into_iter().map(|(i, _)| i)).chain(options.extensions.iter().enumerate().filter(|(_, e)| e.runtime_module.is_some() && !e.members.query_set.is_empty()).map(|(i, _)| i)).collect();
    if !query_sets.is_empty() || !used_exts.is_empty() {
        types.push("QueriesOf");
    }
    if !used_exts.is_empty() {
        types.push("QuerySet");
    }
    if used_exts.iter().any(|i| !options.extensions[*i].members.bound_query_set.is_empty()) {
        types.push("BoundQuerySet");
    }
    types.sort_unstable();
    let imports = types.iter().map(|t| format!("type {t}")).collect::<Vec<_>>().join(", ");
    let runtime_values: String = ["define", if query_sets.is_empty() { "" } else { "useQuerySet" }, if model_methods.is_empty() { "" } else { "useModelMethods" }]
        .into_iter().filter(|v| !v.is_empty()).collect::<Vec<_>>().join(", ");
    writeln!(out, "import {{ {runtime_values}, {imports} }} from {};", quote(runtime)).unwrap();
    for (i, module) in modules.iter().enumerate() {
        writeln!(out, "import * as _q{i} from {};", quote(module)).unwrap();
    }
    for (i, module) in method_modules.iter().enumerate() {
        writeln!(out, "import * as _m{i} from {};", quote(module)).unwrap();
    }
    for i in &used_exts {
        writeln!(out, "import * as _e{i} from {};", quote(&options.extensions[*i].runtime_module.clone().unwrap_or_else(|| format!("{ext_base}/{}/index.js", options.extensions[*i].typescript_dir())))).unwrap();
    }
    let mut ext_options = String::new();
    if !used_exts.is_empty() {
        let modules: Vec<String> = used_exts.iter().map(|i| {
            let e = &options.extensions[*i];
            if e.runtime_module.is_some() { return format!("{}: {{ module: _e{i} }}", quote(&e.members.extension)); }
            format!("{}: {{ module: _e{i}, hash: {}, dir: new URL({}, import.meta.url) }}", quote(&e.members.extension), quote(&e.members.hash), quote(&format!("{ext_base}/{}/", e.typescript_dir())))
        }).collect();
        let members: Vec<String> = schema.models.iter().filter_map(|m| {
            let e = options.model_extensions(ir, &m.ir.name);
            (!e.is_empty()).then(|| format!("{}: [{}]", quote(&m.ir.name), e.iter().map(|(_, x)| quote(&x.members.extension)).collect::<Vec<_>>().join(", ")))
        }).collect();
        write!(ext_options, "extensions: {{ {} }}, members: {{ {} }}", modules.join(", "), members.join(", ")).unwrap();
    }
    out.push('\n');
    writeln!(out, "const SCHEMA: SchemaIR = {ir_json};\n").unwrap();
    #[cfg(feature = "reference-loading")]
    writeln!(out, "const models = define(SCHEMA, {{ requiredCapabilities: [\"reference-loading\"]{} }});\n", if ext_options.is_empty() { String::new() } else { format!(", {ext_options}") }).unwrap();
    #[cfg(not(feature = "reference-loading"))]
    if ext_options.is_empty() { writeln!(out, "const models = define(SCHEMA);\n").unwrap(); } else { writeln!(out, "const models = define(SCHEMA, {{ {ext_options} }});\n").unwrap(); }
    if !ir.types.is_empty() {
        out.push_str("const nested = (models as unknown as { readonly $types: Record<string, unknown> }).$types;\n\n");
    }
    for (i, u) in adapters.uses.iter().enumerate() {
        for line in &u.runtime.imports { writeln!(out, "{line}").unwrap(); }
        writeln!(out, "import {{ {} as _fa{i} }} from {};", u.runtime.install, quote(&u.module("typescript"))).unwrap();
        let installs: Vec<String> = schema.models.iter().filter_map(|m| {
            let fields: Vec<String> = m.field_adapters.iter().filter(|a| a.adapter == u.id).map(|a| {
                format!("[{}, {{ options: {}, nullable: {} }}]", quote(&camel(&m.fields()[a.position].name)), a.options, a.nullable)
            }).collect();
            (!fields.is_empty()).then(|| format!("_fa{i}(models[{}]!, new Map([{}]))", quote(&m.ir.name), fields.join(", ")))
        }).collect();
        writeln!(out, "const _FA{i} = [{}];", installs.join(", ")).unwrap();
        for function in &u.runtime.module_functions {
            writeln!(out, "export const {}: {} = (...args) => {{ for (const adapter of _FA{i}) adapter.{}(...args); }};", function.name, function.stub, function.method).unwrap();
        }
        out.push('\n');
    }
    out.push_str(&body);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::camel;

    #[test]
    fn camel_case_names() {
        assert_eq!(camel("author_id"), "authorId");
        assert_eq!(camel("post_tags"), "postTags");
        assert_eq!(camel("id"), "id");
        assert_eq!(camel("_private"), "_private");
        assert_eq!(camel("a__b"), "aB");
        assert_eq!(camel("trailing_"), "trailing_");
        assert_eq!(camel("v2_x"), "v2X");
        assert_eq!(camel("alreadyCamel"), "alreadyCamel");
    }
}
