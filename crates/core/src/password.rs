//! Password rules: the built-in rules of the auth app, and the `[auth] password_rules` setting.
//!
//! A rule entry is a JSON object: `{"rule": "min_length", "min": 8}`. A name with `:` (`module:attr`) is a custom
//! rule that only the host language can run. The built-in rules run here, so Python and TypeScript agree.
//! Length and similarity count Unicode code points after NFC normalization (NIST SP 800-63B rev 4, 3.1.1.2).

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::OnceLock;

use serde_json::{json, Map, Value};
use unicode_normalization::UnicodeNormalization;
use unicode_properties::{GeneralCategory, UnicodeGeneralCategory};

use crate::settings::{Settings, Source};

const DEFAULT_MIN: u64 = 8;
const DEFAULT_SIMILARITY: f64 = 0.7;
const BUILT_IN: [&str; 4] = ["min_length", "common", "numeric", "user_similarity"];

/// The bundled list of common passwords: lower case, one for each line (see `docs/apps/auth.md` for the source).
static COMMON: &str = include_str!("../data/common-passwords.txt");

/// The default list: used when the project has no `password_rules`.
pub fn default_rules() -> Vec<Value> {
    vec![
        json!({"rule": "min_length", "min": DEFAULT_MIN}),
        json!({"rule": "common"}),
        json!({"rule": "numeric"}),
        json!({"rule": "user_similarity", "attributes": ["username", "email"], "max_similarity": DEFAULT_SIMILARITY}),
    ]
}

/// A built-in rule with its options.
enum Rule {
    MinLength(u64),
    Common(Option<String>),
    Numeric,
    UserSimilarity { attributes: Vec<String>, max: f64 },
}

/// `(key, what)` of a wrong entry: the option key and what is wrong with it. The key is empty for the entry itself.
type Fault = (String, String);

fn fault(key: &str, what: impl Into<String>) -> Fault {
    (key.to_string(), what.into())
}

/// The name of an option as the file spells it: `package.json` uses camelCase.
fn spelled(snake: &str, camel_case: bool) -> String {
    if !camel_case {
        return snake.to_string();
    }
    let mut out = String::new();
    let mut up = false;
    for c in snake.chars() {
        if c == '_' {
            up = true;
        } else if up {
            out.extend(c.to_uppercase());
            up = false;
        } else {
            out.push(c);
        }
    }
    out
}

/// Parses one built-in entry. `camel` says the option keys are spelled as in `package.json`.
/// The normal form (snake_case, every default filled in) is the `Value` next to the rule.
fn parse_built_in(name: &str, entry: &Map<String, Value>, camel: bool) -> Result<(Rule, Value), Fault> {
    let allowed: &[&str] = match name {
        "min_length" => &["min"],
        "common" => &["path"],
        "numeric" => &[],
        _ => &["attributes", "max_similarity"],
    };
    for key in entry.keys().filter(|k| k.as_str() != "rule") {
        if allowed.iter().any(|a| spelled(a, camel) == *key) {
            continue;
        }
        let wrong_spelling = allowed.iter().find(|a| spelled(a, !camel) == *key);
        return Err(match wrong_spelling {
            Some(a) => fault(key, format!("is not an option of `{name}`; use `{}`", spelled(a, camel))),
            None if allowed.is_empty() => fault(key, format!("is not an option: `{name}` has none")),
            None => fault(key, format!("is not an option of `{name}`; the options are {}", allowed.iter().map(|a| format!("`{}`", spelled(a, camel))).collect::<Vec<_>>().join(", "))),
        });
    }
    let get = |snake: &str| entry.get(&spelled(snake, camel));
    match name {
        "min_length" => {
            let min = match get("min") {
                None => DEFAULT_MIN,
                Some(v) => match v.as_u64() {
                    Some(n) if n >= 1 => n,
                    _ => return Err(fault(&spelled("min", camel), "must be a whole number of at least 1")),
                },
            };
            Ok((Rule::MinLength(min), json!({"rule": name, "min": min})))
        }
        "common" => match get("path") {
            None => Ok((Rule::Common(None), json!({"rule": name}))),
            Some(Value::String(p)) if !p.is_empty() => Ok((Rule::Common(Some(p.clone())), json!({"rule": name, "path": p}))),
            Some(_) => Err(fault(&spelled("path", camel), "must be a file name")),
        },
        "numeric" => Ok((Rule::Numeric, json!({"rule": name}))),
        _ => {
            let attributes = match get("attributes") {
                None => vec!["username".to_string(), "email".to_string()],
                Some(Value::Array(items)) => {
                    let names: Option<Vec<String>> = items.iter().map(|v| v.as_str().filter(|s| !s.is_empty()).map(str::to_string)).collect();
                    names.ok_or_else(|| fault(&spelled("attributes", camel), "must be a list of attribute names"))?
                }
                Some(_) => return Err(fault(&spelled("attributes", camel), "must be a list of attribute names")),
            };
            let max = match get("max_similarity") {
                None => DEFAULT_SIMILARITY,
                Some(v) => match v.as_f64() {
                    Some(x) if x > 0.0 && x <= 1.0 => x,
                    _ => return Err(fault(&spelled("max_similarity", camel), "must be a number above 0 and up to 1")),
                },
            };
            Ok((Rule::UserSimilarity { attributes: attributes.clone(), max }, json!({"rule": name, "attributes": attributes, "max_similarity": max})))
        }
    }
}

