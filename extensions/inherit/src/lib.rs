//! Abstract models and model overrides. The pass runs first, so later passes see ordinary models.
use monorm_contracts::{
    extension::{capture_storage, Declaration},
    ir::*,
};
use serde::{de::DeserializeOwned, Serialize};
use std::collections::{BTreeMap, BTreeSet};

fn copy<T: Serialize + DeserializeOwned>(value: &T) -> Result<T, String> {
    serde_json::from_value(serde_json::to_value(value).map_err(|e| e.to_string())?).map_err(|e| e.to_string())
}

fn error(d: &Declaration, message: impl std::fmt::Display) -> String {
    format!("{}:{}:{}: @@{}: {message}", d.location.file, d.location.line, d.location.column, d.attribute)
}

/// Work on a candidate so a rejected declaration cannot partially mutate the schema.
pub fn expand(ir: &mut SchemaIr) -> Result<(), String> {
    let mut candidate: SchemaIr = copy(&*ir)?;
    run(&mut candidate)?;
    *ir = candidate;
    Ok(())
}

#[derive(Default)]
struct Parsed {
    abstracts: BTreeMap<String, Declaration>,
    extends: BTreeMap<String, (Declaration, Vec<String>)>,
    overrides: BTreeMap<String, (Declaration, String)>,
}

fn names(d: &Declaration) -> Result<Vec<String>, String> {
    let usage = || error(d, "takes a model name or a list of model names");
    if !d.arguments.is_empty() || d.positional.len() != 1 { return Err(usage()); }
    let list: Vec<String> = match &d.positional[0] {
        serde_json::Value::String(s) => vec![s.clone()],
        serde_json::Value::Array(items) => items.iter().map(|v| v.as_str().map(str::to_owned)).collect::<Option<_>>().ok_or_else(usage)?,
        _ => return Err(usage()),
    };
    if list.is_empty() || list.iter().any(String::is_empty) { return Err(usage()); }
    let mut seen = BTreeSet::new();
    if let Some(twice) = list.iter().find(|n| !seen.insert(*n)) { return Err(error(d, format!("lists {twice} twice"))); }
    Ok(list)
}

fn parse(ir: &SchemaIr) -> Result<Parsed, String> {
    let mut p = Parsed::default();
    for d in ir.behavior.declarations.iter().filter(|d| !d.lowered && d.attribute.starts_with("inherit.")) {
        if d.field.is_some() { return Err(error(d, "is a model attribute")); }
        if !ir.models.iter().any(|m| m.name == d.model) { return Err(error(d, format!("unknown model {}", d.model))); }
        match d.attribute.as_str() {
            "inherit.abstract" => {
                if !d.arguments.is_empty() || !d.positional.is_empty() { return Err(error(d, "takes no arguments")); }
                p.abstracts.insert(d.model.clone(), d.clone());
            }
            "inherit.extends" => {
                let bases = names(d)?;
                if p.extends.insert(d.model.clone(), (d.clone(), bases)).is_some() {
                    return Err(error(d, format!("{} has a second @@inherit.extends; list every base in one", d.model)));
                }
            }
            "inherit.override" => {
                let base = names(d)?;
                let [base] = <[String; 1]>::try_from(base).map_err(|_| error(d, "takes one model; an override copies one concrete model"))?;
                if p.overrides.insert(d.model.clone(), (d.clone(), base)).is_some() {
                    return Err(error(d, format!("{} has a second @@inherit.override", d.model)));
                }
            }
            other => return Err(error(d, format!("unknown attribute {other}"))),
        }
    }
    Ok(p)
}

fn index(ir: &SchemaIr, name: &str) -> Result<usize, String> {
    ir.models.iter().position(|m| m.name == name).ok_or_else(|| format!("unknown model {name}"))
}

fn same<T: Serialize>(a: &T, b: &T) -> bool {
    serde_json::to_value(a).ok() == serde_json::to_value(b).ok()
}

fn explicit_name<T: Serialize>(item: &T) -> Option<String> {
    serde_json::to_value(item).ok()?.get("name")?.as_str().map(str::to_owned)
}

