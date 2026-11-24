//! The error of the admin config and the admin settings: a code, a path and a text.
//!
//! The checks build a [`Fault`]. Its text marks each option key and each language hint.
//! A public entry point renders the fault with the spelling of the host (`Spelling`) into an [`AdminError`],
//! so a TypeScript user reads `pageSize` and a Python user reads `page_size` for the same rule.

use std::fmt;

/// How the host language spells an option key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Spelling {
    /// `bulk_delete`.
    #[default]
    Snake,
    /// `bulkDelete`.
    Camel,
}

/// The codes. The part before the first dot is the class: the host maps `config` to its config error and `settings` to its settings error.
pub mod code {
    /// The `config` class: the site config.
    pub const CONFIG: &str = "config";
    pub const INVALID_JSON: &str = "config.invalid_json";
    pub const UNKNOWN_OPTION: &str = "config.unknown_option";
    pub const INVALID_VALUE: &str = "config.invalid_value";
    pub const UNKNOWN_MODEL: &str = "config.unknown_model";
    pub const UNKNOWN_FIELD: &str = "config.unknown_field";
    pub const RELATION_UNKNOWN: &str = "config.relation_unknown";
    pub const DUPLICATE: &str = "config.duplicate";
    pub const UNSUPPORTED: &str = "config.unsupported";
    pub const PACKAGE: &str = "config.package";
    pub const ROUTE: &str = "config.route";
    pub const FRAME: &str = "config.frame";
    pub const PATH: &str = "config.path";
    /// The `settings` class: the `[admin]` settings and the secret.
    pub const SETTINGS: &str = "settings";
    pub const SETTINGS_FILE: &str = "settings.file";
    pub const SETTINGS_UNKNOWN_KEY: &str = "settings.unknown_key";
    pub const SETTINGS_SECRET: &str = "settings.secret";
    pub const SETTINGS_MISSING: &str = "settings.missing";
    pub const SETTINGS_INVALID: &str = "settings.invalid_value";
}

/// One step of the place of an error: a model, an option key, a user-named entry (a field, a relation, a tab) or a list position.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathPart {
    Model(String),
    /// An option key in snake case, such as `page_size`. The text renders it in the spelling of the host.
    Option(String),
    Name(String),
    Index(usize),
}

impl PathPart {
    fn render(&self, spelling: Spelling) -> String {
        match self {
            PathPart::Model(s) | PathPart::Name(s) => s.clone(),
            PathPart::Option(s) => spell(s, spelling),
            PathPart::Index(i) => i.to_string(),
        }
    }
}

const OPEN: char = '\u{1}';
const CLOSE: char = '\u{2}';
const HINT: char = '\u{3}';

/// Marks an option key (`page_size`, or a dotted `list.page_size`) in a text, so that the rendering can spell it for the host.
pub fn opt(key: &str) -> String {
    format!("{OPEN}{key}{CLOSE}")
}

/// A hint is a piece of text that names an API of the host language.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hint {
    /// How to make a prepared query set.
    PreparedQuerySet,
}

const HINTS: [(Hint, &str, &str); 1] = [(Hint::PreparedQuerySet, "Model.objects.filter(...).prepare()", "Model.objects.filter(...).prepare()")];

impl Hint {
    fn text(self, spelling: Spelling) -> &'static str {
        let (_, snake, camel) = HINTS.iter().find(|(h, _, _)| *h == self).expect("every hint has an entry");
        match spelling {
            Spelling::Snake => snake,
            Spelling::Camel => camel,
        }
    }
}

/// Marks a hint in a text.
pub fn hint(h: Hint) -> String {
    let at = HINTS.iter().position(|(k, _, _)| *k == h).expect("every hint has an entry");
    format!("{HINT}{at}{HINT}")
}

/// `bulk_delete` as `bulkDelete`, each dotted part on its own.
fn spell(key: &str, spelling: Spelling) -> String {
    if spelling == Spelling::Snake {
        return key.to_owned();
    }
    let mut out = String::with_capacity(key.len());
    let mut up = false;
    for c in key.chars() {
        match c {
            '_' if !out.is_empty() => up = true,
            '.' => {
                out.push('.');
                up = false;
            }
            c if up => {
                out.extend(c.to_uppercase());
                up = false;
            }
            c => out.push(c),
        }
    }
    out
}

fn render_text(text: &str, spelling: Spelling) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        match c {
            OPEN => {
                let key: String = chars.by_ref().take_while(|c| *c != CLOSE).collect();
                out.push_str(&spell(&key, spelling));
            }
            HINT => {
                let at: String = chars.by_ref().take_while(|c| *c != HINT).collect();
                out.push_str(HINTS[at.parse::<usize>().expect("a hint index")].0.text(spelling));
            }
            c => out.push(c),
        }
    }
    out
}

/// An error that no host has seen yet: its text still has the marks of the option keys and hints.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fault {
    pub code: &'static str,
    pub path: Vec<PathPart>,
    pub text: String,
}

impl Fault {
    pub fn new(code: &'static str, text: impl Into<String>) -> Fault {
        Fault { code, path: Vec::new(), text: text.into() }
    }

    /// The same fault with `part` in front of its path.
    pub fn under(mut self, part: PathPart) -> Fault {
        self.path.insert(0, part);
        self
    }

    /// Renders the text in the spelling of the host.
    pub fn render(self, spelling: Spelling) -> AdminError {
        let body = render_text(&self.text, spelling);
        let message = if self.path.is_empty() {
            body
        } else {
            format!("{}: {body}", self.path.iter().map(|p| p.render(spelling)).collect::<Vec<_>>().join(": "))
        };
        AdminError { code: self.code, path: self.path, message }
    }
}

impl fmt::Display for Fault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.clone().render(Spelling::Snake).message)
    }
}

impl std::error::Error for Fault {}

/// A wrong config or wrong settings: the text names the model and the option, in the spelling of the host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdminError {
    /// `config.*` for the site config, `settings.*` for the `[admin]` settings.
    pub code: &'static str,
    /// Where the error is. The `message` holds the same path in front of the text.
    pub path: Vec<PathPart>,
    pub message: String,
}

impl AdminError {
    /// The class of the code: `config` or `settings`.
    pub fn class(&self) -> &'static str {
        self.code.split('.').next().unwrap_or(self.code)
    }
}

impl fmt::Display for AdminError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for AdminError {}

/// A settings error: the text is complete.
pub(crate) fn settings(code: &'static str, message: impl Into<String>) -> AdminError {
    AdminError { code, path: Vec::new(), message: message.into() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spells_dotted_keys_and_marks() {
        let fault = Fault::new(code::INVALID_VALUE, format!("{} is 0, between 1 and 500", opt("list.page_size"))).under(PathPart::Model("Post".into()));
        assert_eq!(fault.clone().render(Spelling::Snake).message, "Post: list.page_size is 0, between 1 and 500");
        assert_eq!(fault.render(Spelling::Camel).message, "Post: list.pageSize is 0, between 1 and 500");
    }

    #[test]
    fn hint_has_one_text_per_spelling() {
        let fault = Fault::new(code::INVALID_VALUE, format!("takes a prepared query set ({})", hint(Hint::PreparedQuerySet)));
        for spelling in [Spelling::Snake, Spelling::Camel] {
            assert_eq!(fault.clone().render(spelling).message, "takes a prepared query set (Model.objects.filter(...).prepare())");
        }
    }
}
