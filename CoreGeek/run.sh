#!/bin/bash
# 《未来战争》参赛入口：bash run.sh <port>
set -e

PORT="${1:?usage: bash run.sh <port>}"
DIR="$(cd "$(dirname "$0")" && pwd)"
BIN="$DIR/target/release/coregeek"

if [ ! -x "$BIN" ]; then
    # `--target-dir` pins the output. The crate is a member of the workspace
    # declared at the repository root (so that the review harness's bare
    # `cargo test --release --locked`, run from the root, finds a manifest at
    # all), and a workspace member otherwise builds into the ROOT's `target/`,
    # leaving this script — and the README — pointing at nothing. A crate-only
    # copy has no workspace above it and lands in the very same directory.
    (cd "$DIR" && cargo build --release --locked --target-dir "$DIR/target")
fi

exec "$BIN" "$PORT"
