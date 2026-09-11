#!/bin/bash
# 《未来战争》参赛入口：bash run.sh <port>
set -e

PORT="${1:?usage: bash run.sh <port>}"
DIR="$(cd "$(dirname "$0")" && pwd)"
BIN="$DIR/target/release/coregeek"

if [ ! -x "$BIN" ]; then
    (cd "$DIR" && cargo build --release --locked)
fi

exec "$BIN" "$PORT"
