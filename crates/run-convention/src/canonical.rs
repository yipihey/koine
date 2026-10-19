//! RFC 8785 (JSON Canonicalization Scheme) and the run key (R-D4.3).

use serde_json::Value;
use sha2::{Digest, Sha256};

/// The RFC 8785 canonical form: object keys sorted by UTF-16 code units, no
/// whitespace, numbers as ECMAScript `Number::toString` of the IEEE double
/// (shortest round-trip), strings escaped as `JSON.stringify` does, UTF-8.
pub fn canonical_json(v: &Value) -> String {
    let mut out = String::new();
    write(v, &mut out);
    out
}

/// `sha256` hex of the canonical form of `{"problem": …, "params": …}`.
/// `params_values` is the object of bare values (`params.json` with sources
/// stripped, see [`values_of`]).
pub fn key_of(problem: &Value, params_values: &Value) -> String {
    let mut m = serde_json::Map::new();
    m.insert("problem".into(), problem.clone());
    m.insert("params".into(), params_values.clone());
    let s = canonical_json(&Value::Object(m));
    let digest = Sha256::digest(s.as_bytes());
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// The value fields of a `params.json` object: `{name: {value, source}}`
/// becomes `{name: value}`. A malformed entry (no `value`) is dropped; the
/// schema check reports it.
pub fn values_of(params: &Value) -> Value {
    let mut m = serde_json::Map::new();
    if let Some(o) = params.as_object() {
        for (k, e) in o {
            if let Some(v) = e.get("value") {
                m.insert(k.clone(), v.clone());
            }
        }
    }
    Value::Object(m)
}

fn write(v: &Value, out: &mut String) {
    match v {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => out.push_str(&number(n.as_f64().unwrap_or(0.0))),
        Value::String(s) => string(s, out),
        Value::Array(a) => {
            out.push('[');
            for (i, e) in a.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write(e, out);
            }
            out.push(']');
        }
        Value::Object(o) => {
            let mut keys: Vec<&String> = o.keys().collect();
            keys.sort_by(|a, b| a.encode_utf16().cmp(b.encode_utf16()));
            out.push('{');
            for (i, k) in keys.into_iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                string(k, out);
                out.push(':');
                write(&o[k], out);
            }
            out.push('}');
        }
    }
}

fn string(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

/// ECMAScript `Number::toString` for a finite double.
fn number(f: f64) -> String {
    if f == 0.0 {
        return "0".into();
    }
    let sign = if f < 0.0 { "-" } else { "" };
    // Rust's `{:e}` yields the shortest round-trip digits: "d.ddde<exp>".
    let e = format!("{:e}", f.abs());
    let (mant, exp) = e.split_once('e').unwrap_or((&e, "0"));
    let digits: String = mant.chars().filter(|c| *c != '.').collect();
    let k = digits.len() as i32;
    // value = 0.<digits> × 10^n
    let n = exp.parse::<i32>().unwrap_or(0) + 1;
    let body = if k <= n && n <= 21 {
        format!("{digits}{}", "0".repeat((n - k) as usize))
    } else if 0 < n && n <= 21 {
        format!("{}.{}", &digits[..n as usize], &digits[n as usize..])
    } else if -6 < n && n <= 0 {
        format!("0.{}{digits}", "0".repeat((-n) as usize))
    } else {
        let x = n - 1;
        let sx = if x < 0 { "-" } else { "+" };
        if k == 1 {
            format!("{digits}e{sx}{}", x.abs())
        } else {
            format!("{}.{}e{sx}{}", &digits[..1], &digits[1..], x.abs())
        }
    };
    format!("{sign}{body}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn numbers_follow_ecmascript() {
        let cases = [
            (1.0, "1"),
            (-0.0, "0"),
            (3.0e6, "3000000"),
            (1.2e34, "1.2e+34"),
            (1e21, "1e+21"),
            (1e20, "100000000000000000000"),
            (0.5, "0.5"),
            (1e-6, "0.000001"),
            (1e-7, "1e-7"),
            (1.5e-7, "1.5e-7"),
            (123.456, "123.456"),
            (0.1 + 0.2, "0.30000000000000004"),
            (-2.5, "-2.5"),
        ];
        for (f, s) in cases {
            assert_eq!(number(f), s, "{f}");
        }
    }

    #[test]
    fn rfc8785_sample() {
        // RFC 8785 section 3.2.3: keys sorted by UTF-16, literals, numbers.
        let v: Value = serde_json::from_str(
            r#"{"numbers":[333333333.33333329, 1E30, 4.50, 2e-3, 0.000000000000000000000000001],
                "string":"\u20ac$\u000F\u000aA'\u0042\u0022\u005c\\\"\/",
                "literals":[null, true, false]}"#,
        )
        .unwrap();
        assert_eq!(
            canonical_json(&v),
            "{\"literals\":[null,true,false],\"numbers\":[333333333.3333333,1e+30,4.5,0.002,1e-27],\"string\":\"€$\\u000f\\nA'B\\\"\\\\\\\\\\\"/\"}"
        );
    }

    #[test]
    fn sorts_by_utf16_units() {
        // U+1F600 (surrogates D83D DE00) sorts before U+FF5E; code point order would not.
        let v = json!({"\u{ff5e}": 1, "\u{1f600}": 2, "b": 3});
        assert_eq!(
            canonical_json(&v),
            "{\"b\":3,\"\u{1f600}\":2,\"\u{ff5e}\":1}"
        );
    }

    #[test]
    fn key_is_stable_and_value_only() {
        let p = json!({"name": "sod"});
        let a = key_of(&p, &json!({"a.b": 1, "a.c": 2.0}));
        let b = key_of(&p, &json!({"a.c": 2, "a.b": 1.0}));
        assert_eq!(a, b);
        assert_eq!(a.len(), 64);
        assert_ne!(a, key_of(&p, &json!({"a.b": 1, "a.c": 3})));
    }
}
