//! Nested types in the planner: whole-field tests, updates, and result objects.
//!
//! A `columns` field has one column per scalar leaf; a `json` field has one JSON column.
//! A leaf in a JSON value is written as in `leaf_json` and `json_param`: BigInt and
//! Decimal as text, DateTime as RFC 3339 UTC text, and a missing subfield as null.

use super::*;
use orm_core::ir::{NestedIr, ObjectField, TypeFieldIr, TypeIr};

/// A nested type on `columns` storage, at `sub` below its model field: the leaf columns under it.
pub(super) struct Group<'s> {
    ty: &'s TypeIr,
    sub: Vec<String>,
    leaves: Vec<&'s FieldIr>,
}

/// What a JSON value written by an update has at one position.
#[derive(Clone, Copy)]
enum Target<'s> {
    Leaf(ColType, bool),
    Type(&'s TypeIr, bool),
}

/// The patch of one JSON column: the new value of each assigned path.
enum Node {
    Val(String),
    /// A whole nested value: the old value of the path is dropped, not merged.
    Replace(String),
    Branch(Vec<(String, Node)>),
}

fn sql_lit(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

fn object_fn(d: Dialect) -> &'static str {
    if d == Dialect::Postgres { "jsonb_build_object" } else { "json_object" }
}

fn json_null(d: Dialect) -> &'static str {
    if d == Dialect::Postgres { "'null'::jsonb" } else { "NULL" }
}

/// SQLite loses the JSON subtype of a `CASE`, so a nested object goes through `json()`.
fn object_value(d: Dialect, sql: String) -> String {
    if d == Dialect::Sqlite { format!("json({sql})") } else { sql }
}

