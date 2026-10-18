//! The item protocol the suite's rows share.
//!
//! [`Item`], [`ItemQuery`], [`OperationSpec`], and the [`ItemStore`] /
//! [`AttributedStore`] traits live here, with the schema-ref constants those
//! rows are named by. `impress-core` keeps `SqliteItemStore` and the SQL
//! compiler, and re-exports this crate, so `impress_core::Item` is this
//! `Item`. Moving the types out is what lets that database stay in
//! impress-apps when koine leaves (`docs/plan-koine-separation.md`, K1b).

pub mod ambient;
pub mod event;
pub mod item;
pub mod kind;
pub mod operation;
pub mod query;
pub mod reference;
pub mod schema;
pub mod store;
pub mod text_search;

pub use ambient::{ambient_batch, set_ambient_batch};
pub use event::*;
pub use item::*;
pub use operation::*;
pub use query::*;
pub use reference::*;
pub use schema::{SchemaRef, UnknownSchemaRef};
pub use store::*;
pub use text_search::{
    searchable_text, text_tokens, ArrivalCursor, TextHit, TEXT_FIELDS, TEXT_SEARCH_CANDIDATES,
};
