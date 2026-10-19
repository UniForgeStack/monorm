//! Nested types: `type` blocks and the model fields that use them.
//!
//! A `columns` field lowers to one ordinary column for each scalar leaf, a `json` field
//! to one `Json` column. Both carry a `NestedIr` link; the planner, the differ and `pull`
//! see plain fields.

use super::*;
use crate::dialect::Dialect;
use crate::ir::{NestedIr, NestedStorage, TypeFieldIr, TypeIr};

/// What a dotted subfield path (`address.geo.lat`) names, for index keys.
pub(super) enum PathTarget {
    /// The lowered field of a scalar leaf on `columns` storage.
    Column(String),
    /// A subfield of a `json` field: the field's column, the path inside it and the leaf type.
    Json { column: String, sub: Vec<String>, leaf: ColType },
    /// A nested type, not a scalar.
    Group,
}

/// The position and `@map` of one member of a type.
pub(super) struct TypeMember {
    pub pos: Pos,
    pub map: Option<String>,
}

pub(super) type Paths = HashMap<String, PathTarget>;

pub(super) struct Expansion {
    pub fields: Vec<FieldIr>,
    pub checks: Vec<ConstraintIr>,
    pub paths: Paths,
}

fn storage_of(pos: Pos, v: &Value, what: &str) -> Result<NestedStorage> {
    match name_of(pos, v, what)?.as_str() {
        "columns" => Ok(NestedStorage::Columns),
        "json" => Ok(NestedStorage::Json),
        o => err(pos, format!("{what}: storage is columns or json, not {o}")),
    }
}

/// Collects and checks the `type` blocks. `models` holds the model names; a type name must not repeat a model, enum or type.
pub(super) fn declare(items: &[Item], models: &HashMap<&str, &ModelDecl>, ctx: &mut Ctx<'_>) -> Result<Vec<TypeIr>> {
    let decls: Vec<&ModelDecl> = items.iter().filter_map(|i| if let Item::Type(t) = i { Some(t) } else { None }).collect();
    let mut names: HashSet<&str> = HashSet::new();
    for t in &decls {
        if models.contains_key(t.name.as_str()) || ctx.enums.contains_key(&t.name) || ctx.types.contains_key(&t.name) || !names.insert(t.name.as_str()) {
            return err(t.pos, format!("{} is declared twice", t.name));
        }
    }
    let mut out: Vec<TypeIr> = vec![];
    for t in &decls {
        let what = format!("type {}", t.name);
        let (mut storage, mut comment) = (NestedStorage::Columns, None);
        for a in &t.blocks {
            let one = match a.args.positional.as_slice() {
                [(p, v)] if a.args.named.is_empty() => Some((*p, v)),
                _ => None,
            };
            match (a.name.as_str(), one) {
                ("storage", Some((p, v))) => storage = storage_of(p, v, &format!("{what}: @@storage"))?,
                ("storage", None) => return err(a.pos, format!("{what}: @@storage takes columns or json")),
                ("comment", Some((_, Value::Str(s)))) => comment = Some(s.clone()),
                ("comment", _) => return err(a.pos, format!("{what}: @@comment(\"...\") takes one string")),
                (o, _) => return err(a.pos, format!("{what}: unknown attribute @@{o}; a type takes @@storage and @@comment")),
            }
        }
        let mut seen = HashSet::new();
        let mut fields = vec![];
        for member in &t.members {
            let what = format!("{}.{}", t.name, member.name);
            if !seen.insert(member.name.as_str()) {
                return err(member.pos, format!("{what} is declared twice"));
            }
            let TypeRef { name, args, list, optional, pos } = &member.ty;
            if names.contains(name.as_str()) {
                if !args.positional.is_empty() || !args.named.is_empty() {
                    return err(args.pos, format!("{what}: {name} takes no arguments"));
                }
                let mut map = None;
                for a in &member.attrs {
                    match (a.name.as_str(), a.args.positional.as_slice()) {
                        ("map", [(_, Value::Str(s))]) if a.args.named.is_empty() => map = Some(s.clone()),
                        ("map", _) => return err(a.pos, format!("{what}: @map(\"...\") takes one string")),
                        _ => return err(a.pos, format!("{what}: a nested type takes only @map; put @@storage on {name}")),
                    }
                }
                ctx.members.insert((t.name.clone(), member.name.clone()), TypeMember { pos: member.pos, map });
                fields.push(TypeFieldIr { name: member.name.clone(), nullable: *optional, list: *list, type_name: Some(name.clone()), leaf: None });
                continue;
            }
            ctx.members.insert((t.name.clone(), member.name.clone()), TypeMember { pos: member.pos, map: None });
            if models.contains_key(name.as_str()) {
                return err(*pos, format!("{what}: a type holds no relation; {name} is a model"));
            }
            let leaf = field(t, member, ctx)?;
            if leaf.primary_key {
                return err(member.pos, format!("{what}: a type holds no @id"));
            }
            if leaf.auto_increment {
                return err(member.pos, format!("{what}: a type holds no @default(autoincrement())"));
            }
            fields.push(TypeFieldIr { name: member.name.clone(), nullable: *optional, list: *list, type_name: None, leaf: Some(leaf) });
        }
        out.push(TypeIr { name: t.name.clone(), storage, fields, comment });
    }
    for t in &out {
        let mut stack = vec![t.name.as_str()];
        if let Err(chain) = cycle(&out, t, &mut stack) {
            let pos = decls.iter().find(|d| d.name == t.name).map(|d| d.pos).unwrap_or_default();
            return err(pos, format!("type {} contains itself: {}", t.name, chain.join(" -> ")));
        }
    }
    for t in &out {
        ctx.nested.insert(t.name.clone(), t.clone());
    }
    Ok(out)
}

