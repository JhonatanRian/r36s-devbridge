#!/usr/bin/env bash
# ==============================================================================
# R36S DevBridge - Script Mestre de Compilação e Empacotamento
# ==============================================================================
set -Eeuo pipefail

ROOT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
echo "=========================================================="
echo "           COMPILANDO R36S DEVBRIDGE TOOLKIT              "
echo "=========================================================="

# Localiza cargo no PATH ou nos diretórios .cargo padrão
for cand in \
    "${CARGO_BIN:-}" \
    "$(command -v cargo 2>/dev/null || true)" \
    "${USER_CARGO:-}" \
    "$HOME/.cargo/bin/cargo" \
    "/home/${SUDO_USER:-$USER}/.cargo/bin/cargo" \
    /home/user/.cargo/bin/cargo \
    /root/.cargo/bin/cargo \
    /usr/bin/cargo \
    /usr/local/bin/cargo; do
    if [ -n "$cand" ] && [ -x "$cand" ]; then
        export PATH="$(dirname "$cand"):$PATH"
        break
    fi
done

if ! command -v cargo >/dev/null 2>&1; then
    echo "ERRO: cargo não encontrado. Artefatos antigos não serão instalados silenciosamente." >&2
    exit 1
fi

# 1. Compilar r36s-agent para AArch64 MUSL (estático para qualquer ArkOS)
echo -e "\n[1/4] Compilando r36s-agent (AArch64 MUSL estático)..."
(cd "$ROOT_DIR/r36s-agent" && cargo build --release --target aarch64-unknown-linux-musl)
echo "r36s-agent compilado com sucesso."

# 2. Empacotar o payload do Port
echo -e "\n[2/4] Empacotando payload.zip do Port..."
DEVBRIDGE_AGENT_BUILT=1 "$ROOT_DIR/r36s-port/build_payload.sh"

# 3. Compilar r36s-studio para o PC Host
echo -e "\n[3/4] Compilando aplicativo desktop r36s-studio (PC)..."
(cd "$ROOT_DIR/r36s-studio" && cargo build --release)
install -m 755 "$ROOT_DIR/target/release/r36s-studio" "$ROOT_DIR/.app.new"
mv -f "$ROOT_DIR/.app.new" "$ROOT_DIR/app"
chmod +x "$ROOT_DIR/app"
echo "Binário ./app gerado com sucesso na raiz do projeto."

# 4. Gerar pasta pronta para copiar no cartão SD
echo -e "\n[4/4] Gerando pacote pronto para o SD card (/roms/ports/)..."
RELEASE_DIR="$ROOT_DIR/dist/ports"
mkdir -p "$RELEASE_DIR/r36s-devbridge"

cp -f "$ROOT_DIR/r36s-port/R36S DevBridge.sh" "$RELEASE_DIR/R36S DevBridge.sh"
chmod +x "$RELEASE_DIR/R36S DevBridge.sh"

cp -f "$ROOT_DIR/r36s-port/install_payload.py" "$RELEASE_DIR/r36s-devbridge/install_payload.py"
cp -f "$ROOT_DIR/r36s-port/payload.zip" "$RELEASE_DIR/r36s-devbridge/payload.zip"
cp -f "$ROOT_DIR/r36s-port/patch_dtb_usb.py" "$RELEASE_DIR/r36s-devbridge/patch_dtb_usb.py"
chmod +x "$RELEASE_DIR/r36s-devbridge/patch_dtb_usb.py"
cp -f "$ROOT_DIR/r36s-port/gamepad_menu.py" "$RELEASE_DIR/r36s-devbridge/gamepad_menu.py"
chmod +x "$RELEASE_DIR/r36s-devbridge/gamepad_menu.py"

echo "=========================================================="
echo "                  COMPILAÇÃO CONCLUÍDA!                   "
echo "=========================================================="
echo "1. No PC: Basta rodar ./app para abrir o estúdio."
echo "2. No R36S: Copie o conteúdo de dist/ports/ para /roms/ports/ no cartão SD."
echo "=========================================================="