/// The name of an entry: its `rule` string.
fn rule_name(entry: &Value) -> Result<(&str, &Map<String, Value>), Fault> {
    let Value::Object(map) = entry else { return Err(fault("", "must be a table with a `rule` name")) };
    match map.get("rule") {
        Some(Value::String(name)) if !name.is_empty() => Ok((name, map)),
        Some(_) => Err(fault("rule", "must be a rule name")),
        None => Err(fault("", "needs a `rule` name")),
    }
}

/// Checks a custom rule name: `module:attr`, both parts not empty.
fn custom_name_ok(name: &str) -> bool {
    matches!(name.rsplit_once(':'), Some((module, attr)) if !module.is_empty() && !attr.is_empty())
}

/// The normal form of one entry: a built-in rule with its defaults, or a custom rule as it is.
fn normalize(entry: &Value, camel: bool) -> Result<Value, Fault> {
    let (name, map) = rule_name(entry)?;
    if name.contains(':') {
        return if custom_name_ok(name) { Ok(entry.clone()) } else { Err(fault("rule", "must be `module:attr`")) };
    }
    if !BUILT_IN.contains(&name) {
        return Err(fault("rule", format!("`{name}` is not a rule; the built-in rules are {}, and a custom rule is `module:attr`", BUILT_IN.join(", "))));
    }
    parse_built_in(name, map, camel).map(|(_, normal)| normal)
}

/// The rule list of the project in `dir`: the `[auth] password_rules` setting, or the default list when it is not set.
/// Every entry is in the normal form: a built-in rule has all its options in snake_case, a custom rule is as written.
/// A name that is not a rule, an unknown option and a bad value are errors that name the file and the key.
/// `path` of `common` is made absolute against `dir`.
pub fn rules_from_settings(settings: &Settings, dir: &Path) -> Result<Vec<Value>, String> {
    let camel = settings.source() == Some(Source::PackageJson);
    let (right, other) = if camel { ("passwordRules", "password_rules") } else { ("password_rules", "passwordRules") };
    let Some(section) = settings.section("auth") else { return Ok(default_rules()) };
    let Value::Object(table) = section else { return Err(settings.wrong("auth", "must be a table")) };
    if let Some(key) = table.keys().find(|k| k.as_str() != right) {
        let what = if key == other { format!("is not a setting; use `{right}`") } else { format!("is not a setting; the key is `{right}`") };
        return Err(settings.wrong(&format!("auth.{key}"), &what));
    }
    let Some(list) = table.get(right) else { return Ok(default_rules()) };
    let Value::Array(items) = list else { return Err(settings.wrong(&format!("auth.{right}"), "must be a list of rules")) };
    let mut out = Vec::with_capacity(items.len());
    for (i, item) in items.iter().enumerate() {
        let at = |key: &str| if key.is_empty() { format!("auth.{right}[{i}]") } else { format!("auth.{right}[{i}].{key}") };
        let mut normal = normalize(item, camel).map_err(|(key, what)| settings.wrong(&at(&key), &what))?;
        if normal["rule"] == "common" {
            if let Some(p) = normal.get("path").and_then(Value::as_str) {
                let full = dir.join(p);
                if !full.is_file() {
                    return Err(settings.wrong(&at(&spelled("path", camel)), &format!("is not a file: {}", full.display())));
                }
                normal["path"] = Value::String(full.to_string_lossy().into_owned());
            }
        }
        out.push(normal);
    }
    Ok(out)
}