fn cycle<'t>(all: &'t [TypeIr], t: &'t TypeIr, stack: &mut Vec<&'t str>) -> std::result::Result<(), Vec<String>> {
    for f in &t.fields {
        let Some(n) = f.type_name.as_deref() else { continue };
        if n == stack[0] {
            let mut chain: Vec<String> = stack.iter().map(|s| (*s).to_owned()).collect();
            chain.push(n.to_owned());
            return Err(chain);
        }
        if stack.contains(&n) {
            continue;
        }
        stack.push(n);
        if let Some(inner) = all.iter().find(|x| x.name == n) {
            cycle(all, inner, stack)?;
        }
        stack.pop();
    }
    Ok(())
}

/// A type is required-bearing when it has a required scalar subfield, or a required
/// subfield of a required-bearing type.
fn required_bearing(ctx: &Ctx<'_>, ty: &str) -> bool {
    ctx.nested[ty].fields.iter().any(|f| match &f.type_name {
        None => !f.nullable,
        Some(n) => !f.nullable && !f.list && required_bearing(ctx, n),
    })
}

/// Every field that uses `ty`, for an error message.
fn uses(items: &[Item], ty: &str) -> String {
    let mut out = vec![];
    for item in items {
        let (Item::Model(d) | Item::Type(d)) = item else { continue };
        for m in d.members.iter().filter(|m| m.ty.name == ty) {
            out.push(format!("{}.{}", d.name, m.name));
        }
    }
    out.join(", ")
}

pub(super) fn is_nested(ctx: &Ctx<'_>, member: &Member) -> bool {
    ctx.nested.contains_key(&member.ty.name)
}

/// Lowers a model field whose type is a nested type.
pub(super) fn expand(m: &ModelDecl, member: &Member, items: &[Item], ctx: &Ctx<'_>) -> Result<Expansion> {
    let what = format!("{}.{}", m.name, member.name);
    let ty = &ctx.nested[&member.ty.name];
    if !member.ty.args.positional.is_empty() || !member.ty.args.named.is_empty() {
        return err(member.ty.args.pos, format!("{what}: {} takes no arguments", ty.name));
    }
    let mut storage = ty.storage;
    let mut prefix = None;
    for a in &member.attrs {
        if a.name != "storage" {
            return err(a.pos, format!("{what}: a field of a nested type takes @storage(columns | json), not @{}", a.name));
        }
        let mut n = Named::new(format!("{what}: @storage"), &a.args);
        storage = match a.args.positional.as_slice() {
            [(p, v)] => storage_of(*p, v, &format!("{what}: @storage"))?,
            _ => return err(a.pos, format!("{what}: @storage takes columns or json")),
        };
        prefix = n.str("prefix")?;
        n.finish()?;
    }
    let optional = member.ty.optional;
    let mut paths = Paths::new();
    if storage == NestedStorage::Json {
        if prefix.is_some() {
            return err(member.pos, format!("{what}: prefix: is for columns storage"));
        }
        json_checks(ty, ctx, &what, member.pos)?;
        walk_paths(ctx, ty, &member.name, &mut vec![], &mut paths);
        let mut f = FieldIr::plain(&member.name, ColType::Json);
        f.nullable = optional;
        f.nested = Some(NestedIr { ty: ty.name.clone(), storage, field: member.name.clone(), path: vec![], list: member.ty.list, under_null: false, present_with: vec![] });
        return Ok(Expansion { fields: vec![f], checks: vec![], paths });
    }
    if member.ty.list {
        return err(member.pos, format!("{what}: a list of {} needs @storage(json); columns storage holds no list of a type", ty.name));
    }
    let mut cx = Columns { ctx, items, what: &what, pos: member.pos, field: &member.name, ty: &ty.name, prefix, out: Expansion { fields: vec![], checks: vec![], paths }, claimed: vec![] };
    cx.level(ty, &mut vec![], &mut vec![], false, optional, member.pos)?;
    Ok(cx.out)
}

