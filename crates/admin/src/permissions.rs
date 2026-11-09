//! Permissions: the names of a site strategy, the grants of one user (loaded once per request),
//! and the metadata that a user may see.
//!
//! The names live in code and the grants in the auth tables (`Permission`, `Role`, and the join
//! models of `User`). Deny by default: an unknown user, an inactive user and a user with no
//! grant get nothing. An active superuser has every name, but `allow=` still limits a resource.

use std::collections::BTreeSet;

use monorm_core::ir::{ColType, RelKind};
use monorm_engine::db::{Cell, Executor};
use sea_query::Value;
use serde_json::{json, Value as Json};

use crate::json;
use crate::site::{Resource, Site};
use crate::sql::{mark, Table};

/// The site gate: every strategy needs it.
pub const GATE: &str = "admin.access";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Strategy {
    Page,
    Model,
    Access,
}

impl Strategy {
    pub fn parse(text: &str) -> Option<Strategy> {
        match text {
            "page" => Some(Strategy::Page),
            "model" => Some(Strategy::Model),
            "access" => Some(Strategy::Access),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Strategy::Page => "page",
            Strategy::Model => "model",
            Strategy::Access => "access",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Op {
    Read,
    Create,
    Update,
    Delete,
}

impl Op {
    pub const ALL: [Op; 4] = [Op::Read, Op::Create, Op::Update, Op::Delete];

    pub fn parse(text: &str) -> Option<Op> {
        match text {
            "read" => Some(Op::Read),
            "create" => Some(Op::Create),
            "update" => Some(Op::Update),
            "delete" => Some(Op::Delete),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Op::Read => "read",
            Op::Create => "create",
            Op::Update => "update",
            Op::Delete => "delete",
        }
    }
}

/// What one user may do. `all` is an active superuser; `names` are `<label>.<codename>`.
#[derive(Debug, Clone, Default)]
pub struct Grants {
    pub all: bool,
    pub names: BTreeSet<String>,
}

impl Grants {
    pub fn has(&self, name: &str) -> bool {
        self.all || self.names.contains(name)
    }

    pub fn has_any(&self, names: &[String]) -> bool {
        self.all || names.iter().any(|n| self.names.contains(n))
    }
}

/// The permission that switches the named read default `default` off: `<label>.bypass_<default>_<key>`.
pub fn bypass_name(r: &Resource, default: &str) -> String {
    format!("{}.bypass_{default}_{}", r.label, r.key)
}

impl Site {
    /// The names of the chosen strategy: the gate first, then the names of each resource.
    pub fn permission_names(&self) -> Vec<String> {
        let mut out = vec![GATE.to_string()];
        let mut seen: BTreeSet<String> = out.iter().cloned().collect();
        let mut push = |name: String| {
            if seen.insert(name.clone()) {
                out.push(name);
            }
        };
        for r in &self.resources {
            let l = &r.label;
            let k = &r.key;
            match self.strategy {
                Strategy::Access => continue,
                Strategy::Page => {
                    push(format!("{l}.read_{k}"));
                    push(format!("{l}.manage_{k}"));
                    if r.split_delete {
                        push(format!("{l}.delete_{k}"));
                    }
                }
                Strategy::Model => {
                    for verb in ["add", "change", "delete", "view"] {
                        push(format!("{l}.{verb}_{k}"));
                    }
                }
            }
            for f in &r.field_permissions {
                push(format!("{l}.{f}"));
            }
            for a in &r.actions {
                push(format!("{l}.{a}_{k}"));
            }
            for d in &r.bypass {
                push(bypass_name(r, d));
            }
        }
        out
    }

    /// The labels of the permission names: the gate label first, then the labels of the resources.
    pub fn labels(&self) -> Vec<String> {
        let mut out = vec![GATE.split('.').next().unwrap_or("admin").to_string()];
        for r in &self.resources {
            if !out.contains(&r.label) {
                out.push(r.label.clone());
            }
        }
        out
    }

    /// The names of which one is enough for `op` on `r`. Empty in the `access` strategy.
    pub fn requires(&self, r: &Resource, op: Op) -> Vec<String> {
        let (l, k) = (&r.label, &r.key);
        let names: &[&str] = match (self.strategy, op) {
            (Strategy::Access, _) => &[],
            (Strategy::Page, Op::Read) => &["read", "manage"],
            (Strategy::Page, Op::Delete) if r.split_delete => &["delete"],
            (Strategy::Page, _) => &["manage"],
            (Strategy::Model, Op::Read) => &["view", "change"],
            (Strategy::Model, Op::Create) => &["add"],
            (Strategy::Model, Op::Update) => &["change"],
            (Strategy::Model, Op::Delete) => &["delete"],
        };
        names.iter().map(|n| format!("{l}.{n}_{k}")).collect()
    }

    /// True when the user passes the site gate.
    pub fn gate(&self, grants: &Grants) -> bool {
        grants.has(GATE)
    }

    /// `allow=` first, for every user; then the gate; then the names of the strategy.
    pub fn allows(&self, grants: &Grants, r: &Resource, op: Op) -> bool {
        if !r.allow.contains(&op) || !self.gate(grants) {
            return false;
        }
        let needed = self.requires(r, op);
        needed.is_empty() || grants.has_any(&needed)
    }

    /// The metadata of one user: only the resources, actions and names that the user can use.
    pub fn metadata(&self, grants: &Grants) -> Json {
        let resources: Vec<Json> = self
            .resources
            .iter()
            .filter(|r| self.allows(grants, r, Op::Read))
            .map(|r| {
                let mut meta = r.meta.clone();
                meta["actions"] = json!(r.actions.iter().filter(|a| self.allowed_extra(grants, r, &format!("{}.{a}_{}", r.label, r.key))).collect::<Vec<_>>());
                meta["ops"] = json!([Op::Create, Op::Update, Op::Delete].iter().filter(|o| r.allow.contains(o) && self.allows(grants, r, **o)).map(|o| o.name()).collect::<Vec<_>>());
                meta["bypass"] = json!(self.bypassable(grants, r));
                meta["field_permissions"] = json!(r.field_permissions.iter().filter(|f| self.allowed_extra(grants, r, &format!("{}.{f}", r.label))).collect::<Vec<_>>());
                meta
            })
            .collect();
        let permissions: Vec<String> = if self.gate(grants) { self.permission_names().into_iter().filter(|n| grants.has(n)).collect() } else { Vec::new() };
        json!({"resources": resources, "permissions": permissions})
    }

    /// The read defaults of `r` that the user may switch off for a request.
    pub fn bypassable(&self, grants: &Grants, r: &Resource) -> Vec<String> {
        r.bypass.iter().filter(|d| self.allowed_extra(grants, r, &bypass_name(r, d))).cloned().collect()
    }

    /// A field or action name: the user holds it, and has read access to the resource.
    fn allowed_extra(&self, grants: &Grants, r: &Resource, name: &str) -> bool {
        grants.has(name) && self.allows(grants, r, Op::Read)
    }
}

/// The grants of `user` (the JSON of the request user): no query for a superuser or an inactive user.
pub async fn load(site: &Site, conn: &dyn Executor, user: &Json) -> Result<Grants, String> {
    if user.get("is_active").and_then(Json::as_bool) == Some(false) {
        return Ok(Grants::default());
    }
    if user.get("is_superuser").and_then(Json::as_bool) == Some(true) {
        return Ok(Grants { all: true, names: BTreeSet::new() });
    }
    let Some(key) = user.get("key").filter(|k| !k.is_null()) else {
        return Ok(Grants::default());
    };
    let Some(plan) = plan(site)? else {
        return Ok(Grants::default());
    };
    let arg = bind_key(plan.key_type, key)?;
    let d = conn.dialect();
    let sql = plan.sql(d);
    let rows = conn.fetch(sql, vec![arg.clone(), arg]).await.map_err(|e| e.message)?;
    let mut names = BTreeSet::new();
    for i in 0..rows.len() {
        let (label, codename) = (rows.cell(i, 0).map_err(|e| e.message)?, rows.cell(i, 1).map_err(|e| e.message)?);
        if let (Cell::Text(label), Cell::Text(codename)) = (label, codename) {
            names.insert(format!("{label}.{codename}"));
        }
    }
    Ok(Grants { all: false, names })
}

fn bind_key(ty: ColType, key: &Json) -> Result<Value, String> {
    let bad = || format!("the user key {key} does not fit the key column");
    let text = match key {
        Json::String(s) => s.clone(),
        Json::Number(n) => n.to_string(),
        _ => return Err(bad()),
    };
    let normal = match ty {
        ColType::Int => Json::from(text.parse::<i64>().map_err(|_| bad())?),
        _ => Json::String(text),
    };
    match ty {
        ColType::Int | ColType::BigInt | ColType::String | ColType::Text | ColType::Uuid => json::bind(ty, &normal),
        _ => Err(bad()),
    }
}

/// The names that locate the grants of a user in the auth tables.
struct Plan {
    key_type: ColType,
    permission: Table,
    permission_key: String,
    label: String,
    codename: String,
    user_permission: Link,
    user_role: Link,
    role_permission: Link,
}

struct Link {
    table: String,
    source: String,
    target: String,
}

impl Plan {
    /// One statement: the permissions of the user, direct and through the roles.
    fn sql(&self, d: monorm_core::dialect::Dialect) -> String {
        format!(
            "SELECT {label}, {codename} FROM {permission} WHERE {key} IN (SELECT {upt} FROM {up} WHERE {ups} = {m1}) OR {key} IN (SELECT {rpt} FROM {rp} WHERE {rps} IN (SELECT {urt} FROM {ur} WHERE {urs} = {m2}))",
            label = self.label,
            codename = self.codename,
            permission = self.permission.quoted(),
            key = self.permission_key,
            upt = self.user_permission.target,
            up = self.user_permission.table,
            ups = self.user_permission.source,
            rpt = self.role_permission.target,
            rp = self.role_permission.table,
            rps = self.role_permission.source,
            urt = self.user_role.target,
            ur = self.user_role.table,
            urs = self.user_role.source,
            m1 = mark(d, 1),
            m2 = mark(d, 2),
        )
    }
}

/// The user model is the model with a to-many `roles` and a to-many `permissions` through join models.
fn plan(site: &Site) -> Result<Option<Plan>, String> {
    let schema = &site.schema;
    let has_both = |idx: usize| {
        let m = schema.model(idx);
        ["roles", "permissions"].iter().all(|n| matches!(m.relation(n), Ok((r, _)) if r.kind == RelKind::Many && r.through.is_some()))
    };
    let user = schema.model_idx("User").ok().filter(|&i| has_both(i)).or_else(|| (0..schema.models.len()).find(|&i| has_both(i)));
    let Some(user) = user else {
        return Ok(None);
    };
    let um = schema.model(user);
    let (roles, role_idx) = um.relation("roles")?;
    let (perms, perm_idx) = um.relation("permissions")?;
    let (role_perms, role_perm_idx) = schema.model(role_idx).relation("permissions")?;
    if role_perm_idx != perm_idx || role_perms.through.is_none() {
        return Ok(None);
    }
    let key_field = roles.from.first().ok_or("the user model has no key")?;
    let link = |rel: &monorm_core::ir::RelationIr| -> Result<Link, String> {
        let through = rel.through.as_ref().ok_or("a relation without a join model")?;
        let t = Table::of(site, &through.model)?;
        Ok(Link { table: t.quoted(), source: t.column(&through.source)?, target: t.column(&through.target)? })
    };
    let permission_model = schema.model(perm_idx);
    let permission = Table::of(site, &permission_model.ir.name)?;
    let permission_key = permission.column(perms.to.first().ok_or("the permission model has no key")?)?;
    Ok(Some(Plan {
        key_type: um.field(key_field)?.ty,
        label: permission.column("label")?,
        codename: permission.column("codename")?,
        permission_key,
        permission,
        user_permission: link(perms)?,
        user_role: link(roles)?,
        role_permission: link(role_perms)?,
    }))
}