/// The concrete models that reach `name` through `extends` (`seen` stops a cycle; `order` reports it later).
fn children(p: &Parsed, name: &str, seen: &mut Vec<String>) -> usize {
    if seen.iter().any(|n| n == name) { return 0; }
    seen.push(name.to_owned());
    p.extends.iter().filter(|(_, (_, bases))| bases.iter().any(|b| b == name))
        .map(|(c, _)| if p.abstracts.contains_key(c) { children(p, c, seen) } else { 1 }).sum()
}

fn validate(ir: &SchemaIr, p: &Parsed) -> Result<(), String> {
    let mut involved: BTreeSet<&String> = p.abstracts.keys().collect();
    involved.extend(p.extends.iter().flat_map(|(c, (_, b))| std::iter::once(c).chain(b)));
    involved.extend(p.overrides.iter().flat_map(|(c, (_, b))| [c, b]));
    if let Some(name) = involved.iter().find(|n| ir.behavior.lowered_models.contains(**n)) {
        return Err(format!("{name}: inheritance across schema batches is not supported; declare the base and the child in one schema set"));
    }
    for (name, d) in &p.abstracts {
        let m = &ir.models[index(ir, name)?];
        for (set, what) in [(m.mapped, "@@map"), (m.renamed_from.is_some(), "@@renamed_from"), (m.partition.is_some(), "@@partition")] {
            if set { return Err(error(d, format!("{name} is abstract and has no table; {what} is not allowed on it"))); }
        }
        if p.overrides.contains_key(name) { return Err(error(d, format!("{name} is abstract and cannot override a model"))); }
        if children(p, name, &mut vec![]) >= 2 {
            let named = m.indexes.iter().filter_map(explicit_name).chain(m.constraints.iter().filter_map(explicit_name)).chain(m.pk_name.clone()).next();
            if let Some(n) = named {
                return Err(error(d, format!("{name} has an explicit index or constraint name {n:?} and two or more children; the name would repeat in each table. Remove the name")));
            }
        }
    }
    for m in &ir.models {
        for r in &m.relations {
            if let Some(d) = p.abstracts.get(&r.target) {
                return Err(error(d, format!("{}.{}: {} is abstract and has no table; a relation to it is not allowed", m.name, r.name, r.target)));
            }
        }
    }
    for (child, (d, bases)) in &p.extends {
        for base in bases {
            if base == child { return Err(error(d, format!("{child} extends itself"))); }
            if !ir.models.iter().any(|m| &m.name == base) { return Err(error(d, format!("unknown model {base}"))); }
            if !p.abstracts.contains_key(base) { return Err(error(d, format!("`{base}` is not abstract; use `@@inherit.override(\"{base}\")`"))); }
        }
    }
    let mut taken: BTreeMap<&String, &String> = BTreeMap::new();
    for (child, (d, base)) in &p.overrides {
        if child == base { return Err(error(d, format!("{child} overrides itself"))); }
        if !ir.models.iter().any(|m| &m.name == base) { return Err(error(d, format!("unknown model {base}"))); }
        if p.abstracts.contains_key(base) { return Err(error(d, format!("`{base}` is abstract; use `@@inherit.extends`"))); }
        if p.overrides.contains_key(base) { return Err(error(d, format!("{base} is itself an override; override the concrete model, not a chain"))); }
        if let Some(other) = taken.insert(base, child) { return Err(error(d, format!("{other} and {child} both override {base}"))); }
    }
    Ok(())
}

/// The models that extend something, bases first. `Err` holds the cycle.
fn order(p: &Parsed) -> Result<Vec<String>, Vec<String>> {
    fn visit(p: &Parsed, name: &str, path: &mut Vec<String>, done: &mut Vec<String>) -> Result<(), Vec<String>> {
        if done.iter().any(|n| n == name) { return Ok(()); }
        if let Some(at) = path.iter().position(|n| n == name) {
            let mut cycle = path[at..].to_vec();
            cycle.push(name.to_owned());
            return Err(cycle);
        }
        let Some((_, bases)) = p.extends.get(name) else { return Ok(()) };
        path.push(name.to_owned());
        for base in bases.iter().chain(p.overrides.get(name).map(|(_, b)| b)) { visit(p, base, path, done)?; }
        path.pop();
        done.push(name.to_owned());
        Ok(())
    }
    let mut done = vec![];
    for name in p.extends.keys() { visit(p, name, &mut vec![], &mut done)?; }
    Ok(done)
}

