//! Result checks: preparation resolves field positions once; operations check decoded
//! physical values without any declaration lookup.
use std::collections::BTreeMap;
use monorm_contracts::{extension::ResultCheck, ir::{EnumIr, FieldIr, ValueType}};
use serde::Serialize;
use serde_json::Value;

#[derive(Clone, Debug, Default)]
pub struct PreparedChecks {
    pub model: String,
    pub fields: Vec<PreparedField>,
}
#[derive(Clone, Debug)]
pub struct PreparedField {
    pub position: usize,
    pub name: String,
    pub ty: ValueType,
    pub expected: ExpectedShape,
    pub allowed: Option<Vec<Value>>,
    pub code: String,
}
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct ExpectedShape {
    pub non_null: bool,
    pub enum_members: Option<Vec<String>>,
}
#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum Category { Null, EnumSubset }
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct Warning {
    pub code: String,
    pub model: String,
    pub field: String,
    pub expected_shape: ExpectedShape,
    pub category: Category,
    pub occurrence_count: usize,
}

/// The checks of one model, or `None` when it has none.
pub fn prepare(model: &str, fields: &[FieldIr], enums: &[EnumIr], checks: &[&ResultCheck]) -> Result<Option<PreparedChecks>, String> {
    if checks.is_empty() { return Ok(None); }
    let mut prepared = PreparedChecks { model: model.into(), ..Default::default() };
    let mut seen = std::collections::BTreeSet::new();
    for check in checks {
        if !seen.insert(&check.field) { return Err(format!("{model}: duplicate result check field {}", check.field)); }
        let position = fields.iter().position(|f| f.name == check.field).ok_or_else(|| format!("{model}: unknown result check field {}", check.field))?;
        let field = &fields[position];
        let allowed = if let Some(members) = &check.allowed {
            let e = field.enum_idx.and_then(|i| enums.get(i as usize)).ok_or_else(|| format!("{model}.{}: allowed values require an enum field", field.name))?;
            if members.is_empty() || members.iter().collect::<std::collections::BTreeSet<_>>().len() != members.len() { return Err(format!("{model}.{}: allowed enum members must be nonempty and unique", field.name)); }
            Some(members.iter().map(|m| e.values.iter().find(|v| v.name == *m).map(|v| v.value.clone()).ok_or_else(|| format!("{model}.{}: unknown enum member {m}", field.name))).collect::<Result<_, _>>()?)
        } else { None };
        prepared.fields.push(PreparedField { position, name: field.name.clone(), ty: field.value_type(),
            expected: ExpectedShape { non_null: check.non_null, enum_members: check.allowed.clone() }, allowed, code: check.code.clone() });
    }
    Ok(Some(prepared))
}

/// Borrow an ordinary decoded enum scalar, retaining its field's representation.
pub enum EnumScalar<'a> { Text(&'a str), Int(i64) }
impl PreparedField {
    pub fn accepts_enum(&self, value: EnumScalar<'_>) -> bool {
        self.allowed.as_ref().is_none_or(|allowed| allowed.iter().any(|v| match value {
            EnumScalar::Text(s) => v.as_str() == Some(s),
            EnumScalar::Int(n) => v.as_i64() == Some(n),
        }))
    }
}

/// One accumulator per operation, shared by root/joins/prefetch/returning shapes.
/// Only metadata enters a warning. Decoded field values are never retained.
#[derive(Default)]
pub struct Warnings<'a> { entries: BTreeMap<(&'a str, &'a str, Category), Warning> }
impl<'a> Warnings<'a> {
    pub fn record(&mut self, checks: &'a PreparedChecks, field: &'a PreparedField, category: Category) {
        let key = (checks.model.as_str(), field.name.as_str(), category);
        self.entries.entry(key).and_modify(|w| w.occurrence_count += 1).or_insert_with(|| Warning {
            code: field.code.clone(), model: checks.model.clone(), field: field.name.clone(),
            expected_shape: field.expected.clone(), category, occurrence_count: 1,
        });
    }
    pub fn finish(self) -> Vec<Warning> { self.entries.into_values().collect() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn fields() -> Vec<FieldIr> {
        serde_json::from_value(json!([{"name":"name","column":"name","type":"string","nullable":true},
            {"name":"status","column":"status","type":"string","enum":"Status"}])).map(|mut f: Vec<FieldIr>| { f[1].enum_idx = Some(0); f }).unwrap()
    }
    fn enums() -> Vec<EnumIr> {
        serde_json::from_value(json!([{"name":"Status","db_name":"status","storage":"text","values":[
            {"name":"ACTIVE","value":"active"},{"name":"OLD","value":"old"}]}])).unwrap()
    }
    fn check(field: &str, non_null: bool, allowed: Option<&[&str]>) -> ResultCheck {
        ResultCheck { model: "V".into(), field: field.into(), non_null, allowed: allowed.map(|a| a.iter().map(|s| s.to_string()).collect()), code: "acme.code".into() }
    }
    fn prepared(checks: &[ResultCheck]) -> Result<Option<PreparedChecks>, String> { prepare("V", &fields(), &enums(), &checks.iter().collect::<Vec<_>>()) }

    #[test]
    fn a_model_without_checks_prepares_nothing() {
        assert!(prepared(&[]).unwrap().is_none());
    }

    #[test]
    fn a_check_resolves_its_position_members_and_code() {
        let p = prepared(&[check("status", false, Some(&["ACTIVE"])), check("name", true, None)]).unwrap().unwrap();
        assert_eq!(p.fields[0].position, 1);
        assert!(p.fields[0].accepts_enum(EnumScalar::Text("active")) && !p.fields[0].accepts_enum(EnumScalar::Text("old")));
        assert!(p.fields[1].accepts_enum(EnumScalar::Text("anything")));
        let mut warnings = Warnings::default();
        warnings.record(&p, &p.fields[1], Category::Null);
        warnings.record(&p, &p.fields[1], Category::Null);
        let out = warnings.finish();
        assert_eq!((out[0].code.as_str(), out[0].occurrence_count), ("acme.code", 2));
    }

    #[test]
    fn a_bad_check_fails_at_preparation() {
        assert!(prepared(&[check("missing", true, None)]).unwrap_err().contains("unknown result check field missing"));
        assert!(prepared(&[check("name", true, None), check("name", true, None)]).unwrap_err().contains("duplicate result check field name"));
        assert!(prepared(&[check("name", false, Some(&["A"]))]).unwrap_err().contains("allowed values require an enum field"));
        assert!(prepared(&[check("status", false, Some(&["NOPE"]))]).unwrap_err().contains("unknown enum member NOPE"));
        assert!(prepared(&[check("status", false, Some(&[]))]).unwrap_err().contains("nonempty and unique"));
        assert!(prepared(&[check("status", false, Some(&["OLD", "OLD"]))]).unwrap_err().contains("nonempty and unique"));
    }
}
