//! Reading and writing a `state.…` path — the ONE walker every part of the
//! surface uses (review RS-S9): a field's `bind` (read by `resolve`, written
//! by `reduce` on a `change`), a `set` action's `path`, `publish.ids`, and a
//! `call`'s `into` (written by the runtime after the verb answered).
//!
//! The segment rules are the template language's (`template.rs`): a segment
//! names a key of an object, and a numeric segment indexes an array. Writes
//! follow the same rules, so a path that reads an array element writes that
//! element — before, a numeric segment on a write REPLACED the array with an
//! object whose key was the number.
//!
//! * A missing object key on a write is created (as an object when more
//!   segments follow); `null` on the way counts as missing.
//! * An array is never turned into an object, and a scalar is never
//!   overwritten by a deeper write: both are [`StatePathError::NotAContainer`].
//! * An array index must be in range, for a read and for a write
//!   ([`StatePathError::OutOfRange`]); a write never grows an array.
//! * A read of a path that is not there is [`StatePathError::Missing`] —
//!   never `null` — exactly as a `{{state.…}}` template reports it.

use serde_json::{Map, Value};

/// Why a state path could not be read or written.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum StatePathError {
    #[error("path '{path}' must start with 'state.' and name a key under it")]
    NotAStatePath { path: String },
    #[error("state path '{path}' does not resolve to a value ('{at}' is missing)")]
    Missing { path: String, at: String },
    #[error("state path '{path}': index {index} is out of range ('{at}' has {len} element(s))")]
    OutOfRange {
        path: String,
        at: String,
        index: usize,
        len: usize,
    },
    #[error("state path '{path}': '{at}' is {found}, so '{segment}' cannot be looked up in it")]
    NotAContainer {
        path: String,
        at: String,
        segment: String,
        found: &'static str,
    },
}

impl StatePathError {
    /// The refusal code a dispatch carries for this error.
    pub fn code(&self) -> &'static str {
        match self {
            StatePathError::NotAStatePath { .. } => "invalid-path",
            StatePathError::Missing { .. } | StatePathError::OutOfRange { .. } => {
                "missing-state-path"
            }
            StatePathError::NotAContainer { .. } => "state-path-conflict",
        }
    }
}

/// The segments after `state` (`state.a.0.b` → `[a, 0, b]`); at least one.
pub fn segments(path: &str) -> Result<Vec<&str>, StatePathError> {
    let mut parts = path.split('.');
    if parts.next() != Some("state") {
        return Err(StatePathError::NotAStatePath {
            path: path.to_string(),
        });
    }
    let rest: Vec<&str> = parts.collect();
    if rest.is_empty() || rest.iter().any(|s| s.is_empty()) {
        return Err(StatePathError::NotAStatePath {
            path: path.to_string(),
        });
    }
    Ok(rest)
}

fn found(value: &Value) -> &'static str {
    crate::spec::json_type(value)
}

/// The value at `path` in `state`.
pub fn read(state: &Value, path: &str) -> Result<Value, StatePathError> {
    let segs = segments(path)?;
    let mut cur = state;
    let mut at = String::from("state");
    for seg in segs {
        cur = match cur {
            Value::Object(map) => map.get(seg).ok_or_else(|| StatePathError::Missing {
                path: path.to_string(),
                at: format!("{at}.{seg}"),
            })?,
            Value::Array(items) => {
                let index = seg
                    .parse::<usize>()
                    .map_err(|_| StatePathError::NotAContainer {
                        path: path.to_string(),
                        at: at.clone(),
                        segment: seg.to_string(),
                        found: "an array",
                    })?;
                items.get(index).ok_or_else(|| StatePathError::OutOfRange {
                    path: path.to_string(),
                    at: at.clone(),
                    index,
                    len: items.len(),
                })?
            }
            Value::Null => {
                return Err(StatePathError::Missing {
                    path: path.to_string(),
                    at,
                })
            }
            other => {
                return Err(StatePathError::NotAContainer {
                    path: path.to_string(),
                    at,
                    segment: seg.to_string(),
                    found: found(other),
                })
            }
        };
        at.push('.');
        at.push_str(seg);
    }
    Ok(cur.clone())
}

