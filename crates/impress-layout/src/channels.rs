//! Channel state (ADR-0031 D3): what every numbered channel currently carries.
//!
//! A channel holds **one current selection per record kind**, so publishing a
//! manuscript on channel 2 leaves that channel's `publication` selection
//! untouched. A selection is a list of ids (possibly empty — "nothing is
//! selected" is a value, not an absence), because list panes select ranges.
//!
//! Channel values are ephemeral (ADR-0031 D7, invariant 4): closing the
//! publishing pane leaves the last value in place, and anything worth keeping
//! is committed to a store item by the service, never by this crate.

use std::collections::BTreeMap;

use impress_pane_query::{ItemId, RecordKindId};
use serde::{Deserialize, Serialize};

/// Per channel number (`1..=8`), per record kind, the current selection.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(transparent)]
pub struct ChannelState {
    #[cfg_attr(
        feature = "schema",
        schemars(with = "BTreeMap<String, BTreeMap<String, Vec<String>>>")
    )]
    pub channels: BTreeMap<u8, BTreeMap<RecordKindId, Vec<ItemId>>>,
}

impl ChannelState {
    pub fn new() -> Self {
        Self::default()
    }

    /// The current selection of `kind` on channel `channel`.
    pub fn selection(&self, channel: u8, kind: &str) -> &[ItemId] {
        self.channels
            .get(&channel)
            .and_then(|k| k.get(kind))
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    /// The first id of the current selection of `kind` — what a single-valued
    /// parameter binds to.
    pub fn current(&self, channel: u8, kind: &str) -> Option<ItemId> {
        self.selection(channel, kind).first().copied()
    }

    /// Publish a selection. An empty `ids` is a real value: it records that
    /// nothing of that kind is selected, which is what a detail pane renders
    /// its empty state from.
    pub fn publish(&mut self, channel: u8, kind: impl Into<RecordKindId>, ids: Vec<ItemId>) {
        self.channels
            .entry(channel)
            .or_default()
            .insert(kind.into(), ids);
    }
}
