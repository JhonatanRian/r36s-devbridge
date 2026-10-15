#!/usr/bin/env bash
# ==============================================================================
# R36S DevBridge - Instalador Direto no Cartão SD montado no PC
# ==============================================================================
set -Eeuo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
USER_NAME="${SUDO_USER:-$USER}"

echo "=========================================================="
echo "          INSTALAÇÃO DO DEVBRIDGE NO CARTÃO SD            "
echo "=========================================================="

# Localiza pontos de montagem
MEDIA_ROOT="/run/media/$USER_NAME"
if [ ! -d "$MEDIA_ROOT" ]; then
    MEDIA_ROOT="/media/$USER_NAME"
fi

# Função auxiliar para extrair o disco físico de um diretório montado
get_parent_disk() {
    local mnt="$1"
    local dev
    dev="$(findmnt -n -o SOURCE "$mnt" 2>/dev/null || true)"
    if [ -n "$dev" ] && [ -b "$dev" ]; then
        lsblk -no PKNAME "$dev" 2>/dev/null || echo "$dev"
    fi
}

for base in "$MEDIA_ROOT" /run/media/* /media/*; do
    [ -d "$base/EASYROMS" ] && [ -z "$EASYROMS" ] && EASYROMS="$base/EASYROMS"
    [ -d "$base/BOOT" ] && [ -z "$BOOT" ] && BOOT="$base/BOOT"
    [ -d "$base/root" ] && [ -z "$ROOT" ] && ROOT="$base/root"
done

if [ -z "$EASYROMS" ]; then
    echo "ERRO: Partição EASYROMS não encontrada montada no sistema." >&2
    echo "Verifique se o cartão SD está inserido e montado." >&2
    exit 1
fi

EASYROMS_DISK="$(get_parent_disk "$EASYROMS")"
echo "Partição EASYROMS: $EASYROMS (disco: ${EASYROMS_DISK:-desconhecido})"

# Valida se BOOT e ROOT pertencem ao mesmo dispositivo de armazenamento para evitar contaminação
if [ -n "$BOOT" ]; then
    BOOT_DISK="$(get_parent_disk "$BOOT")"
    if [ -n "$EASYROMS_DISK" ] && [ -n "$BOOT_DISK" ] && [ "$EASYROMS_DISK" != "$BOOT_DISK" ]; then
        echo "AVISO: Partição BOOT ($BOOT, disco $BOOT_DISK) não pertence ao mesmo disco de EASYROMS ($EASYROMS_DISK); ignorando para evitar corrupção." >&2
        BOOT=""
    else
        echo "Partição BOOT:     $BOOT"
    fi
fi

if [ -n "$ROOT" ]; then
    ROOT_DISK="$(get_parent_disk "$ROOT")"
    if [ -n "$EASYROMS_DISK" ] && [ -n "$ROOT_DISK" ] && [ "$EASYROMS_DISK" != "$ROOT_DISK" ]; then
        echo "AVISO: Partição root ($ROOT, disco $ROOT_DISK) não pertence ao mesmo disco de EASYROMS ($EASYROMS_DISK); ignorando." >&2
        ROOT=""
    else
        echo "Partição root:     $ROOT"
    fi
fi

# Sempre recompila e atualiza o pacote dist/ports para garantir versão atualizada
echo -e "\nCompilando e gerando pacote atualizado dist/ports..."
"$SCRIPT_DIR/build.sh"

# 1. Instalação em Ports (/roms/ports/)
PORTS_DIR="$EASYROMS/ports"
DEST_DEVBRIDGE="$PORTS_DIR/r36s-devbridge"
mkdir -p "$PORTS_DIR" "$DEST_DEVBRIDGE"

echo -e "\n[1/3] Copiando arquivos do Port para $PORTS_DIR..."
cp -f "$SCRIPT_DIR/dist/ports/R36S DevBridge.sh" "$PORTS_DIR/R36S DevBridge.sh"
chmod +x "$PORTS_DIR/R36S DevBridge.sh"

cp -f "$SCRIPT_DIR/dist/ports/r36s-devbridge/payload.zip" "$DEST_DEVBRIDGE/payload.zip"
cp -f "$SCRIPT_DIR/dist/ports/r36s-devbridge/patch_dtb_usb.py" "$DEST_DEVBRIDGE/patch_dtb_usb.py"
chmod +x "$DEST_DEVBRIDGE/patch_dtb_usb.py"

echo "[OK] Port copiado com sucesso para o menu Ports do console!"

# 2. Verificação do DTB (se BOOT estiver acessível)
if [ -n "$BOOT" ] && [ -f "$BOOT/rk3326-r35s-linux.dtb" ]; then
    echo -e "\n[2/3] Verificando Device Tree no BOOT..."
    DTB_STATE="$(python3 "$SCRIPT_DIR/r36s-port/patch_dtb_usb.py" check "$BOOT/rk3326-r35s-linux.dtb" 2>/dev/null || echo "unknown")"
    echo "Estado atual do DTB USB: $DTB_STATE"

    if [[ "$DTB_STATE" != *"dr_mode=peripheral"* ]]; then
        echo "Aplicando patch periférico no DTB..."
        mkdir -p "$DEST_DEVBRIDGE/backup"
        if [ ! -f "$DEST_DEVBRIDGE/backup/original.dtb" ]; then
            cp -p "$BOOT/rk3326-r35s-linux.dtb" "$DEST_DEVBRIDGE/backup/original.dtb"
            sha256sum "$BOOT/rk3326-r35s-linux.dtb" > "$DEST_DEVBRIDGE/backup/metadata.txt"
            echo "[OK] Backup do DTB original criado em $DEST_DEVBRIDGE/backup/original.dtb"
        fi
        TMP_DTB="$(mktemp)"
        python3 "$SCRIPT_DIR/r36s-port/patch_dtb_usb.py" patch "$BOOT/rk3326-r35s-linux.dtb" "$TMP_DTB"
        cp -f "$TMP_DTB" "$BOOT/rk3326-r35s-linux.dtb"
        rm -f "$TMP_DTB"
        echo "[OK] DTB atualizado para modo periférico!"
    else
        echo "[OK] DTB já está pronto em modo periférico!"
    fi
else
    echo -e "\n[2/3] Partição BOOT não disponível ou sem DTB; o script Port fará isso no console."
fi

# 3. Pré-instalação no root (se ROOT estiver acessível e com permissão)
if [ -n "$ROOT" ] && [ -d "$ROOT/usr/local/bin" ] && [ -w "$ROOT/usr/local/bin" ]; then
    echo -e "\n[3/3] Pré-instalando daemon e serviço systemd diretamente na partição root..."
    TMP_UNZIP="$(mktemp -d)"
    unzip -q -o "$SCRIPT_DIR/dist/ports/r36s-devbridge/payload.zip" -d "$TMP_UNZIP"

    cp -f "$TMP_UNZIP/r36s-agent" "$ROOT/usr/local/bin/r36s-agent"
    chmod 755 "$ROOT/usr/local/bin/r36s-agent"

    cp -f "$TMP_UNZIP/r36s-agent.service" "$ROOT/etc/systemd/system/r36s-agent.service"
    chmod 644 "$ROOT/etc/systemd/system/r36s-agent.service"

    # Habilita o serviço criando o symlink no multi-user.target.wants
    WANTS_DIR="$ROOT/etc/systemd/system/multi-user.target.wants"
    mkdir -p "$WANTS_DIR"
    ln -sf "/etc/systemd/system/r36s-agent.service" "$WANTS_DIR/r36s-agent.service"

    # Registra módulos persistentes em /etc/modules
    if [ -f "$ROOT/etc/modules" ]; then
        grep -q '^dwc2' "$ROOT/etc/modules" 2>/dev/null || echo 'dwc2' >> "$ROOT/etc/modules"
        grep -q '^g_ether' "$ROOT/etc/modules" 2>/dev/null || echo 'g_ether' >> "$ROOT/etc/modules"
        echo "[OK] Módulos dwc2 e g_ether adicionados a /etc/modules"
    fi

    # Regra udev para manter usb0 configurado
    mkdir -p "$ROOT/etc/udev/rules.d"
    cat << 'EOF_UDEV' > "$ROOT/etc/udev/rules.d/99-r36s-devbridge-usb0.rules"
SUBSYSTEM=="net", ACTION=="add", KERNEL=="usb0", RUN+="/sbin/ip link set dev usb0 up", RUN+="/sbin/ip -4 address replace 192.168.7.2/24 dev usb0"
EOF_UDEV

    rm -rf "$TMP_UNZIP"
    echo "[OK] r36s-agent pré-instalado e serviço habilitado no systemd do ArkOS!"
else
    echo -e "\n[3/3] Partição root requer permissão de escrita (sudo ./install_to_sd.sh para pré-instalação total)."
    echo "      O atalho 'R36S DevBridge' no menu Ports fará a instalação automaticamente."
fi

echo -e "\nSincronizando gravações no cartão SD..."
sync

echo "=========================================================="
echo "          INSTALAÇÃO NO CARTÃO SD CONCLUÍDA!              "
echo "=========================================================="
echo "1. Ejete o cartão com segurança e coloque no R36S."
echo "2. No R36S, abra Ports -> R36S DevBridge (se quiser conferir)."
echo "3. Plugue o cabo USB OTG no PC e inicie: ./app"
echo "=========================================================="