/// Write `value` at `path` in `state`, touching nothing else. `state` itself
/// must be an object (or `null`, which becomes one).
pub fn write(state: &mut Value, path: &str, value: Value) -> Result<(), StatePathError> {
    let segs = segments(path)?;
    if state.is_null() {
        *state = Value::Object(Map::new());
    }
    let mut cur = state;
    let mut at = String::from("state");
    let last = segs.len() - 1;
    for (i, seg) in segs.iter().enumerate() {
        let is_last = i == last;
        cur = match cur {
            Value::Object(map) => {
                if is_last {
                    map.insert((*seg).to_string(), value);
                    return Ok(());
                }
                let slot = map.entry((*seg).to_string()).or_insert(Value::Null);
                if slot.is_null() {
                    *slot = Value::Object(Map::new());
                }
                slot
            }
            Value::Array(items) => {
                let index = seg
                    .parse::<usize>()
                    .map_err(|_| StatePathError::NotAContainer {
                        path: path.to_string(),
                        at: at.clone(),
                        segment: (*seg).to_string(),
                        found: "an array",
                    })?;
                let len = items.len();
                let slot = items
                    .get_mut(index)
                    .ok_or_else(|| StatePathError::OutOfRange {
                        path: path.to_string(),
                        at: at.clone(),
                        index,
                        len,
                    })?;
                if is_last {
                    *slot = value;
                    return Ok(());
                }
                if slot.is_null() {
                    *slot = Value::Object(Map::new());
                }
                slot
            }
            other => {
                return Err(StatePathError::NotAContainer {
                    path: path.to_string(),
                    at,
                    segment: (*seg).to_string(),
                    found: found(other),
                })
            }
        };
        at.push('.');
        at.push_str(seg);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_numeric_segment_reads_and_writes_the_same_array_element() {
        let mut state = json!({"rows": [{"n": 1}, {"n": 2}]});
        assert_eq!(read(&state, "state.rows.1.n").unwrap(), json!(2));
        write(&mut state, "state.rows.1.n", json!(20)).unwrap();
        assert_eq!(state, json!({"rows": [{"n": 1}, {"n": 20}]}));
    }

    #[test]
    fn an_array_is_never_turned_into_an_object() {
        let mut state = json!({"rows": [1, 2]});
        let err = write(&mut state, "state.rows.x", json!(0)).unwrap_err();
        assert!(matches!(err, StatePathError::NotAContainer { .. }), "{err}");
        assert_eq!(state, json!({"rows": [1, 2]}));
    }

    #[test]
    fn an_index_out_of_range_is_refused_both_ways() {
        let mut state = json!({"rows": [1]});
        assert!(matches!(
            read(&state, "state.rows.3"),
            Err(StatePathError::OutOfRange {
                index: 3,
                len: 1,
                ..
            })
        ));
        assert!(matches!(
            write(&mut state, "state.rows.3", json!(0)),
            Err(StatePathError::OutOfRange { .. })
        ));
    }

    #[test]
    fn a_missing_key_is_created_on_write_and_missing_on_read() {
        let mut state = json!({});
        assert!(matches!(
            read(&state, "state.a.b"),
            Err(StatePathError::Missing { .. })
        ));
        write(&mut state, "state.a.b", json!(1)).unwrap();
        assert_eq!(state, json!({"a": {"b": 1}}));
    }

    #[test]
    fn a_scalar_in_the_way_is_not_overwritten() {
        let mut state = json!({"a": 5});
        assert!(matches!(
            write(&mut state, "state.a.b", json!(1)),
            Err(StatePathError::NotAContainer {
                found: "a number",
                ..
            })
        ));
        assert_eq!(state, json!({"a": 5}));
    }

    #[test]
    fn only_state_paths_are_accepted() {
        for bad in ["param.x", "state", "state.", "state..x", "x"] {
            assert!(matches!(
                segments(bad),
                Err(StatePathError::NotAStatePath { .. })
            ));
        }
    }
}
