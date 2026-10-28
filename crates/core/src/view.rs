//! Model views: the fixed row filter and insert table of a view over a shared table.
//! `define()` prepares one `Option` per model, so a plan reads no record list.
use monorm_contracts::{extension::ModelView, ir::{ColType, EnumIr, FieldIr}};
use serde_json::Value;

#[derive(Clone, Debug)]
pub struct PreparedView {
    pub filter: Option<PreparedFilter>,
    pub insert_table: Option<String>,
}
#[derive(Clone, Debug)]
pub struct PreparedFilter {
    pub column: String,
    /// SQL literals from validated schema values, never query input.
    pub values: String,
}

pub fn prepare(model: &str, fields: &[FieldIr], enums: &[EnumIr], view: Option<&ModelView>) -> Result<Option<PreparedView>, String> {
    let Some(view) = view else { return Ok(None) };
    let filter = view.row_filter.as_ref().map(|filter| {
        let field = fields.iter().find(|f| f.name == filter.field).ok_or_else(|| format!("{model}: unknown row filter field {}", filter.field))?;
        if filter.values.is_empty() { return Err(format!("{model}: empty row filter values")); }
        let values = filter.values.iter().map(|v| literal(model, field, enums, v)).collect::<Result<Vec<_>, _>>()?.join(", ");
        Ok(PreparedFilter { column: field.column.clone(), values })
    }).transpose()?;
    Ok(Some(PreparedView { filter, insert_table: view.insert_table.clone() }))
}

fn quoted(s: &str) -> String { format!("'{}'", s.replace('\'', "''")) }

fn literal(model: &str, field: &FieldIr, enums: &[EnumIr], value: &str) -> Result<String, String> {
    let bad = |what: &str| format!("{model}.{}: row filter value {value:?} is not {what}", field.name);
    if let Some(e) = field.enum_idx.and_then(|i| enums.get(i as usize)) {
        let member = e.values.iter().find(|m| m.name == value).ok_or_else(|| format!("{model}.{}: row filter value {value:?} is not a member of enum {}", field.name, e.name))?;
        return match &member.value {
            Value::String(s) => Ok(quoted(s)),
            v @ (Value::Number(_) | Value::Bool(_)) => Ok(v.to_string()),
            _ => Err(bad("a scalar enum value")),
        };
    }
    if field.array { return Err(format!("{model}.{}: a row filter field cannot be an array", field.name)); }
    match field.ty {
        ColType::String | ColType::Text | ColType::Uuid => Ok(quoted(value)),
        ColType::Int | ColType::BigInt => value.parse::<i64>().map(|n| n.to_string()).map_err(|_| bad("an integer")),
        ColType::Float => value.parse::<f64>().ok().filter(|n| n.is_finite()).map(|n| n.to_string()).ok_or_else(|| bad("a number")),
        ColType::Bool => value.parse::<bool>().map(|b| b.to_string()).map_err(|_| bad("a boolean")),
        _ => Err(format!("{model}.{}: a row filter field must be a string, number, boolean or enum", field.name)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use monorm_contracts::extension::RowFilter;
    use serde_json::json;

    fn fields() -> Vec<FieldIr> {
        serde_json::from_value(json!([
            {"name":"kind","column":"kind_col","type":"string"},
            {"name":"rank","column":"rank","type":"int"},
            {"name":"live","column":"live","type":"bool"},
            {"name":"status","column":"status","type":"string","enum":"Status"}])).map(|mut f: Vec<FieldIr>| { f[3].enum_idx = Some(0); f }).unwrap()
    }
    fn enums() -> Vec<EnumIr> {
        serde_json::from_value(json!([{"name":"Status","db_name":"status","storage":"text","values":[
            {"name":"ACTIVE","value":"it's active"},{"name":"OLD","value":"old"}]}])).unwrap()
    }
    fn view(field: &str, values: &[&str]) -> ModelView {
        ModelView { model: "V".into(), source: "S".into(), root: "S".into(),
            row_filter: Some(RowFilter { field: field.into(), values: values.iter().map(|v| v.to_string()).collect() }), insert_table: Some("t".into()) }
    }
    fn prepared(field: &str, values: &[&str]) -> Result<Option<PreparedView>, String> { prepare("V", &fields(), &enums(), Some(&view(field, values))) }

    #[test]
    fn a_model_without_a_view_prepares_nothing() {
        assert!(prepare("V", &fields(), &enums(), None).unwrap().is_none());
    }

    #[test]
    fn literals_follow_the_column_type_and_the_enum_members() {
        let v = prepared("kind", &["a'b", "c"]).unwrap().unwrap();
        let filter = v.filter.as_ref().unwrap();
        assert_eq!((filter.column.as_str(), filter.values.as_str(), v.insert_table.as_deref()), ("kind_col", "'a''b', 'c'", Some("t")));
        assert_eq!(prepared("rank", &["1", "-2"]).unwrap().unwrap().filter.unwrap().values, "1, -2");
        assert_eq!(prepared("live", &["true"]).unwrap().unwrap().filter.unwrap().values, "true");
        assert_eq!(prepared("status", &["ACTIVE", "OLD"]).unwrap().unwrap().filter.unwrap().values, "'it''s active', 'old'");
    }

    #[test]
    fn a_value_outside_the_field_fails_at_preparation() {
        assert!(prepared("status", &["UNKNOWN"]).unwrap_err().contains("is not a member of enum Status"));
        assert!(prepared("status", &["old"]).unwrap_err().contains("is not a member of enum Status"));
        assert!(prepared("rank", &["x"]).unwrap_err().contains("is not an integer"));
        assert!(prepared("live", &["yes"]).unwrap_err().contains("is not a boolean"));
        assert!(prepared("missing", &["a"]).unwrap_err().contains("unknown row filter field missing"));
        assert!(prepared("kind", &[]).unwrap_err().contains("empty row filter values"));
    }

    #[test]
    fn a_view_may_have_no_row_filter() {
        let mut v = view("kind", &[]);
        v.row_filter = None;
        let v = prepare("V", &fields(), &enums(), Some(&v)).unwrap().unwrap();
        assert!(v.filter.is_none() && v.insert_table.as_deref() == Some("t"));
    }
}
