//! Compile real downstream fixtures: these errors must reach rustc callers.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

struct FixtureTarget(PathBuf);

impl Drop for FixtureTarget {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn current_proc_macro_library(target_dir: &Path) -> PathBuf {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
    let output = Command::new(env!("CARGO"))
        .arg("build")
        .arg("--manifest-path")
        .arg(manifest)
        .arg("--target-dir")
        .arg(target_dir)
        .arg("--message-format=json")
        .output()
        .expect("Cargo builds the current proc macro for downstream fixtures");
    assert!(
        output.status.success(),
        "fixture proc-macro build failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let artifacts: Vec<PathBuf> = String::from_utf8(output.stdout)
        .expect("Cargo JSON is UTF-8")
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter(|message| {
            message["reason"] == "compiler-artifact"
                && message["target"]["name"] == "impress_service_macros"
                && message["target"]["kind"]
                    .as_array()
                    .is_some_and(|kinds| kinds.iter().any(|kind| kind == "proc-macro"))
        })
        .flat_map(|message| message["filenames"].as_array().cloned().unwrap_or_default())
        .filter_map(|filename| filename.as_str().map(PathBuf::from))
        .filter(|path| {
            matches!(
                path.extension().and_then(|ext| ext.to_str()),
                Some("dylib" | "so" | "dll")
            )
        })
        .collect();
    assert_eq!(
        artifacts.len(),
        1,
        "Cargo must report exactly one current proc-macro library: {artifacts:?}"
    );
    artifacts.into_iter().next().unwrap()
}

#[test]
fn missing_construction_declarations_fail_in_downstream_crates() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock after Unix epoch")
        .as_nanos();
    let target_dir = FixtureTarget(std::env::temp_dir().join(format!(
        "impress-service-macros-fixtures-{}-{nonce}",
        std::process::id()
    )));
    let macro_library = current_proc_macro_library(&target_dir.0);
    let deps = target_dir.0.join("debug/deps");
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/ui");
    for (fixture, expected) in [
        ("missing_method_doc.rs", "UndocumentedService::run"),
        ("missing_safety.rs", "missing `safety ="),
        ("missing_since.rs", "missing `since ="),
        ("invalid_example_tier.rs", "`tier` must be `a` or `b`"),
    ] {
        let source = fixtures.join(fixture);
        let result = Command::new(std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into()))
            .arg("--edition=2021")
            .arg("--crate-type=lib")
            .arg("--emit=metadata")
            .arg("-L")
            .arg(format!("dependency={}", deps.display()))
            .arg("--extern")
            .arg(format!(
                "impress_service_macros={}",
                macro_library.display()
            ))
            .arg(&source)
            .output()
            .expect("rustc runs for fixture");
        let stderr = String::from_utf8_lossy(&result.stderr);
        assert!(!result.status.success(), "{fixture} unexpectedly compiled");
        assert!(
            stderr.contains(expected),
            "{fixture} missed its diagnostic:\n{stderr}"
        );
    }
}
