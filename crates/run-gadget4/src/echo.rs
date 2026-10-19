//! Reading GADGET-4's own statements of its parameters: a parameter file, the
//! `parameters-usedvalues` it writes, or the startup echo in its stdout.
//!
//! The reader mirrors `parameters::read_parameter_file` in
//! `src/io/parameters.cc`: a line is skipped unless `sscanf("%s%s%s")` finds two
//! tokens, a first token starting with `%` or `#` is a comment, and the value is
//! the second token (anything after it is a comment).

use std::collections::BTreeSet;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Echo {
    /// `(tag, value text)` in file order, duplicates kept.
    pub params: Vec<(String, String)>,
    /// `This is Gadget, version 4.0.`
    pub version: Option<String>,
    /// The 40-hex commit from the `Git commit` banner line.
    pub git_commit: Option<String>,
    /// `Code was compiled with the following settings`: `NAME` or `NAME=VALUE`.
    pub config: Vec<String>,
    /// Tags that appear more than once (GADGET-4 refuses these).
    pub duplicates: Vec<String>,
    /// The text was a startup echo (had the `Obtaining parameters` block).
    pub from_startup_echo: bool,
}

/// One `Tag value` line, or `None` for a blank, comment or one-token line.
pub fn parse_line(line: &str) -> Option<(String, String)> {
    let mut it = line.split_whitespace();
    let tag = it.next()?;
    let value = it.next()?;
    if tag.starts_with('%') || tag.starts_with('#') {
        return None;
    }
    Some((tag.to_string(), value.to_string()))
}

pub fn parse(text: &str) -> Echo {
    let mut e = Echo::default();
    let lines: Vec<&str> = text.lines().collect();

    for (i, l) in lines.iter().enumerate() {
        let t = l.trim();
        if let Some(v) = t.strip_prefix("This is Gadget, version ") {
            e.version = Some(v.trim_end_matches('.').trim().to_string());
        } else if let Some(rest) = t.strip_prefix("Git commit ") {
            let hash = rest.split(',').next().unwrap_or("").trim();
            if hash.len() == 40 && hash.bytes().all(|b| b.is_ascii_hexdigit()) {
                e.git_commit = Some(hash.to_ascii_lowercase());
            }
        } else if t.starts_with("Code was compiled with the following settings") {
            for c in lines.iter().skip(i + 1) {
                let c = c.trim();
                if c.is_empty() {
                    break;
                }
                e.config.push(c.to_string());
            }
        }
    }

    let start = lines
        .iter()
        .position(|l| l.trim_start().starts_with("Obtaining parameters from file"));
    let block: Vec<&str> = match start {
        Some(s) => {
            e.from_startup_echo = true;
            let mut b = Vec::new();
            let mut seen = false;
            for l in lines.iter().skip(s + 1) {
                if l.trim().is_empty() {
                    if seen {
                        break;
                    }
                    continue;
                }
                seen = true;
                b.push(*l);
            }
            b
        }
        None => lines,
    };

    let mut seen = BTreeSet::new();
    let mut dup = BTreeSet::new();
    for l in block {
        if let Some((k, v)) = parse_line(l) {
            if !seen.insert(k.clone()) {
                dup.insert(k.clone());
            }
            e.params.push((k, v));
        }
    }
    e.duplicates = dup.into_iter().collect();
    e
}