fn bundled() -> &'static HashSet<&'static str> {
    static SET: OnceLock<HashSet<&'static str>> = OnceLock::new();
    SET.get_or_init(|| COMMON.lines().collect())
}

/// The entries of the normal form, as the bindings send them: snake_case, built-in rules only.
fn rules_of(entries: &[Value]) -> Result<Vec<Rule>, String> {
    entries
        .iter()
        .map(|entry| {
            let (name, map) = rule_name(entry).map_err(|(key, what)| format!("password rule: `{key}` {what}"))?;
            if !BUILT_IN.contains(&name) {
                return Err(format!("`{name}` is not a built-in password rule"));
            }
            parse_built_in(name, map, false).map(|(rule, _)| rule).map_err(|(key, what)| format!("password rule `{name}`: `{key}` {what}"))
        })
        .collect()
}

fn error(code: &str, message: String, params: Value) -> Value {
    json!({"code": code, "message": message, "params": params})
}

fn plural(n: u64) -> String {
    if n == 1 { "1 character".to_string() } else { format!("{n} characters") }
}

/// Runs the built-in `entries` on `password`, in list order, and gives every error as `{code, message, params}`.
/// `attrs` maps an attribute name to its value (a string; anything else is ignored) for `user_similarity`.
/// An entry that is not a built-in rule, or a `common` file that cannot be read, is an error.
pub fn check(entries: &[Value], password: &str, attrs: &Map<String, Value>) -> Result<Vec<Value>, String> {
    let rules = rules_of(entries)?;
    let pw: String = password.nfc().collect();
    let mut errors = Vec::new();
    for rule in &rules {
        match rule {
            Rule::MinLength(min) => {
                if (pw.chars().count() as u64) < *min {
                    errors.push(error("password_too_short", format!("This password is too short. It must contain at least {}.", plural(*min)), json!({"min": min})));
                }
            }
            Rule::Common(path) => {
                let key = pw.trim().to_lowercase();
                let common = match path {
                    None => bundled().contains(key.as_str()),
                    Some(p) => std::fs::read_to_string(p).map_err(|e| format!("password rule `common`: {p}: {e}"))?.lines().any(|l| l.trim().to_lowercase() == key),
                };
                if common {
                    errors.push(error("password_too_common", "This password is too common.".to_string(), json!({})));
                }
            }
            Rule::Numeric => {
                if !pw.is_empty() && pw.chars().all(|c| c.general_category() == GeneralCategory::DecimalNumber) {
                    errors.push(error("password_entirely_numeric", "This password is entirely numeric.".to_string(), json!({})));
                }
            }
            Rule::UserSimilarity { attributes, max } => {
                let lower = pw.to_lowercase();
                for name in attributes {
                    let Some(Value::String(value)) = attrs.get(name) else { continue };
                    if similar(&lower, &value.nfc().collect::<String>().to_lowercase(), *max) {
                        errors.push(error("password_too_similar", format!("The password is too similar to the {name}."), json!({"attribute": name})));
                    }
                }
            }
        }
    }
    Ok(errors)
}