fn json_checks(ty: &TypeIr, ctx: &Ctx<'_>, what: &str, pos: Pos) -> Result<()> {
    for f in &ty.fields {
        match (&f.type_name, &f.leaf) {
            (Some(n), _) => json_checks(&ctx.nested[n], ctx, what, pos)?,
            (None, Some(l)) if l.check.is_some() || l.unique || l.index => {
                return err(
                    pos,
                    format!("{what}: {}.{} has a @check, @unique or @index, which a json field can't carry yet; use @storage(columns)", ty.name, f.name),
                )
            }
            _ => {}
        }
    }
    Ok(())
}

fn walk_paths(ctx: &Ctx<'_>, ty: &TypeIr, base: &str, sub: &mut Vec<String>, out: &mut Paths) {
    for f in &ty.fields {
        let path = format!("{base}.{}", f.name);
        sub.push(f.name.clone());
        match &f.type_name {
            Some(n) => {
                out.insert(path.clone(), PathTarget::Group);
                walk_paths(ctx, &ctx.nested[n], &path, sub, out);
            }
            None => {
                let leaf = if f.list { ColType::Json } else { f.leaf.as_ref().map_or(ColType::Text, |l| l.ty) };
                let column = base.split('.').next().unwrap_or(base).to_owned();
                out.insert(path, PathTarget::Json { column, sub: sub.clone(), leaf });
            }
        }
        sub.pop();
    }
}

struct Columns<'a> {
    ctx: &'a Ctx<'a>,
    items: &'a [Item],
    what: &'a str,
    pos: Pos,
    field: &'a str,
    ty: &'a str,
    prefix: Option<String>,
    out: Expansion,
    /// Per lowered field: a nullable level has already set its `present_with`.
    claimed: Vec<bool>,
}

impl Columns<'_> {
    fn column(&self, segments: &[String]) -> String {
        match &self.prefix {
            Some(p) => format!("{p}{}", segments.join("_")),
            None => format!("{}_{}", self.field, segments.join("_")),
        }
    }

    /// One level of the tree. `names` are the subfield names down to here, `segments` the
    /// column name parts; `under_null` says a nullable level is above.
    fn level(&mut self, ty: &TypeIr, names: &mut Vec<String>, segments: &mut Vec<String>, under_null: bool, nullable: bool, at_pos: Pos) -> Result<()> {
        if nullable && !required_bearing(self.ctx, &ty.name) {
            let at = if names.is_empty() { self.what.to_owned() } else { format!("{}.{}", self.what, names.join(".")) };
            return err(
                at_pos,
                format!(
                    "{at}: {t} has no required subfield, so a null value and an object with every subfield null are the same row; \
                     used by {users}. Make a subfield of {t} required, make the field non-null, or use @storage(json) on the field",
                    t = ty.name,
                    users = uses(self.items, &ty.name),
                ),
            );
        }
        let first = self.out.fields.len();
        let under_null = under_null || nullable;
        for f in &ty.fields {
            names.push(f.name.clone());
            let dotted = format!("{}.{}", self.field, names.join("."));
            match (&f.type_name, &f.leaf) {
                (Some(n), _) => {
                    if f.list {
                        return err(self.pos, format!("{}.{}: a list of {n} needs @storage(json); columns storage holds no list of a type", self.what, names.join(".")));
                    }
                    self.out.paths.insert(dotted, PathTarget::Group);
                    let member = &self.ctx.members[&(ty.name.clone(), f.name.clone())];
                    let (sub_pos, segment) = (member.pos, member.map.clone().unwrap_or_else(|| f.name.clone()));
                    segments.push(segment);
                    let inner = self.ctx.nested[n].clone();
                    self.level(&inner, names, segments, under_null, f.nullable, sub_pos)?;
                    segments.pop();
                }
                (None, Some(leaf)) => {
                    let mut l = leaf.clone();
                    let seg = |last: &str| {
                        let mut s = segments.clone();
                        s.push(last.to_owned());
                        s
                    };
                    let column = self.column(&seg(&leaf.column));
                    l.renamed_from = leaf.renamed_from.as_ref().map(|old| self.column(&seg(old)));
                    l.name = format!("{}_{}", self.field, names.join("_"));
                    l.column = column;
                    l.nullable = leaf.nullable || under_null;
                    if let Some(c) = &l.check {
                        if !c.contains("{}") {
                            let at = self.ctx.members[&(ty.name.clone(), f.name.clone())].pos;
                            return err(at, format!("{}.{}: a @check on a subfield names its column with {{}}, as in @check({{}} > 0); the joined column name changes with prefix: and @map", self.what, names.join(".")));
                        }
                        l.check = Some(c.replace("{}", &ident(&l.column)));
                    }
                    l.nested = Some(NestedIr { ty: self.ty.to_owned(), storage: NestedStorage::Columns, field: self.field.to_owned(), path: names.clone(), list: false, under_null, present_with: vec![] });
                    self.out.paths.insert(dotted, PathTarget::Column(l.name.clone()));
                    self.out.fields.push(l);
                }
                (None, None) => unreachable!("a subfield is a leaf or a nested type"),
            }
            names.pop();
        }
        if nullable {
            let last = self.out.fields.len();
            self.claimed.resize(last, false);
            if last - first > 1 {
                for i in first..last {
                    if !self.claimed[i] {
                        let others = (first..last).filter(|&j| j != i).map(|j| self.out.fields[j].column.clone()).collect();
                        self.out.fields[i].nested.as_mut().unwrap().present_with = others;
                    }
                }
            }
            self.claimed[first..].fill(true);
        }
        if nullable && self.out.fields.len() - first > 1 {
            let leaves: Vec<(&FieldIr, bool)> = self.out.fields[first..]
                .iter()
                .map(|f| {
                    let required = leaf_required(self.ctx, ty, &f.nested.as_ref().unwrap().path[names.len()..]);
                    (f, required)
                })
                .collect();
            let all = leaves.iter().map(|(f, _)| format!("{} IS NULL", ident(&f.column))).collect::<Vec<_>>().join(" AND ");
            let req = leaves.iter().filter(|(_, r)| *r).map(|(f, _)| format!("{} IS NOT NULL", ident(&f.column))).collect::<Vec<_>>().join(" AND ");
            self.out.checks.push(ConstraintIr::Check { name: None, expr: format!("({all}) OR ({req})") });
        }
        Ok(())
    }
}