fn extend_from<T: Serialize + DeserializeOwned>(out: &mut Vec<T>, items: &[T], skip_equal: bool) -> Result<(), String> {
    for item in items {
        if !(skip_equal && out.iter().any(|o| same(o, item))) { out.push(copy(item)?); }
    }
    Ok(())
}

fn check_member(model: &ModelIr, name: &str) -> Result<(), String> {
    if model.fields.iter().any(|f| f.name == name) || model.relations.iter().any(|r| r.name == name) {
        return Err(format!("{}.{}: generated name collides with a member", model.name, name));
    }
    if name.is_empty() || !name.chars().enumerate().all(|(i, c)| c == '_' || c.is_ascii_alphabetic() || i > 0 && c.is_ascii_digit()) {
        return Err(format!("invalid relation name {name}"));
    }
    Ok(())
}

/// Copy the members of `bases` into `child`; the child's own members win, and the bases must not disagree.
/// `override_base` is the first of `bases` when the child overrides it: it already holds the members it took from
/// the other bases, so an equal member from another base is the same member and not a conflict.
/// Returns the names of the inherited relations that still wait for an owning relation.
fn merge(ir: &mut SchemaIr, child: &str, bases: &[String], override_base: Option<&str>, d: &Declaration) -> Result<Vec<String>, String> {
    let ci = index(ir, child)?;
    let mut cm: ModelIr = copy(&ir.models[ci])?;
    let own_fields: BTreeSet<String> = cm.fields.iter().map(|f| f.name.clone()).collect();
    let own_relations: BTreeSet<String> = cm.relations.iter().map(|r| r.name.clone()).collect();
    let own_declarations: BTreeSet<(String, Option<String>)> =
        ir.behavior.declarations.iter().filter(|x| x.model == child).map(|x| (x.attribute.clone(), x.field.clone())).collect();
    let own_key: Vec<String> = cm.key_fields().into_iter().map(str::to_owned).collect();
    let (own_query_set, own_methods) = (cm.query_set.is_some(), cm.model_methods.is_some());

    let mut fields: Vec<FieldIr> = vec![];
    let mut relations: Vec<RelationIr> = vec![];
    let (mut indexes, mut constraints, mut triggers) = (vec![], vec![], vec![]);
    let mut waiting = vec![];
    let mut new_declarations = vec![];
    let mut field_from: BTreeMap<String, &String> = BTreeMap::new();
    let mut relation_from: BTreeMap<String, &String> = BTreeMap::new();
    let mut declaration_from: BTreeMap<(String, Option<String>), &String> = BTreeMap::new();
    let mut known_key: Option<(String, Vec<String>)> = (!own_key.is_empty()).then(|| (child.to_owned(), own_key.clone()));
    let (mut query_set_from, mut methods_from): (Option<&String>, Option<&String>) = (None, None);
    let mut class_source: Option<String> = None;

    for base in bases {
        let bm: ModelIr = copy(&ir.models[index(ir, base)?])?;
        let repeat = override_base.is_some() && Some(base.as_str()) != override_base;
        let key: Vec<String> = bm.key_fields().into_iter().map(str::to_owned).collect();
        for f in bm.fields.iter().cloned() {
            if own_fields.contains(&f.name) { continue; }
            if let Some(first) = field_from.insert(f.name.clone(), base) {
                if Some(first.as_str()) == override_base && fields.iter().any(|x| x.name == f.name && same(x, &f)) { continue; }
                return Err(error(d, format!("{child}.{}: the field is in both {first} and {base}; redeclare it in {child}", f.name)));
            }
            fields.push(f);
        }
        for r in bm.relations.iter().cloned() {
            if own_relations.contains(&r.name) { continue; }
            if let Some(first) = relation_from.insert(r.name.clone(), base) {
                // The override base resolved a placeholder of the abstract base already.
                let kept = relations.iter().find(|x| x.name == r.name);
                if Some(first.as_str()) == override_base && kept.is_some_and(|x| same(x, &r) || r.label.is_some() && x.label.is_none() && x.target == r.target) { continue; }
                return Err(error(d, format!("{child}.{}: the relation is in both {first} and {base}; redeclare it in {child}", r.name)));
            }
            if r.label.is_some() { waiting.push(r.name.clone()); }
            relations.push(r);
        }
        extend_from(&mut indexes, &bm.indexes, repeat)?;
        extend_from(&mut constraints, &bm.constraints, repeat)?;
        extend_from(&mut triggers, &bm.triggers, repeat)?;
        if !key.is_empty() {
            match &known_key {
                Some((src, known)) if *known != key => {
                    let hint = if src == child { format!("; redeclare `{}` in {child} to change its type or default", key.join("`, `")) } else { String::new() };
                    return Err(error(d, format!("{child}: the primary key of {base} ({}) differs from the one of {src} ({}){hint}", key.join(", "), known.join(", "))));
                }
                Some(_) => {
                    if Some(base.as_str()) == override_base {
                        for name in &key {
                            let (Some(own), Some(old)) = (cm.fields.iter().find(|f| &f.name == name), bm.fields.iter().find(|f| &f.name == name)) else { continue };
                            if own_fields.contains(name) && (own.ty != old.ty || own.db_type != old.db_type || own.max_length != old.max_length || own.array != old.array || own.enum_name != old.enum_name) {
                                return Err(error(d, format!("`{child}.{name}`: an override cannot change the key type of `{base}`; incoming foreign keys keep it")));
                            }
                        }
                    }
                }
                None => {
                    known_key = Some((base.clone(), key));
                    cm.primary_key = bm.primary_key.clone();
                    cm.pk_name = bm.pk_name.clone();
                }
            }
        }
        if let Some(q) = &bm.query_set {
            if !own_query_set {
                if let Some(first) = query_set_from.replace(base) {
                    if !(Some(first.as_str()) == override_base && cm.query_set.as_ref() == Some(q)) {
                        return Err(error(d, format!("{child}: @@query_set is in both {first} and {base}; declare one on {child}")));
                    }
                }
                cm.query_set = Some(q.clone());
                class_source.get_or_insert_with(|| bm.classes_from.clone().unwrap_or_else(|| base.clone()));
            }
        }
        if let Some(m) = &bm.model_methods {
            if !own_methods {
                if let Some(first) = methods_from.replace(base) {
                    if !(Some(first.as_str()) == override_base && cm.model_methods.as_ref() == Some(m)) {
                        return Err(error(d, format!("{child}: @@model_methods is in both {first} and {base}; declare one on {child}")));
                    }
                }
                cm.model_methods = Some(m.clone());
                class_source.get_or_insert_with(|| bm.classes_from.clone().unwrap_or_else(|| base.clone()));
            }
        }
        cm.protected_write |= bm.protected_write;
        if cm.comment.is_none() { cm.comment = bm.comment.clone(); }
        for x in ir.behavior.declarations.iter().filter(|x| &x.model == base && !x.attribute.starts_with("inherit.")) {
            let key = (x.attribute.clone(), x.field.clone());
            if x.field.as_ref().is_some_and(|f| own_fields.contains(f)) || own_declarations.contains(&key) { continue; }
            let copied = Declaration { model: child.to_owned(), lowered: false, ..x.clone() };
            if let Some(first) = declaration_from.insert(key, base) {
                if Some(first.as_str()) == override_base && new_declarations.iter().any(|n| same(n, &copied)) { continue; }
                return Err(error(d, format!("{child}: @{} is in both {first} and {base}; declare it on {child}", x.attribute)));
            }
            new_declarations.push(copied);
        }
    }
    // The classes of one package: a child with a class of its own looks them all up in its own package.
    if !own_query_set && !own_methods { cm.classes_from = class_source; }
    fields.extend(cm.fields);
    cm.fields = fields;
    relations.extend(cm.relations);
    cm.relations = relations;
    indexes.extend(cm.indexes);
    cm.indexes = indexes;
    constraints.extend(cm.constraints);
    cm.constraints = constraints;
    triggers.extend(cm.triggers);
    cm.triggers = triggers;
    ir.models[ci] = cm;
    ir.behavior.declarations.extend(new_declarations);
    Ok(waiting)
}

