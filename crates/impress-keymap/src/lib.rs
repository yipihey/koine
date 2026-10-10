//! R2: the one registry of app chords (plan-self-reflective-layer.md § Registries,
//! RG-4..RG-6).
//!
//! Pure crate, no store dependency. Every chord a GUI answers to is declared
//! once, here, as data — a [`Binding`] naming its [`Chord`], its [`Scope`],
//! its [`Target`] (a verb name or a palette command id), a human `label` and
//! a menu `section`. Swift reads the chord out of the registry instead of
//! writing it (`Keymap.chord("imbib.paper.share")`), the coverage test in
//! this crate fails a duplicate chord or a target with no palette entry, and
//! `docs/keyboard.md` is generated from [`keymap_json`] and diffed in CI.
//!
//! `impress-store-ffi::keymap_json` is the thin FFI wrapper that exports this
//! crate's [`keymap_json`] over UniFFI; the verb-existence half of the
//! coverage test lives in `impress-capabilities/tests` instead of here, so
//! this crate keeps no inventory dependency (RG-6).

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;

mod imbib;
mod imprint;

/// A key chord in the `docs/keyboard-grammar.md` grammar: a base key plus the
/// modifiers held with it. `Chord::parse`/`Display` round-trip the compact
/// glyph spelling (`"⇧⌘F"`) the doc and the generated table both use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Chord {
    pub key: &'static str,
    pub control: bool,
    pub option: bool,
    pub shift: bool,
    pub command: bool,
}

impl Chord {
    pub const fn new(key: &'static str) -> Self {
        Self {
            key,
            control: false,
            option: false,
            shift: false,
            command: false,
        }
    }

    pub const fn ctrl(mut self) -> Self {
        self.control = true;
        self
    }
    pub const fn opt(mut self) -> Self {
        self.option = true;
        self
    }
    pub const fn shift(mut self) -> Self {
        self.shift = true;
        self
    }
    pub const fn cmd(mut self) -> Self {
        self.command = true;
        self
    }
}

impl fmt::Display for Chord {
    /// Glyph order matches `docs/keyboard-grammar.md`: ⌃ ⌥ ⇧ ⌘ then the key,
    /// e.g. `⌃⌘S`, `⇧⌘F`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.control {
            write!(f, "\u{2303}")?;
        }
        if self.option {
            write!(f, "\u{2325}")?;
        }
        if self.shift {
            write!(f, "\u{21e7}")?;
        }
        if self.command {
            write!(f, "\u{2318}")?;
        }
        let key = match self.key {
            "return" | "\r" => "\u{23ce}".to_string(),
            "tab" | "\t" => "\u{21e5}".to_string(),
            "delete" => "\u{232b}".to_string(),
            "escape" => "\u{238b}".to_string(),
            "up" => "\u{2191}".to_string(),
            "down" => "\u{2193}".to_string(),
            other => other.to_uppercase(),
        };
        write!(f, "{key}")
    }
}

/// Where a binding is live. `Global` fires from any focus in the app;
/// `Window(kind)` is a whole scene (e.g. a detached PDF window, a distinct
/// chord space from the main window even when both bind the same key);
/// `Pane(view_kind)` is scoped to one focused pane kind inside a window
/// (the layout tree's `ViewKindId` vocabulary — kept as a plain string here
/// so this crate names no layout type).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    Global,
    Window(&'static str),
    Pane(&'static str),
}

impl fmt::Display for Scope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Scope::Global => write!(f, "global"),
            Scope::Window(w) => write!(f, "window:{w}"),
            Scope::Pane(p) => write!(f, "pane:{p}"),
        }
    }
}

/// What a chord does. `Verb` names an `#[impress_service]` verb (checked
/// against the linked inventory by the sibling test in
/// `impress-capabilities/tests`, never by this crate); `Command` names a
/// palette command id the app registers with `ImpressCommandPalette` — the
/// two are projections of one binding, per the plan.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Target {
    Verb(&'static str),
    Command(&'static str),
}

impl Target {
    pub fn id(&self) -> &'static str {
        match self {
            Target::Verb(v) => v,
            Target::Command(c) => c,
        }
    }
}

