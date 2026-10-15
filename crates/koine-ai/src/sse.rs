//! Server-sent-events forwarding shared by every streaming backend.
//!
//! The transport loop (byte chunks → frames → parsed events → channel) is
//! identical for OpenAI-compatible hosts and Anthropic; only the per-frame
//! parser differs, so the loop lives here once.

use futures_util::StreamExt;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

use crate::provider::EventStream;
use crate::types::StreamEvent;
use crate::{Error, Result};

/// One SSE frame: the optional `event:` name and the joined `data:` lines.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SseFrame {
    pub event: Option<String>,
    pub data: String,
}

/// What a backend parser made of one frame.
#[derive(Debug, Default)]
pub struct SseOutcome {
    pub events: Vec<StreamEvent>,
    /// The frame ended the stream (`[DONE]`, `message_stop`); nothing after
    /// it is read.
    pub terminal: bool,
}

impl SseOutcome {
    pub fn events(events: Vec<StreamEvent>) -> Self {
        Self {
            events,
            terminal: false,
        }
    }

    pub fn terminal(events: Vec<StreamEvent>) -> Self {
        Self {
            events,
            terminal: true,
        }
    }
}

/// Spawn the forwarding task for `response` and hand back the event stream.
///
/// `transport_error` classifies a broken connection for the provider (the
/// caller knows which provider it is; this module does not). `parse` sees
/// every complete frame. A `Done` event is appended when the stream ends
/// without a terminal frame so consumers always observe a terminator.
pub fn spawn_sse_stream<P, T>(
    response: reqwest::Response,
    transport_error: T,
    parse: P,
) -> EventStream
where
    P: Fn(&SseFrame) -> Result<SseOutcome> + Send + Sync + 'static,
    T: Fn(String) -> Error + Send + Sync + 'static,
{
    let (sender, receiver) = mpsc::channel(64);
    tokio::spawn(forward_sse(response, sender, transport_error, parse));
    ReceiverStream::new(receiver)
}

pub async fn forward_sse<P, T>(
    response: reqwest::Response,
    sender: mpsc::Sender<Result<StreamEvent>>,
    transport_error: T,
    parse: P,
) where
    P: Fn(&SseFrame) -> Result<SseOutcome> + Sync,
    T: Fn(String) -> Error + Sync,
{
    let mut upstream = response.bytes_stream();
    let mut pending = String::new();
    let mut frame = SseFrame::default();
    let mut has_data = false;
    let mut terminal_sent = false;

    'outer: while let Some(chunk) = upstream.next().await {
        let chunk = match chunk {
            Ok(chunk) => chunk,
            Err(error) => {
                let _ = sender.send(Err(transport_error(error.to_string()))).await;
                return;
            }
        };
        pending.push_str(&String::from_utf8_lossy(&chunk));
        while let Some(newline) = pending.find('\n') {
            let line = pending[..newline].trim_end_matches('\r').to_string();
            pending.drain(..=newline);
            if line.is_empty() {
                if has_data {
                    let complete = std::mem::take(&mut frame);
                    has_data = false;
                    match dispatch(&complete, &sender, &parse).await {
                        Dispatch::Continue => {}
                        Dispatch::Terminal => {
                            terminal_sent = true;
                            break 'outer;
                        }
                        Dispatch::Closed => return,
                    }
                }
                continue;
            }
            if line.starts_with(':') {
                continue;
            }
            if let Some(event) = line.strip_prefix("event:") {
                frame.event = Some(event.trim().to_string());
            } else if let Some(data) = line.strip_prefix("data:") {
                if has_data {
                    frame.data.push('\n');
                }
                frame.data.push_str(data.strip_prefix(' ').unwrap_or(data));
                has_data = true;
            }
        }
    }

    if !terminal_sent && has_data {
        // Some hosts close the connection right after the last frame without
        // the trailing blank line.
        match dispatch(&frame, &sender, &parse).await {
            Dispatch::Continue => {}
            Dispatch::Terminal => terminal_sent = true,
            Dispatch::Closed => return,
        }
    }
    if !terminal_sent {
        let _ = sender
            .send(Ok(StreamEvent::Done {
                finish_reason: None,
            }))
            .await;
    }
}

enum Dispatch {
    Continue,
    Terminal,
    Closed,
}

async fn dispatch<P>(
    frame: &SseFrame,
    sender: &mpsc::Sender<Result<StreamEvent>>,
    parse: &P,
) -> Dispatch
where
    P: Fn(&SseFrame) -> Result<SseOutcome>,
{
    match parse(frame) {
        Ok(outcome) => {
            for event in outcome.events {
                if sender.send(Ok(event)).await.is_err() {
                    return Dispatch::Closed;
                }
            }
            if outcome.terminal {
                Dispatch::Terminal
            } else {
                Dispatch::Continue
            }
        }
        Err(error) => {
            let _ = sender.send(Err(error)).await;
            Dispatch::Closed
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frames_from(body: &'static str) -> Vec<SseFrame> {
        // Drive the frame splitter through a mock HTTP response so the test
        // exercises the same path production does.
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async move {
            let mut server = mockito::Server::new_async().await;
            let _mock = server
                .mock("GET", "/events")
                .with_status(200)
                .with_header("content-type", "text/event-stream")
                .with_body(body)
                .create_async()
                .await;
            let response = reqwest::get(format!("{}/events", server.url()))
                .await
                .unwrap();
            let seen = std::sync::Arc::new(std::sync::Mutex::new(vec![]));
            let record = seen.clone();
            let mut stream = spawn_sse_stream(
                response,
                |message| Error::Provider {
                    provider: "test".into(),
                    status: None,
                    message,
                },
                move |frame| {
                    record.lock().unwrap().push(frame.clone());
                    Ok(if frame.data == "[DONE]" {
                        SseOutcome::terminal(vec![])
                    } else {
                        SseOutcome::events(vec![StreamEvent::Token {
                            text: frame.data.clone(),
                        }])
                    })
                },
            );
            while stream.next().await.is_some() {}
            let frames = seen.lock().unwrap().clone();
            frames
        })
    }

    #[test]
    fn splits_frames_on_blank_lines_and_tracks_event_names() {
        let frames = frames_from(
            ": keep-alive\nevent: message_start\ndata: {\"a\":1}\n\ndata: line one\ndata: line two\n\ndata: [DONE]\n\ndata: ignored after done\n\n",
        );
        assert_eq!(frames.len(), 3);
        assert_eq!(frames[0].event.as_deref(), Some("message_start"));
        assert_eq!(frames[0].data, r#"{"a":1}"#);
        assert_eq!(frames[1].event, None);
        assert_eq!(frames[1].data, "line one\nline two");
        assert_eq!(frames[2].data, "[DONE]");
    }

    #[test]
    fn flushes_a_trailing_frame_without_a_blank_line() {
        let frames = frames_from("data: only\n");
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].data, "only");
    }
}
