#!/usr/bin/env bash
set -Eeuo pipefail

DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
PROJECT_ROOT="$(cd -- "$DIR/.." && pwd -P)"
AGENT_BIN="$PROJECT_ROOT/r36s-agent/target/aarch64-unknown-linux-musl/release/r36s-agent"
SERVICE_FILE="$PROJECT_ROOT/r36s-agent/systemd/r36s-agent.service"
OUT_ZIP="$DIR/payload.zip"

# Localiza cargo no PATH ou no diretório .cargo do usuário
if [ -n "${SUDO_USER:-}" ]; then
    USER_CARGO="$(getent passwd "$SUDO_USER" | cut -d: -f6)/.cargo/bin"
    [ -d "$USER_CARGO" ] && export PATH="$USER_CARGO:$PATH"
fi
[ -d "$HOME/.cargo/bin" ] && export PATH="$HOME/.cargo/bin:$PATH"

if [ ! -f "$AGENT_BIN" ]; then
    echo "Compilando r36s-agent para aarch64 musl (binário 100% estático)..."
    (cd "$PROJECT_ROOT/r36s-agent" && cargo build --release --target aarch64-unknown-linux-musl)
fi

TMP_DIR="$(mktemp -d)"
trap 'rm -rf "$TMP_DIR"' EXIT
cp "$AGENT_BIN" "$TMP_DIR/r36s-agent"
chmod 755 "$TMP_DIR/r36s-agent"
cp "$SERVICE_FILE" "$TMP_DIR/r36s-agent.service"
AGENT_SHA="$(sha256sum "$AGENT_BIN" | awk '{print $1}')"
cat << EOF_MANIFEST > "$TMP_DIR/manifest.json"
{
  "version": "0.2.0",
  "protocol_version": "0.2.0",
  "architecture": "aarch64",
  "build_timestamp": "$(date -u +'%Y-%m-%dT%H:%M:%SZ')",
  "agent_sha256": "$AGENT_SHA"
}
EOF_MANIFEST

rm -f "$OUT_ZIP"
(cd "$TMP_DIR" && zip -q -9 "$OUT_ZIP" r36s-agent r36s-agent.service manifest.json)
echo "Payload gerado com sucesso em: $OUT_ZIP ($(ls -lh "$OUT_ZIP" | awk '{print $5}'))"
