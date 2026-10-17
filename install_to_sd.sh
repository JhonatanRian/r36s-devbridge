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

# 1. Elevação automática para root (necessária para gravar em root/usr/local/bin)
if [ "$(id -u)" -ne 0 ]; then
    echo "Privilégios de superusuário (sudo) necessários para gravação completa no SD."
    echo "Solicitando autorização..."
    exec sudo -E /bin/bash "$0" "$@"
fi

# Resolve all identities on one physical card before mounting or writing anything.
MEDIA_ROOT="/run/media/$USER_NAME"
[ -d "$MEDIA_ROOT" ] || MEDIA_ROOT="/media/$USER_NAME"
PARTITIONS="$(python3 "$SCRIPT_DIR/r36s-port/sd_card.py")"
mount_partition() {
    local label="$1" dev="$2" target options
    [ -b "$dev" ] || { echo "Dispositivo desapareceu: $dev" >&2; return 1; }
    target="$(findmnt -rn -S "$dev" -o TARGET || true)"
    if [ -z "$target" ]; then
        target="$MEDIA_ROOT/DevBridge-$label"
        if mountpoint -q "$target"; then
            echo "Destino ocupado por outro dispositivo: $target. Ejete a montagem antiga antes de instalar." >&2
            return 1
        fi
        mkdir -p "$target" || return 1
        mount "$dev" "$target" || return 1
    fi
    options="$(findmnt -rn -M "$target" -o OPTIONS)" || return 1
    case ",$options," in *,ro,*) echo "Partição somente leitura: $target" >&2; return 1 ;; esac
    # EIO is an error, never an instruction to force a remount or detach another filesystem.
    ls -A "$target" >/dev/null || return 1
    printf '%s\n' "$target"
}
EASYROMS="" BOOT="" ROOT=""
while IFS=$'\t' read -r label device; do
    case "$label" in
        EASYROMS) EASYROMS="$(mount_partition "$label" "$device")" ;;
        BOOT) BOOT="$(mount_partition "$label" "$device")" ;;
        root) ROOT="$(mount_partition "$label" "$device")" ;;
    esac
done <<< "$PARTITIONS"
echo "Partição EASYROMS: $EASYROMS"
echo "Partição BOOT: $BOOT"
echo "Partição root: $ROOT"

# 3. Compilação usando usuário normal para preservar propriedade dos artefatos
echo -e "\nCompilando e gerando pacote atualizado dist/ports..."
for cpath in "$HOME/.cargo/bin" "/home/${SUDO_USER:-}/.cargo/bin" /home/user/.cargo/bin /usr/local/bin /usr/bin; do
    [ -d "$cpath" ] && export PATH="$cpath:$PATH"
done

if [ -n "${SUDO_USER:-}" ] && [ "$SUDO_USER" != "root" ]; then
    USER_HOME="$(getent passwd "$SUDO_USER" | cut -d: -f6 2>/dev/null || echo "/home/$SUDO_USER")"
    sudo -u "$SUDO_USER" env HOME="$USER_HOME" PATH="$PATH" "$SCRIPT_DIR/build.sh" || {
        if [ -f "$SCRIPT_DIR/dist/ports/r36s-devbridge/payload.zip" ]; then
            echo "Aviso: compilação falhou ou cargo ausente; utilizando artefatos pré-existentes em dist/."
        else
            echo "ERRO crítico na compilação!" >&2; exit 1
        fi
    }
else
    "$SCRIPT_DIR/build.sh" || {
        if [ -f "$SCRIPT_DIR/dist/ports/r36s-devbridge/payload.zip" ]; then
            echo "Aviso: compilação falhou ou cargo ausente; utilizando artefatos pré-existentes em dist/."
        else
            echo "ERRO crítico na compilação!" >&2; exit 1
        fi
    }
fi

# 4. Instalação em Ports (/roms/ports/)
PORTS_DIR="$EASYROMS/ports"
DEST_DEVBRIDGE="$PORTS_DIR/r36s-devbridge"
mkdir -p "$PORTS_DIR" "$DEST_DEVBRIDGE"

echo -e "\n[1/3] Copiando arquivos do Port para $PORTS_DIR..."
cp -f "$SCRIPT_DIR/dist/ports/R36S DevBridge.sh" "$PORTS_DIR/R36S DevBridge.sh"
chmod +x "$PORTS_DIR/R36S DevBridge.sh"

cp -f "$SCRIPT_DIR/dist/ports/r36s-devbridge/install_payload.py" "$DEST_DEVBRIDGE/install_payload.py"
cp -f "$SCRIPT_DIR/dist/ports/r36s-devbridge/payload.zip" "$DEST_DEVBRIDGE/payload.zip"
cp -f "$SCRIPT_DIR/dist/ports/r36s-devbridge/patch_dtb_usb.py" "$DEST_DEVBRIDGE/patch_dtb_usb.py"
chmod +x "$DEST_DEVBRIDGE/patch_dtb_usb.py"
cp -f "$SCRIPT_DIR/dist/ports/r36s-devbridge/gamepad_menu.py" "$DEST_DEVBRIDGE/gamepad_menu.py"
chmod +x "$DEST_DEVBRIDGE/gamepad_menu.py"

echo "[OK] Port copiado com sucesso para o menu Ports do console!"

