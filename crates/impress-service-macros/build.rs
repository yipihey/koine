//! Embed the canonical schema refs so `effects = { reads: ["…"] }` can be
//! checked at expansion time (ADR-0036 D1: a misspelt ref is a compile error,
//! which is what the manifest rule exists for — see CLAUDE.md § schema refs).
//!
//! [`impress_schema_ref_codegen::manifest_path`] is the live suite manifest
//! when `IMPRESS_SCHEMA_REFS` is set, and the snapshot beside this grammar
//! otherwise. An absent file leaves the list empty and skips the check; the
//! descriptor test in `impress-capabilities` checks every linked declaration
//! against the suite manifest.

use std::env;
use std::fs;
use std::path::Path;

fn main() {
    let manifest = impress_schema_ref_codegen::manifest_path();
    println!("cargo::rerun-if-changed={}", manifest.display());
    println!("cargo::rerun-if-env-changed=IMPRESS_SCHEMA_REFS");
    println!("cargo::rerun-if-changed=build.rs");

    let mut refs: Vec<String> = Vec::new();
    if let Ok(text) = fs::read_to_string(&manifest) {
        let json: serde_json::Value =
            serde_json::from_str(&text).expect("schema-refs.json is JSON");
        if let Some(canonical) = json.get("canonical").and_then(|c| c.as_object()) {
            refs.extend(canonical.keys().cloned());
        }
        refs.sort();
    }

    let out = Path::new(&env::var("OUT_DIR").expect("OUT_DIR")).join("schema_refs.rs");
    let mut body = String::from(
        "/// The manifest's `canonical` keys, or empty when the manifest was not beside the \
         workspace at build time.\npub const CANONICAL_SCHEMA_REFS: &[&str] = &[\n",
    );
    for r in &refs {
        body.push_str(&format!("    {r:?},\n"));
    }
    body.push_str("];\n");
    fs::write(&out, body).expect("write schema_refs.rs");
}