/// Give each inherited to-many relation its owning relation and foreign key field on the target model, or its join model.
fn resolve(ir: &mut SchemaIr, child: &str, waiting: &[String], d: &Declaration, generated: &mut BTreeMap<(String, String), String>, touched: &mut BTreeSet<String>) -> Result<(), String> {
    for member in waiting {
        let ci = index(ir, child)?;
        let rel = ir.models[ci].relations.iter().find(|r| &r.name == member).cloned().ok_or("lost a relation")?;
        let key = match ir.models[ci].key_fields().as_slice() {
            [one] => one.to_string(),
            _ => return Err(error(d, format!("{child}.{member}: a generated back relation needs a one-field primary key on {child}"))),
        };
        let key_field = ir.models[ci].fields.iter().find(|f| f.name == key).cloned().ok_or_else(|| error(d, format!("{child}: unknown key field {key}")))?;
        let label = rel.label.clone().filter(|l| !l.is_empty()).unwrap_or_else(|| format!("{{model}}_{member}"));
        let back = label.replace("{model}", &child.to_lowercase());
        if rel.join_label.is_some() {
            join_model(ir, child, &rel, &back, d, generated, touched)?;
            continue;
        }
        let fk = format!("{back}_{key}");
        if ir.behavior.lowered_models.contains(&rel.target) {
            return Err(error(d, format!("{}: inheritance across schema batches is not supported", rel.target)));
        }
        let ti = index(ir, &rel.target)?;
        if let Some(other) = generated.get(&(rel.target.clone(), back.clone())) {
            return Err(error(d, format!("{other}.{member} and {child}.{member} both generate {}.{back}; put {{model}} in the relation name", rel.target)));
        }
        check_member(&ir.models[ti], &back)?;
        check_member(&ir.models[ti], &fk)?;
        generated.insert((rel.target.clone(), back.clone()), child.to_owned());
        let mut field = FieldIr::plain(&fk, key_field.ty);
        field.enum_name = key_field.enum_name.clone();
        field.max_length = key_field.max_length;
        field.db_type = key_field.db_type.clone();
        field.read_sql = key_field.read_sql.clone();
        field.write_sql = key_field.write_sql.clone();
        field.requires = key_field.requires.clone();
        field.unique = rel.kind == RelKind::One;
        let target = &mut ir.models[ti];
        target.fields.push(field);
        target.relations.push(RelationIr {
            name: back, kind: RelKind::One, target: child.to_owned(), from: vec![fk.clone()], to: vec![key.clone()],
            foreign_key: true, on_delete: rel.on_delete, on_update: None, deferrable: None, fk_name: None, through: None, label: None, join_label: None,
        });
        let owner = ir.models[ci].relations.iter_mut().find(|r| &r.name == member).ok_or("lost a relation")?;
        owner.from = vec![key];
        owner.to = vec![fk];
        owner.label = None;
        owner.on_delete = None;
        touched.insert(rel.target.clone());
    }
    Ok(())
}