# 5. Verificação e Patch do DTB (se BOOT estiver acessível)
ACTIVE_DTB=""
if [ -n "$BOOT" ]; then
    if [ -f "$BOOT/boot.ini" ]; then
        INI_DTB="$( (grep -E '^[[:space:]]*setenv[[:space:]]+(fdtfile|fdt_file)[[:space:]]+' "$BOOT/boot.ini" 2>/dev/null || true) | head -n1 | awk '{print $NF}' | tr -d '\r"' || true)"
        if [ -n "$INI_DTB" ] && [ -f "$BOOT/$INI_DTB" ]; then
            ACTIVE_DTB="$BOOT/$INI_DTB"
        fi
    fi
    if [ -z "$ACTIVE_DTB" ] && [ -f "$BOOT/rk3326-r35s-linux.dtb" ]; then
        ACTIVE_DTB="$BOOT/rk3326-r35s-linux.dtb"
    fi
fi

if [ -n "$ACTIVE_DTB" ] && [ -f "$ACTIVE_DTB" ]; then
    echo -e "\n[2/3] Verificando Device Tree ativo ($ACTIVE_DTB)..."
    DTB_STATE="$(python3 "$SCRIPT_DIR/r36s-port/patch_dtb_usb.py" check "$ACTIVE_DTB" 2>/dev/null || echo "unknown")"
    echo "Estado atual do DTB USB: $DTB_STATE"

    if [[ "$DTB_STATE" != *"dr_mode=peripheral"* ]]; then
        echo "Aplicando patch periférico no DTB..."
        mkdir -p "$DEST_DEVBRIDGE/backup"
        if [ ! -f "$DEST_DEVBRIDGE/backup/original.dtb" ]; then
            cp -p "$ACTIVE_DTB" "$DEST_DEVBRIDGE/backup/original.dtb"
            sha256sum "$ACTIVE_DTB" > "$DEST_DEVBRIDGE/backup/metadata.txt"
            echo "[OK] Backup do DTB original criado em $DEST_DEVBRIDGE/backup/original.dtb"
        fi
        TMP_DTB="$(mktemp)"
        python3 "$SCRIPT_DIR/r36s-port/patch_dtb_usb.py" patch "$ACTIVE_DTB" "$TMP_DTB"
        cp -f "$TMP_DTB" "$ACTIVE_DTB"
        rm -f "$TMP_DTB"
        echo "[OK] DTB atualizado para modo periférico!"
    else
        echo "[OK] DTB já está pronto em modo periférico!"
    fi
else
    echo -e "\n[2/3] Partição BOOT não disponível ou DTB ativo não localizado; o script Port fará isso no console."
fi

# 6. Instalação Direta no Root (garante atualização imediata do daemon)
if [ -n "$ROOT" ] && [ -d "$ROOT/usr/local/bin" ]; then
    echo -e "\n[3/3] Instalando daemon e serviço systemd diretamente na partição root..."
    TMP_UNZIP="$(mktemp -d)"
    unzip -q -o "$SCRIPT_DIR/dist/ports/r36s-devbridge/payload.zip" -d "$TMP_UNZIP"

    python3 "$SCRIPT_DIR/r36s-port/install_payload.py" "$TMP_UNZIP" --root "$ROOT"

    WANTS_DIR="$ROOT/etc/systemd/system/multi-user.target.wants"
    mkdir -p "$WANTS_DIR"
    ln -sf "/etc/systemd/system/r36s-agent.service" "$WANTS_DIR/r36s-agent.service"



    mkdir -p "$ROOT/etc/udev/rules.d"
    cat << 'EOF_UDEV' > "$ROOT/etc/udev/rules.d/99-r36s-devbridge-usb0.rules"
SUBSYSTEM=="net", ACTION=="add", KERNEL=="usb0", RUN+="/sbin/ip link set dev usb0 up", RUN+="/sbin/ip -4 address replace 192.168.7.2/24 dev usb0"
EOF_UDEV

    mkdir -p "$ROOT/etc/NetworkManager/conf.d"
    cat << 'EOF_NM' > "$ROOT/etc/NetworkManager/conf.d/99-r36s-devbridge-usb0.conf"
[keyfile]
unmanaged-devices=interface-name:usb0
EOF_NM

    mkdir -p "$ROOT/etc/logrotate.d"
    cat << 'EOF_LOGROTATE' > "$ROOT/etc/logrotate.d/r36s-agent"
/var/log/r36s-agent.log {
    size 1M
    rotate 2
    missingok
    notifempty
    copytruncate
}
EOF_LOGROTATE
    rm -f "$ROOT/etc/systemd/journald.conf.d/99-r36s-devbridge.conf"
    rm -rf "$TMP_UNZIP"
    echo "[OK] r36s-agent verificado e instalado com sucesso em /usr/local/bin e habilitado no systemd!"
else
    echo -e "\n[3/3] AVISO: Partição root não disponível diretamente; a instalação será feita via menu Ports no console."
fi

echo -e "\nSincronizando gravações pendentes no cartão SD..."
sync

echo "=========================================================="
echo "          INSTALAÇÃO NO CARTÃO SD CONCLUÍDA!              "
echo "=========================================================="
echo "1. Ejete o cartão com segurança e coloque de volta no R36S."
echo "2. O daemon está instalado; valide a conexão após ligar o console."
echo "3. Ligue o R36S, plugue o cabo USB OTG no PC e rode: ./app"
echo "=========================================================="
