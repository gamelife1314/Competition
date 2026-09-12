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

async fn handle(req: Request<Incoming>) -> Result<Response<Full<Bytes>>, hyper::Error> {
    let body = match Limited::new(req.into_body(), MAX_BODY_BYTES).collect().await {
        Ok(collected) => collected.to_bytes(),
        Err(_) => return Ok(json_response(EMPTY_RESPONSE)),
    };

    let out = brain::respond(&body);
    Ok(json_response(&out))
}

fn json_response(body: &str) -> Response<Full<Bytes>> {
    Response::builder()
        .status(200)
        .header("Content-Type", "application/json; charset=utf-8")
        .body(Full::from(Bytes::from(body.to_owned())))
        .expect("response builder")
}