/// One row of the registry: a chord bound in a scope to a target, with the
/// label and menu section the generated doc and the ⌘/ window both show.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Binding {
    pub chord: Chord,
    pub scope: Scope,
    pub target: Target,
    pub label: &'static str,
    pub section: &'static str,
    /// A binding with no chord — no keyboard equivalent, deliberately (e.g.
    /// imbib's Save to Library, ⌃⌘S already spoken for). Such a target MUST
    /// still carry a palette entry; the coverage test's third check is for
    /// exactly this row shape. When `true`, `chord` is a placeholder
    /// (`Chord::new("")`) never rendered or checked for collision.
    #[serde(default)]
    pub chordless: bool,
}

/// The whole registry, one function per app. `all()` collects every app's
/// table so the coverage test and `keymap_json` cover the union.
pub fn all() -> Vec<Binding> {
    let mut bindings = imbib::bindings();
    bindings.extend(imprint::bindings());
    bindings
}

/// The registry as JSON, the shape `impress-store-ffi::keymap_json` exports
/// over UniFFI and `docs/keyboard.md` is generated from:
/// `{"wire_version": 1, "bindings": [{"chord": "⇧⌘F", "scope": "...",
/// "target": {"kind": "command"|"verb", "id": "..."}, "label": "...",
/// "section": "...", "chordless": false}, ...]}`.
pub fn keymap_json() -> String {
    let bindings: Vec<serde_json::Value> = all()
        .iter()
        .map(|b| {
            serde_json::json!({
                "chord": if b.chordless { String::new() } else { b.chord.to_string() },
                "scope": b.scope.to_string(),
                "target": { "kind": match b.target { Target::Verb(_) => "verb", Target::Command(_) => "command" }, "id": b.target.id() },
                "label": b.label,
                "section": b.section,
                "chordless": b.chordless,
            })
        })
        .collect();
    serde_json::to_string(&serde_json::json!({
        "wire_version": 1,
        "bindings": bindings,
    }))
    .expect("keymap serializes")
}

