//! Row hand-off: the cells of a row set packed into one byte buffer, which the TypeScript
//! runtime decodes (`src/rows.ts`; keep the two in step). One Node-API call per row set.
//!
//! Row after row, each cell a tag byte then its payload, little-endian and unaligned:
//!
//! | tag | cell                         | payload                                  |
//! |-----|------------------------------|------------------------------------------|
//! | 0   | null                         |                                          |
//! | 1/2 | false / true                 |                                          |
//! | 3   | int                          | i32                                      |
//! | 4   | bigint                       | i64                                      |
//! | 5   | float                        | f64                                      |
//! | 6   | text, ASCII                  | u32 length, bytes                        |
//! | 7   | text, UTF-8                  | u32 length, bytes                        |
//! | 8   | datetime                     | f64 milliseconds                         |
//! | 9   | datetime, sub-millisecond    | f64 milliseconds, u16 microseconds below |
//! | 10  | date                         | f64 milliseconds of midnight UTC         |
//! | 11  | uuid                         | 36 ASCII bytes                           |
//! | 12  | json                         | u32 length, JSON text                    |
//! | 13  | decimal                      | u32 length, ASCII text                   |
//! | 14  | array                        | u32 count, the cells                     |

use crate::convert::midnight_ms;
use monorm_core::ir::ValueType;
use monorm_engine::db::{Cell, DbResult, RowSet};

mod tag {
    pub const NULL: u8 = 0;
    pub const FALSE: u8 = 1;
    pub const TRUE: u8 = 2;
    pub const INT: u8 = 3;
    pub const BIGINT: u8 = 4;
    pub const FLOAT: u8 = 5;
    pub const ASCII: u8 = 6;
    pub const UTF8: u8 = 7;
    pub const DATETIME: u8 = 8;
    pub const DATETIME_MICROS: u8 = 9;
    pub const DATE: u8 = 10;
    pub const UUID: u8 = 11;
    pub const JSON: u8 = 12;
    pub const DECIMAL: u8 = 13;
    pub const ARRAY: u8 = 14;
}

fn len(out: &mut Vec<u8>, n: usize) {
    out.extend_from_slice(&(n as u32).to_le_bytes());
}

fn text(out: &mut Vec<u8>, tag: u8, s: &str) {
    out.push(tag);
    len(out, s.len());
    out.extend_from_slice(s.as_bytes());
}

fn cell(out: &mut Vec<u8>, c: Cell<'_>) {
    match c {
        Cell::Null => out.push(tag::NULL),
        Cell::Bool(b) => out.push(if b { tag::TRUE } else { tag::FALSE }),
        Cell::Int(n) => {
            out.push(tag::INT);
            out.extend_from_slice(&n.to_le_bytes());
        }
        Cell::BigInt(n) => {
            out.push(tag::BIGINT);
            out.extend_from_slice(&n.to_le_bytes());
        }
        Cell::Float(n) => {
            out.push(tag::FLOAT);
            out.extend_from_slice(&n.to_le_bytes());
        }
        Cell::Text(s) => text(out, if s.is_ascii() { tag::ASCII } else { tag::UTF8 }, s),
        Cell::DateTime(d) => {
            let ms = d.timestamp_millis() as f64;
            let us = d.timestamp_subsec_micros() % 1000;
            out.push(if us == 0 { tag::DATETIME } else { tag::DATETIME_MICROS });
            out.extend_from_slice(&ms.to_le_bytes());
            if us != 0 {
                out.extend_from_slice(&(us as u16).to_le_bytes());
            }
        }
        Cell::Date(d) => {
            out.push(tag::DATE);
            out.extend_from_slice(&midnight_ms(d).to_le_bytes());
        }
        Cell::Uuid(u) => {
            out.push(tag::UUID);
            out.extend_from_slice(u.hyphenated().encode_lower(&mut uuid::Uuid::encode_buffer()).as_bytes());
        }
        Cell::Json(j) => text(out, tag::JSON, &j.to_string()),
        Cell::Decimal(s) => text(out, tag::DECIMAL, &s),
        Cell::Array(items) => {
            out.push(tag::ARRAY);
            len(out, items.len());
            for item in items {
                cell(out, item);
            }
        }
    }
}

/// The cells of `rows`, row after row, in the layout above.
pub fn pack(rows: &dyn RowSet, types: &[ValueType]) -> DbResult<Vec<u8>> {
    // One tag and an eight-byte scalar payload, rounded up; variable-length cells grow it.
    const ESTIMATED_CELL_BYTES: usize = 10;
    let mut out = Vec::with_capacity(rows.len() * types.len() * ESTIMATED_CELL_BYTES);
    for r in 0..rows.len() {
        for (col, ty) in types.iter().enumerate() {
            cell(&mut out, rows.cell(r, col, *ty)?);
        }
    }
    Ok(out)
}
