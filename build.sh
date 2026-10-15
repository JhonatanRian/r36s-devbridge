#!/usr/bin/env bash
# ==============================================================================
# R36S DevBridge - Script Mestre de Compilação e Empacotamento
# ==============================================================================
set -Eeuo pipefail

ROOT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
echo "=========================================================="
echo "           COMPILANDO R36S DEVBRIDGE TOOLKIT              "
echo "=========================================================="

# 1. Compilar r36s-agent para AArch64 MUSL (estático para qualquer ArkOS)
echo -e "\n[1/4] Compilando r36s-agent (AArch64 MUSL estático)..."
(cd "$ROOT_DIR/r36s-agent" && cargo build --release --target aarch64-unknown-linux-musl)
echo "r36s-agent compilado com sucesso."

# 2. Empacotar o payload do Port
echo -e "\n[2/4] Empacotando payload.zip do Port..."
"$ROOT_DIR/r36s-port/build_payload.sh"

# 3. Compilar r36s-studio para o PC Host
echo -e "\n[3/4] Compilando aplicativo desktop r36s-studio (PC)..."
(cd "$ROOT_DIR/r36s-studio" && cargo build --release)
cp -f "$ROOT_DIR/r36s-studio/target/release/r36s-studio" "$ROOT_DIR/app"
chmod +x "$ROOT_DIR/app"
echo "Binário ./app gerado com sucesso na raiz do projeto."

# 4. Gerar pasta pronta para copiar no cartão SD
echo -e "\n[4/4] Gerando pacote pronto para o SD card (/roms/ports/)..."
RELEASE_DIR="$ROOT_DIR/dist/ports"
mkdir -p "$RELEASE_DIR/r36s-devbridge"

cp -f "$ROOT_DIR/r36s-port/R36S DevBridge.sh" "$RELEASE_DIR/R36S DevBridge.sh"
chmod +x "$RELEASE_DIR/R36S DevBridge.sh"

cp -f "$ROOT_DIR/r36s-port/payload.zip" "$RELEASE_DIR/r36s-devbridge/payload.zip"
cp -f "$ROOT_DIR/r36s-port/patch_dtb_usb.py" "$RELEASE_DIR/r36s-devbridge/patch_dtb_usb.py"
chmod +x "$RELEASE_DIR/r36s-devbridge/patch_dtb_usb.py"

echo "=========================================================="
echo "                  COMPILAÇÃO CONCLUÍDA!                   "
echo "=========================================================="
echo "1. No PC: Basta rodar ./app para abrir o estúdio."
echo "2. No R36S: Copie o conteúdo de dist/ports/ para /roms/ports/ no cartão SD."
echo "=========================================================="
