#!/usr/bin/env bash
# ==============================================================================
# R36S DevBridge - Instalador e Ativador do Modo Desenvolvedor ArkOS
# ==============================================================================
set -u

# Redireciona para o console virtual da tela do R36S se disponível
if [ -e /dev/tty1 ]; then
    exec < /dev/tty1 > /dev/tty1 2>&1
    clear
fi

# Elevação automática para root se necessário
if [ "$(id -u)" -ne 0 ]; then
    exec sudo -E /bin/bash "$0" "$@"
fi

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
DEVBRIDGE_DIR="$SCRIPT_DIR/r36s-devbridge"
LOG_FILE="$DEVBRIDGE_DIR/install.log"
BACKUP_DIR="$DEVBRIDGE_DIR/backup"
PATCHER="$DEVBRIDGE_DIR/patch_dtb_usb.py"
PAYLOAD_ZIP="$DEVBRIDGE_DIR/payload.zip"

mkdir -p "$DEVBRIDGE_DIR" "$BACKUP_DIR"

# ANSI Colors
C_RESET="\033[0m"
C_BOLD="\033[1m"
C_CYAN="\033[1;36m"
C_GREEN="\033[1;32m"
C_YELLOW="\033[1;33m"
C_RED="\033[1;31m"
C_MAGENTA="\033[1;35m"

log() {
    local msg="$1"
    echo -e "$msg"
    echo -e "$(date '+%Y-%m-%d %H:%M:%S') $msg" | sed -r "s/\x1B\[([0-9]{1,2}(;[0-9]{1,2})?)?[mGK]//g" >> "$LOG_FILE"
}

print_header() {
    echo -e "${C_CYAN}"
    echo "=========================================================="
    echo "            R 3 6 S   D E V B R I D G E                   "
    echo "       Toolkit de Desenvolvimento ArkOS / RK3326          "
    echo "=========================================================="
    echo -e "${C_RESET}"
}

