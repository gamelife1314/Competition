use std::net::{SocketAddr, TcpListener};

fn main() {
    let port: u16 = std::env::args()
        .nth(1)
        .and_then(|arg| arg.parse().ok())
        .unwrap_or_else(|| {
            eprintln!("Usage: coregeek <port>");
            std::process::exit(2);
        });

    // Bind synchronously before entering the runtime so the listening socket
    // is up as early as possible (judger treats >10s connect as timeout).
    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    let listener = TcpListener::bind(addr).unwrap_or_else(|err| {
        eprintln!("failed to bind {addr}: {err}");
        std::process::exit(1);
    });
    listener
        .set_nonblocking(true)
        .expect("failed to set listener non-blocking");

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("failed to build tokio runtime");

    println!("listening on 0.0.0.0:{port}");
    // Startup log record (JSONL to stdout) for post-match diagnostics.
    coregeek::log::event(
        "startup",
        serde_json::json!({"port": port, "version": env!("CARGO_PKG_VERSION")}),
    );
    let local_set = tokio::task::LocalSet::new();
    rt.block_on(local_set.run_until(coregeek::server::serve(listener)));
}
