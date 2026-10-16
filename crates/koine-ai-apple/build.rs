//! Builds the Swift package in `swift/` (macOS only) and links it.
//!
//! The package is a static library, `KoineFM`, built into `OUT_DIR`. Linking
//! it into a Rust binary needs, beyond the archive itself:
//!
//! * the Swift runtime: `/usr/lib/swift` (the OS copy, with an rpath so the
//!   dynamic `libswiftCore` and `libswift_Concurrency` resolve at run time)
//!   and the toolchain's `lib/swift/macosx`, which holds the static
//!   compatibility shims the Swift objects ask for with autolink entries
//!   (`swiftCompatibility56`, `swiftCompatibilityConcurrency`, ...). The
//!   objects name their own libraries, so no `-lswiftCore` is needed here;
//! * `Foundation` and `FoundationModels` frameworks.
//!
//! The workspace sets `MACOSX_DEPLOYMENT_TARGET=14.0` for the Rust side; it is
//! removed for `swift build`, whose manifest says macOS 26. The linker prints
//! a "built for newer macOS" note for that mismatch; it is harmless here
//! because `available()` checks the running system before any call.
//! A binary that links this crate does not start on a macOS before 26.

use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
        return;
    }
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let package = manifest.join("swift");
    println!(
        "cargo:rerun-if-changed={}",
        package.join("Package.swift").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        package.join("Sources").display()
    );
    let scratch = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("swift-build");

    let swift = |extra: &[&str]| {
        let mut command = Command::new("swift");
        command
            .args(["build", "-c", "release", "--package-path"])
            .arg(&package)
            .args(["--product", "KoineFM", "--scratch-path"])
            .arg(&scratch)
            .args(extra)
            .env_remove("MACOSX_DEPLOYMENT_TARGET");
        command
    };
    let status = swift(&[]).status().expect("could not run `swift build`");
    assert!(
        status.success(),
        "`swift build` failed for {}",
        package.display()
    );
    // The build system picks the directory (`<scratch>/release` or
    // `<scratch>/out/Products/Release`): ask it.
    let bin = swift(&["--show-bin-path"])
        .output()
        .expect("swift --show-bin-path");
    let bin = String::from_utf8(bin.stdout).unwrap().trim().to_string();
    assert!(
        Path::new(&bin).join("libKoineFM.a").exists(),
        "libKoineFM.a not in {bin}"
    );
    println!("cargo:rustc-link-search=native={bin}");
    println!("cargo:rustc-link-lib=static=KoineFM");

    println!("cargo:rustc-link-search=native=/usr/lib/swift");
    if let Ok(found) = Command::new("xcrun").args(["--find", "swift"]).output() {
        let swift_bin = PathBuf::from(String::from_utf8_lossy(&found.stdout).trim());
        if let Some(lib) = swift_bin
            .parent()
            .map(|bin| bin.join("../lib/swift/macosx"))
        {
            println!("cargo:rustc-link-search=native={}", lib.display());
        }
    }
    println!("cargo:rustc-link-arg=-Wl,-rpath,/usr/lib/swift");
    println!("cargo:rustc-link-lib=framework=Foundation");
    println!("cargo:rustc-link-lib=framework=FoundationModels");
}
