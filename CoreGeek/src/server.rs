use std::net::TcpListener;

use http_body_util::{BodyExt, Full, Limited};
use hyper::body::{Bytes, Incoming};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response};
use hyper_util::rt::TokioIo;

use crate::brain;

const EMPTY_RESPONSE: &str = r#"{"roleCommandMap":{},"prompt":"","executeCmd":""}"#;

pub async fn serve(listener: TcpListener) {
    let listener = tokio::net::TcpListener::from_std(listener).expect("tokio listener");
    loop {
        match listener.accept().await {
            Ok((stream, _peer)) => {
                let io = TokioIo::new(stream);
                tokio::task::spawn_local(async move {
                    // The judger speaks plain HTTP/1.1; errors on a single
                    // connection must never take the process down.
                    let _ = http1::Builder::new()
                        .serve_connection(io, service_fn(handle))
                        .await;
                });
            }
            Err(err) => {
                eprintln!("accept error: {err}");
            }
        }
    }
}

/// Hard cap on request body size: a legitimate round payload is a few KiB;
/// anything larger is abusive and must not be buffered (5s budget / memory).
const MAX_BODY_BYTES: usize = 8 * 1024 * 1024;

/// The most characters of one request or response the traffic log prints whole.
///
/// The traffic log is one round's worth of lines per round and a match is 2600
/// rounds, so a cap is what keeps it a log rather than a transcript. It is set
/// an order of magnitude above anything this game produces — a 41×32 board, a
/// few hundred map cells, a dozen roles — so **a normal round is always printed
/// complete**. The owner's complaint was that the old 500-character slice cut
/// off exactly the two fields worth reading (`prompt` and `executeCmd`, which
/// `Response` serialises last); a body that does reach this cap says so on the
/// line, with its real length, instead of trailing off into silence.
pub const TRAFFIC_CAP: usize = 128 * 1024;

/// The `[REQ …]` line for one round: the platform's request in full.
///
/// Prints the WHOLE body — the news text and the model's `llmResp` ride at the
/// end of it, and a slice that stops before them is a log that answers nothing.
/// See [`TRAFFIC_CAP`] for the one bound that remains, and [`capped`] for how a
/// body past it declares itself.
///
/// Separate from [`response_lines`] because the request is printed BEFORE the
/// decision runs: if a round ever hangs, the input is the half worth having.
pub fn request_line(request: &[u8]) -> String {
    match std::str::from_utf8(request) {
        Ok(text) => format!("[REQ {} bytes] {}", text.len(), capped(&flatten(text))),
        Err(_) => format!("[REQ {} bytes] <binary>", request.len()),
    }
}

/// The `[RESP …]` lines for one round: the whole response, then each of the two
/// fields the round is actually about on a line of its own.
///
/// The response is one JSON object and its key order is `roleCommandMap`,
/// `prompt`, `executeCmd` — so a reader who wants the prompt has to get past the
/// whole command map first, and a slice cuts off precisely the tail. Printing
/// the blob AND the fields repeats `prompt`/`executeCmd` and nothing else; the
/// `roleCommandMap` — the bulky, least interesting third — still appears once.
///
/// Kept as a pure function so a test can drive a real round through
/// `brain::decide_with` and assert what the log would say: stdout belongs to the
/// process that writes it and cannot be captured from inside it.
pub fn response_lines(response: &str) -> Vec<String> {
    let mut lines = vec![format!(
        "[RESP {} bytes] {}",
        response.len(),
        capped(&flatten(response))
    )];
    let Ok(value) = serde_json::from_str::<serde_json::Value>(response) else {
        return lines;
    };
    for field in ["prompt", "executeCmd"] {
        let Some(text) = value.get(field).and_then(|value| value.as_str()) else {
            continue;
        };
        if !text.is_empty() {
            lines.push(field_line(field, text));
        }
    }
    lines
}

/// `[RESP prompt 1180 chars] "…"` — one response field, whole, under its own
/// greppable prefix.
///
/// Printed as a JSON string so the prompt's own line breaks stay escaped. The
/// log is read line by line and a record that spans thirty lines is a record no
/// grep finds, while `\n` is exactly how the prompt spells a line break anyway.
fn field_line(field: &str, text: &str) -> String {
    format!(
        "[RESP {field} {} chars] {}",
        text.chars().count(),
        capped(&serde_json::Value::String(text.to_owned()).to_string())
    )
}

/// A body as one line: the literal newlines, carriage returns and tabs that
/// separate JSON tokens become spaces.
///
/// A whitespace character *inside* a JSON string is always escaped (`\n`, two
/// bytes), so a raw newline in a valid body is structure and nothing else —
/// this cannot touch a value, and the pretty-printed bodies the platform
/// sometimes sends stop each costing a line per token.
fn flatten(text: &str) -> String {
    text.replace(['\n', '\r', '\t'], " ")
}

/// The text whole, or its first [`TRAFFIC_CAP`] characters and a marker naming
/// exactly what was left out — a truncation the reader can see is not a
/// truncation that reads as a fact about the round.
fn capped(text: &str) -> String {
    let total = text.chars().count();
    if total <= TRAFFIC_CAP {
        return text.to_owned();
    }
    format!(
        "{}…[TRUNCATED: first {TRAFFIC_CAP} of {total} chars]",
        text.chars().take(TRAFFIC_CAP).collect::<String>()
    )
}

async fn handle(req: Request<Incoming>) -> Result<Response<Full<Bytes>>, hyper::Error> {
    let body = match Limited::new(req.into_body(), MAX_BODY_BYTES)
        .collect()
        .await
    {
        Ok(collected) => collected.to_bytes(),
        Err(_) => return Ok(json_response(EMPTY_RESPONSE)),
    };

    // Print the platform request and our response to stdout so the user can
    // see task processing results in real time.
    println!("{}", request_line(&body));
    let out = brain::respond(&body);
    for line in response_lines(&out) {
        println!("{line}");
    }
    Ok(json_response(&out))
}

fn json_response(body: &str) -> Response<Full<Bytes>> {
    Response::builder()
        .status(200)
        .header("Content-Type", "application/json; charset=utf-8")
        .body(Full::from(Bytes::from(body.to_owned())))
        .expect("response builder")
}
