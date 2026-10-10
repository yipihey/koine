//! One manifest reader for Rust constants and the Swift-facing UniFFI enum.
//! Build scripts that need a generated vocabulary call [`generate`].
//! Generated files stay in that crate's `OUT_DIR`.
//!
//! The manifest is `IMPRESS_SCHEMA_REFS` when the surrounding build sets it
//! (impress-apps points this at its live `schema-refs.json`), and otherwise
//! `schema-refs.json` two directories above the crate. The second path is
//! the snapshot this grammar carries so it builds on its own.

use std::{collections::BTreeSet, env, fs, path::PathBuf};

/// The schema-ref manifest this build should read.
pub fn manifest_path() -> PathBuf {
    if let Some(from_env) = env::var_os("IMPRESS_SCHEMA_REFS") {
        if !from_env.is_empty() {
            return PathBuf::from(from_env);
        }
    }
    PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"))
        .join("../../schema-refs.json")
}

pub fn generate() {
    let manifest = manifest_path();
    println!("cargo::rerun-if-changed={}", manifest.display());
    println!("cargo::rerun-if-env-changed=IMPRESS_SCHEMA_REFS");
    let manifest: serde_json::Value = serde_json::from_slice(
        &fs::read(&manifest).unwrap_or_else(|error| panic!("read {}: {error}", manifest.display())),
    )
    .unwrap_or_else(|error| panic!("parse {}: {error}", manifest.display()));
    let refs = manifest["canonical"]
        .as_object()
        .expect("canonical schema refs");
    let mut constants = String::from("// Generated from schema-refs.json. Do not edit.\n");
    let mut names_source = constants.clone();
    let mut ffi = String::from(
        "// Generated from schema-refs.json. Do not edit.\n\
         /// Canonical store schema names, generated from the suite manifest.\n\
         #[derive(Debug, Clone, Copy, PartialEq, Eq)]\n\
         #[cfg_attr(feature = \"native\", derive(uniffi::Enum))]\n\
         pub enum SchemaRef {\n",
    );
    let mut names = BTreeSet::new();
    let mut entries = Vec::new();
    let mut values: Vec<_> = refs.keys().collect();
    values.sort(); // Independent of serde_json's preserve_order feature.
    for value in values {
        let base = value.split('@').next().unwrap();
        assert!(
            value
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "/@.-_".contains(c)),
            "schema name is not an identifier: {value}"
        );
        let words: Vec<_> = base.split(|c: char| !c.is_ascii_alphanumeric()).collect();
        let constant = words.join("_").to_ascii_uppercase();
        assert!(
            names.insert(constant.clone()),
            "schema constant collision: {value}"
        );
        let variant: String = words
            .iter()
            .map(|word| {
                let mut chars = word.chars();
                chars.next().unwrap().to_ascii_uppercase().to_string() + chars.as_str()
            })
            .collect();
        constants.push_str(&format!(
            "/// Canonical `{value}` schema.\npub const {constant}: SchemaRef = SchemaRef::canonical({value:?});\n"
        ));
        names_source.push_str(&format!("pub const {constant}: &str = {value:?};\n"));
        ffi.push_str(&format!("    {variant},\n"));
        entries.push((constant, variant, value));
    }
    constants.push_str(
        "/// Every canonical schema reference, in lexical order.\npub const ALL: &[SchemaRef] = &[\n",
    );
    for (constant, _, _) in &entries {
        constants.push_str(&format!("    {constant},\n"));
    }
    constants.push_str("];\n");
    ffi.push_str("}\nimpl SchemaRef {\n    pub const ALL: &[Self] = &[\n");
    for (_, variant, _) in &entries {
        ffi.push_str(&format!("        Self::{variant},\n"));
    }
    ffi.push_str(
        "    ];\n    pub fn as_core(self) -> impress_core::schema::SchemaRef {\n        match self {\n",
    );
    for (constant, variant, _) in &entries {
        ffi.push_str(&format!(
            "            Self::{variant} => impress_core::schema::refs::{constant},\n"
        ));
    }
    ffi.push_str("        }\n    }\n}\n");
    let out = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    fs::write(out.join("schema_refs.rs"), constants).unwrap();
    fs::write(out.join("schema_ref_names.rs"), names_source).unwrap();
    fs::write(out.join("schema_refs_ffi.rs"), ffi).unwrap();
}
