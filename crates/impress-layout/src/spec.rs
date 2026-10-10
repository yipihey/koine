//! The pane specification (ADR-0031 D1): what a pane shows, how it is
//! rendered, and where each of its parameters gets its value.

use impress_pane_query::{ItemId, PaneQuery, ParamDecl};
use serde::{Deserialize, Serialize};

use crate::ids::{ChannelId, Role, SessionId, ViewKindId};

/// Where a parameter's value comes from (ADR-0031 D3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "source", rename_all = "kebab-case")]
pub enum ParamSource {
    /// Follow a channel: the parameter takes the channel's current selection
    /// for the parameter's declared kind.
    Channel { channel: ChannelId },
    /// Pinned to one item, whatever the channels do.
    Fixed {
        #[cfg_attr(feature = "schema", schemars(with = "String"))]
        item: ItemId,
    },
    /// The view kind's own default. Yields no binding, never an error: a plot
    /// without a colormap pane still renders.
    Default,
}

impl ParamSource {
    /// Follow a numbered channel (clamped into `1..=8`).
    pub fn channel(n: u8) -> Self {
        ParamSource::Channel {
            channel: ChannelId::number(n),
        }
    }

    /// Follow the window's default channel.
    pub fn follow() -> Self {
        ParamSource::Channel {
            channel: ChannelId::Follow,
        }
    }
}

/// One of a pane's parameters: its declaration (from the query algebra) plus
/// where its value comes from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ParamBinding {
    pub decl: ParamDecl,
    pub source: ParamSource,
}

impl ParamBinding {
    /// A required parameter of `kind` named `name`, following `source`.
    pub fn required(name: impl Into<String>, kind: impl Into<String>, source: ParamSource) -> Self {
        Self {
            decl: ParamDecl {
                name: name.into(),
                kind: kind.into(),
                required: true,
            },
            source,
        }
    }

    /// An optional parameter of `kind` named `name`, following `source`.
    pub fn optional(name: impl Into<String>, kind: impl Into<String>, source: ParamSource) -> Self {
        Self {
            decl: ParamDecl {
                name: name.into(),
                kind: kind.into(),
                required: false,
            },
            source,
        }
    }
}

/// A pane: a query, a view kind that renders it, the parameters that fill the
/// query's blanks, the channel the pane publishes its own selection on, and
/// the optional role universal chords act on.
///
/// There is no sidebar type, no list type and no detail type — the difference
/// between them is entirely the value of this struct (ADR-0031 D1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct PaneSpec {
    /// What the pane shows.
    #[serde(default)]
    pub query: PaneQuery,
    /// How it is rendered.
    pub view_kind: ViewKindId,
    /// Opaque state owned by the view kind (scroll offset, expanded nodes,
    /// colormap …). The layout never interprets it.
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    #[cfg_attr(feature = "schema", schemars(with = "serde_json::Value"))]
    pub view_state: serde_json::Value,
    /// The query's parameters and where each value comes from.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub params: Vec<ParamBinding>,
    /// The channel this pane PUBLISHES its selection on.
    #[serde(default)]
    pub channel: ChannelId,
    /// What universal chords act on, if anything.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<Role>,
    /// For session-bearing view kinds (ADR-0031 D6).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<SessionId>,
    /// The share this pane had in its split before `set-collapsed` hid it —
    /// what showing it again restores, exactly (review RL-L13: restoring to
    /// the siblings' average turned a 240 pt sidebar into a column as wide
    /// as the list). Present only while the pane is collapsed; `normalize`
    /// drops it once the pane's share is no longer hidden, however that
    /// happened (a drag, a resize, an undo).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collapsed_share: Option<f32>,
}

impl PaneSpec {
    /// A pane over `query`, rendered by `view_kind`, publishing on channel 1
    /// with no parameters and no role.
    pub fn new(query: PaneQuery, view_kind: ViewKindId) -> Self {
        Self {
            query,
            view_kind,
            view_state: serde_json::Value::Null,
            params: Vec::new(),
            channel: ChannelId::ONE,
            role: None,
            session: None,
            collapsed_share: None,
        }
    }

    pub fn with_role(mut self, role: Role) -> Self {
        self.role = Some(role);
        self
    }

    pub fn with_channel(mut self, channel: ChannelId) -> Self {
        self.channel = channel;
        self
    }

    pub fn with_param(mut self, binding: ParamBinding) -> Self {
        self.params.push(binding);
        self
    }

    pub fn with_session(mut self, session: SessionId) -> Self {
        self.session = Some(session);
        self
    }

    /// The binding for a named parameter, if the pane declares one.
    pub fn param(&self, name: &str) -> Option<&ParamBinding> {
        self.params.iter().find(|b| b.decl.name == name)
    }

    pub(crate) fn param_mut(&mut self, name: &str) -> Option<&mut ParamBinding> {
        self.params.iter_mut().find(|b| b.decl.name == name)
    }
}
