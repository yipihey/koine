//! `run-import <folder>... [--dry-run] [--force]` writes best-effort run
//! convention records (`run.json`, `params.json`, `outputs.json`, optionally
//! `notes.json`) beside what is already in old run folders. Nothing is moved.
//! Exit 0 when every folder was handled, 1 when one failed, 2 on usage.

use run_convention::import::{import_dir, ImportOptions, Outcome};
use std::process::ExitCode;

fn main() -> ExitCode {
    let mut opts = ImportOptions::default();
    let mut dirs = Vec::new();
    for a in std::env::args().skip(1) {
        match a.as_str() {
            "--dry-run" => opts.dry_run = true,
            "--force" => opts.force = true,
            s if s.starts_with('-') => return usage(),
            _ => dirs.push(a),
        }
    }
    if dirs.is_empty() {
        return usage();
    }
    let single = dirs.len() == 1;
    let mut bad = false;
    for d in &dirs {
        match import_dir(d, &opts) {
            Ok(Outcome::Written(p)) => println!("{d}: wrote {}", p.summary()),
            Ok(Outcome::DryRun(p)) => {
                println!("{d}: would write {}", p.summary());
                if single {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&p.run).unwrap_or_default()
                    );
                }
            }
            Ok(Outcome::Skipped(why)) => println!("{d}: skipped ({why})"),
            Ok(Outcome::Unrecognised) => {
                println!("{d}: unrecognised (no manifest.toml, command.sh or metadata.toml)")
            }
            Err(e) => {
                eprintln!("{d}: error: {e}");
                bad = true;
            }
        }
    }
    if bad {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

fn usage() -> ExitCode {
    eprintln!("usage: run-import <folder>... [--dry-run] [--force]");
    ExitCode::from(2)
}
