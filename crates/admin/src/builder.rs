//! The stateful config builder of a host: it holds what `Admin()` and its calls say, refuses a wrong call at once, and builds the site.
//!
//! A binding keeps the host values (functions, models). It sends the registered model names and option keys here, so the
//! eager checks (an unknown option, a duplicate, a route without a function) and their texts live in one place for both hosts.
//! The resource pages still cross as JSON at `build`, because their function ids change with each lowering.

use std::collections::BTreeSet;
use std::sync::Arc;

use monorm_core::schema::Schema;
use serde::Deserialize;
use serde_json::{Map, Value};

use crate::error::{code, spell, AdminError, Fault, PathPart, Spelling};
use crate::site::{parse_fault, Site};
use crate::site_config::{BrandConfig, Handles, SwitcherConfig, ThemeConfig};

/// The keys of a page that the user sets, in snake case. A host spells them its own way.
const RESOURCE_KEYS: &[&str] = &[
    "list", "title", "group", "icon", "allow", "perms", "label", "field_permissions", "see", "change", "actions", "queryset", "can", "check_messages", "form",
    "readonly", "widgets", "form_component", "detail_tabs", "frames", "fieldsets", "relations", "files", "create", "update", "delete", "inputs",
    "allow_protected_writes", "inlines", "bulk_delete", "set_password",
];

const INLINE_SAVE: &[&str] = &["with_parent", "row"];
const INLINE_STYLE: &[&str] = &["table", "stacked"];

/// The options whose keys the builder checks at once. A wrong type waits for `build`.
#[derive(Deserialize, Default)]
#[allow(dead_code)] // the parse is the check; nothing reads the values
struct EagerOptions {
    brand: Option<BrandConfig>,
    theme: Option<ThemeConfig>,
    switcher: Option<SwitcherConfig>,
}

pub struct AdminConfigBuilder {
    spelling: Spelling,
    options: Map<String, Value>,
    statement_timeout: f64,
    models: BTreeSet<String>,
    packages: Vec<Value>,
    package_names: BTreeSet<String>,
    routes: Vec<Value>,
    route_keys: BTreeSet<(String, String)>,
}

fn invalid(text: impl Into<String>) -> Fault {
    Fault::new(code::INVALID_VALUE, text)
}

impl AdminConfigBuilder {
    /// `options` is a JSON object of the site options in wire form (`brand`, `theme`, `switcher`, `widgets`, `groups` and so on).
    /// `roles` is a JSON object of role name to a list of patterns. `statement_timeout` is in seconds.
    pub fn new(options: &str, statement_timeout: f64, roles: &str, spelling: Spelling) -> Result<AdminConfigBuilder, AdminError> {
        Self::checked(options, statement_timeout, roles, spelling).map_err(|f| f.render(spelling))
    }

    fn checked(options: &str, statement_timeout: f64, roles: &str, spelling: Spelling) -> Result<AdminConfigBuilder, Fault> {
        if !statement_timeout.is_finite() || statement_timeout < 0.0 {
            return Err(Fault::new(code::INVALID_VALUE, format!("{} is a number of seconds, 0 or more (0 sets no limit)", crate::error::opt("statement_timeout"))));
        }
        let options: Map<String, Value> = match serde_json::from_str(options) {
            Ok(Value::Object(o)) => o,
            _ => return Err(invalid("the admin options are a JSON object")),
        };
        let probe = Value::Object(options.clone());
        if let Err(e) = serde_path_to_error::deserialize::<_, EagerOptions>(&probe) {
            let fault = parse_fault(&probe, e);
            if fault.code == code::UNKNOWN_OPTION {
                return Err(fault);
            }
        }
        let roles: Value = serde_json::from_str(roles).map_err(|_| invalid("roles is a JSON object"))?;
        if let Value::Object(roles) = &roles {
            for (name, patterns) in roles {
                if !patterns.as_array().is_some_and(|l| l.iter().all(Value::is_string)) {
                    return Err(invalid(format!("roles['{name}'] must be a list of patterns")));
                }
            }
        }
        Ok(AdminConfigBuilder {
            spelling,
            options,
            statement_timeout,
            models: BTreeSet::new(),
            packages: Vec::new(),
            package_names: BTreeSet::new(),
            routes: Vec::new(),
            route_keys: BTreeSet::new(),
        })
    }

