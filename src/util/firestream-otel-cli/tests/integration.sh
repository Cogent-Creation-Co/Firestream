#!/usr/bin/env bash
# End-to-end smoke test for the otel-cli binary.
#
# Exercises every top-level command using only `json+file` and the embedded
# `server` so no external OTLP collector is required. Each section prints its
# name and "ok" on success; the script exits 0 with "ALL OK" if everything
# passes.
#
# Usage:
#   cargo build && bash tests/integration.sh

set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$HERE/.." && pwd)"
BIN="$ROOT/target/debug/otel-cli"

# Build if not present.
if [ ! -x "$BIN" ]; then
    (cd "$ROOT" && cargo build)
fi

TMP="$(mktemp -d)"
cleanup() { rm -rf "$TMP"; }
trap cleanup EXIT

# Pick a free localhost TCP port. Prefer python3 (always present in CI image),
# fall back to a high random port.
free_port() {
    if command -v python3 >/dev/null 2>&1; then
        python3 -c 'import socket;s=socket.socket();s.bind(("127.0.0.1",0));print(s.getsockname()[1]);s.close()'
    else
        echo $((19000 + RANDOM % 1000))
    fi
}

echo "=== span json+file ==="
"$BIN" span --protocol json+file --json-dir "$TMP/spans" \
    --service test --name basic-span --tp-ignore-env
[ "$(find "$TMP/spans" -name 'span.json' | wc -l)" -ge 1 ]
echo "  ok"

echo "=== exec writes child exit code ==="
"$BIN" exec --protocol json+file --json-dir "$TMP/exec" \
    --service test --name exec-task --tp-ignore-env \
    -- sh -c 'echo hi; exit 0' >/dev/null
[ "$(find "$TMP/exec" -name 'span.json' | wc -l)" -ge 1 ]
echo "  ok"

echo "=== exec propagates non-zero exit ==="
set +e
"$BIN" exec --protocol json+file --json-dir "$TMP/exec2" \
    --service test --name fail --tp-ignore-env \
    -- sh -c 'exit 7' >/dev/null
ec=$?
set -e
[ "$ec" -eq 7 ]
echo "  ok"

echo "=== server json receives grpc span ==="
PORT_GRPC="$(free_port)"
"$BIN" server --grpc-addr "127.0.0.1:$PORT_GRPC" --http-addr '' \
    --max-spans 1 json --dir "$TMP/srv-grpc" >/dev/null 2>&1 &
SVR=$!
sleep 0.5
"$BIN" span --endpoint "http://127.0.0.1:$PORT_GRPC" --protocol grpc \
    --service test --name srv-recv-grpc --tp-ignore-env
wait $SVR
[ "$(find "$TMP/srv-grpc" -name 'span.json' | wc -l)" -ge 1 ]
echo "  ok"

echo "=== server json receives http/protobuf span ==="
PORT_HTTP="$(free_port)"
"$BIN" server --grpc-addr '' --http-addr "127.0.0.1:$PORT_HTTP" \
    --max-spans 1 json --dir "$TMP/srv-http" >/dev/null 2>&1 &
SVR=$!
sleep 0.5
"$BIN" span --endpoint "http://127.0.0.1:$PORT_HTTP" --protocol http/protobuf \
    --service test --name srv-recv-http --tp-ignore-env
wait $SVR
[ "$(find "$TMP/srv-http" -name 'span.json' | wc -l)" -ge 1 ]
echo "  ok"

echo "=== status emits JSON ==="
"$BIN" status --tp-ignore-env | python3 -m json.tool > /dev/null
echo "  ok"

echo "=== version + completion ==="
"$BIN" version > /dev/null
"$BIN" completion bash > /dev/null
"$BIN" completion zsh > /dev/null
"$BIN" completion fish > /dev/null
"$BIN" completion powershell > /dev/null
echo "  ok"

echo "ALL OK"