/// The help text of each built-in entry, in list order.
pub fn help_texts(entries: &[Value]) -> Result<Vec<String>, String> {
    Ok(rules_of(entries)?
        .iter()
        .map(|rule| match rule {
            Rule::MinLength(min) => format!("Your password must contain at least {}.", plural(*min)),
            Rule::Common(_) => "Your password can't be a commonly used password.".to_string(),
            Rule::Numeric => "Your password can't be entirely numeric.".to_string(),
            Rule::UserSimilarity { .. } => "Your password can't be too similar to your other personal information.".to_string(),
        })
        .collect())
}

/// The JSON form of the functions above, for the bindings: every function takes and gives text.
pub mod json {
    use super::*;

    /// `rules_from_settings` for the project in `dir`, as a JSON list.
    pub fn settings(dir: &Path) -> Result<String, String> {
        let rules = rules_from_settings(&Settings::load(dir)?, dir)?;
        serde_json::to_string(&rules).map_err(|e| e.to_string())
    }

    fn entries(rules_json: &str) -> Result<Vec<Value>, String> {
        serde_json::from_str(rules_json).map_err(|e| format!("password rules: {e}"))
    }

    /// `check` with the entries, and the user attributes (a JSON object), as text; gives the errors as a JSON list.
    pub fn check(rules_json: &str, password: &str, attrs_json: &str) -> Result<String, String> {
        let attrs: Value = serde_json::from_str(attrs_json).map_err(|e| format!("password attributes: {e}"))?;
        let Value::Object(attrs) = attrs else { return Err("password attributes: must be an object".into()) };
        let errors = super::check(&entries(rules_json)?, password, &attrs)?;
        serde_json::to_string(&errors).map_err(|e| e.to_string())
    }

    /// `help_texts` as a JSON list of strings.
    pub fn help(rules_json: &str) -> Result<String, String> {
        serde_json::to_string(&help_texts(&entries(rules_json)?)?).map_err(|e| e.to_string())
    }
}

