//! The error that app code raises for a value that breaks a rule of its own: messages per field
//! path, and non-field messages that belong to no field. Python, TypeScript and Rust read the same shape.
use std::fmt;

use serde::{Deserialize, Serialize};

/// Messages per field path (`address.city`, `lines.2.qty`), and non-field messages: messages with no field.
/// A path joins field names and list indexes with dots, and a dot in a field name is not escaped.
/// Fields keep the order of first use.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValidationError {
    #[serde(with = "ordered_map")]
    fields: Vec<(String, Vec<String>)>,
    non_field_messages: Vec<String>,
}

impl ValidationError {
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds `message` to the field at `path`.
    pub fn add(&mut self, path: impl Into<String>, message: impl Into<String>) -> &mut Self {
        merge(&mut self.fields, path.into(), vec![message.into()]);
        self
    }

    /// Adds a message that belongs to no field.
    pub fn add_non_field_message(&mut self, message: impl Into<String>) -> &mut Self {
        self.non_field_messages.push(message.into());
        self
    }

    /// The field paths with their messages, in the order of first use.
    pub fn fields(&self) -> &[(String, Vec<String>)] {
        &self.fields
    }

    /// The messages of one field path.
    pub fn field(&self, path: &str) -> Option<&[String]> {
        self.fields.iter().find(|(p, _)| p == path).map(|(_, list)| list.as_slice())
    }

    /// The messages that belong to no field.
    pub fn non_field_messages(&self) -> &[String] {
        &self.non_field_messages
    }

    pub fn is_empty(&self) -> bool {
        self.fields.is_empty() && self.non_field_messages.is_empty()
    }
}

/// Appends `messages` to the field at `path`, which keeps its place when it exists.
fn merge(fields: &mut Vec<(String, Vec<String>)>, path: String, messages: Vec<String>) {
    match fields.iter_mut().find(|(p, _)| *p == path) {
        Some((_, list)) => list.extend(messages),
        None => fields.push((path, messages)),
    }
}

/// Every language writes the same text: the messages with no field, then `path: message` for each field message,
/// joined with `; `.
impl fmt::Display for ValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let parts: Vec<String> = self.non_field_messages.iter().cloned().chain(self.fields.iter().flat_map(|(path, list)| list.iter().map(move |m| format!("{path}: {m}")))).collect();
        if parts.is_empty() {
            f.write_str("validation failed")
        } else {
            f.write_str(&parts.join("; "))
        }
    }
}

impl std::error::Error for ValidationError {}

/// A JSON object whose keys keep their order.
mod ordered_map {
    use serde::{de::{MapAccess, Visitor}, ser::SerializeMap, Deserializer, Serializer};
    use std::fmt;

    pub fn serialize<S: Serializer>(fields: &[(String, Vec<String>)], s: S) -> Result<S::Ok, S::Error> {
        let mut map = s.serialize_map(Some(fields.len()))?;
        for (path, list) in fields {
            map.serialize_entry(path, list)?;
        }
        map.end()
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<(String, Vec<String>)>, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Vec<(String, Vec<String>)>;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("an object of field paths and message lists")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut m: A) -> Result<Self::Value, A::Error> {
                let mut out: Self::Value = Vec::new();
                while let Some((path, list)) = m.next_entry::<String, Vec<String>>()? {
                    super::merge(&mut out, path, list);
                }
                Ok(out)
            }
        }
        d.deserialize_map(V)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_and_messages_keep_their_order() {
        let mut e = ValidationError::new();
        e.add("lines.2.qty", "must be positive").add("address.city", "is required").add("lines.2.qty", "is too large").add_non_field_message("The order is locked.");
        assert_eq!(e.fields().iter().map(|(p, _)| p.as_str()).collect::<Vec<_>>(), ["lines.2.qty", "address.city"]);
        assert_eq!(e.field("lines.2.qty"), Some(&["must be positive".to_owned(), "is too large".to_owned()][..]));
        assert_eq!(e.non_field_messages(), ["The order is locked."]);
        assert!(!e.is_empty() && ValidationError::new().is_empty());
    }

    #[test]
    fn display_lists_the_messages_with_no_field_first() {
        let mut e = ValidationError::new();
        e.add("address.city", "is required").add_non_field_message("The order is locked.");
        assert_eq!(e.to_string(), "The order is locked.; address.city: is required");
        assert_eq!(ValidationError::new().to_string(), "validation failed");
    }

    #[test]
    fn json_round_trip_keeps_the_order_of_the_paths() {
        let mut e = ValidationError::new();
        e.add("b", "x").add("a", "y").add_non_field_message("m");
        let json = serde_json::to_string(&e).unwrap();
        assert_eq!(json, r#"{"fields":{"b":["x"],"a":["y"]},"non_field_messages":["m"]}"#);
        assert_eq!(serde_json::from_str::<ValidationError>(&json).unwrap(), e);
    }

    #[test]
    fn a_path_that_repeats_in_json_keeps_its_first_place() {
        let e: ValidationError = serde_json::from_str(r#"{"fields":{"b":["x"],"a":["y"],"b":["z"]},"non_field_messages":[]}"#).unwrap();
        assert_eq!(e.fields(), [("b".to_owned(), vec!["x".to_owned(), "z".to_owned()]), ("a".to_owned(), vec!["y".to_owned()])]);
    }
}
