//! `koine-tui` — edit the shared style file, or render with it.
//!
//! ```text
//! koine-tui show [frontend]
//! koine-tui set <frontend> <slot> <value>
//! koine-tui reset <frontend> <slot>
//! koine-tui render
//! koine-tui edit [frontend]
//! ```
//!
//! The file is `<workspace>/settings/koine-style.json`. `IMPRESS_WORKSPACE`
//! overrides the suite workspace directory.

use std::io::{self, BufRead, Write};
use std::process::ExitCode;

use koine_style_file::default_workspace;
use koine_tui::host::{self, edit_line};

fn main() -> ExitCode {
    let workspace = default_workspace();
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("edit") || args.is_empty() {
        let frontend = args.get(1).map(String::as_str).unwrap_or("tui");
        return match edit_stdin(&workspace, frontend) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("{error}");
                ExitCode::from(1)
            }
        };
    }
    match host::run(&args, &workspace) {
        Ok(text) => {
            if !text.is_empty() {
                println!("{text}");
            }
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("{error}");
            ExitCode::from(1)
        }
    }
}

fn edit_stdin(workspace: &std::path::Path, frontend: &str) -> Result<(), host::HostError> {
    let mut book = koine_style_file::load_book(workspace)?;
    println!("{}", host::show(workspace, frontend)?);
    println!("set <slot> <value>   reset <slot>   show   quit");
    let stdin = io::stdin();
    let mut stdout = io::stdout();
    for line in stdin.lock().lines() {
        let line = line.map_err(koine_style_file::StyleFileError::Io)?;
        match edit_line(&mut book, frontend, &line)? {
            None => {}
            Some(answer) if answer.is_empty() => break,
            Some(answer) => {
                koine_style_file::save_book(workspace, &book)?;
                println!("{answer}");
                let _ = stdout.flush();
            }
        }
    }
    Ok(())
}