/// Give a many-to-many relation of `child` its join model, `through:` and back relation on the target.
fn join_model(
    ir: &mut SchemaIr, child: &str, rel: &RelationIr, back: &str, d: &Declaration,
    generated: &mut BTreeMap<(String, String), String>, touched: &mut BTreeSet<String>,
) -> Result<(), String> {
    let (member, join) = (rel.name.as_str(), rel.join_label.as_deref().ok_or("lost a join name")?);
    let ci = index(ir, child)?;
    if ir.behavior.lowered_models.contains(&rel.target) {
        return Err(error(d, format!("{}: inheritance across schema batches is not supported", rel.target)));
    }
    let ti = index(ir, &rel.target)?;
    let join_name = join.replace("{model}", child);
    if ir.models.iter().any(|m| m.name == join_name) {
        return Err(error(d, format!("{child}.{member}: the join model {join_name} already exists; name it differently with {{model}} in through:")));
    }
    if let Some(other) = generated.get(&(rel.target.clone(), back.to_owned())) {
        return Err(error(d, format!("{other}.{member} and {child}.{member} both generate {}.{back}; put {{model}} in the relation name", rel.target)));
    }
    check_member(&ir.models[ti], back)?;
    let one_key = |m: &ModelIr| -> Result<FieldIr, String> {
        match m.key_fields().as_slice() {
            [one] => m.fields.iter().find(|f| f.name == *one).cloned().ok_or_else(|| error(d, format!("{}: unknown key field {one}", m.name))),
            _ => Err(error(d, format!("{child}.{member}: a generated join model needs a one-field primary key on {child} and {}", rel.target))),
        }
    };
    let (child_key, target_key) = (one_key(&ir.models[ci])?, one_key(&ir.models[ti])?);
    let (child_side, target_side) = (child.to_lowercase(), rel.target.to_lowercase());
    if child_side == target_side {
        return Err(error(d, format!("{child}.{member}: a generated join model needs two different models; write the join model and through: yourself")));
    }
    let table = format!("{}_{member}", ir.models[ci].table);
    let side_field = |side: &str, key: &FieldIr| {
        let mut f = FieldIr::plain(&format!("{side}_{}", key.name), key.ty);
        f.nullable = false;
        f.enum_name = key.enum_name.clone();
        f.max_length = key.max_length;
        f.db_type = key.db_type.clone();
        f.read_sql = key.read_sql.clone();
        f.write_sql = key.write_sql.clone();
        f.requires = key.requires.clone();
        f
    };
    let (source, target) = (side_field(&child_side, &child_key), side_field(&target_side, &target_key));
    let mut id = FieldIr::plain("id", ColType::Int);
    id.nullable = false;
    id.primary_key = true;
    id.auto_increment = true;
    let link = |side: &str, field: &FieldIr, to: &str, key: &FieldIr| RelationIr {
        name: side.to_owned(), kind: RelKind::One, target: to.to_owned(), from: vec![field.name.clone()], to: vec![key.name.clone()],
        foreign_key: true, on_delete: Some(OnDelete::Cascade), on_update: None, deferrable: None, fk_name: None, through: None, label: None, join_label: None,
    };
    let model = ModelIr {
        name: join_name.clone(), table: table.clone(), mapped: false, primary_key: vec![], pk_name: None, partition: None, renamed_from: None, comment: None,
        protected_write: false, query_set: None, model_methods: None, classes_from: None, indexes: vec![], triggers: vec![],
        relations: vec![link(&child_side, &source, child, &child_key), link(&target_side, &target, &rel.target, &target_key)],
        constraints: vec![ConstraintIr::Unique {
            name: None, fields: vec![source.name.clone(), target.name.clone()],
            nulls_not_distinct: false, deferrable: None,
        }],
        fields: vec![id, source.clone(), target.clone()],
    };
    let (child_key, target_key) = (child_key.name, target_key.name);
    generated.insert((rel.target.clone(), back.to_owned()), child.to_owned());
    ir.models.push(model);
    ir.models[ti].relations.push(RelationIr {
        name: back.to_owned(), kind: RelKind::Many, target: child.to_owned(), from: vec![target_key.clone()], to: vec![child_key.clone()],
        foreign_key: false, on_delete: None, on_update: None, deferrable: None, fk_name: None,
        through: Some(ThroughIr { model: join_name.clone(), source: target.name.clone(), target: source.name.clone() }), label: None, join_label: None,
    });
    let owner = ir.models[ci].relations.iter_mut().find(|r| r.name == member).ok_or("lost a relation")?;
    owner.from = vec![child_key];
    owner.to = vec![target_key];
    owner.through = Some(ThroughIr { model: join_name.clone(), source: source.name, target: target.name });
    owner.label = None;
    owner.join_label = None;
    touched.insert(rel.target.clone());
    touched.insert(join_name);
    Ok(())
}

