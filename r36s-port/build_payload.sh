#!/usr/bin/env bash
set -Eeuo pipefail

DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
PROJECT_ROOT="$(cd -- "$DIR/.." && pwd -P)"
AGENT_BIN="$PROJECT_ROOT/r36s-agent/target/aarch64-unknown-linux-musl/release/r36s-agent"
SERVICE_FILE="$PROJECT_ROOT/r36s-agent/systemd/r36s-agent.service"
OUT_ZIP="$DIR/payload.zip"

if [ ! -f "$AGENT_BIN" ]; then
    echo "Compilando r36s-agent para aarch64 musl (binário 100% estático)..."
    (cd "$PROJECT_ROOT/r36s-agent" && cargo build --release --target aarch64-unknown-linux-musl)
fi

TMP_DIR="$(mktemp -d)"
trap 'rm -rf "$TMP_DIR"' EXIT
cp "$AGENT_BIN" "$TMP_DIR/r36s-agent"
chmod 755 "$TMP_DIR/r36s-agent"
cp "$SERVICE_FILE" "$TMP_DIR/r36s-agent.service"
chmod 644 "$TMP_DIR/r36s-agent.service"

rm -f "$OUT_ZIP"
(cd "$TMP_DIR" && zip -q -9 "$OUT_ZIP" r36s-agent r36s-agent.service)
echo "Payload gerado com sucesso em: $OUT_ZIP ($(ls -lh "$OUT_ZIP" | awk '{print $5}'))"