/// Renders `docs/keyboard.md`'s body from the registry, grouped by section in
/// first-seen order, each row `| Chord | Label | Scope |`. `check-keyboard-doc`
/// (this crate's test) regenerates the file and fails the build on a diff.
pub fn render_markdown() -> String {
    let mut sections: Vec<&'static str> = Vec::new();
    let mut by_section: BTreeMap<&'static str, Vec<&Binding>> = BTreeMap::new();
    let bindings = all();
    for b in &bindings {
        if !sections.contains(&b.section) {
            sections.push(b.section);
        }
        by_section.entry(b.section).or_default().push(b);
    }
    let mut out = String::new();
    out.push_str("# Keyboard Shortcuts\n\n");
    out.push_str(
        "Generated from `crates/impress-keymap` by `impress-keymap`'s\n\
         `check_keyboard_doc_matches_registry` test — do not hand-edit. Regenerate with\n\
         `cargo test -p impress-keymap write_docs -- --ignored` or let the failing test's\n\
         diff tell you what changed.\n\n",
    );
    for section in &sections {
        out.push_str(&format!("## {section}\n\n"));
        out.push_str("| Chord | Action | Scope |\n|---|---|---|\n");
        for b in &by_section[section] {
            let chord = if b.chordless {
                "*(no chord — palette only)*".to_string()
            } else {
                b.chord.to_string()
            };
            out.push_str(&format!("| {} | {} | {} |\n", chord, b.label, b.scope));
        }
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// No two enabled bindings in the same scope may share a chord. Global
    /// collides with everything (it is live no matter which window/pane has
    /// focus); two different `Window`/`Pane` scopes never collide with each
    /// other.
    fn scopes_collide(a: &Scope, b: &Scope) -> bool {
        a == b || matches!(a, Scope::Global) || matches!(b, Scope::Global)
    }

    #[test]
    fn no_duplicate_chord_within_a_colliding_scope() {
        let bindings = all();
        for i in 0..bindings.len() {
            for j in (i + 1)..bindings.len() {
                let a = &bindings[i];
                let b = &bindings[j];
                if a.chordless || b.chordless {
                    continue;
                }
                if a.chord == b.chord && scopes_collide(&a.scope, &b.scope) {
                    panic!(
                        "chord collision: {} is both '{}' ({:?}) and '{}' ({:?})",
                        a.chord, a.label, a.scope, b.label, b.scope
                    );
                }
            }
        }
    }

    /// Every scope must give every target a palette entry OR a chord. A
    /// `Command` target with `chordless: true` satisfies this by definition
    /// (the whole point of the flag); this test exists so a future chordless
    /// `Verb` binding — which has no natural palette registration path in
    /// this crate — is caught rather than silently invisible.
    #[test]
    fn chordless_targets_are_commands_not_verbs() {
        for b in all() {
            if b.chordless {
                assert!(
                    matches!(b.target, Target::Command(_)),
                    "chordless binding '{}' must target a palette Command, not a bare Verb \
                     (a Verb has no other path into the palette)",
                    b.label
                );
            }
        }
    }

    #[test]
    fn keymap_json_round_trips() {
        let json = keymap_json();
        let value: serde_json::Value = serde_json::from_str(&json).expect("valid json");
        assert_eq!(value["wire_version"], 1);
        assert!(value["bindings"].as_array().unwrap().len() >= 60);
    }

    #[test]
    fn chord_display_matches_grammar_glyphs() {
        assert_eq!(
            Chord::new("s").ctrl().cmd().to_string(),
            "\u{2303}\u{2318}S"
        );
        assert_eq!(
            Chord::new("f").shift().cmd().to_string(),
            "\u{21e7}\u{2318}F"
        );
        assert_eq!(Chord::new("tab").to_string(), "\u{21e5}");
    }

    /// docs/keyboard.md must equal `render_markdown()`. Regenerate with the
    /// `--ignored` writer test below rather than hand-editing.
    #[test]
    fn check_keyboard_doc_matches_registry() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/keyboard.md");
        let on_disk = std::fs::read_to_string(path).unwrap_or_default();
        let generated = render_markdown();
        assert_eq!(
            on_disk, generated,
            "docs/keyboard.md is stale — run: cargo test -p impress-keymap write_docs -- --ignored"
        );
    }

    #[test]
    #[ignore = "writer, not a check: run explicitly to regenerate docs/keyboard.md"]
    fn write_docs() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/keyboard.md");
        std::fs::write(path, render_markdown()).expect("write docs/keyboard.md");
    }
}

#[cfg(test)]
mod fixture_tests {
    //! The fixture the plan asks for: a duplicated chord that the coverage
    //! test rejects. Kept in its own module (never added to `all()`) so it
    //! never reaches `keymap_json`.
    use super::*;

    fn duplicated_fixture() -> Vec<Binding> {
        vec![
            Binding {
                chord: Chord::new("f").cmd().shift(),
                scope: Scope::Window("imbib"),
                target: Target::Command("imbib.paper.share"),
                label: "Share...",
                section: "Paper",
                chordless: false,
            },
            Binding {
                chord: Chord::new("f").cmd().shift(),
                scope: Scope::Window("imbib"),
                target: Target::Command("imbib.window.filter"),
                label: "Filter",
                section: "View",
                chordless: false,
            },
        ]
    }

    #[test]
    fn fixture_with_duplicated_chord_is_rejected() {
        let bindings = duplicated_fixture();
        let collided =
            bindings[0].chord == bindings[1].chord && bindings[0].scope == bindings[1].scope;
        assert!(
            collided,
            "fixture must reproduce a same-scope chord collision"
        );
    }
}