/// Merge `child`, then resolve it. A join model reads the key of its target, so a target that extends a base steps first.
fn step(
    ir: &mut SchemaIr, p: &Parsed, child: &str, started: &mut BTreeSet<String>,
    generated: &mut BTreeMap<(String, String), String>, touched: &mut BTreeSet<String>,
) -> Result<(), String> {
    if !started.insert(child.to_owned()) { return Ok(()); }
    let (d, bases) = &p.extends[child];
    let override_base = p.overrides.get(child).map(|(_, b)| b.as_str());
    let all: Vec<String> = override_base.map(str::to_owned).into_iter().chain(bases.iter().cloned()).collect();
    for base in all.iter().filter(|b| p.extends.contains_key(*b)) { step(ir, p, base, started, generated, touched)?; }
    let waiting = merge(ir, child, &all, override_base, d)?;
    touched.insert(child.to_owned());
    if p.abstracts.contains_key(child) { return Ok(()); }
    let ci = index(ir, child)?;
    let targets: Vec<String> = ir.models[ci].relations.iter().filter(|r| r.join_label.is_some() && waiting.contains(&r.name)).map(|r| r.target.clone()).collect();
    for target in targets.iter().filter(|t| p.extends.contains_key(*t)) { step(ir, p, target, started, generated, touched)?; }
    resolve(ir, child, &waiting, d, generated, touched)
}

