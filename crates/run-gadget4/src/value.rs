//! Typed values: GADGET-4 text <-> JSON, and the printing of a float so that
//! GADGET-4's `atof` reads back exactly the number we hold.

use crate::table::Kind;
use serde_json::{json, Value};

/// Infer a kind for a tag the table does not know.
pub fn infer_kind(text: &str) -> Kind {
    if text.parse::<i64>().is_ok() {
        Kind::Int
    } else if text.parse::<f64>().map(f64::is_finite).unwrap_or(false) {
        Kind::Float
    } else if text.contains('/') || text.contains('.') {
        Kind::Path
    } else {
        Kind::String
    }
}

/// Text -> JSON value of `kind`; `Err` says why the text is not that type.
pub fn from_text(kind: Kind, text: &str) -> Result<Value, String> {
    match kind {
        Kind::Int => text
            .parse::<i64>()
            .map(|n| json!(n))
            .map_err(|_| format!("`{text}` is not an integer")),
        Kind::Float => match text.parse::<f64>() {
            Ok(f) if f.is_finite() => Ok(json!(f)),
            _ => Err(format!("`{text}` is not a finite number")),
        },
        Kind::String | Kind::Path => Ok(json!(text)),
    }
}

/// JSON value -> the text written into `param.txt`.
pub fn to_text(kind: Kind, v: &Value) -> Result<String, String> {
    match kind {
        Kind::Int => match v {
            Value::Number(n) if n.is_i64() || n.is_u64() => Ok(n.to_string()),
            other => Err(format!("expected an integer, got {other}")),
        },
        Kind::Float => match v.as_f64() {
            Some(f) if f.is_finite() => Ok(fmt_float(f)),
            _ => Err(format!("expected a number, got {v}")),
        },
        Kind::String | Kind::Path => match v.as_str() {
            Some("") => Err("an empty string cannot be written (GADGET-4 reads the value as the second token on the line)".into()),
            Some(s) if s.chars().any(char::is_whitespace) => Err(format!(
                "`{s}` contains whitespace; GADGET-4 reads only the first token of a value"
            )),
            Some(s) => Ok(s.to_string()),
            None => Err(format!("expected a string, got {v}")),
        },
    }
}

/// Shortest decimal that round-trips, in a form C's `atof` reads: plain for
/// moderate magnitudes, exponent form beyond (`3.085678e24`, never 25 digits).
pub fn fmt_float(f: f64) -> String {
    if f == 0.0 {
        return "0".into();
    }
    let a = f.abs();
    if !(1e-4..1e15).contains(&a) {
        format!("{f:e}")
    } else {
        format!("{f}")
    }
}

/// Equal within a relative tolerance (absolute near zero).
pub fn close(a: f64, b: f64, rel: f64) -> bool {
    if a == b {
        return true;
    }
    (a - b).abs() <= rel * a.abs().max(b.abs())
}
