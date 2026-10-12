//! `GET/PUT/DELETE /api/blob/<item>/<name>`: bytes kept beside an item.
//!
//! Present when the store declares `blobs` and the host holds the blob view
//! of it (`koine_store::open_with_blobs`: `memory:` and `dir:`); otherwise
//! every method answers 404 and says which of the two is missing.

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use impress_item::{BlobStore, ItemId};
use impress_service_core::refusal::codes;
use serde_json::json;

use super::{json_response, refusal, Shared};

/// The blob view and the parsed item id, or the 404/400 that says why not.
fn target(state: &Shared, item: &str) -> Result<(Arc<dyn BlobStore>, ItemId), Box<Response>> {
    if !state.capabilities.blobs {
        return Err(Box::new(refusal(
            codes::NOT_FOUND,
            format!(
                "the store at {} does not hold blobs (its capabilities say blobs: false)",
                state.store_url
            ),
        )));
    }
    let Some(blobs) = state.blobs.clone() else {
        return Err(Box::new(refusal(
            codes::NOT_FOUND,
            format!(
                "the store at {} declares blobs but was opened through a registered scheme, \
                 which gives the host no blob view; memory: and dir: do",
                state.store_url
            ),
        )));
    };
    let id = item.parse::<ItemId>().map_err(|_| {
        refusal(
            codes::INVALID_ARGUMENT,
            format!("'{item}' is not an item id"),
        )
    })?;
    Ok((blobs, id))
}

/// Store I/O on a blocking thread.
async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> T + Send + 'static,
) -> Result<T, Box<Response>> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|e| Box::new(refusal(codes::INTERNAL, format!("blob task failed: {e}"))))
}

fn store_error(error: impress_item::StoreError) -> Response {
    match error {
        impress_item::StoreError::Validation(message) => refusal(codes::INVALID_ARGUMENT, message),
        missing @ impress_item::StoreError::NotFound(_) => {
            refusal(codes::NOT_FOUND, missing.to_string())
        }
        other => refusal(codes::STORE_ERROR, other.to_string()),
    }
}

pub(super) async fn get(
    State(state): State<Shared>,
    Path((item, name)): Path<(String, String)>,
) -> Response {
    let (blobs, id) = match target(&state, &item) {
        Ok(found) => found,
        Err(response) => return *response,
    };
    let wanted = name.clone();
    let found = match blocking(move || blobs.get_blob(id, &wanted)).await {
        Ok(found) => found,
        Err(response) => return *response,
    };
    match found {
        Ok(Some(bytes)) => (
            StatusCode::OK,
            [(
                header::CONTENT_TYPE,
                HeaderValue::from_static(super::ui::content_type_of(&name)),
            )],
            bytes,
        )
            .into_response(),
        Ok(None) => refusal(codes::NOT_FOUND, format!("item {id} has no blob '{name}'")),
        Err(error) => store_error(error),
    }
}

pub(super) async fn put(
    State(state): State<Shared>,
    Path((item, name)): Path<(String, String)>,
    body: Bytes,
) -> Response {
    let (blobs, id) = match target(&state, &item) {
        Ok(found) => found,
        Err(response) => return *response,
    };
    let length = body.len();
    let wanted = name.clone();
    let put = match blocking(move || blobs.put_blob(id, &wanted, &body)).await {
        Ok(put) => put,
        Err(response) => return *response,
    };
    match put {
        Ok(()) => json_response(
            200,
            &json!({
                "ok": true,
                "wire_version": 1,
                "item": id.to_string(),
                "name": name,
                "bytes": length,
            }),
        ),
        Err(error) => store_error(error),
    }
}

pub(super) async fn delete(
    State(state): State<Shared>,
    Path((item, name)): Path<(String, String)>,
) -> Response {
    let (blobs, id) = match target(&state, &item) {
        Ok(found) => found,
        Err(response) => return *response,
    };
    let wanted = name.clone();
    let deleted = match blocking(move || blobs.delete_blob(id, &wanted)).await {
        Ok(deleted) => deleted,
        Err(response) => return *response,
    };
    match deleted {
        Ok(()) => json_response(
            200,
            &json!({ "ok": true, "wire_version": 1, "item": id.to_string(), "name": name }),
        ),
        Err(error) => store_error(error),
    }
}