fn run(ir: &mut SchemaIr) -> Result<(), String> {
    let p = parse(ir)?;
    if p.abstracts.is_empty() && p.extends.is_empty() && p.overrides.is_empty() { return Ok(()); }
    validate(ir, &p)?;
    capture_storage(ir)?;
    let sequence = order(&p).map_err(|cycle| {
        let (d, _) = &p.extends[&cycle[0]];
        error(d, format!("extends cycle: {}", cycle.join(" -> ")))
    })?;
    let mut touched: BTreeSet<String> = BTreeSet::new();
    let mut generated = BTreeMap::new();
    let mut started = BTreeSet::new();
    for child in &sequence { step(ir, &p, child, &mut started, &mut generated, &mut touched)?; }
    for (child, (d, base)) in &p.overrides {
        if !p.extends.contains_key(child) { merge(ir, child, std::slice::from_ref(base), Some(base.as_str()), d)?; }
        let (bi, ci) = (index(ir, base)?, index(ir, child)?);
        let (table, renamed_from, partition) = (ir.models[bi].table.clone(), ir.models[bi].renamed_from.clone(), ir.models[bi].partition.clone());
        let cm = &mut ir.models[ci];
        if !cm.mapped {
            cm.table = table;
            if cm.renamed_from.is_none() { cm.renamed_from = renamed_from; }
        } else if cm.renamed_from.is_none() {
            cm.renamed_from = Some(table);
        }
        if cm.partition.is_none() { cm.partition = partition; }
        for part in ir.partitions.iter_mut().filter(|part| &part.of == base) { part.of = child.clone(); }
        touched.insert(child.clone());
        for m in &mut ir.models {
            for r in &mut m.relations {
                if &r.target == base {
                    r.target = child.clone();
                    touched.insert(m.name.clone());
                }
                if let Some(through) = r.through.as_mut().filter(|t| &t.model == base) {
                    through.model = child.clone();
                    touched.insert(m.name.clone());
                }
            }
        }
    }
    for m in ir.models.iter().filter(|m| p.abstracts.contains_key(&m.name)) {
        if !ir.behavior.type_models.iter().any(|t| t.name == m.name) {
            let mut extended_by: Vec<String> = ir.models.iter().map(|c| &c.name)
                .filter(|c| p.extends.get(*c).is_some_and(|(_, bases)| bases.contains(&m.name))).cloned().collect();
            // An override takes the abstract bases of the model it replaces.
            for (child, (_, base)) in &p.overrides {
                if p.extends.get(base).is_some_and(|(_, bases)| bases.contains(&m.name)) && !extended_by.contains(child) {
                    extended_by.push(child.clone());
                }
            }
            ir.behavior.type_models.push(TypeModel { name: m.name.clone(), fields: m.fields.clone(), extended_by });
        }
    }
    for (child, (_, base)) in &p.overrides {
        match ir.behavior.model_overrides.insert(base.clone(), child.clone()) {
            Some(other) if &other != child => return Err(format!("{base} is overridden by {other} and {child}")),
            _ => {}
        }
    }
    let removed: BTreeSet<String> = p.abstracts.keys().cloned().chain(p.overrides.values().map(|(_, b)| b.clone())).collect();
    ir.models.retain(|m| !removed.contains(&m.name));
    ir.behavior.declarations.retain(|d| !removed.contains(&d.model));
    let storage = ir.behavior.storage.as_mut().ok_or("inherit requires captured physical storage")?;
    storage.models.retain(|m| !removed.contains(&m.name));
    for name in touched.iter().filter(|n| !removed.contains(*n)) {
        let physical: ModelIr = copy(ir.models.iter().find(|m| &m.name == name).ok_or("lost a model")?)?;
        match storage.models.iter_mut().find(|m| &m.name == name) {
            Some(slot) => *slot = physical,
            None => storage.models.push(physical),
        }
    }
    for d in ir.behavior.declarations.iter_mut().filter(|d| d.attribute.starts_with("inherit.")) { d.lowered = true; }
    Ok(())
}

// The package entry point; Rust callers keep the rlib API.
monorm_contracts::extension!(("monorm.inherit.expand", expand));
