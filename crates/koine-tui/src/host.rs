//! The text host for the shared style file.
//!
//! `show`, `set`, `reset`, and `render` talk to
//! `<workspace>/settings/koine-style.json`. `edit` reads those same words
//! from the terminal, one per line.

use std::path::Path;

use koine::style::StyleBook;
use koine::surface::resolve;
use koine_style_file::{load_book, save_book, StyleFileError};

use crate::TextFrontend;

#[derive(Debug, thiserror::Error)]
pub enum HostError {
    #[error("{0}")]
    File(#[from] StyleFileError),
    #[error("{0}")]
    Style(#[from] koine::style::StyleError),
    #[error(
        "usage: koine-tui show [frontend] | set <frontend> <slot> <value> | reset <frontend> <slot> | render | edit [frontend]"
    )]
    Usage,
}

/// Run one invocation. `edit` is not here: it reads the terminal. The
/// returned text is what the command prints.
pub fn run(args: &[String], workspace: &Path) -> Result<String, HostError> {
    let mut words = args.iter().map(String::as_str);
    match words.next() {
        None | Some("edit") => Err(HostError::Usage),
        Some("show") => {
            let frontend = words.next().unwrap_or("tui");
            show(workspace, frontend)
        }
        Some("set") => {
            let frontend = words.next().ok_or(HostError::Usage)?;
            let slot = words.next().ok_or(HostError::Usage)?;
            let value = remaining(words);
            if value.is_empty() {
                return Err(HostError::Usage);
            }
            set_slot(workspace, frontend, slot, &value)
        }
        Some("reset") => {
            let frontend = words.next().ok_or(HostError::Usage)?;
            let slot = words.next().ok_or(HostError::Usage)?;
            reset_slot(workspace, frontend, slot)
        }
        Some("render") => Ok(render_with_saved_sheet(workspace)?),
        Some(_) => Err(HostError::Usage),
    }
}

/// One line of `edit`: `show`, `set <slot> <value>`, `reset <slot>`, `quit`.
/// The frontend is the one the session was opened on.
pub fn edit_line(
    book: &mut StyleBook,
    frontend: &str,
    line: &str,
) -> Result<Option<String>, HostError> {
    let line = line.trim();
    if line.is_empty() {
        return Ok(None);
    }
    let mut words = line.split_whitespace();
    match words.next() {
        Some("quit") | Some("q") => Ok(Some(String::new())),
        Some("show") => Ok(Some(format_sheet(book, frontend)?)),
        Some("reset") => {
            let slot = words.next().ok_or(HostError::Usage)?;
            book.reset(frontend, slot)?;
            Ok(Some(format!("{frontend} {slot} reset")))
        }
        Some("set") => {
            let slot = words.next().ok_or(HostError::Usage)?;
            let value = after_slot(line, slot);
            if value.is_empty() {
                return Err(HostError::Usage);
            }
            book.set(frontend, slot, value)?;
            Ok(Some(format!("{frontend} {slot} = {value}")))
        }
        _ => Err(HostError::Usage),
    }
}

fn after_slot<'a>(line: &'a str, slot: &str) -> &'a str {
    let rest = line.trim_start_matches("set").trim_start();
    rest.trim_start_matches(slot).trim()
}

pub fn show(workspace: &Path, frontend: &str) -> Result<String, HostError> {
    let book = load_book(workspace)?;
    format_sheet(&book, frontend)
}

pub fn set_slot(
    workspace: &Path,
    frontend: &str,
    slot: &str,
    value: &str,
) -> Result<String, HostError> {
    let mut book = load_book(workspace)?;
    book.set(frontend, slot, value)?;
    save_book(workspace, &book)?;
    Ok(format!("{frontend} {slot} = {value}"))
}

pub fn reset_slot(workspace: &Path, frontend: &str, slot: &str) -> Result<String, HostError> {
    let mut book = load_book(workspace)?;
    book.reset(frontend, slot)?;
    save_book(workspace, &book)?;
    Ok(format!("{frontend} {slot} reset"))
}

pub fn render_with_saved_sheet(workspace: &Path) -> Result<String, HostError> {
    let book = load_book(workspace)?;
    // The binary draws plots; `render` alone keeps one line per node.
    let mut frontend = TextFrontend::new().with_plots(true);
    if let Some(sheet) = book.sheet("tui") {
        for (slot, value) in sheet.values() {
            frontend.set_style(slot, value)?;
        }
    }
    let spec = koine::surface::example_paper_triage();
    let tree = resolve(
        &spec,
        &serde_json::json!({}),
        &serde_json::json!({}),
        &serde_json::json!({}),
    );
    Ok(frontend.render(&tree))
}

fn format_sheet(book: &StyleBook, frontend: &str) -> Result<String, HostError> {
    let sheet = match book.sheet(frontend) {
        Some(sheet) => sheet.clone(),
        None => koine::style::StyleSheet::default_for(frontend)?,
    };
    let mut lines = Vec::new();
    for slot in koine::style::slots(frontend)? {
        lines.push(format!(
            "{} {} {}",
            slot.name,
            slot.kind.name(),
            sheet.value(slot.name)
        ));
    }
    Ok(lines.join("\n"))
}

fn remaining<'a>(words: impl Iterator<Item = &'a str>) -> String {
    let parts: Vec<&str> = words.collect();
    parts.join(" ")
}

/// The interactive editor. `lines` are the commands; each answer is one
/// entry. `quit` ends the session. The book is saved when a line changes it.
pub fn edit_lines(
    workspace: &Path,
    frontend: &str,
    lines: &[&str],
) -> Result<Vec<String>, HostError> {
    let mut book = load_book(workspace)?;
    let mut out = vec![format_sheet(&book, frontend)?];
    for line in lines {
        match edit_line(&mut book, frontend, line)? {
            None => {}
            Some(answer) if answer.is_empty() => break,
            Some(answer) => {
                save_book(workspace, &book)?;
                out.push(answer);
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("koine-tui-host-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn set_writes_the_file_and_render_uses_it() {
        let dir = scratch("set");
        let answer = run(
            &[
                "set".into(),
                "tui".into(),
                "button".into(),
                "[{label}]".into(),
            ],
            &dir,
        )
        .unwrap();
        assert_eq!(answer, "tui button = [{label}]");
        let text = render_with_saved_sheet(&dir).unwrap();
        assert!(text.contains("[Save]") || text.contains("["), "{text}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn edit_lines_reset_a_slot() {
        let dir = scratch("edit");
        let answers = edit_lines(
            &dir,
            "swift",
            &["set button accent", "reset button", "quit"],
        )
        .unwrap();
        assert!(answers.iter().any(|line| line.contains("button = accent")));
        assert!(answers.iter().any(|line| line.contains("button reset")));
        let shown = show(&dir, "swift").unwrap();
        assert!(shown.contains("button color automatic"), "{shown}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_wrong_kind_is_refused_and_the_file_stays() {
        let dir = scratch("refuse");
        assert!(run(
            &["set".into(), "swift".into(), "button".into(), "nope".into()],
            &dir
        )
        .is_err());
        assert!(show(&dir, "swift")
            .unwrap()
            .contains("button color automatic"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
