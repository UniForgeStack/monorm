//! The one place that turns a model name written in a schema into the model identity.
//!
//! A manifest argument of kind `model` or `model_list` goes through `resolve` before any pass runs, so a pass never resolves a name.
//! The identity is the model name while the project has one app; the app label joins it when apps get labels.

use std::collections::HashSet;

/// The identity of the model `name` that the app `app` owns.
pub fn identity(_app: &str, name: &str) -> String {
    name.to_owned()
}

/// Resolve `written` (a model name as an author wrote it in app `writer_app`) against the declared `models`.
pub fn resolve(models: &HashSet<String>, writer_app: &str, written: &str) -> Result<String, String> {
    if models.contains(written) { Ok(identity(writer_app, written)) } else { Err(format!("unknown model {written}")) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_declared_name_resolves_and_an_unknown_one_is_an_error() {
        let models: HashSet<String> = ["User".to_owned()].into();
        assert_eq!(resolve(&models, "", "User").unwrap(), "User");
        assert_eq!(resolve(&models, "", "Nope").unwrap_err(), "unknown model Nope");
    }
}
