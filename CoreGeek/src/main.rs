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

    println!("CoreGeek v{} listening on 0.0.0.0:{port}", env!("CARGO_PKG_VERSION"));
    // Startup log record (JSONL to stdout) for post-match diagnostics.
    coregeek::log::event(
        "startup",
        serde_json::json!({"port": port, "version": env!("CARGO_PKG_VERSION")}),
    );
    // Build metadata: rustc version + opt-level, useful for reproducing a
    // specific bot build from match logs.
    coregeek::log::event(
        "build_info",
        serde_json::json!({
            "rustc": option_env!("RUSTC_VERSION").unwrap_or("unknown"),
            "profile": "release",
            "edition": "2021",
            "opt_level": 3,
            "lto": true,
            "codegen_units": 1,
        }),
    );
    // Adaptive coach: load what the previous battle taught us (best effort —
    // a missing or unreadable file is simply a fresh coach) and let this battle
    // write its own half-time summary back. See `brain::coach`.
    coregeek::brain::coach::install();
    // One guard, one statement: `BotState::locked()` hands out a `MutexGuard` and
    // the global mutex is not reentrant, so a second `locked()` inside this same
    // expression would deadlock the process before it ever serves a request.
    let ready = coregeek::state::BotState::locked().coach.ready_json();
    coregeek::log::event("coach_ready", ready);
    let local_set = tokio::task::LocalSet::new();
    rt.block_on(local_set.run_until(coregeek::server::serve(listener)));
}
