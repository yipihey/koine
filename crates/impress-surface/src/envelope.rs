//! The `{rows, next}` envelope (plan-koine-live L-D5).
//!
//! A result that is an object with a `rows` array is rows with a little more
//! to say about them: which `columns` to show and which field is the `id_field`
//! when the node that shows them declares neither (a query result knows its
//! columns only at run time), and `next`, the cursor of the page after this
//! one (a string, or null on the last page).
//!
//! ```json
//! {"rows": [{"id": 1, "title": "a"}], "columns": ["id", "title"],
//!  "id_field": "id", "next": "cursor-2"}
//! ```
//!
//! Anything else is not an envelope and passes through unchanged, so a table
//! handed a bare array is as it always was. [`unwrap_rows`] is the one reader:
//! the table, the list and the bound plot all go through it.

use serde_json::Value;

/// What [`unwrap_rows`] found.
///
/// The first element is the `rows` array of an envelope, or `value` itself
/// when it is not one. `columns` is the envelope's `columns` when it is an
/// array of strings, `id_field` its `id_field` when it is a string, and `next`
/// its `next` when the key is present (a `null` is `Some(Value::Null)`: the
/// last page, said out loud).
pub type Unwrapped<'a> = (
    &'a Value,
    Option<Vec<String>>,
    Option<String>,
    Option<Value>,
);

/// See the module docs.
pub fn unwrap_rows(value: &Value) -> Unwrapped<'_> {
    let Some(rows) = value.get("rows").filter(|rows| rows.is_array()) else {
        return (value, None, None, None);
    };
    let columns = value
        .get("columns")
        .and_then(Value::as_array)
        .and_then(|columns| {
            columns
                .iter()
                .map(|column| column.as_str().map(str::to_string))
                .collect::<Option<Vec<String>>>()
        });
    let id_field = value
        .get("id_field")
        .and_then(Value::as_str)
        .map(str::to_string);
    let next = value.get("next").cloned();
    (rows, columns, id_field, next)
}

/// The keys of the first row when it is an object, in the map's (sorted)
/// order: the columns of a table whose envelope named none.
pub fn first_row_keys(rows: &Value) -> Vec<String> {
    rows.as_array()
        .and_then(|rows| rows.first())
        .and_then(Value::as_object)
        .map(|row| row.keys().cloned().collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn a_bare_array_and_a_scalar_pass_through() {
        let rows = json!([{"id": 1}]);
        assert_eq!(unwrap_rows(&rows), (&rows, None, None, None));
        let null = Value::Null;
        assert_eq!(unwrap_rows(&null), (&null, None, None, None));
    }

    #[test]
    fn an_object_without_a_rows_array_is_not_an_envelope() {
        let object = json!({"rows": "no", "next": "x"});
        assert_eq!(unwrap_rows(&object), (&object, None, None, None));
        let other = json!({"items": [1], "next": "x"});
        assert_eq!(unwrap_rows(&other), (&other, None, None, None));
    }

    #[test]
    fn an_envelope_gives_its_rows_columns_id_field_and_next() {
        let envelope = json!({
            "rows": [{"id": 1, "t": "a"}],
            "columns": ["id", "t"],
            "id_field": "id",
            "next": "c2"
        });
        let (rows, columns, id_field, next) = unwrap_rows(&envelope);
        assert_eq!(rows, &json!([{"id": 1, "t": "a"}]));
        assert_eq!(columns, Some(vec!["id".to_string(), "t".to_string()]));
        assert_eq!(id_field.as_deref(), Some("id"));
        assert_eq!(next, Some(json!("c2")));
    }

    #[test]
    fn a_null_next_is_the_last_page_and_a_missing_one_is_nothing() {
        let last = json!({"rows": [], "next": null});
        assert_eq!(unwrap_rows(&last).3, Some(Value::Null));
        let bare = json!({"rows": []});
        assert_eq!(unwrap_rows(&bare).3, None);
    }

    #[test]
    fn columns_that_are_not_all_strings_are_ignored() {
        let envelope = json!({"rows": [], "columns": ["a", 2]});
        assert_eq!(unwrap_rows(&envelope).1, None);
    }

    #[test]
    fn the_first_rows_keys_are_the_fallback_columns() {
        assert_eq!(
            first_row_keys(&json!([{"b": 1, "a": 2}, {"z": 0}])),
            vec!["a".to_string(), "b".to_string()]
        );
        assert!(first_row_keys(&json!([])).is_empty());
        assert!(first_row_keys(&json!(["x"])).is_empty());
    }
}
