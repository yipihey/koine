//! Seed data (R2): imbib's current chords, collected with
//! `grep -rn "keyboardShortcut(" apps/imbib packages/ --include="*.swift"` and
//! read against `apps/imbib/imbib/imbib/imbibApp.swift`,
//! `apps/imbib/imbib/imbib/ContentView.swift`,
//! `.../Chassis/Shared/{PaneLayoutCommands,FindCoordinator}.swift` and
//! `.../Chassis/Windows/DetachedViews.swift`. Sheet-local `.cancelAction` /
//! `.defaultAction` shortcuts are not app chords and are not seeded.
//!
//! Scope model: `Scope::Window("imbib")` is imbib's one macOS main window —
//! both `imbibApp.swift`'s `Commands` (app-wide, but this app has one
//! window) and `ContentView.swift`'s in-window buttons live there, so a
//! chord bound in both really does collide. `Scope::Window("imbib.detached-
//! content")` is `DetachedViews.swift`'s Notes-detached and BibTeX-detached
//! windows — two separate scenes, but only one is ever open+focused at a
//! time and both bind the same ⌘S "Save" to the same effect on that window's
//! own content, so they are seeded as one row in one scope rather than two
//! rows that would otherwise look like an unresolved duplicate. Neither
//! collides with the main window.
//!
//! Where imbib's own sites disagree (RG-4: ⌘5/⌘6, ⇧⌘F ×3, ⌘S ×2), the
//! decision and its source are recorded per binding below, per
//! `docs/keyboard-grammar.md`.

use super::{Binding, Chord, Scope, Target};

