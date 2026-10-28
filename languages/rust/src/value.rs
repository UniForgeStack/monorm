//! Stored-value codecs shared by generated rows and query parameters.
use crate::{Error, Result};
use monorm_core::ir::ValueType;
use monorm_engine::db::Cell;
use sea_query::Value;

pub trait Codec: Sized {
    fn decode(cell: Cell<'_>) -> Result<Self>;
    fn encode(self) -> Value;
}
macro_rules! codec {
    ($t:ty, $variant:ident) => {
        impl Codec for $t {
            fn decode(cell: Cell<'_>) -> Result<Self> {
                match cell {
                    Cell::$variant(v) => Ok(v),
                    other => Err(Error::Decode(format!(
                        "expected {}, received {other:?}",
                        stringify!($t)
                    ))),
                }
            }
            fn encode(self) -> Value {
                self.into()
            }
        }
    };
}
codec!(i32, Int);
codec!(i64, BigInt);
codec!(f64, Float);
codec!(bool, Bool);
codec!(chrono::NaiveDate, Date);
codec!(uuid::Uuid, Uuid);
codec!(serde_json::Value, Json);
impl Codec for chrono::DateTime<chrono::Utc> {
    fn decode(cell: Cell<'_>) -> Result<Self> {
        if let Cell::DateTime(v) = cell {
            Ok(v)
        } else {
            Err(Error::Decode("expected datetime".into()))
        }
    }
    fn encode(self) -> Value {
        self.fixed_offset().into()
    }
}
impl Codec for String {
    fn decode(cell: Cell<'_>) -> Result<Self> {
        if let Cell::Text(v) = cell {
            Ok(v.into())
        } else {
            Err(Error::Decode("expected text".into()))
        }
    }
    fn encode(self) -> Value {
        self.into()
    }
}
impl Codec for bigdecimal::BigDecimal {
    fn decode(cell: Cell<'_>) -> Result<Self> {
        if let Cell::Decimal(v) = cell {
            v.parse()
                .map_err(|e| Error::Decode(format!("invalid decimal: {e}")))
        } else {
            Err(Error::Decode("expected decimal".into()))
        }
    }
    fn encode(self) -> Value {
        self.into()
    }
}
impl<T: Codec> Codec for Option<T> {
    fn decode(cell: Cell<'_>) -> Result<Self> {
        if cell == Cell::Null {
            Ok(None)
        } else {
            T::decode(cell).map(Some)
        }
    }
    // The planner supplies the actual NULL type.
    fn encode(self) -> Value {
        self.map_or(Value::String(None), T::encode)
    }
}
impl<T: Codec> Codec for Vec<T> {
    fn decode(cell: Cell<'_>) -> Result<Self> {
        if let Cell::Array(v) = cell {
            v.into_iter().map(T::decode).collect()
        } else {
            Err(Error::Decode("expected array".into()))
        }
    }
    fn encode(self) -> Value {
        let values: Vec<_> = self.into_iter().map(T::encode).collect();
        let kind = values.first().map_or(sea_query::ArrayType::String, |v| {
            monorm_engine::params::array_type(monorm_engine::params::type_of(v).expect("scalar codec").ty)
        });
        Value::Array(kind, Some(Box::new(values)))
    }
}
#[derive(Clone, Default)]
pub(crate) struct Values(pub Vec<Value>);
impl monorm_engine::Params for Values {
    fn len(&self) -> usize {
        self.0.len()
    }
    fn value(&self, i: usize, ty: Option<ValueType>) -> monorm_engine::Result<Value> {
        let v = self
            .0
            .get(i)
            .ok_or_else(|| monorm_engine::Error::value("parameter index out of range"))?;
        if *v == v.as_null() {
            return Ok(monorm_engine::params::null_of(ty));
        }
        if let (Value::Array(_, Some(v)), Some(t)) = (v, ty) {
            if t.array {
                return Ok(Value::Array(
                    monorm_engine::params::array_type(t.ty),
                    Some(v.clone()),
                ));
            }
        }
        if let Some(t) = ty {
            match (v, t.ty) {
                (Value::Int(Some(n)), monorm_core::ir::ColType::BigInt) => {
                    return Ok(Value::BigInt(Some(i64::from(*n))))
                }
                (Value::BigInt(Some(n)), monorm_core::ir::ColType::Int) => {
                    return i32::try_from(*n)
                        .map(|n| Value::Int(Some(n)))
                        .map_err(|_| monorm_engine::Error::value("integer out of range"))
                }
                _ => {}
            }
        }
        Ok(v.clone())
    }
    fn text(&self, i: usize) -> monorm_engine::Result<String> {
        match self.0.get(i) {
            Some(Value::String(Some(v))) => Ok(v.to_string()),
            _ => Err(monorm_engine::Error::value("expected text")),
        }
    }
    fn count(&self, i: usize) -> monorm_engine::Result<u64> {
        match self.0.get(i) {
            Some(Value::BigInt(Some(v))) if *v >= 0 => Ok(*v as u64),
            _ => Err(monorm_engine::Error::value("expected nonnegative row count")),
        }
    }
}
