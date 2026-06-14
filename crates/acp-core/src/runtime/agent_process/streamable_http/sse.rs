use super::super::process_lifecycle::io_other;
use crate::events::SessionConfig;
use crate::mapping::append_runtime_event_log;
use futures::{StreamExt, channel::mpsc as futures_mpsc};
use serde_json::json;

pub(super) fn spawn_streamable_http_sse_consumer(
    response: reqwest::Response,
    incoming_tx: futures_mpsc::UnboundedSender<std::io::Result<String>>,
    log_config: Option<SessionConfig>,
) {
    tokio::spawn(async move {
        let result = consume_streamable_http_sse(response, &incoming_tx).await;
        if let Some(config) = log_config.as_ref() {
            match result {
                Ok(()) => {
                    let _ = append_runtime_event_log(
                        config,
                        "agent/streamable_http_sse_finished",
                        &json!({}),
                    );
                }
                Err(error) => {
                    let _ = append_runtime_event_log(
                        config,
                        "agent/streamable_http_sse_error",
                        &json!({ "error": error.to_string() }),
                    );
                }
            }
        }
    });
}

async fn consume_streamable_http_sse(
    response: reqwest::Response,
    incoming_tx: &futures_mpsc::UnboundedSender<std::io::Result<String>>,
) -> std::io::Result<()> {
    let mut stream = response.bytes_stream();
    let mut buffer = String::new();
    let mut event_data = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(io_other)?;
        buffer.push_str(&String::from_utf8_lossy(&chunk));
        drain_sse_buffer(&mut buffer, &mut event_data, incoming_tx)?;
    }
    if !buffer.is_empty() {
        buffer.push('\n');
        drain_sse_buffer(&mut buffer, &mut event_data, incoming_tx)?;
    }
    flush_sse_event(&mut event_data, incoming_tx)?;
    Ok(())
}

fn drain_sse_buffer(
    buffer: &mut String,
    event_data: &mut Vec<String>,
    incoming_tx: &futures_mpsc::UnboundedSender<std::io::Result<String>>,
) -> std::io::Result<()> {
    while let Some(line_end) = buffer.find('\n') {
        let mut line = buffer[..line_end].to_string();
        buffer.drain(..=line_end);
        if line.ends_with('\r') {
            line.pop();
        }
        if line.is_empty() {
            flush_sse_event(event_data, incoming_tx)?;
        } else if let Some(data) = line.strip_prefix("data:") {
            event_data.push(data.strip_prefix(' ').unwrap_or(data).to_string());
        }
    }
    Ok(())
}

fn flush_sse_event(
    event_data: &mut Vec<String>,
    incoming_tx: &futures_mpsc::UnboundedSender<std::io::Result<String>>,
) -> std::io::Result<()> {
    if event_data.is_empty() {
        return Ok(());
    }
    let payload = event_data.join("\n");
    event_data.clear();
    feed_streamable_http_payload(&payload, incoming_tx)
}

pub(super) fn feed_streamable_http_payload(
    payload: &str,
    incoming_tx: &futures_mpsc::UnboundedSender<std::io::Result<String>>,
) -> std::io::Result<()> {
    let payload = payload.trim();
    if payload.is_empty() || payload == "[DONE]" {
        return Ok(());
    }
    incoming_tx
        .unbounded_send(Ok(payload.to_string()))
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::BrokenPipe, "ACP receiver closed"))
}