/// The check of Django's `UserAttributeSimilarityValidator`: the character overlap of the password with the whole
/// value and with each of its words (`SequenceMatcher.quick_ratio`), at least `max`. Both strings are lower case.
fn similar(password: &str, value: &str, max: f64) -> bool {
    if password.is_empty() {
        return false;
    }
    let words = value.split(|c: char| !(c.is_alphanumeric() || c == '_')).chain(std::iter::once(value));
    let pw_len = password.chars().count();
    for part in words.filter(|w| !w.is_empty()) {
        let len = part.chars().count();
        // a long password cannot be close to a short value
        if pw_len >= 10 * len && (len as f64) < max / 2.0 * pw_len as f64 {
            continue;
        }
        let mut available: HashMap<char, usize> = HashMap::new();
        for c in part.chars() {
            *available.entry(c).or_default() += 1;
        }
        let mut matches = 0usize;
        for c in password.chars() {
            if let Some(n) = available.get_mut(&c).filter(|n| **n > 0) {
                *n -= 1;
                matches += 1;
            }
        }
        if 2.0 * matches as f64 / (pw_len + len) as f64 >= max {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn attrs(pairs: &[(&str, &str)]) -> Map<String, Value> {
        pairs.iter().map(|(k, v)| (k.to_string(), json!(v))).collect()
    }

    fn codes(rules: &[Value], password: &str, user: &Map<String, Value>) -> Vec<String> {
        check(rules, password, user).unwrap().iter().map(|e| e["code"].as_str().unwrap().to_string()).collect()
    }

    fn settings(file: &str, text: &str) -> (Settings, std::path::PathBuf) {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!("monorm-password-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(file), text).unwrap();
        (Settings::load(&dir).unwrap(), dir)
    }

    #[test]
    fn numeric_means_decimal_digits_of_any_script_and_not_other_numerals() {
        let rules = default_rules();
        let user = Map::new();
        assert!(codes(&rules, "٣٤٥٦٧٨٩٠١", &user).contains(&"password_entirely_numeric".to_string()));
        assert!(!codes(&rules, "½½½½½½½½", &user).contains(&"password_entirely_numeric".to_string()));
    }

    #[test]
    fn the_default_rules_refuse_a_short_a_common_a_numeric_and_a_similar_password() {
        let rules = default_rules();
        let user = attrs(&[("username", "annabelle"), ("email", "ann@example.com")]);
        assert_eq!(codes(&rules, "x7!Qr", &user), ["password_too_short"]);
        assert_eq!(codes(&rules, "password", &user), ["password_too_common"]);
        assert_eq!(codes(&rules, "PassWord ", &user), ["password_too_common"]);
        assert_eq!(codes(&rules, "20250101", &user), ["password_entirely_numeric"]);
        assert_eq!(codes(&rules, "annabelle1", &user), ["password_too_similar"]);
        assert!(codes(&rules, "correct horse battery", &user).is_empty());
        assert_eq!(codes(&rules, "1234", &user), ["password_too_short", "password_too_common", "password_entirely_numeric"]);
    }

    #[test]
    fn length_counts_code_points_after_nfc() {
        let rules = [json!({"rule": "min_length", "min": 4})];
        let none = Map::new();
        // NIST counts code points, not bytes or UTF-16 units.
        assert_eq!(codes(&rules, "e\u{301}e\u{301}e\u{301}", &none), ["password_too_short"]);
        assert!(codes(&rules, "e\u{301}e\u{301}e\u{301}e\u{301}", &none).is_empty());
        assert_eq!(codes(&rules, "😀😀😀", &none), ["password_too_short"]);
        assert!(codes(&rules, "😀😀😀😀", &none).is_empty());
    }

    #[test]
    fn the_error_has_a_code_a_message_and_params() {
        let errors = check(&[json!({"rule": "min_length", "min": 12})], "short", &Map::new()).unwrap();
        assert_eq!(errors, [json!({"code": "password_too_short", "message": "This password is too short. It must contain at least 12 characters.", "params": {"min": 12}})]);
    }

    #[test]
    fn similarity_ignores_a_missing_or_not_text_attribute_and_matches_a_word_of_the_value() {
        let rules = [json!({"rule": "user_similarity", "attributes": ["username", "first_name", "email"], "max_similarity": 0.7})];
        let mut user = attrs(&[("username", "x"), ("first_name", "Mohammed Ali")]);
        user.insert("email".into(), Value::Null);
        assert_eq!(codes(&rules, "mohammed1", &user), ["password_too_similar"]);
        assert!(codes(&rules, "qwerty-zebra-42", &user).is_empty());
        assert!(codes(&rules, "", &user).is_empty());
    }

    #[test]
    fn a_common_file_replaces_the_bundled_list() {
        let dir = std::env::temp_dir().join(format!("monorm-common-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("list.txt");
        std::fs::write(&file, "Hunter2hunter2\nother\n").unwrap();
        let rules = [json!({"rule": "common", "path": file.to_string_lossy()})];
        assert_eq!(codes(&rules, "hunter2HUNTER2", &Map::new()), ["password_too_common"]);
        assert!(codes(&rules, "password", &Map::new()).is_empty());
    }

    #[test]
    fn a_custom_rule_name_is_not_run_here() {
        assert!(check(&[json!({"rule": "a.b:C"})], "x", &Map::new()).unwrap_err().contains("not a built-in"));
    }

    #[test]
    fn help_texts_follow_the_list() {
        let texts = help_texts(&default_rules()).unwrap();
        assert_eq!(texts.len(), 4);
        assert_eq!(texts[0], "Your password must contain at least 8 characters.");
    }

    #[test]
    fn no_setting_gives_the_default_list_and_an_empty_list_turns_the_rules_off() {
        let (none, dir) = settings("monorm.toml", "schema = \"s.prisma\"\n");
        assert_eq!(rules_from_settings(&none, &dir).unwrap(), default_rules());
        let (off, dir) = settings("monorm.toml", "[auth]\npassword_rules = []\n");
        assert!(rules_from_settings(&off, &dir).unwrap().is_empty());
    }

    #[test]
    fn a_list_fills_the_defaults_and_keeps_a_custom_rule_as_written() {
        let (s, dir) = settings("monorm.toml", "[auth]\npassword_rules = [{rule = \"min_length\", min = 12}, {rule = \"user_similarity\"}, {rule = \"my.mod:NoCompany\", word = \"acme\"}]\n");
        let rules = rules_from_settings(&s, &dir).unwrap();
        assert_eq!(rules[0], json!({"rule": "min_length", "min": 12}));
        assert_eq!(rules[1], json!({"rule": "user_similarity", "attributes": ["username", "email"], "max_similarity": 0.7}));
        assert_eq!(rules[2], json!({"rule": "my.mod:NoCompany", "word": "acme"}));
    }

    #[test]
    fn package_json_spells_options_in_camel_case() {
        let (s, dir) = settings("package.json", r#"{"monorm": {"auth": {"passwordRules": [{"rule": "user_similarity", "maxSimilarity": 0.5}]}}}"#);
        assert_eq!(rules_from_settings(&s, &dir).unwrap()[0]["max_similarity"], json!(0.5));
        let (s, dir) = settings("package.json", r#"{"monorm": {"auth": {"passwordRules": [{"rule": "user_similarity", "max_similarity": 0.5}]}}}"#);
        let e = rules_from_settings(&s, &dir).unwrap_err();
        assert_eq!(e, "package.json: `monorm.auth.passwordRules[0].max_similarity` is not an option of `user_similarity`; use `maxSimilarity`");
        let (s, dir) = settings("package.json", r#"{"monorm": {"auth": {"password_rules": []}}}"#);
        assert_eq!(rules_from_settings(&s, &dir).unwrap_err(), "package.json: `monorm.auth.password_rules` is not a setting; use `passwordRules`");
    }

    #[test]
    fn a_wrong_entry_is_an_error_that_names_the_file_and_the_key() {
        let cases = [
            ("[auth]\npassword_rules = [{rule = \"lenght\"}]\n", "monorm.toml: `auth.password_rules[0].rule` `lenght` is not a rule"),
            ("[auth]\npassword_rules = [{rule = \"min_length\", mn = 3}]\n", "monorm.toml: `auth.password_rules[0].mn` is not an option of `min_length`; the options are `min`"),
            ("[auth]\npassword_rules = [{rule = \"min_length\", min = 0}]\n", "monorm.toml: `auth.password_rules[0].min` must be a whole number of at least 1"),
            ("[auth]\npassword_rules = [{rule = \"numeric\", x = 1}]\n", "monorm.toml: `auth.password_rules[0].x` is not an option: `numeric` has none"),
            ("[auth]\npassword_rules = [{rule = \"user_similarity\", max_similarity = 2}]\n", "monorm.toml: `auth.password_rules[0].max_similarity` must be a number above 0 and up to 1"),
            ("[auth]\npassword_rules = [{min = 3}]\n", "monorm.toml: `auth.password_rules[0]` needs a `rule` name"),
            ("[auth]\npassword_rules = [{rule = \"mod:\"}]\n", "monorm.toml: `auth.password_rules[0].rule` must be `module:attr`"),
            ("[auth]\npassword_rules = \"min_length\"\n", "monorm.toml: `auth.password_rules` must be a list of rules"),
            ("[auth]\nmin = 3\n", "monorm.toml: `auth.min` is not a setting; the key is `password_rules`"),
            ("[auth]\npassword_rules = [{rule = \"common\", path = \"missing.txt\"}]\n", "monorm.toml: `auth.password_rules[0].path` is not a file"),
        ];
        for (text, expected) in cases {
            let (s, dir) = settings("monorm.toml", text);
            let e = rules_from_settings(&s, &dir).unwrap_err();
            assert!(e.starts_with(expected), "{e} should start with {expected}");
        }
    }
}