/// Whether the leaf at `path` below `ty` must be set when its nullable ancestors are set.
fn leaf_required(ctx: &Ctx<'_>, ty: &TypeIr, path: &[String]) -> bool {
    let Some(f) = ty.fields.iter().find(|f| f.name == path[0]) else { return false };
    match &f.type_name {
        None => !f.nullable,
        Some(n) => !f.nullable && leaf_required(ctx, &ctx.nested[n], &path[1..]),
    }
}

/// Rewrites the dotted subfield paths of an `@@index` or `@@unique` key list to the lowered
/// field names.
pub(super) fn resolve_paths(b: &Attr, paths: &Paths, model: &str, dialect: Dialect) -> Result<Attr> {
    if !matches!(b.name.as_str(), "index" | "unique") {
        return Ok(b.clone());
    }
    let mut out = b.clone();
    let mut expression = false;
    if let Some((_, Value::List(items))) = out.args.positional.first_mut() {
        for (p, v) in items.iter_mut() {
            let inner = match v {
                Value::Desc(i) => i.as_mut(),
                v => v,
            };
            let Value::Path(path, key_args) = inner else { continue };
            if path.len() < 2 {
                continue;
            }
            let dotted = path.join(".");
            match paths.get(&dotted) {
                Some(PathTarget::Column(name)) => *path = vec![name.clone()],
                Some(PathTarget::Json { column, sub, leaf }) => {
                    // The planner renders the same text, so the database can use the index for a filter.
                    let expr = dialect.json_subfield_sql(&ident(column), sub, *leaf);
                    let mut args = key_args.take().unwrap_or(Args { pos: *p, ..Default::default() });
                    args.positional.insert(0, (*p, Value::Str(expr)));
                    expression = true;
                    *path = vec!["sql".to_owned()];
                    *key_args = Some(args);
                }
                Some(PathTarget::Group) => return err(*p, format!("{model}: @@{}: {dotted} is a nested type, not a scalar; name one of its subfields", b.name)),
                None => return err(*p, format!("{model}: @@{}: no field {dotted}", b.name)),
            }
        }
    }
    // A UNIQUE constraint can't hold an expression; a unique index can.
    if expression && out.name == "unique" {
        out.name = "index".to_owned();
        out.args.named.push(("unique".to_owned(), b.pos, Value::Bool(true)));
    }
    Ok(out)
}