/// A leaf column (slot `$n`) as a JSON value.
fn leaf_json(d: Dialect, ty: ColType, n: usize) -> String {
    match (d, ty) {
        (Dialect::Postgres, ColType::DateTime) => format!("to_char(${n} AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"')"),
        (Dialect::Postgres, ColType::Date) => format!("to_char(${n}, 'YYYY-MM-DD')"),
        (Dialect::Postgres, ColType::BigInt | ColType::Decimal) => format!("${n}::text"),
        (Dialect::Sqlite, ColType::BigInt) => format!("CAST(${n} AS TEXT)"),
        (Dialect::Sqlite, ColType::Bool) => format!("json(CASE WHEN ${n} IS NULL THEN NULL WHEN ${n} THEN 'true' ELSE 'false' END)"),
        (Dialect::Sqlite, ColType::Json) => format!("json(${n})"),
        _ => format!("${n}"),
    }
}

/// A bound parameter (slot `$n`, bound as `json_subfield_type` of `ty`) as a JSON value.
fn json_param(d: Dialect, ty: ColType, n: usize) -> String {
    if d == Dialect::Sqlite {
        return leaf_json(d, ty, n);
    }
    let value = match ty {
        ColType::Json => return format!("COALESCE(${n}::jsonb, 'null'::jsonb)"),
        ColType::Int => format!("${n}::integer"),
        ColType::Float => format!("${n}::double precision"),
        ColType::Bool => format!("${n}::boolean"),
        ColType::BigInt => format!("(${n}::bigint)::text"),
        ColType::Decimal => format!("(${n}::numeric)::text"),
        ColType::Uuid => format!("(${n}::uuid)::text"),
        ColType::String | ColType::Text | ColType::DateTime | ColType::Date => format!("${n}::text"),
    };
    format!("COALESCE(to_jsonb({value}), 'null'::jsonb)")
}

fn target_of<'s>(planner: &Planner<'s>, tf: &'s TypeFieldIr) -> Result<Target<'s>> {
    Ok(match (&tf.type_name, &tf.leaf) {
        (Some(t), _) => Target::Type(planner.type_named(t)?, tf.list),
        (None, _) if tf.list => Target::Leaf(ColType::Json, true),
        (None, Some(l)) => Target::Leaf(l.ty, false),
        (None, None) => unreachable!("a subfield is a leaf or a nested type"),
    })
}

fn insert_node(children: &mut Vec<(String, Node)>, sub: &[String], sql: String, replace: bool) -> Result<()> {
    let (k, rest) = sub.split_first().expect("a patch path is not empty");
    let at = children.iter().position(|(n, _)| n == k);
    if rest.is_empty() {
        if at.is_some() {
            return Err(Error::query(format!("{k} is assigned twice")));
        }
        children.push((k.clone(), if replace { Node::Replace(sql) } else { Node::Val(sql) }));
        return Ok(());
    }
    match at {
        Some(i) => match &mut children[i].1 {
            Node::Branch(c) => insert_node(c, rest, sql, replace),
            _ => Err(Error::query(format!("{k} is assigned as a whole and by subfield"))),
        },
        None => {
            let mut c = vec![];
            insert_node(&mut c, rest, sql, replace)?;
            children.push((k.clone(), Node::Branch(c)));
            Ok(())
        }
    }
}

/// PostgreSQL: `base || jsonb_build_object(...)` at each level, reading `base` from the column
/// so the text grows with the number of paths, not with their depth.
fn pg_merge(children: &[(String, Node)], at: &mut Vec<String>, col: &str) -> String {
    let base = if at.is_empty() { col.to_owned() } else { format!("({col} #> ARRAY[{}])", at.iter().map(|k| sql_lit(k)).collect::<Vec<_>>().join(", ")) };
    let mut parts = vec![];
    for (k, n) in children {
        let v = match n {
            Node::Val(s) | Node::Replace(s) => s.clone(),
            Node::Branch(c) => {
                at.push(k.clone());
                let s = pg_merge(c, at, col);
                at.pop();
                s
            }
        };
        parts.push(format!("{}, {v}", sql_lit(k)));
    }
    format!("COALESCE(NULLIF({base}, 'null'::jsonb), '{{}}'::jsonb) || jsonb_build_object({})", parts.join(", "))
}

/// SQLite: the patch as a JSON object for `json_patch`; `drop` keeps only the replaced paths, as null.
fn sqlite_tree(children: &[(String, Node)], drop: bool) -> Option<String> {
    let mut parts = vec![];
    for (k, n) in children {
        let v = match n {
            Node::Val(s) if !drop => s.clone(),
            Node::Replace(s) => if drop { "NULL".to_owned() } else { s.clone() },
            Node::Branch(c) => match sqlite_tree(c, drop) {
                Some(s) => s,
                None => continue,
            },
            Node::Val(_) => continue,
        };
        parts.push(format!("{}, {v}", sql_lit(k)));
    }
    (!parts.is_empty()).then(|| format!("json_object({})", parts.join(", ")))
}

fn has_replace(children: &[(String, Node)]) -> bool {
    children.iter().any(|(_, n)| match n {
        Node::Replace(_) => true,
        Node::Branch(c) => has_replace(c),
        Node::Val(_) => false,
    })
}

impl<'s> Planner<'s> {
    fn type_named(&self, name: &str) -> Result<&'s TypeIr> {
        self.schema.types.iter().find(|t| t.name == name).ok_or_else(|| Error::query(format!("unknown nested type {name}")))
    }

    /// The nested type at `sub` below `nested`, if the path names one (not a list).
    fn type_at(&self, nested: &NestedIr, sub: &[String]) -> Option<&'s TypeIr> {
        if nested.list {
            return None;
        }
        let mut ty = self.type_named(&nested.ty).ok()?;
        for k in sub {
            let tf = ty.fields.iter().find(|f| &f.name == k)?;
            if tf.list {
                return None;
            }
            ty = self.type_named(tf.type_name.as_deref()?).ok()?;
        }
        Some(ty)
    }

    /// The leaf columns of a `columns` nested type at `sub` below the field `name`.
    pub(super) fn group(&self, model: &'s Model, name: &str, sub: &[String]) -> Option<Group<'s>> {
        let leaves: Vec<&FieldIr> = model
            .fields()
            .iter()
            .filter(|f| f.nested.as_ref().is_some_and(|n| n.storage == NestedStorage::Columns && n.field == name && n.path.starts_with(sub)))
            .collect();
        let ty = self.type_at(leaves.first()?.nested.as_ref()?, sub)?;
        Some(Group { ty, sub: sub.to_vec(), leaves })
    }

    /// Whether the leaf at `path` below `ty` must be set when its nullable ancestors are set.
    fn required(&self, ty: &TypeIr, path: &[String]) -> bool {
        let Some((first, rest)) = path.split_first() else { return false };
        let Some(f) = ty.fields.iter().find(|f| &f.name == first) else { return false };
        match &f.type_name {
            None => !f.nullable,
            Some(n) => !f.nullable && !f.list && self.type_named(n).is_ok_and(|t| self.required(t, rest)),
        }
    }

    /// The column that is NULL exactly when the group at `prefix` is null: its generated
    /// CHECK says a null group has every column null and a set one has each required column set.
    fn null_leaf(&self, leaves: &[&'s FieldIr], ty: &TypeIr, prefix: &[String]) -> Option<&'s FieldIr> {
        leaves.iter().copied().find(|l| l.nullable && l.nested.as_ref().is_some_and(|n| n.path.starts_with(prefix) && self.required(ty, &n.path[prefix.len()..])))
    }

    fn guard(&self, path: &[String], g: &Group<'s>, ty: &TypeIr, prefix: &[String], sql: String, args: &mut Vec<SExpr>) -> Result<String> {
        let Some(leaf) = self.null_leaf(&g.leaves, ty, prefix) else { return Ok(sql) };
        args.push(self.resolve(path, &leaf.name)?);
        Ok(format!("CASE WHEN ${} IS NULL THEN NULL ELSE {sql} END", args.len()))
    }

    /// The object of a `columns` nested type, built from its leaf columns.
    pub(super) fn assemble(&self, path: &[String], g: &Group<'s>) -> Result<SExpr> {
        let d = self.target.dialect;
        let mut args = vec![];
        let sql = self.object_sql(path, g, g.ty, &mut g.sub.clone(), &mut args)?;
        let sql = self.guard(path, g, g.ty, &g.sub, sql, &mut args)?;
        Ok(template(d, object_value(d, sql), args))
    }

    fn object_sql(&self, path: &[String], g: &Group<'s>, ty: &'s TypeIr, prefix: &mut Vec<String>, args: &mut Vec<SExpr>) -> Result<String> {
        let d = self.target.dialect;
        let mut parts = vec![];
        for tf in &ty.fields {
            prefix.push(tf.name.clone());
            let value = match &tf.type_name {
                Some(n) => {
                    let inner = self.type_named(n)?;
                    let obj = self.object_sql(path, g, inner, prefix, args)?;
                    let obj = if tf.nullable { self.guard(path, g, inner, prefix, obj, args)? } else { obj };
                    object_value(d, obj)
                }
                None => {
                    let leaf = g.leaves.iter().find(|l| l.nested.as_ref().is_some_and(|n| n.path == *prefix));
                    let leaf = leaf.ok_or_else(|| Error::query(format!("no column for the subfield {}", prefix.join("."))))?;
                    args.push(self.resolve(path, &leaf.name)?);
                    leaf_json(d, leaf.ty, args.len())
                }
            };
            parts.push(format!("{}, {value}", sql_lit(&tf.name)));
            prefix.pop();
        }
        Ok(format!("{}({})", object_fn(d), parts.join(", ")))
    }

    /// The nested type a column expression names as a whole, if it does.
    fn whole_type(&self, e: &Expr) -> Option<&'s TypeIr> {
        let Expr::Col { path, name, sub } = e else { return None };
        let m = self.walk(self.root, path).ok()?;
        match self.sub_ref(m, name, sub).ok()? {
            SubRef::Group(g) => Some(g.ty),
            SubRef::Json { field, group: true, .. } => self.type_at(field.nested.as_ref()?, sub),
            SubRef::Column(f) if sub.is_empty() => self.type_at(f.nested.as_ref().filter(|n| n.storage == NestedStorage::Json)?, sub),
            _ => None,
        }
    }

    /// A whole nested field has no order.
    pub(super) fn reject_whole_nested(&self, e: &Expr) -> Result<()> {
        match e {
            Expr::Col { .. } if self.whole_type(e).is_some() => Err(Error::query("a nested field has no order; order by one of its subfields")),
            _ => Ok(()),
        }
    }

    /// Rewrites a test of a whole nested field into tests of its leaves: `== object` is the
    /// `AND` of the leaf comparisons (a subfield left out is null), `== None` and `IS NULL`
    /// on `columns` storage read the one column that is NULL exactly when the group is null.
    pub(super) fn whole_field(&self, e: &Expr) -> Result<Option<Expr>> {
        match e {
            Expr::Cmp { op: op @ (CmpOp::Eq | CmpOp::Ne), l, r } => {
                let (col, other) = match (&**l, &**r) {
                    (c @ Expr::Col { .. }, o) | (o, c @ Expr::Col { .. }) => (c, o),
                    _ => return Ok(None),
                };
                let Some(ty) = self.whole_type(col) else { return Ok(None) };
                let Expr::Col { path, name, sub } = col else { unreachable!("whole_type needs a column") };
                let eq = match other {
                    Expr::Null => return Ok(Some(Expr::IsNull { item: Box::new(col.clone()), neg: matches!(op, CmpOp::Ne) })),
                    Expr::Object { fields } => {
                        let mut items = vec![];
                        self.object_cmp(path, name, &mut sub.clone(), ty, fields, &mut items)?;
                        Expr::And { items }
                    }
                    _ => return Err(Error::query("a nested field compares with an object or None, not a parameter or an expression")),
                };
                Ok(Some(if matches!(op, CmpOp::Ne) { Expr::Not { item: Box::new(eq) } } else { eq }))
            }
            Expr::IsNull { item, neg } => {
                let Expr::Col { path, name, sub } = &**item else { return Ok(None) };
                let m = self.walk(self.root, path)?;
                let Ok(SubRef::Group(g)) = self.sub_ref(m, name, sub) else { return Ok(None) };
                Ok(Some(match self.null_leaf(&g.leaves, g.ty, &g.sub) {
                    Some(leaf) => Expr::IsNull { item: Box::new(Expr::Col { path: path.clone(), name: name.clone(), sub: leaf.nested.as_ref().unwrap().path.clone() }), neg: *neg },
                    None => Expr::Const { value: *neg },
                }))
            }
            _ => Ok(None),
        }
    }

    fn object_cmp(&self, path: &[String], name: &str, sub: &mut Vec<String>, ty: &TypeIr, fields: &[ObjectField], out: &mut Vec<Expr>) -> Result<()> {
        self.check_object(ty, fields)?;
        for tf in &ty.fields {
            sub.push(tf.name.clone());
            let at = Expr::Col { path: path.to_vec(), name: name.to_owned(), sub: sub.clone() };
            match fields.iter().find(|f| f.name == tf.name).map(|f| &f.value) {
                None | Some(Expr::Null) => out.push(Expr::IsNull { item: Box::new(at), neg: false }),
                Some(Expr::Object { fields }) if tf.type_name.is_some() && !tf.list => {
                    self.object_cmp(path, name, sub, self.type_named(tf.type_name.as_deref().unwrap())?, fields, out)?
                }
                Some(v) if tf.type_name.is_none() && !tf.list && !matches!(v, Expr::Object { .. }) => {
                    out.push(Expr::Cmp { op: CmpOp::Eq, l: Box::new(at), r: Box::new(v.clone()) })
                }
                Some(_) => return Err(Error::query(format!("{}.{}: a list or a mismatched value can't compare in a whole-field test", ty.name, tf.name))),
            }
            sub.pop();
        }
        Ok(())
    }

    /// An object names only subfields of its type.
    fn check_object(&self, ty: &TypeIr, fields: &[ObjectField]) -> Result<()> {
        for f in fields {
            let tf = ty.fields.iter().find(|t| t.name == f.name).ok_or_else(|| Error::query(format!("{} has no subfield {}", ty.name, f.name)))?;
            if let Expr::Object { fields: inner } = &f.value {
                match (&tf.type_name, tf.list) {
                    (Some(n), false) => self.check_object(self.type_named(n)?, inner)?,
                    _ => return Err(Error::query(format!("{}.{} is not a nested type, so it takes no object", ty.name, tf.name))),
                }
            }
        }
        Ok(())
    }

    /// The value for the leaf at `rel` in an assigned object; `None` is null (left out, or under a null).
    fn pick<'e>(&self, value: &'e Expr, rel: &[String]) -> Result<Option<&'e Expr>> {
        let mut cur = Some(value);
        for k in rel {
            match cur {
                Some(Expr::Object { fields }) => cur = fields.iter().find(|f| &f.name == k).map(|f| &f.value),
                Some(Expr::Null) | None => return Ok(None),
                Some(_) => return Err(Error::query(format!("{k}: a nested subfield takes an object or None"))),
            }
        }
        match cur {
            Some(Expr::Null) | None => Ok(None),
            Some(Expr::Object { .. }) => Err(Error::query("a scalar subfield takes no object")),
            some => Ok(some),
        }
    }

    /// Splits `set` into the assignments of plain fields, which `update()` plans as before,
    /// and the `(column, value)` pairs of nested fields: one per leaf column of a `columns`
    /// field, one patched or replaced value per `json` field.
    pub(super) fn nested_sets(&mut self, set: &[Assignment]) -> Result<NestedSets> {
        let root = self.model(self.root);
        let d = self.target.dialect;
        let mut regular = vec![];
        let mut out: Vec<(String, SExpr)> = vec![];
        let mut patches: Vec<Patch<'s>> = vec![];
        for a in set {
            let Ok(f) = root.field(&a.field) else {
                match self.sub_ref(self.root, &a.field, &a.sub)? {
                    SubRef::Column(leaf) => {
                        let v = self.value(&a.value, Hint { ty: Some(leaf.value_type()), field: Some(leaf) })?;
                        out.push((leaf.column.clone(), v));
                    }
                    SubRef::Group(g) => {
                        match &a.value {
                            Expr::Null => {}
                            Expr::Object { fields } => self.check_object(g.ty, fields)?,
                            _ => return Err(Error::query(format!("{}: a nested field is assigned an object or None", a.field))),
                        }
                        for leaf in &g.leaves {
                            let rel = &leaf.nested.as_ref().unwrap().path[g.sub.len()..];
                            let v = match self.pick(&a.value, rel)? {
                                None => SExpr::cust("NULL"),
                                Some(e) => self.value(e, Hint { ty: Some(leaf.value_type()), field: Some(leaf) })?,
                            };
                            out.push((leaf.column.clone(), v));
                        }
                    }
                    SubRef::Json { .. } => unreachable!("a JSON field is found by name"),
                }
                continue;
            };
            let Some(n) = f.nested.as_ref().filter(|n| n.storage == NestedStorage::Json && n.path.is_empty()) else {
                if !a.sub.is_empty() {
                    return Err(Error::query(format!("{} has no subfields", a.field)));
                }
                regular.push(a.clone());
                continue;
            };
            if a.sub.is_empty() {
                if matches!(a.value, Expr::Object { .. }) {
                    let ty = self.type_at(n, &[]).ok_or_else(|| Error::query(format!("{} is a list; assign a parameter", a.field)))?;
                    let mut args = vec![];
                    let sql = self.json_in(Target::Type(ty, false), &a.value, &mut args)?;
                    out.push((f.column.clone(), template(d, sql, args)));
                } else {
                    regular.push(a.clone());
                }
                continue;
            }
            let target = self.json_target(n, &a.sub)?;
            let at = match patches.iter().position(|(pf, ..)| pf.name == f.name) {
                Some(i) => i,
                None => {
                    patches.push((f, vec![], vec![]));
                    patches.len() - 1
                }
            };
            let (_, tree, args) = &mut patches[at];
            let sql = self.json_in(target, &a.value, args)?;
            insert_node(tree, &a.sub, sql, matches!(target, Target::Type(..)))?;
        }
        for (f, tree, mut args) in patches {
            args.push(self.resolve(&[], &f.name)?);
            let col = format!("${}", args.len());
            let sql = if d == Dialect::Postgres {
                pg_merge(&tree, &mut vec![], &col)
            } else {
                let mut base = format!("COALESCE({col}, '{{}}')");
                if has_replace(&tree) {
                    base = format!("json_patch({base}, {})", sqlite_tree(&tree, true).expect("a replaced path"));
                }
                format!("json_patch({base}, {})", sqlite_tree(&tree, false).expect("an assigned path"))
            };
            out.push((f.column.clone(), template(d, sql, args)));
        }
        let mut seen = std::collections::BTreeSet::new();
        if let Some((c, _)) = out.iter().find(|(c, _)| !seen.insert(c.as_str())) {
            return Err(Error::query(format!("column {c} is assigned twice")));
        }
        Ok((regular, out))
    }

    fn json_target(&self, n: &NestedIr, sub: &[String]) -> Result<Target<'s>> {
        let fail = |why: &str| Error::query(format!("{}.{}: {why}", n.field, sub.join(".")));
        if n.list {
            return Err(fail("a subfield of a list can't be assigned yet"));
        }
        let mut ty = self.type_named(&n.ty)?;
        for (i, k) in sub.iter().enumerate() {
            let tf = ty.fields.iter().find(|f| &f.name == k).ok_or_else(|| fail(&format!("{} has no subfield {k}", ty.name)))?;
            let last = i + 1 == sub.len();
            if !last && (tf.list || tf.type_name.is_none()) {
                return Err(fail(&format!("{k} has no subfields to assign")));
            }
            if last {
                return target_of(self, tf);
            }
            ty = self.type_named(tf.type_name.as_deref().unwrap())?;
        }
        unreachable!("a subfield path is not empty")
    }

    /// An assigned value as a JSON value of the dialect, in the slots of `args`.
    fn json_in(&self, t: Target<'s>, e: &Expr, args: &mut Vec<SExpr>) -> Result<String> {
        let d = self.target.dialect;
        match (e, t) {
            (Expr::Null, _) => Ok(json_null(d).to_owned()),
            (Expr::Param { i }, t) => {
                let ty = match t {
                    Target::Leaf(ty, false) => ty,
                    _ => ColType::Json,
                };
                let v = self.params.value(self.param(*i)?, Some(ValueType::scalar(d.json_subfield_type(ty))))?;
                args.push(bind(v, None));
                Ok(json_param(d, ty, args.len()))
            }
            (Expr::Object { fields }, Target::Type(ty, false)) => {
                self.check_object(ty, fields)?;
                let mut parts = vec![];
                for tf in &ty.fields {
                    let v = match fields.iter().find(|f| f.name == tf.name) {
                        Some(f) => self.json_in(target_of(self, tf)?, &f.value, args)?,
                        None => json_null(d).to_owned(),
                    };
                    parts.push(format!("{}, {v}", sql_lit(&tf.name)));
                }
                Ok(format!("{}({})", object_fn(d), parts.join(", ")))
            }
            _ => Err(Error::query("a nested value is a parameter, an object or None")),
        }
    }
}