wait_any_key() {
    local timeout="${1:-30}"
    echo -e "\n${C_YELLOW}Pressione qualquer botão (A/B/Start) ou aguarde ${timeout}s...${C_RESET}"
    
    # Tenta ler de dispositivo de input do console se existir
    local evdev=""
    for dev in /dev/input/by-path/*event* /dev/input/event*; do
        if [ -r "$dev" ]; then
            evdev="$dev"
            break
        fi
    done

    if [ -n "$evdev" ] && command -v evtest >/dev/null 2>&1; then
        timeout "$timeout" evtest --grab "$evdev" >/dev/null 2>&1 || true
    else
        read -t "$timeout" -n 1 -s -r < /dev/tty1 2>/dev/null || read -t "$timeout" -n 1 -s -r 2>/dev/null || true
    fi
}

remount_rw() {
    local target="$1"
    if [ -d "$target" ]; then
        if mount | grep -E "on $target " | grep -q "ro,"; then
            log "Remontando $target para leitura/escrita (RW)..."
            mount -o remount,rw "$target" 2>/dev/null || {
                log "${C_RED}[ERRO] Não foi possível remontar $target em RW.${C_RESET}"
                return 1
            }
        fi
    fi
    return 0
}

find_dtb() {
    local candidate="/boot/rk3326-r35s-linux.dtb"
    if [ -f "$candidate" ]; then
        echo "$candidate"
        return 0
    fi
    for f in /boot/*.dtb; do
        if [ -f "$f" ]; then
            echo "$f"
            return 0
        fi
    done
    return 1
}

uninstall_and_restore() {
    print_header
    log "${C_YELLOW}[!] Iniciando desinstalação e restauração do sistema...${C_RESET}"

    # Para e remove serviço
    systemctl stop r36s-agent.service 2>/dev/null || true
    systemctl disable r36s-agent.service 2>/dev/null || true
    rm -f /etc/systemd/system/r36s-agent.service /usr/local/bin/r36s-agent
    systemctl daemon-reload

    # Restaura DTB original
    local dtb
    dtb="$(find_dtb)"
    if [ -f "$BACKUP_DIR/original.dtb" ] && [ -n "$dtb" ]; then
        cp -f "$BACKUP_DIR/original.dtb" "$dtb"
        sync
        log "${C_GREEN}[OK] DTB original restaurado com sucesso em $dtb.${C_RESET}"
    else
        log "${C_YELLOW}[!] Backup do DTB original não encontrado ou DTB ausente.${C_RESET}"
    fi

    log "${C_GREEN}[OK] DevBridge desinstalado. O R36S voltou ao estado original.${C_RESET}"
    wait_any_key 15
    exit 0
}

restart_service() {
    print_header
    log "${C_CYAN}[*] Reiniciando serviço DevBridge e rede USB...${C_RESET}"
    modprobe dwc2 2>/dev/null || true
    modprobe g_ether 2>/dev/null || true
    sleep 1
    ip link set dev usb0 up 2>/dev/null || true
    ip -4 address replace 192.168.7.2/24 dev usb0 2>/dev/null || true
    systemctl restart r36s-agent.service 2>/dev/null || true

    if systemctl is-active --quiet r36s-agent.service; then
        log "${C_GREEN}[OK] Serviço r36s-agent reiniciado e ativo.${C_RESET}"
        log "${C_GREEN}[OK] IP: 192.168.7.2 (usb0)${C_RESET}"
    else
        log "${C_RED}[ERRO] Falha ao reiniciar r36s-agent.service.${C_RESET}"
    fi
    wait_any_key 15
    exit 0
}

install_or_update() {
    print_header
    log "${C_BOLD}[1/5] Verificando partições e permissões...${C_RESET}"
    remount_rw "/boot" || exit 1
    remount_rw "/roms" || exit 1
    remount_rw "/" || exit 1
    log "${C_GREEN}[OK] Partições acessíveis em modo escrita.${C_RESET}"

    log "\n${C_BOLD}[2/5] Verificando Device Tree (DTB USB OTG)...${C_RESET}"
    local dtb
    dtb="$(find_dtb)" || {
        log "${C_RED}[ERRO] Arquivo DTB do ArkOS não encontrado em /boot.${C_RESET}"
        wait_any_key 20
        exit 1
    }

    if [ -f "$PATCHER" ]; then
        local dtb_state
        dtb_state="$(python3 "$PATCHER" check "$dtb" 2>/dev/null || echo "check_failed")"
        log "Estado atual do DTB: $dtb_state"

        if [[ "$dtb_state" != *"dr_mode=peripheral"* ]]; then
            log "Configurando porta OTG para modo periférico/gadget..."
            if [ ! -f "$BACKUP_DIR/original.dtb" ]; then
                cp -p "$dtb" "$BACKUP_DIR/original.dtb"
                sha256sum "$dtb" > "$BACKUP_DIR/metadata.txt"
                log "${C_GREEN}[OK] Backup do DTB salvo em backup/original.dtb.${C_RESET}"
            fi

            TMP_DTB="$(mktemp)"
            python3 "$PATCHER" patch "$dtb" "$TMP_DTB" 2>/dev/null || {
                log "${C_RED}[ERRO] Falha ao patchear DTB.${C_RESET}"
                rm -f "$TMP_DTB"
                wait_any_key 20
                exit 1
            }
            cp -f "$TMP_DTB" "$dtb"
            rm -f "$TMP_DTB"
            sync
            log "${C_GREEN}[OK] DTB atualizado com sucesso para modo periférico.${C_RESET}"
        else
            log "${C_GREEN}[OK] DTB já configurado em modo periférico.${C_RESET}"
        fi
    else
        log "${C_YELLOW}[!] patch_dtb_usb.py não encontrado; pulando patch do DTB.${C_RESET}"
    fi

    log "\n${C_BOLD}[3/5] Carregando módulos do kernel para USB Gadget...${C_RESET}"
    modprobe dwc2 2>/dev/null || true
    modprobe g_ether 2>/dev/null || true

    log "Aguardando interface de rede usb0..."
    local found_usb=0
    for _ in {1..5}; do
        if ip link show dev usb0 >/dev/null 2>&1; then
            found_usb=1
            break
        fi
        sleep 1
    done

    if [ "$found_usb" -eq 1 ]; then
        ip link set dev usb0 up
        ip -4 address replace 192.168.7.2/24 dev usb0
        log "${C_GREEN}[OK] Interface usb0 configurada em 192.168.7.2/24.${C_RESET}"
    else
        log "${C_YELLOW}[!] usb0 não apareceu imediatamente (normal se o cabo não estiver plugado).${C_RESET}"
    fi

    log "\n${C_BOLD}[4/5] Instalando r36s-agent e serviço systemd...${C_RESET}"
    if [ -f "$PAYLOAD_ZIP" ]; then
        TMP_EXTRACT="$(mktemp -d)"
        unzip -q -o "$PAYLOAD_ZIP" -d "$TMP_EXTRACT" 2>/dev/null || {
            log "${C_RED}[ERRO] Falha ao descompactar payload.zip.${C_RESET}"
            rm -rf "$TMP_EXTRACT"
            wait_any_key 20
            exit 1
        }

        if [ -f "$TMP_EXTRACT/r36s-agent" ]; then
            cp -f "$TMP_EXTRACT/r36s-agent" /usr/local/bin/r36s-agent
            chmod 755 /usr/local/bin/r36s-agent
            log "${C_GREEN}[OK] Binário r36s-agent instalado em /usr/local/bin.${C_RESET}"
        else
            log "${C_RED}[ERRO] Binário r36s-agent ausente no payload.zip.${C_RESET}"
        fi

        if [ -f "$TMP_EXTRACT/r36s-agent.service" ]; then
            cp -f "$TMP_EXTRACT/r36s-agent.service" /etc/systemd/system/r36s-agent.service
            chmod 644 /etc/systemd/system/r36s-agent.service
            log "${C_GREEN}[OK] Serviço r36s-agent.service instalado no systemd.${C_RESET}"
        fi
        rm -rf "$TMP_EXTRACT"
    elif [ -f "$DEVBRIDGE_DIR/r36s-agent" ]; then
        cp -f "$DEVBRIDGE_DIR/r36s-agent" /usr/local/bin/r36s-agent
        chmod 755 /usr/local/bin/r36s-agent
        log "${C_GREEN}[OK] Binário r36s-agent instalado.${C_RESET}"
    else
        log "${C_RED}[ERRO] payload.zip ou r36s-agent não encontrado em $DEVBRIDGE_DIR.${C_RESET}"
        wait_any_key 20
        exit 1
    fi

    log "\n${C_BOLD}[5/5] Ativando serviço de segundo plano...${C_RESET}"
    systemctl daemon-reload
    systemctl enable r36s-agent.service 2>/dev/null || true
    systemctl restart r36s-agent.service 2>/dev/null || true
    sleep 1

    if systemctl is-active --quiet r36s-agent.service; then
        log "${C_GREEN}[OK] Serviço r36s-agent está ativo e rodando!${C_RESET}"
    else
        log "${C_YELLOW}[!] Serviço r36s-agent registrado; iniciará automaticamente.${C_RESET}"
    fi

    echo -e "\n${C_GREEN}==========================================================${C_RESET}"
    echo -e "${C_GREEN}         [PRONTO] R36S PREPARADO PARA O DEVBRIDGE        ${C_RESET}"
    echo -e "${C_GREEN}==========================================================${C_RESET}"
    echo -e "IP do Portátil:   ${C_BOLD}192.168.7.2${C_RESET}"
    echo -e "Porta Controle:   ${C_BOLD}7365${C_RESET}"
    echo -e "Porta Vídeo:      ${C_BOLD}7366${C_RESET}"
    echo -e "\n1. Conecte o cabo USB na porta OTG e no seu PC."
    echo -e "2. No computador, abra o app: ${C_CYAN}./app${C_RESET}"
    echo -e "3. Você pode jogar normalmente, a ferramenta espelha e monitora."
    
    wait_any_key 30
    exit 0
}

# --- Ponto de Entrada: Menu Inteligente se já instalado ---
if systemctl is-active --quiet r36s-agent.service 2>/dev/null; then
    print_header
    echo -e "${C_GREEN}O R36S DevBridge já está instalado e ativo neste console!${C_RESET}\n"
    echo -e "Escolha uma opção:"
    echo -e "  ${C_BOLD}[1]${C_RESET} Reiniciar Serviço & Rede USB"
    echo -e "  ${C_BOLD}[2]${C_RESET} Reinstalar / Atualizar Binários"
    echo -e "  ${C_BOLD}[3]${C_RESET} Desinstalar e Restaurar DTB Original"
    echo -e "  ${C_BOLD}[4]${C_RESET} Sair para o ArkOS"
    echo -e "\nDigite 1-4 ou aguarde 15s para sair:"

    choice=""
    read -t 15 -n 1 -r choice < /dev/tty1 2>/dev/null || read -t 15 -n 1 -r choice 2>/dev/null || true
    echo

    case "$choice" in
        1) restart_service ;;
        2) install_or_update ;;
        3) uninstall_and_restore ;;
        *) log "Saindo para o ArkOS..."; exit 0 ;;
    esac
else
    install_or_update
fi
