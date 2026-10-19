//! `run-check <dir>...` validates run directories against the run convention.
//! `run-check --print-key <dir>` prints the key the directory's run.json should
//! hold. Exit 0 when every directory conforms, 1 on any finding, 2 on usage.

use serde_json::Value;
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("--print-key") && args.len() == 2 {
        return print_key(&args[1]);
    }
    if args.is_empty() || args.iter().any(|a| a.starts_with('-')) {
        eprintln!("usage: run-check <run-dir>...\n       run-check --print-key <run-dir>");
        return ExitCode::from(2);
    }
    let mut bad = false;
    for a in &args {
        let report = run_convention::check_dir(a);
        println!("{report}");
        bad |= !report.ok();
    }
    if bad {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

fn print_key(dir: &str) -> ExitCode {
    let read = |f: &str| -> Option<Value> {
        let t = std::fs::read_to_string(std::path::Path::new(dir).join(f)).ok()?;
        serde_json::from_str(&t).ok()
    };
    match (read("run.json"), read("params.json")) {
        (Some(run), Some(params)) => {
            let problem = run.get("problem").cloned().unwrap_or(Value::Null);
            println!(
                "{}",
                run_convention::key_of(&problem, &run_convention::values_of(&params))
            );
            ExitCode::SUCCESS
        }
        _ => {
            eprintln!("cannot read run.json and params.json in {dir}");
            ExitCode::from(1)
        }
    }
}
