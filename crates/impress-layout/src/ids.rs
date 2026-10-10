//! Identifiers and the small string-ish newtypes the layout is built from.
//!
//! Every one of these is a plain value with a stable serde form: the layout is
//! the payload of an `impress/ui/layout` item (ADR-0019 D1), so the wire shape
//! here is a compatibility surface, pinned by `tests/golden/three_column.json`.

use std::borrow::Cow;
use std::fmt;

use serde::{Deserialize, Serialize};

/// A tile's identity inside one [`crate::Layout`] arena.
///
/// Sequential `u64` rather than a UUID: the ids are dense (a window has tens of
/// tiles, not millions), they make the golden JSON and every test assertion
/// readable, and [`crate::Layout::next_tile`] makes allocation deterministic —
/// which is what lets a preset serialize byte-identically on every device. Ids
/// are never reused within a layout, so a stale reference in a log is always
/// detectably stale rather than silently pointing at a different pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(transparent)]
pub struct TileId(pub u64);

impl TileId {
    pub const fn new(raw: u64) -> Self {
        Self(raw)
    }

    pub const fn raw(self) -> u64 {
        self.0
    }
}

impl fmt::Display for TileId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<u64> for TileId {
    fn from(raw: u64) -> Self {
        Self(raw)
    }
}

/// A window's identity inside one [`crate::Layout`]. Sequential, like
/// [`TileId`], and for the same reasons.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(transparent)]
pub struct WindowId(pub u64);

impl WindowId {
    pub const fn new(raw: u64) -> Self {
        Self(raw)
    }

    pub const fn raw(self) -> u64 {
        self.0
    }
}

impl fmt::Display for WindowId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<u64> for WindowId {
    fn from(raw: u64) -> Self {
        Self(raw)
    }
}

/// A coordination channel (ADR-0031 D3): eight numbered channels plus the
/// reserved `follow`, meaning "whatever this pane's window says its default
/// channel is" ([`crate::Window::default_channel`]).
///
/// A channel carries one current selection *per record kind*, so a pane
/// publishing a manuscript on channel 2 leaves a `publication` parameter on the
/// same channel untouched.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "kebab-case")]
pub enum ChannelId {
    /// One of the eight numbered channels, `1..=8`.
    Number(u8),
    /// The window's default channel.
    Follow,
}

impl ChannelId {
    /// The lowest numbered channel; the one every preset uses.
    pub const ONE: ChannelId = ChannelId::Number(1);
    /// Channels are numbered `1..=MAX`.
    pub const MAX: u8 = 8;

    /// A numbered channel, clamped into `1..=8`. Out-of-range numbers are
    /// clamped rather than rejected: a channel is a coordination convenience,
    /// and no gesture should fail because a caller said "channel 9".
    pub fn number(n: u8) -> Self {
        ChannelId::Number(n.clamp(1, Self::MAX))
    }

    /// The concrete channel number this id denotes, resolving `Follow` against
    /// a window default (itself resolved, defensively, in case a window default
    /// was persisted as `Follow`).
    pub fn resolve(self, window_default: ChannelId) -> u8 {
        match self {
            ChannelId::Number(n) => n.clamp(1, Self::MAX),
            ChannelId::Follow => match window_default {
                ChannelId::Number(n) => n.clamp(1, Self::MAX),
                ChannelId::Follow => 1,
            },
        }
    }
}

impl Default for ChannelId {
    fn default() -> Self {
        ChannelId::ONE
    }
}

impl fmt::Display for ChannelId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ChannelId::Number(n) => write!(f, "{n}"),
            ChannelId::Follow => write!(f, "follow"),
        }
    }
}

