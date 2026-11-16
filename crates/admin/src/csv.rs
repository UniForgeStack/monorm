//! The rows of a list as CSV text (RFC 4180, CRLF line ends). The header and the cells follow the columns of the list.
//! A text cell that a spreadsheet could read as a formula starts with `'`.

use serde_json::Value;

use crate::meta::humanize;
use crate::site::{Resource, Site};
use monorm_core::ir::ColType;

/// The characters that start a formula in a spreadsheet (OWASP "CSV injection").
const FORMULA: [char; 6] = ['=', '+', '-', '@', '\t', '\r'];

/// The header line: the title of each column, as the list shows it.
pub fn header(resource: &Resource) -> String {
    let mut out = String::new();
    for (i, c) in resource.columns.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        let title = resource.labels.get(&c.label).cloned().unwrap_or_else(|| humanize(&c.label));
        field(&escape(&title), &mut out);
    }
    out.push_str("\r\n");
    out
}

/// One line for each row. A row is the JSON array of the cells of the list, in the order of `resource.columns`.
pub fn rows(resource: &Resource, rows: &[Value]) -> String {
    let mut out = String::new();
    for row in rows {
        let cells = row.as_array().map(Vec::as_slice).unwrap_or_default();
        for (i, c) in resource.columns.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            cell(c, cells.get(i).unwrap_or(&Value::Null), &mut out);
        }
        out.push_str("\r\n");
    }
    out
}

impl Site {
    /// `rows` as CSV, with the header line first when `with_header` is set.
    pub fn encode_csv(&self, resource: &Resource, rows_of: &[Value], with_header: bool) -> String {
        let mut out = if with_header { header(resource) } else { String::new() };
        out.push_str(&rows(resource, rows_of));
        out
    }
}

fn cell(c: &crate::site::Column, v: &Value, out: &mut String) {
    match v {
        Value::Null => {}
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => out.push_str(&n.to_string()),
        // BigInt and Decimal travel as text, but they are numbers: a minus sign is not a formula.
        Value::String(t) if !c.ty.array && matches!(c.ty.ty, ColType::BigInt | ColType::Decimal) => field(t, out),
        Value::String(t) => field(&escape(t), out),
        other => field(&other.to_string(), out),
    }
}

fn escape(text: &str) -> String {
    if text.starts_with(FORMULA) {
        format!("'{text}")
    } else {
        text.to_owned()
    }
}

fn field(text: &str, out: &mut String) {
    if !text.contains([',', '"', '\r', '\n']) {
        out.push_str(text);
        return;
    }
    out.push('"');
    out.push_str(&text.replace('"', "\"\""));
    out.push('"');
}