    /// Registers a model with the option keys that the user wrote. A duplicate and an unknown key are refused.
    pub fn register(&mut self, model: &str, keys: &[String]) -> Result<(), AdminError> {
        let spelling = self.spelling;
        let check = || -> Result<(), Fault> {
            if self.models.contains(model) {
                return Err(Fault::new(code::DUPLICATE, format!("{model} is already registered")));
            }
            let known = |k: &str| RESOURCE_KEYS.iter().any(|r| spell(r, spelling) == k);
            if let Some(key) = keys.iter().find(|k| !known(k)) {
                return Err(Fault::new(code::UNKNOWN_OPTION, format!("unknown option '{key}'")).under(PathPart::Model(model.to_owned())));
            }
            Ok(())
        };
        check().map_err(|f| f.render(spelling))?;
        self.models.insert(model.to_owned());
        Ok(())
    }

    /// Adds an extension package. `json` is its wire form.
    pub fn package(&mut self, name: &str, json: &str) -> Result<(), AdminError> {
        if !self.package_names.insert(name.to_owned()) {
            return Err(Fault::new(code::PACKAGE, format!("the package '{name}' is added twice")).render(self.spelling));
        }
        let value = serde_json::from_str(json).map_err(|e| Fault::new(code::INVALID_JSON, format!("invalid package: {e}")).render(self.spelling))?;
        self.packages.push(value);
        Ok(())
    }

    /// Adds a custom route. `callable` tells whether the host has a function for it.
    pub fn route(&mut self, method: &str, path: &str, permission: Option<&str>, callable: bool) -> Result<(), AdminError> {
        if !callable {
            return Err(Fault::new(code::ROUTE, "a route needs a function: route(method, path, fn)").render(self.spelling));
        }
        let key = (method.to_uppercase(), path.strip_prefix('/').unwrap_or(path).to_owned());
        if self.route_keys.contains(&key) {
            return Err(Fault::new(code::DUPLICATE, format!("the route {} {} is registered twice", key.0, key.1)).render(self.spelling));
        }
        let mut route = Map::new();
        route.insert("method".into(), key.0.clone().into());
        route.insert("path".into(), key.1.clone().into());
        if let Some(p) = permission {
            route.insert("permission".into(), p.into());
        }
        self.routes.push(Value::Object(route));
        self.route_keys.insert(key);
        Ok(())
    }

    /// Checks the `save` and `style` of an inline. It needs no builder: `inline()` is a free function of the host.
    pub fn check_inline(model: &str, save: &str, style: &str, spelling: Spelling) -> Result<(), AdminError> {
        for (what, value, allowed) in [("save", save, INLINE_SAVE), ("style", style, INLINE_STYLE)] {
            if !allowed.contains(&value) {
                return Err(invalid(format!("inline {model}: {what} is one of {}", allowed.join(", "))).render(spelling));
            }
        }
        Ok(())
    }

    /// Plans the site from the stored state and `late`, the JSON of the parts that the host lowers at each build (the resources and the paths).
    /// It keeps its state: the host may build again, for a check or for another registry.
    pub fn build(&self, schema: Arc<Schema>, late: &str) -> Result<(Site, Handles), AdminError> {
        let mut config = self.options.clone();
        match serde_json::from_str::<Value>(late) {
            Ok(Value::Object(late)) => config.extend(late),
            _ => return Err(Fault::new(code::INVALID_JSON, "invalid admin config: the late part is a JSON object").render(self.spelling)),
        }
        config.insert("statement_timeout".into(), self.statement_timeout.into());
        if !self.packages.is_empty() {
            config.insert("packages".into(), Value::Array(self.packages.clone()));
        }
        if !self.routes.is_empty() {
            config.insert("routes".into(), Value::Array(self.routes.clone()));
        }
        config.insert("spelling".into(), match self.spelling {
            Spelling::Snake => "snake",
            Spelling::Camel => "camel",
        }.into());
        Site::build_with_handles(schema, &Value::Object(config).to_string())
    }
}