macro_rules! string_newtype {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
        #[cfg_attr(feature = "schema", schemars(transparent))]
        #[serde(transparent)]
        pub struct $name(
            #[cfg_attr(feature = "schema", schemars(with = "String"))] pub Cow<'static, str>,
        );

        impl $name {
            /// Borrow the underlying string.
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl From<&'static str> for $name {
            fn from(s: &'static str) -> Self {
                Self(Cow::Borrowed(s))
            }
        }

        impl From<String> for $name {
            fn from(s: String) -> Self {
                Self(Cow::Owned(s))
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl PartialEq<str> for $name {
            fn eq(&self, other: &str) -> bool {
                self.0 == other
            }
        }
    };
}

string_newtype! {
    /// What a pane holds the role of (ADR-0031 D5). Roles, not slots, are what
    /// the universal chords act on: ⌃⌘S toggles whichever pane carries
    /// [`Role::NAVIGATOR`], ⌘0 the one carrying [`Role::DETAIL`]. Open-ended by
    /// design — the consts below are the ones the shipped presets assign.
    Role
}

impl Role {
    pub const NAVIGATOR: Role = Role(Cow::Borrowed("navigator"));
    pub const LIST: Role = Role(Cow::Borrowed("list"));
    pub const DETAIL: Role = Role(Cow::Borrowed("detail"));
    pub const PREVIEW: Role = Role(Cow::Borrowed("preview"));
    pub const CONSOLE: Role = Role(Cow::Borrowed("console"));
}

string_newtype! {
    /// How a pane is rendered: the key the Swift `ViewKindRegistry` resolves
    /// (ADR-0031 D1). A view kind the platform cannot render becomes a
    /// placeholder that keeps its spec (D4) — see
    /// [`crate::Layout::placeholder_for_missing_view_kinds`].
    ViewKindId
}

/// The view-kind vocabulary is this list and nothing else (review PH-M7,
/// RL-L12). A verb that names a kind outside it (`set-view-kind`,
/// `set-pane`, a split's new pane) is refused with `unknown-view-kind`; a
/// STORED tree that names one still loads and renders a placeholder that
/// keeps its spec (ADR-0031 D4), because a newer build may have written it.
///
/// The Swift hosts register exactly these: the kit `placeholder`, `surface`
/// and `console`, the chassis the rest. `ChassisViewKindsTests` compares the
/// Swift registry with [`ViewKindId::KNOWN`] as the FFI exports it
/// (`layout_vocabulary_json`), so a kind added on one side only fails a test.
impl ViewKindId {
    /// The navigator: the chassis sidebar as a pane.
    pub const OUTLINE: ViewKindId = ViewKindId(Cow::Borrowed("outline"));
    /// Rows of the pane's query.
    pub const LIST: ViewKindId = ViewKindId(Cow::Borrowed("list"));
    /// One record's detail: imbib's Info tab, a manuscript's info.
    pub const INFO: ViewKindId = ViewKindId(Cow::Borrowed("info"));
    /// A publication's PDF, or a manuscript's compiled preview.
    pub const PDF: ViewKindId = ViewKindId(Cow::Borrowed("pdf"));
    /// A publication's notes (imbib's Notes tab).
    pub const NOTES: ViewKindId = ViewKindId(Cow::Borrowed("notes"));
    /// A publication's BibTeX (imbib's BibTeX tab).
    pub const BIBTEX: ViewKindId = ViewKindId(Cow::Borrowed("bibtex"));
    /// A source buffer. Session-bearing (ADR-0031 D6): the spelling is
    /// `source`, which is what the presets and the Swift `ViewKindRegistry`
    /// both say. (`editor` is not a view kind.)
    pub const SOURCE: ViewKindId = ViewKindId(Cow::Borrowed("source"));
    /// A figure's rendered plot.
    pub const PLOT: ViewKindId = ViewKindId(Cow::Borrowed("plot"));
    /// The app's own log.
    pub const CONSOLE: ViewKindId = ViewKindId(Cow::Borrowed("console"));
    /// An agent-authored `impress/ui/surface@1.0.0` document (ADR-0033).
    pub const SURFACE: ViewKindId = ViewKindId(Cow::Borrowed("surface"));
    /// Hosts a legacy `SectionContentView` route unchanged (ADR-0031 D11).
    pub const LEGACY: ViewKindId = ViewKindId(Cow::Borrowed("legacy"));
    pub const PLACEHOLDER: ViewKindId = ViewKindId(Cow::Borrowed("placeholder"));

    /// Every view kind a host renders — the closed vocabulary (see above).
    pub const KNOWN: &'static [ViewKindId] = &[
        ViewKindId::OUTLINE,
        ViewKindId::LIST,
        ViewKindId::INFO,
        ViewKindId::PDF,
        ViewKindId::NOTES,
        ViewKindId::BIBTEX,
        ViewKindId::SOURCE,
        ViewKindId::PLOT,
        ViewKindId::CONSOLE,
        ViewKindId::SURFACE,
        ViewKindId::LEGACY,
        ViewKindId::PLACEHOLDER,
    ];

    /// Is this a kind some host renders?
    pub fn is_known(&self) -> bool {
        Self::KNOWN.contains(self)
    }

    /// The vocabulary as prose, for a refusal: `outline, list, …`.
    pub fn known_list() -> String {
        Self::KNOWN
            .iter()
            .map(ViewKindId::as_str)
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// The view kinds whose panes carry a [`SessionId`] (ADR-0031 D6): the
    /// ones that own state a re-layout must not destroy — an `NSTextView`
    /// and its undo stack. The one list; the Swift `ViewKindRegistry` marks
    /// the same kinds `isSessionBearing`, and the layout gives every pane of
    /// one of these kinds a session of its own
    /// ([`crate::Layout::ensure_sessions`]).
    pub const SESSION_BEARING: &'static [ViewKindId] = &[ViewKindId::SOURCE];

    /// Does a pane of this kind carry a session?
    pub fn is_session_bearing(&self) -> bool {
        Self::SESSION_BEARING.contains(self)
    }
}

/// The `view_state` keys a pane's spec is known to carry — defined once here
/// (review PH-M7). `view_state` itself stays free-form JSON: a view kind may
/// keep anything of its own there. These are the keys more than one party
/// reads, so their spelling is a contract:
///
/// * the **legacy** pane (ADR-0031 D11): the outline writes `section`, `node`
///   and `reason` for a row the query algebra cannot express yet
///   (`impress_layout_service::outline`), and the chassis' legacy pane reads
///   them to show ONE section's route;
/// * the **info** pane's `tab` (plan wave 7 T4): which tab of a record's
///   detail is showing, so an agent's `set-pane` can switch it.
///
/// The FFI exports them (`layout_vocabulary_json`) and a Swift test pins the
/// chassis' spellings to that export.
pub mod view_state {
    /// Legacy pane: the section name (`SidebarSectionType` spelling).
    pub const SECTION: &str = "section";
    /// Legacy pane: the outline node the row was (`OutlineNode` JSON).
    pub const NODE: &str = "node";
    /// Legacy pane: why this row is not a query yet.
    pub const REASON: &str = "reason";
    /// Info pane: which detail tab shows.
    pub const TAB: &str = "tab";
    /// Detail strip order, written by [`crate::Verb::SetTabOrder`] as a JSON
    /// array of view-kind ids. The info pane reads this spelling. It stays
    /// out of [`KNOWN`]: that list is what the checked-in store framework
    /// exports, and a host reaches the order through the verb.
    pub const TAB_ORDER: &str = "tab_order";

    /// Every key the vocabulary export publishes.
    pub const KNOWN: &[&str] = &[SECTION, NODE, REASON, TAB];
}

string_newtype! {
    /// The handle of a session-bearing view kind's session (ADR-0031 D6). The
    /// session lives in a registry *outside* the tree; no layout mutation ever
    /// tears one down, which is why the tree stores only this id.
    SessionId
}

impl SessionId {
    /// A new, unique session handle. Random rather than counted: a session
    /// outlives the layout value that named it (a preset re-application
    /// replaces the whole tree, and a registry on the Swift side still holds
    /// the old id), so a counter restarting with a new tree could hand one
    /// pane's editor to another.
    pub fn fresh() -> Self {
        SessionId::from(format!("session-{}", uuid::Uuid::new_v4()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_follow_resolves_to_the_window_default() {
        assert_eq!(ChannelId::Follow.resolve(ChannelId::Number(3)), 3);
        assert_eq!(ChannelId::Number(2).resolve(ChannelId::Number(3)), 2);
        // A window whose default was persisted as `follow` still resolves.
        assert_eq!(ChannelId::Follow.resolve(ChannelId::Follow), 1);
    }

    #[test]
    fn channel_numbers_are_clamped() {
        assert_eq!(ChannelId::number(0), ChannelId::Number(1));
        assert_eq!(ChannelId::number(99), ChannelId::Number(8));
    }

    #[test]
    fn the_view_kind_vocabulary_is_closed_and_names_every_constant() {
        for kind in [
            "outline",
            "list",
            "info",
            "pdf",
            "notes",
            "bibtex",
            "source",
            "plot",
            "console",
            "surface",
            "legacy",
            "placeholder",
        ] {
            assert!(ViewKindId::from(kind).is_known(), "{kind}");
        }
        assert_eq!(ViewKindId::KNOWN.len(), 12);
        assert!(!ViewKindId::from("editor").is_known());
        assert!(!ViewKindId::from("publications").is_known());
        for kind in ViewKindId::SESSION_BEARING {
            assert!(kind.is_known());
        }
    }

    #[test]
    fn ids_round_trip_through_json() {
        let json = serde_json::to_string(&TileId(7)).unwrap();
        assert_eq!(json, "7");
        assert_eq!(serde_json::from_str::<TileId>(&json).unwrap(), TileId(7));

        let json = serde_json::to_string(&ChannelId::Follow).unwrap();
        assert_eq!(json, "\"follow\"");
        let json = serde_json::to_string(&ChannelId::Number(2)).unwrap();
        assert_eq!(json, "{\"number\":2}");

        let json = serde_json::to_string(&Role::DETAIL).unwrap();
        assert_eq!(json, "\"detail\"");
        assert_eq!(serde_json::from_str::<Role>(&json).unwrap(), Role::DETAIL);
    }
}