pub fn bindings() -> Vec<Binding> {
    let main = Scope::Window("imbib");
    let detached = Scope::Window("imbib.detached-content");

    vec![
        // ── Universal chords (docs/keyboard-grammar.md § Universal chords),
        // imbib's pre-chassis window (`PaneLayoutChordTarget
        // .imbibPreChassisWindow`, `Chassis/Shared/PaneLayoutCommands.swift`).
        Binding {
            chord: Chord::new("0").cmd(),
            scope: main.clone(),
            target: Target::Command("imbib.pane.toggle_detail"),
            label: "Toggle Detail Pane",
            section: "View — panes",
            chordless: false,
        },
        Binding {
            chord: Chord::new("0").opt().cmd(),
            scope: main.clone(),
            target: Target::Command("imbib.pane.toggle_list"),
            label: "Toggle List",
            section: "View — panes",
            chordless: false,
        },
        Binding {
            // Decision (docs/keyboard-grammar.md "⌃⌘S is Toggle Sidebar,
            // only", 2026-09-24): this chord is the sidebar toggle in EVERY
            // app and nothing else. Paper ▸ Save to Library carried it too
            // from cdca0b23 until 2026-09-24 and is deliberately chordless
            // below.
            chord: Chord::new("s").ctrl().cmd(),
            scope: main.clone(),
            target: Target::Command("imbib.pane.toggle_sidebar"),
            label: "Toggle Sidebar",
            section: "View — panes",
            chordless: false,
        },
        // ── View ▸ Layouts (⌃⌘1–9): imbib's own window keeps its own saved
        // arrangements rather than the chassis tree's ordinals (imbibApp.swift:935).
        Binding {
            chord: Chord::new("1").ctrl().cmd(),
            scope: main.clone(),
            target: Target::Command("imbib.layout.apply_1"),
            label: "Apply Saved Layout 1",
            section: "View — Layouts",
            chordless: false,
        },
        // File menu
        Binding {
            chord: Chord::new("i").cmd(),
            scope: main.clone(),
            target: Target::Command("imbib.file.import"),
            label: "Import...",
            section: "File",
            chordless: false,
        },
        Binding {
            chord: Chord::new("e").shift().cmd(),
            scope: main.clone(),
            target: Target::Command("imbib.file.export"),
            label: "Export...",
            section: "File",
            chordless: false,
        },
        // Edit menu
        Binding {
            chord: Chord::new("c").cmd(),
            scope: main.clone(),
            target: Target::Command("imbib.edit.copy"),
            label: "Copy",
            section: "Edit",
            chordless: false,
        },
        Binding {
            chord: Chord::new("c").shift().cmd(),
            scope: main.clone(),
            target: Target::Command("imbib.edit.copy_as_citation"),
            label: "Copy as Citation",
            section: "Edit",
            chordless: false,
        },
        Binding {
            chord: Chord::new("c").opt().cmd(),
            scope: main.clone(),
            target: Target::Command("imbib.edit.copy_identifier"),
            label: "Copy DOI/URL",
            section: "Edit",
            chordless: false,
        },
        Binding {
            chord: Chord::new("x").cmd(),
            scope: main.clone(),
            target: Target::Command("imbib.edit.cut"),
            label: "Cut",
            section: "Edit",
            chordless: false,
        },
        Binding {
            chord: Chord::new("v").cmd(),
            scope: main.clone(),
            target: Target::Command("imbib.edit.paste"),
            label: "Paste",
            section: "Edit",
            chordless: false,
        },
        Binding {
            chord: Chord::new("a").cmd(),
            scope: main.clone(),
            target: Target::Command("imbib.edit.select_all"),
            label: "Select All",
            section: "Edit",
            chordless: false,
        },
        Binding {
            // Decision (RG-4, "⌘S ×2"): `imbibApp.swift`'s Edit ▸ Find ▸
            // "Smart Search (AI)..." owns plain ⌘S in the main window.
            // `DetachedViews.swift`'s two ⌘S "Save" bindings are in the
            // separate `imbib.detached-content` scope below and do not collide.
            chord: Chord::new("s").cmd(),
            scope: main.clone(),
            target: Target::Command("imbib.edit.smart_search"),
            label: "Smart Search (AI)...",
            section: "Edit — Find",
            chordless: false,
        },
        Binding {
            chord: Chord::new("z").opt().cmd(),
            scope: main.clone(),
            target: Target::Command("imbib.edit.undo_history"),
            label: "Undo History",
            section: "Edit",
            chordless: false,
        },
        // View menu
        Binding {
            chord: Chord::new("p").shift().cmd(),
            scope: main.clone(),
            target: Target::Command("imbib.view.command_palette"),
            label: "Command Palette...",
            section: "View",
            chordless: false,
        },
        Binding {
            chord: Chord::new("1").cmd(),
            scope: main.clone(),
            target: Target::Command("imbib.view.show_library"),
            label: "Show Library",
            section: "View — tabs",
            chordless: false,
        },
        Binding {
            chord: Chord::new("2").cmd(),
            scope: main.clone(),
            target: Target::Command("imbib.view.show_search"),
            label: "Show Search",
            section: "View — tabs",
            chordless: false,
        },
        Binding {
            chord: Chord::new("3").cmd(),
            scope: main.clone(),
            target: Target::Command("imbib.view.show_inbox"),
            label: "Show Inbox",
            section: "View — tabs",
            chordless: false,
        },
        Binding {
            chord: Chord::new("4").cmd(),
            scope: main.clone(),
            target: Target::Command("imbib.view.show_pdf_tab"),
            label: "Show PDF Tab",
            section: "View — tabs",
            chordless: false,
        },
        Binding {
            // Decision (RG-4, "⌘5/⌘6 swapped"): the doc's fix
            // (KeyboardShortcutsSettings.retiredDefaults) already landed in
            // this file — ⌘5 is Notes, ⌘6 is BibTeX, matching View ▸.
            chord: Chord::new("5").cmd(),
            scope: main.clone(),
            target: Target::Command("imbib.view.show_notes_tab"),
            label: "Show Notes Tab",
            section: "View — tabs",
            chordless: false,
        },
        Binding {
            chord: Chord::new("6").cmd(),
            scope: main.clone(),
            target: Target::Command("imbib.view.show_bibtex_tab"),
            label: "Show BibTeX Tab",
            section: "View — tabs",
            chordless: false,
        },
        Binding {
            chord: Chord::new("p").ctrl().cmd(),
            scope: main.clone(),
            target: Target::Command("imbib.view.detach_pdf"),
            label: "Open PDF on Second Display",
            section: "View",
            chordless: false,
        },
        Binding {
            chord: Chord::new("d").ctrl().cmd(),
            scope: main.clone(),
            target: Target::Command("imbib.view.all_dark_light"),
            label: "All Dark / All Light",
            section: "View — Appearance",
            chordless: false,
        },
        Binding {
            chord: Chord::new("1").opt().cmd(),
            scope: main.clone(),
            target: Target::Command("imbib.view.focus_sidebar"),
            label: "Focus Sidebar",
            section: "View",
            chordless: false,
        },
        Binding {
            chord: Chord::new("2").opt().cmd(),
            scope: main.clone(),
            target: Target::Command("imbib.view.focus_list"),
            label: "Focus List",
            section: "View",
            chordless: false,
        },
        Binding {
            chord: Chord::new("3").opt().cmd(),
            scope: main.clone(),
            target: Target::Command("imbib.view.focus_detail"),
            label: "Focus Detail",
            section: "View",
            chordless: false,
        },
        Binding {
            chord: Chord::new("c").ctrl().cmd(),
            scope: main.clone(),
            target: Target::Command("imbib.view.show_console"),
            label: "Show Console",
            section: "View",
            chordless: false,
        },
        Binding {
            chord: Chord::new("=").shift().cmd(),
            scope: main.clone(),
            target: Target::Command("imbib.view.increase_text_size"),
            label: "Increase Text Size",
            section: "View",
            chordless: false,
        },
        Binding {
            chord: Chord::new("-").shift().cmd(),
            scope: main.clone(),
            target: Target::Command("imbib.view.decrease_text_size"),
            label: "Decrease Text Size",
            section: "View",
            chordless: false,
        },
        // Paper menu
        Binding {
            chord: Chord::new("return"),
            scope: main.clone(),
            target: Target::Command("imbib.paper.open_pdf"),
            label: "Open PDF",
            section: "Paper",
            chordless: false,
        },
        Binding {
            chord: Chord::new("r").cmd(),
            scope: main.clone(),
            target: Target::Command("imbib.paper.open_notes"),
            label: "Open Notes",
            section: "Paper",
            chordless: false,
        },
        Binding {
            chord: Chord::new("r").shift().cmd(),
            scope: main.clone(),
            target: Target::Command("imbib.paper.open_references"),
            label: "Open References",
            section: "Paper",
            chordless: false,
        },
        Binding {
            chord: Chord::new("u").shift().cmd(),
            scope: main.clone(),
            target: Target::Command("imbib.paper.toggle_read"),
            label: "Toggle Read/Unread",
            section: "Paper",
            chordless: false,
        },
        Binding {
            chord: Chord::new("u").opt().cmd(),
            scope: main.clone(),
            target: Target::Command("imbib.paper.mark_all_read"),
            label: "Mark All as Read",
            section: "Paper",
            chordless: false,
        },
        Binding {
            // Decision (docs/keyboard-grammar.md "⌃⌘S is Toggle Sidebar,
            // only"): Save to Library lost its ⌃⌘S in 2026-09-24 and has no
            // chord at all — the keyboard save path is the inbox list's own
            // guarded ⏎ / `s` keys, not this menu item. Chordless and a
            // palette command per the coverage test's third rule.
            chord: Chord::new(""),
            scope: main.clone(),
            target: Target::Command("imbib.paper.save_to_library"),
            label: "Save to Library",
            section: "Paper",
            chordless: true,
        },
        Binding {
            chord: Chord::new("j").shift().cmd(),
            scope: main.clone(),
            target: Target::Command("imbib.paper.dismiss_from_inbox"),
            label: "Dismiss from Inbox",
            section: "Paper",
            chordless: false,
        },
        Binding {
            chord: Chord::new("m").ctrl().cmd(),
            scope: main.clone(),
            target: Target::Command("imbib.paper.move_to_collection"),
            label: "Move to Collection...",
            section: "Paper",
            chordless: false,
        },
        Binding {
            chord: Chord::new("l").cmd(),
            scope: main.clone(),
            target: Target::Command("imbib.paper.add_to_collection"),
            label: "Add to Collection...",
            section: "Paper",
            chordless: false,
        },
        Binding {
            chord: Chord::new("l").shift().cmd(),
            scope: main.clone(),
            target: Target::Command("imbib.paper.remove_from_collection"),
            label: "Remove from Collection",
            section: "Paper",
            chordless: false,
        },
        Binding {
            // Decision (RG-4, "⇧⌘F ×3"): Paper ▸ Share... is the winner
            // (docs/keyboard-grammar.md "the last nine (2026-09-25)").
            // `ContentView.swift`'s hidden "Filter" button and
            // `FindCoordinator.swift`'s "Find" both also claim ⇧⌘F in this
            // same window scope and are superseded — dead until one of them
            // is moved to a free chord, which is a product decision this
            // registry does not make silently. Recorded here rather than
            // seeded twice so the coverage test's duplicate-chord check
            // stays meaningful.
            chord: Chord::new("f").shift().cmd(),
            scope: main.clone(),
            target: Target::Command("imbib.paper.share"),
            label: "Share...",
            section: "Paper",
            chordless: false,
        },
        Binding {
            chord: Chord::new("e").ctrl().cmd(),
            scope: main.clone(),
            target: Target::Command("imbib.paper.mirror_to_remarkable"),
            label: "Mirror to reMarkable",
            section: "Paper — reMarkable",
            chordless: false,
        },
        Binding {
            chord: Chord::new("delete").cmd(),
            scope: main.clone(),
            target: Target::Command("imbib.paper.delete"),
            label: "Delete",
            section: "Paper",
            chordless: false,
        },
        // Annotate menu
        Binding {
            chord: Chord::new("h").ctrl(),
            scope: main.clone(),
            target: Target::Command("imbib.annotate.highlight"),
            label: "Highlight Selection",
            section: "Annotate",
            chordless: false,
        },
        Binding {
            chord: Chord::new("u").ctrl(),
            scope: main.clone(),
            target: Target::Command("imbib.annotate.underline"),
            label: "Underline Selection",
            section: "Annotate",
            chordless: false,
        },
        Binding {
            chord: Chord::new("t").ctrl(),
            scope: main.clone(),
            target: Target::Command("imbib.annotate.strikethrough"),
            label: "Strikethrough Selection",
            section: "Annotate",
            chordless: false,
        },
        Binding {
            chord: Chord::new("n").ctrl(),
            scope: main.clone(),
            target: Target::Command("imbib.annotate.add_note"),
            label: "Add Note at Selection",
            section: "Annotate",
            chordless: false,
        },
        // Go menu
        Binding {
            chord: Chord::new("[").cmd(),
            scope: main.clone(),
            target: Target::Command("imbib.go.back"),
            label: "Back",
            section: "Go",
            chordless: false,
        },
        Binding {
            chord: Chord::new("]").cmd(),
            scope: main.clone(),
            target: Target::Command("imbib.go.forward"),
            label: "Forward",
            section: "Go",
            chordless: false,
        },
        Binding {
            chord: Chord::new("up").cmd(),
            scope: main.clone(),
            target: Target::Command("imbib.go.first_paper"),
            label: "First Paper",
            section: "Go",
            chordless: false,
        },
        Binding {
            chord: Chord::new("down").cmd(),
            scope: main.clone(),
            target: Target::Command("imbib.go.last_paper"),
            label: "Last Paper",
            section: "Go",
            chordless: false,
        },
        Binding {
            chord: Chord::new("down").opt(),
            scope: main.clone(),
            target: Target::Command("imbib.go.next_unread"),
            label: "Next Unread",
            section: "Go",
            chordless: false,
        },
        Binding {
            chord: Chord::new("up").opt(),
            scope: main.clone(),
            target: Target::Command("imbib.go.previous_unread"),
            label: "Previous Unread",
            section: "Go",
            chordless: false,
        },
        Binding {
            chord: Chord::new("g").cmd(),
            scope: main.clone(),
            target: Target::Command("imbib.go.to_page"),
            label: "Go to Page...",
            section: "Go",
            chordless: false,
        },
        // Window menu additions
        Binding {
            chord: Chord::new("n").shift().cmd(),
            scope: main.clone(),
            target: Target::Command("imbib.window.refresh"),
            label: "Refresh",
            section: "Window",
            chordless: false,
        },
        Binding {
            chord: Chord::new("\\").cmd(),
            scope: main.clone(),
            target: Target::Command("imbib.window.toggle_unread_filter"),
            label: "Toggle Unread Filter",
            section: "Window",
            chordless: false,
        },
        Binding {
            chord: Chord::new("\\").shift().cmd(),
            scope: main.clone(),
            target: Target::Command("imbib.window.toggle_pdf_filter"),
            label: "Toggle PDF Filter",
            section: "Window",
            chordless: false,
        },
        Binding {
            chord: Chord::new("p").shift(),
            scope: main.clone(),
            target: Target::Command("imbib.window.open_pdf_fullscreen"),
            label: "Open PDF in Fullscreen",
            section: "Window",
            chordless: false,
        },
        Binding {
            chord: Chord::new("n").shift(),
            scope: main.clone(),
            target: Target::Command("imbib.window.open_notes_fullscreen"),
            label: "Open Notes in Fullscreen",
            section: "Window",
            chordless: false,
        },
        Binding {
            chord: Chord::new("i").shift(),
            scope: main.clone(),
            target: Target::Command("imbib.window.open_info_fullscreen"),
            label: "Open Info in Fullscreen",
            section: "Window",
            chordless: false,
        },
        Binding {
            chord: Chord::new("b").shift(),
            scope: main.clone(),
            target: Target::Command("imbib.window.open_bibtex_fullscreen"),
            label: "Open BibTeX in Fullscreen",
            section: "Window",
            chordless: false,
        },
        Binding {
            chord: Chord::new("f").shift(),
            scope: main.clone(),
            target: Target::Command("imbib.window.flip_positions"),
            label: "Flip Window Positions",
            section: "Window",
            chordless: false,
        },
        Binding {
            chord: Chord::new("w").shift().opt().cmd(),
            scope: main.clone(),
            target: Target::Command("imbib.window.close_detached"),
            label: "Close Detached Windows",
            section: "Window",
            chordless: false,
        },
        // Help menu
        Binding {
            chord: Chord::new("?").cmd(),
            scope: main.clone(),
            target: Target::Command("imbib.help.open"),
            label: "imbib Help",
            section: "Help",
            chordless: false,
        },
        Binding {
            chord: Chord::new("?").shift().cmd(),
            scope: main.clone(),
            target: Target::Command("imbib.help.search"),
            label: "Search Help...",
            section: "Help",
            chordless: false,
        },
        Binding {
            chord: Chord::new("/").cmd(),
            scope: main.clone(),
            target: Target::Command("imbib.help.keyboard_shortcuts"),
            label: "Keyboard Shortcuts",
            section: "Help",
            chordless: false,
        },
        // The Notes-detached and BibTeX-detached windows
        // (`Chassis/Windows/DetachedViews.swift`) — their own scope, so their
        // two ⌘S "Save" bindings do not collide with the main window's ⌘S
        // (Smart Search).
        Binding {
            chord: Chord::new("s").cmd(),
            scope: detached.clone(),
            target: Target::Command("imbib.detached.save"),
            label: "Save",
            section: "Detached content window (Notes / BibTeX)",
            chordless: false,
        },
    ]
}