/// An insert writes NULL, not the column default, for each nullable leaf of a `columns`
/// field whose cells are all unset in a row. Returns the fields with the missing leaf
/// columns added, and which cells hold that NULL.
pub(super) fn null_nested(m: &Model, fields: &[String], rows: &mut [Vec<Option<Value>>]) -> (Vec<String>, Vec<Vec<bool>>) {
    let mut fields = fields.to_vec();
    let mut owners: Vec<&str> = vec![];
    for f in m.fields() {
        if let Some(n) = f.nested.as_ref().filter(|n| n.storage == NestedStorage::Columns) {
            if !owners.iter().any(|o| *o == n.field) {
                owners.push(&n.field);
            }
        }
    }
    let mut forced: Vec<Vec<bool>> = vec![vec![]; rows.len()];
    for owner in owners {
        let leaves: Vec<&FieldIr> = m.fields().iter().filter(|f| f.nested.as_ref().is_some_and(|n| n.storage == NestedStorage::Columns && n.field == owner)).collect();
        for leaf in leaves.iter().filter(|l| l.nullable) {
            #[allow(clippy::manual_contains)] // `contains` is shadowed by sea_query::ExprTrait
            if !fields.iter().any(|f| *f == leaf.name) {
                fields.push(leaf.name.clone());
            }
        }
        let cells: Vec<usize> = leaves.iter().filter_map(|l| fields.iter().position(|f| *f == l.name)).collect();
        for (row, mask) in rows.iter_mut().zip(forced.iter_mut()) {
            row.resize(fields.len(), None);
            mask.resize(fields.len(), false);
            if cells.iter().all(|&c| row[c].is_none()) {
                for (&c, _) in cells.iter().zip(&leaves).filter(|(_, l)| l.nullable) {
                    mask[c] = true;
                }
            }
        }
    }
    for mask in &mut forced {
        mask.resize(fields.len(), false);
    }
    (fields, forced)
}

type NestedSets = (Vec<Assignment>, Vec<(String, SExpr)>);
type Patch<'s> = (&'s FieldIr, Vec<(String, Node)>, Vec<SExpr>);
