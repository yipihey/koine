//! R3 seed: imprint's current macOS menu chords. Sources are
//! `apps/imprint/Shared/ImprintApp.swift` (app commands),
//! `apps/imprint/Shared/Layout/PaneLayout.swift` (focus-dependent layouts),
//! and the shared `PaneLayoutCommands.swift` / `FindCoordinator.swift` menus.
//! Dialog-local default/cancel keys and TemplateEditorView's local ⌘R are
//! outside the app command layer. `ProjectBuildPanel` repeats the menu's
//! ⌥⌘B Build command; it is one binding, not two competing commands.
//!
//! Imprint's app commands are live across its windows, represented by the
//! app's window scope as imbib's R2 seed does. Layout ordinals are different:
//! a focused chassis window routes ⌃⌘1–9 to the Rust layout tree, while a
//! focused manuscript editor uses its N-th saved editor layout. Both rows
//! are retained under disjoint window scopes, preserving that focus choice.

use super::{Binding, Chord, Scope, Target};

fn command(
    chord: Chord,
    scope: Scope,
    id: &'static str,
    label: &'static str,
    section: &'static str,
) -> Binding {
    Binding {
        chord,
        scope,
        target: Target::Command(id),
        label,
        section,
        chordless: false,
    }
}

pub fn bindings() -> Vec<Binding> {
    let main = Scope::Window("imprint");
    let chassis = Scope::Window("imprint.chassis");
    let editor = Scope::Window("imprint.editor");
    let mut bindings = vec![
        // Shared universal pane buttons, with imprint's own IDs so the
        // shared Swift menu can select the focused app's registry rows.
        command(
            Chord::new("0").cmd(),
            main.clone(),
            "imprint.pane.toggle_detail",
            "Toggle Detail Pane",
            "Imprint — View: panes",
        ),
        command(
            Chord::new("0").opt().cmd(),
            main.clone(),
            "imprint.pane.toggle_list",
            "Toggle Manuscript List",
            "Imprint — View: panes",
        ),
        command(
            Chord::new("s").ctrl().cmd(),
            main.clone(),
            "imprint.pane.toggle_sidebar",
            "Toggle Sidebar",
            "Imprint — View: panes",
        ),
        // File.
        command(
            Chord::new("n").cmd(),
            main.clone(),
            "imprint.file.new_typst",
            "New Typst Manuscript",
            "Imprint — File",
        ),
        command(
            Chord::new("n").opt().cmd(),
            main.clone(),
            "imprint.file.new_latex",
            "New LaTeX Manuscript",
            "Imprint — File",
        ),
        command(
            Chord::new("n").shift().cmd(),
            main.clone(),
            "imprint.file.new_from_template",
            "New Manuscript from Template…",
            "Imprint — File",
        ),
        command(
            Chord::new("l").shift().cmd(),
            main.clone(),
            "imprint.file.open_library",
            "Open Manuscript Library",
            "Imprint — File",
        ),
        command(
            Chord::new("i").shift().cmd(),
            main.clone(),
            "imprint.file.import_library",
            "Import to Manuscript Library…",
            "Imprint — File",
        ),
        // Edit and search.
        command(
            Chord::new("k").shift().cmd(),
            main.clone(),
            "imprint.edit.insert_citation",
            "Insert Citation…",
            "Imprint — Edit",
        ),
        command(
            Chord::new("r").opt().cmd(),
            main.clone(),
            "imprint.edit.papers",
            "Papers…",
            "Imprint — Edit",
        ),
        command(
            Chord::new("m").shift().cmd(),
            main.clone(),
            "imprint.edit.add_comment",
            "Add Comment…",
            "Imprint — Edit",
        ),
        command(
            Chord::new("y").shift().cmd(),
            main.clone(),
            "imprint.edit.symbol_palette",
            "Symbol Palette…",
            "Imprint — Edit",
        ),
        command(
            Chord::new("a").shift().cmd(),
            main.clone(),
            "imprint.edit.ai_assistant",
            "AI Assistant…",
            "Imprint — Edit",
        ),
        command(
            Chord::new("b").opt().cmd(),
            main.clone(),
            "imprint.edit.build_manuscript",
            "Build Manuscript",
            "Imprint — Edit",
        ),
        command(
            Chord::new("return").cmd(),
            main.clone(),
            "imprint.edit.compile_pdf",
            "Compile to PDF",
            "Imprint — Edit",
        ),
        command(
            Chord::new("f").shift().cmd(),
            main.clone(),
            "imprint.edit.search_manuscripts",
            "Search Across Manuscripts…",
            "Imprint — Edit",
        ),
        command(
            Chord::new("f").cmd(),
            main.clone(),
            "imprint.edit.find_in_list",
            "Find in List",
            "Imprint — Edit",
        ),
        // View: the three edit-mode choices and the menu's implicit ⌘Tab
        // (`keyboardShortcut(.tab)` defaults modifiers to `.command`).
        command(
            Chord::new("1").cmd(),
            main.clone(),
            "imprint.view.text_only",
            "Text Only",
            "Imprint — View",
        ),
        command(
            Chord::new("2").cmd(),
            main.clone(),
            "imprint.view.split_view",
            "Split View",
            "Imprint — View",
        ),
        command(
            Chord::new("3").cmd(),
            main.clone(),
            "imprint.view.direct_pdf",
            "Direct PDF",
            "Imprint — View",
        ),
        command(
            Chord::new("tab").cmd(),
            main.clone(),
            "imprint.view.cycle_edit_mode",
            "Cycle Edit Mode",
            "Imprint — View",
        ),
        command(
            Chord::new("\\").cmd(),
            main.clone(),
            "imprint.view.split_editor",
            "Split Editor",
            "Imprint — View",
        ),
        command(
            Chord::new("\\").opt().cmd(),
            main.clone(),
            "imprint.view.split_editor_orientation",
            "Split Editor Orientation",
            "Imprint — View",
        ),
        command(
            Chord::new("p").ctrl().cmd(),
            main.clone(),
            "imprint.view.detach_pdf",
            "Open PDF on Second Display",
            "Imprint — View",
        ),
        command(
            Chord::new("f").opt().cmd(),
            main.clone(),
            "imprint.view.focus_mode",
            "Focus Mode",
            "Imprint — View",
        ),
        command(
            Chord::new(".").cmd(),
            main.clone(),
            "imprint.view.toggle_ai_assistant",
            "Show / Hide AI Assistant",
            "Imprint — View",
        ),
        command(
            Chord::new("k").opt().cmd(),
            main.clone(),
            "imprint.view.toggle_comments",
            "Show / Hide Comments",
            "Imprint — View",
        ),
        command(
            Chord::new("t").opt().cmd(),
            main.clone(),
            "imprint.view.toggle_throughline",
            "Show / Hide Throughline",
            "Imprint — View",
        ),
        command(
            Chord::new("c").shift().cmd(),
            main.clone(),
            "imprint.view.show_console",
            "Show Console",
            "Imprint — View",
        ),
        command(
            Chord::new("p").opt().cmd(),
            main.clone(),
            "imprint.view.show_plots",
            "Show Plots Panel",
            "Imprint — View",
        ),
        command(
            Chord::new("/").cmd(),
            main.clone(),
            "imprint.view.keyboard_shortcuts",
            "Keyboard Shortcuts…",
            "Imprint — View",
        ),
        command(
            Chord::new("d").ctrl().cmd(),
            main.clone(),
            "imprint.view.all_dark_light",
            "All Dark / All Light",
            "Imprint — View: Appearance",
        ),
        // Format, print, Git and document commands.
        command(
            Chord::new("b").cmd(),
            main.clone(),
            "imprint.format.bold",
            "Bold",
            "Imprint — Format",
        ),
        command(
            Chord::new("i").cmd(),
            main.clone(),
            "imprint.format.italic",
            "Italic",
            "Imprint — Format",
        ),
        command(
            Chord::new("1").opt().cmd(),
            main.clone(),
            "imprint.format.heading_1",
            "Heading 1",
            "Imprint — Format",
        ),
        command(
            Chord::new("2").opt().cmd(),
            main.clone(),
            "imprint.format.heading_2",
            "Heading 2",
            "Imprint — Format",
        ),
        command(
            Chord::new("3").opt().cmd(),
            main.clone(),
            "imprint.format.heading_3",
            "Heading 3",
            "Imprint — Format",
        ),
        command(
            Chord::new("p").cmd(),
            main.clone(),
            "imprint.file.print_pdf",
            "Print Compiled PDF…",
            "Imprint — File",
        ),
        command(
            Chord::new("g").opt().cmd(),
            main.clone(),
            "imprint.git.commit",
            "Commit…",
            "Imprint — Git",
        ),
        command(
            Chord::new("p").shift().cmd(),
            main.clone(),
            "imprint.git.push",
            "Push",
            "Imprint — Git",
        ),
        command(
            Chord::new("u").shift().cmd(),
            main.clone(),
            "imprint.git.pull",
            "Pull",
            "Imprint — Git",
        ),
        command(
            Chord::new("e").shift().cmd(),
            main,
            "imprint.document.export_pdf",
            "Export PDF…",
            "Imprint — Document",
        ),
    ];

    // The menu selects exactly one branch by focused window. The chassis
    // uses a tree ordinal; the editor uses its N-th saved PaneLayoutState.
    const DIGITS: [&str; 9] = ["1", "2", "3", "4", "5", "6", "7", "8", "9"];
    const CHASSIS_IDS: [&str; 9] = [
        "imprint.layout.chassis_1",
        "imprint.layout.chassis_2",
        "imprint.layout.chassis_3",
        "imprint.layout.chassis_4",
        "imprint.layout.chassis_5",
        "imprint.layout.chassis_6",
        "imprint.layout.chassis_7",
        "imprint.layout.chassis_8",
        "imprint.layout.chassis_9",
    ];
    const EDITOR_IDS: [&str; 9] = [
        "imprint.layout.editor_1",
        "imprint.layout.editor_2",
        "imprint.layout.editor_3",
        "imprint.layout.editor_4",
        "imprint.layout.editor_5",
        "imprint.layout.editor_6",
        "imprint.layout.editor_7",
        "imprint.layout.editor_8",
        "imprint.layout.editor_9",
    ];
    const CHASSIS_LABELS: [&str; 9] = [
        "Apply Layout 1",
        "Apply Layout 2",
        "Apply Layout 3",
        "Apply Layout 4",
        "Apply Layout 5",
        "Apply Layout 6",
        "Apply Layout 7",
        "Apply Layout 8",
        "Apply Layout 9",
    ];
    const EDITOR_LABELS: [&str; 9] = [
        "Apply Editor Layout 1",
        "Apply Editor Layout 2",
        "Apply Editor Layout 3",
        "Apply Editor Layout 4",
        "Apply Editor Layout 5",
        "Apply Editor Layout 6",
        "Apply Editor Layout 7",
        "Apply Editor Layout 8",
        "Apply Editor Layout 9",
    ];
    for index in 0..9 {
        let chord = Chord::new(DIGITS[index]).ctrl().cmd();
        bindings.push(command(
            chord,
            chassis.clone(),
            CHASSIS_IDS[index],
            CHASSIS_LABELS[index],
            "Imprint — Layouts: chassis",
        ));
        bindings.push(command(
            chord,
            editor.clone(),
            EDITOR_IDS[index],
            EDITOR_LABELS[index],
            "Imprint — Layouts: editor",
        ));
    }
    bindings
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn imprint_targets_are_unique_and_layout_scopes_preserve_focus() {
        let bindings = bindings();
        let mut ids = BTreeSet::new();
        for binding in &bindings {
            assert!(binding.target.id().starts_with("imprint."));
            assert!(
                ids.insert(binding.target.id()),
                "duplicate imprint command id {}",
                binding.target.id()
            );
        }
        assert_eq!(bindings.len(), 60);
        assert_eq!(
            bindings
                .iter()
                .find(|b| b.target.id() == "imprint.view.cycle_edit_mode")
                .unwrap()
                .chord,
            Chord::new("tab").cmd(),
            "SwiftUI's keyboardShortcut(.tab) has implicit command modifiers"
        );
        for digit in 1..=9 {
            let chord = Chord::new(["1", "2", "3", "4", "5", "6", "7", "8", "9"][digit - 1])
                .ctrl()
                .cmd();
            let scoped: BTreeSet<_> = bindings
                .iter()
                .filter(|b| b.chord == chord)
                .map(|b| &b.scope)
                .collect();
            assert_eq!(
                scoped,
                BTreeSet::from([
                    &Scope::Window("imprint.chassis"),
                    &Scope::Window("imprint.editor")
                ])
            );
        }
        assert_eq!(
            bindings
                .iter()
                .filter(|b| b.chord == Chord::new("b").opt().cmd())
                .count(),
            1,
            "the Build menu and panel button are one command"
        );
    }
}
