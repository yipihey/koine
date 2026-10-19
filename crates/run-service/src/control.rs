//! The control inbox (R-D8), from the requester's side: write one request
//! as `control/<request id>.json`, and read what became of every request
//! (`control/done/`, `control/refused/`). The code is the server; koine
//! only writes files and reads them back.

use std::fs;
use std::path::Path;

use run_convention::schemas;
use serde_json::{json, Value};

use crate::dto::{ControlOutcome, Refused};

/// Validate `body` against `control.schema.json`, give it an id, and write
/// it whole into `control/` (a temp file renamed into place, so the code
/// never reads a half-written request). Returns the id.
pub fn write_request(dir: &Path, mut body: Value) -> Result<String, Refused> {
    let id = uuid::Uuid::new_v4().to_string();
    body["id"] = json!(id);
    let schema: Value = serde_json::from_str(schemas::CONTROL).expect("bundled schema is JSON");
    let validator = jsonschema::options()
        .build(&schema)
        .expect("bundled schema compiles");
    if let Some(e) = validator.iter_errors(&body).next() {
        return Err(Refused::invalid(format!("control request: {e}")));
    }
    let inbox = dir.join("control");
    fs::create_dir_all(&inbox)
        .map_err(|e| Refused::new("io", format!("{}: {e}", inbox.display())))?;
    let text = serde_json::to_string(&body).map_err(|e| Refused::new("io", e.to_string()))?;
    let tmp = inbox.join(format!(".{id}.json.tmp"));
    let dest = inbox.join(format!("{id}.json"));
    fs::write(&tmp, text).map_err(|e| Refused::new("io", format!("{}: {e}", tmp.display())))?;
    fs::rename(&tmp, &dest).map_err(|e| Refused::new("io", format!("{}: {e}", dest.display())))?;
    Ok(id)
}

/// Every request under `control/`, `control/done/` and `control/refused/`,
/// oldest first (by file modification time, then name).
pub fn outcomes(dir: &Path) -> Vec<ControlOutcome> {
    let mut out: Vec<(std::time::SystemTime, String, ControlOutcome)> = Vec::new();
    for (sub, status) in [("", "pending"), ("done", "applied"), ("refused", "refused")] {
        let d = dir.join("control").join(sub);
        let Ok(rd) = fs::read_dir(&d) else { continue };
        for e in rd.filter_map(Result::ok) {
            let path = e.path();
            let name = e.file_name().to_string_lossy().into_owned();
            if !path.is_file() || !name.ends_with(".json") || name.starts_with('.') {
                continue;
            }
            let Ok(text) = fs::read_to_string(&path) else {
                continue;
            };
            let Ok(body) = serde_json::from_str::<Value>(&text) else {
                continue;
            };
            let id = body
                .get("id")
                .and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| name.trim_end_matches(".json").to_string());
            let modified = e
                .metadata()
                .and_then(|m| m.modified())
                .unwrap_or(std::time::UNIX_EPOCH);
            out.push((
                modified,
                name,
                ControlOutcome {
                    id,
                    request: body
                        .get("request")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                    status: status.to_string(),
                    reason: body
                        .get("reason")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    body,
                },
            ));
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    out.into_iter().map(|(_, _, o)| o).collect()
}
