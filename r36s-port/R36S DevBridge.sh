#!/usr/bin/env bash
# ==============================================================================
# R36S DevBridge - Instalador e Gerenciador do Modo Desenvolvedor ArkOS
# ==============================================================================
set -u

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
MENU_HELPER="$DEVBRIDGE_DIR/gamepad_menu.py"

mkdir -p "$DEVBRIDGE_DIR" "$BACKUP_DIR"

# ANSI Colors
C_RESET="\033[0m"
C_BOLD="\033[1m"
C_CYAN="\033[1;36m"
C_GREEN="\033[1;32m"
C_YELLOW="\033[1;33m"
C_RED="\033[1;31m"

log() {
    local msg="$1"
    echo -e "$msg"
    echo -e "$msg" > /dev/tty1 2>/dev/null || true
    echo -e "$(date '+%Y-%m-%d %H:%M:%S') $msg" | sed -r "s/\x1B\[([0-9]{1,2}(;[0-9]{1,2})?)?[mGK]//g" >> "$LOG_FILE"
}

print_header() {
    printf "\033c" > /dev/tty1 2>/dev/null || true
    log "${C_CYAN}"
    log "=========================================================="
    log "            R 3 6 S   D E V B R I D G E                   "
    log "       Toolkit de Desenvolvimento ArkOS / RK3326          "
    log "=========================================================="
    log "${C_RESET}"
}

wait_and_finish() {
    local timeout="${1:-4}"
    local exit_code="${2:-0}"
    if [ "$exit_code" -eq 0 ]; then
        log "\n${C_GREEN}Operação concluída. Pressione qualquer tecla ou aguarde ${timeout}s...${C_RESET}"
    else
        log "\n${C_RED}Encerrando com erro. Pressione qualquer tecla ou aguarde ${timeout}s...${C_RESET}"
    fi
    read -t "$timeout" -n 1 _ < /dev/tty1 2>/dev/null || sleep "$timeout"
    printf "\033c" > /dev/tty1 2>/dev/null || true
    exit "$exit_code"
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
    # 1. Procura no boot.ini pelo DTB ativo configurado
    if [ -f /boot/boot.ini ]; then
        local ini_dtb
        ini_dtb=$(grep -E '^[[:space:]]*setenv[[:space:]]+(fdtfile|fdt_file)[[:space:]]+' /boot/boot.ini 2>/dev/null | head -n1 | awk '{print $NF}' | tr -d '\r"')
        if [ -n "$ini_dtb" ] && [ -f "/boot/$ini_dtb" ]; then
            echo "/boot/$ini_dtb"
            return 0
        fi
    fi

    # 2. Padrão do ArkOS para R35S/R36S
    if [ -f "/boot/rk3326-r35s-linux.dtb" ]; then
        echo "/boot/rk3326-r35s-linux.dtb"
        return 0
    fi

    # 3. Imagem RG351P se usada como base
    if [ -f "/boot/rk3326-rg351p-linux.dtb" ]; then
        echo "/boot/rk3326-rg351p-linux.dtb"
        return 0
    fi

    # 4. Outro único DTB em /boot se for inequívoco
    local dtb_count
    dtb_count=$(ls -1 /boot/*.dtb 2>/dev/null | wc -l)
    if [ "$dtb_count" -eq 1 ]; then
        ls -1 /boot/*.dtb 2>/dev/null | head -n1
        return 0
    fi

    return 1
}

uninstall_and_restore() {
    print_header
    log "${C_YELLOW}[!] Iniciando desinstalação e restauração do sistema...${C_RESET}"

    # 1. Parar e remover serviço e binário
    systemctl stop r36s-agent.service 2>/dev/null || true
    systemctl disable r36s-agent.service 2>/dev/null || true
    rm -f /etc/systemd/system/r36s-agent.service /usr/local/bin/r36s-agent /etc/udev/rules.d/99-r36s-devbridge-usb0.rules /etc/NetworkManager/conf.d/99-r36s-devbridge-usb0.conf /etc/systemd/journald.conf.d/99-r36s-devbridge.conf /etc/logrotate.d/r36s-agent
    systemctl daemon-reload
    udevadm control --reload-rules 2>/dev/null || true

    # 2. Limpar módulos persistentes de /etc/modules
    if [ -f /etc/modules ]; then
        sed -i '/^dwc2$/d' /etc/modules 2>/dev/null || true
        sed -i '/^g_ether$/d' /etc/modules 2>/dev/null || true
        log "${C_GREEN}[OK] Módulos dwc2 e g_ether removidos de /etc/modules.${C_RESET}"
    fi

    # 3. Restaurar DTB original se existir backup verificado
    local dtb
    dtb="$(find_dtb)" || dtb=""
    local dtb_restored=0

    # Procura backup em múltiplos caminhos conhecidos do ArkOS
    local backup_candidate=""
    local meta_candidate=""
    if [ -f "$BACKUP_DIR/original.dtb" ]; then
        backup_candidate="$BACKUP_DIR/original.dtb"
        [ -f "$BACKUP_DIR/metadata.txt" ] && meta_candidate="$BACKUP_DIR/metadata.txt"
    elif [ -f "/boot/R36S_USB_SSH_BACKUP/rk3326-r35s-linux.dtb" ]; then
        backup_candidate="/boot/R36S_USB_SSH_BACKUP/rk3326-r35s-linux.dtb"
    elif [ -f "/roms/ports/r36s-devbridge/backup/original.dtb" ]; then
        backup_candidate="/roms/ports/r36s-devbridge/backup/original.dtb"
        [ -f "/roms/ports/r36s-devbridge/backup/metadata.txt" ] && meta_candidate="/roms/ports/r36s-devbridge/backup/metadata.txt"
    fi

    if [ -n "$backup_candidate" ] && [ -f "$backup_candidate" ] && [ -n "$dtb" ]; then
        if [ -n "$meta_candidate" ] && [ -f "$meta_candidate" ]; then
            local expected_hash actual_hash
            expected_hash=$(awk '{print $1}' "$meta_candidate")
            actual_hash=$(sha256sum "$backup_candidate" | awk '{print $1}')
            if [ "$expected_hash" = "$actual_hash" ]; then
                cp -f "$backup_candidate" "$dtb"
                sync
                dtb_restored=1
                log "${C_GREEN}[OK] DTB original verificado (SHA-256) e restaurado em $dtb.${C_RESET}"
            else
                log "${C_RED}[AVISO] Backup diverge do hash original; restauração abortada por cautela.${C_RESET}"
            fi
        else
            cp -f "$backup_candidate" "$dtb"
            sync
            dtb_restored=1
            log "${C_GREEN}[OK] DTB original restaurado a partir de $backup_candidate em $dtb.${C_RESET}"
        fi
    else
        log "${C_YELLOW}[!] Nenhum backup do DTB original foi localizado; o arquivo atual em $dtb foi mantido.${C_RESET}"
    fi

    log "${C_GREEN}[OK] R36S DevBridge desinstalado com sucesso.${C_RESET}"
    if [ "$dtb_restored" -eq 1 ]; then
        log "${C_YELLOW}[AVISO] REINICIE O R36S para restaurar a porta OTG ao modo host padrão.${C_RESET}"
    fi
    wait_and_finish 6 0
}

install_or_update() {
    print_header
    local REBOOT_REQUIRED=0

    log "${C_BOLD}[1/5] Verificando partições e permissões...${C_RESET}"
    remount_rw "/boot" || wait_and_finish 5 1
    remount_rw "/roms" || wait_and_finish 5 1
    remount_rw "/" || wait_and_finish 5 1
    log "${C_GREEN}[OK] Partições acessíveis em modo escrita.${C_RESET}"

    log "\n${C_BOLD}[2/5] Verificando Device Tree (DTB USB OTG)...${C_RESET}"
    local dtb
    dtb="$(find_dtb)" || {
        log "${C_RED}[ERRO] Arquivo DTB ativo não pôde ser identificado em /boot.${C_RESET}"
        wait_and_finish 6 1
    }
    log "DTB ativo identificado: $dtb"

    if [ -f "$PATCHER" ]; then
        local dtb_state
        dtb_state="$(python3 "$PATCHER" check "$dtb" 2>/dev/null || echo "check_failed")"
        log "Estado do DTB: $dtb_state"

        if [[ "$dtb_state" != *"dr_mode=peripheral"* ]]; then
            log "Configurando porta OTG para modo periférico/gadget..."
            if [ ! -f "$BACKUP_DIR/original.dtb" ]; then
                cp -p "$dtb" "$BACKUP_DIR/original.dtb"
                sha256sum "$dtb" > "$BACKUP_DIR/metadata.txt"
                log "${C_GREEN}[OK] Backup do DTB salvo com SHA-256 verificado.${C_RESET}"
            fi

            TMP_DTB="$(mktemp)"
            if python3 "$PATCHER" patch "$dtb" "$TMP_DTB" 2>/dev/null; then
                cp -f "$TMP_DTB" "$dtb"
                rm -f "$TMP_DTB"
                sync
                REBOOT_REQUIRED=1
                log "${C_GREEN}[OK] DTB atualizado para modo periférico.${C_RESET}"
            else
                rm -f "$TMP_DTB"
                log "${C_RED}[ERRO] Falha ao aplicar patch no DTB.${C_RESET}"
                wait_and_finish 6 1
            fi
        else
            log "${C_GREEN}[OK] DTB já configurado em modo periférico.${C_RESET}"
        fi
    else
        log "${C_YELLOW}[!] patch_dtb_usb.py não encontrado; pulando patch do DTB.${C_RESET}"
    fi

    log "\n${C_BOLD}[3/5] Configurando módulos persistentes e rede USB Gadget...${C_RESET}"
    if [ -f /etc/modules ]; then
        grep -q '^dwc2' /etc/modules 2>/dev/null || echo 'dwc2' >> /etc/modules
        grep -q '^g_ether' /etc/modules 2>/dev/null || echo 'g_ether' >> /etc/modules
        log "${C_GREEN}[OK] Módulos dwc2 e g_ether registrados em /etc/modules.${C_RESET}"
    fi
    mkdir -p /etc/udev/rules.d
    cat << 'EOF_UDEV' > /etc/udev/rules.d/99-r36s-devbridge-usb0.rules
SUBSYSTEM=="net", ACTION=="add", KERNEL=="usb0", RUN+="/sbin/ip link set dev usb0 up", RUN+="/sbin/ip -4 address replace 192.168.7.2/24 dev usb0"
EOF_UDEV
    udevadm control --reload-rules 2>/dev/null || true

    # NetworkManager não deve substituir o IP fixo da interface gadget.
    mkdir -p /etc/NetworkManager/conf.d
    cat << 'EOF_NM' > /etc/NetworkManager/conf.d/99-r36s-devbridge-usb0.conf
[keyfile]
unmanaged-devices=interface-name:usb0
EOF_NM

    # ArkOS mantém journald mascarado nesta imagem; usa arquivo de log rotativo.
    mkdir -p /etc/logrotate.d
    cat << 'EOF_LOGROTATE' > /etc/logrotate.d/r36s-agent
/var/log/r36s-agent.log {
    size 1M
    rotate 2
    missingok
    notifempty
    copytruncate
}
EOF_LOGROTATE
    rm -f /etc/systemd/journald.conf.d/99-r36s-devbridge.conf

    log "\n${C_BOLD}[4/5] Instalando r36s-agent e serviço systemd...${C_RESET}"
    if [ -f "$PAYLOAD_ZIP" ]; then
        TMP_EXTRACT="$(mktemp -d)"
        if unzip -q -o "$PAYLOAD_ZIP" -d "$TMP_EXTRACT" 2>/dev/null; then
            if [ -f "$TMP_EXTRACT/r36s-agent" ]; then
                cp -f "$TMP_EXTRACT/r36s-agent" /usr/local/bin/r36s-agent
                chmod 755 /usr/local/bin/r36s-agent
                log "${C_GREEN}[OK] Binário r36s-agent instalado em /usr/local/bin.${C_RESET}"
            else
                log "${C_RED}[ERRO] Binário r36s-agent ausente no payload.zip.${C_RESET}"
                rm -rf "$TMP_EXTRACT"
                wait_and_finish 5 1
            fi

            if [ -f "$TMP_EXTRACT/r36s-agent.service" ]; then
                cp -f "$TMP_EXTRACT/r36s-agent.service" /etc/systemd/system/r36s-agent.service
                chmod 644 /etc/systemd/system/r36s-agent.service
                log "${C_GREEN}[OK] Serviço r36s-agent.service instalado no systemd.${C_RESET}"
            fi
            rm -rf "$TMP_EXTRACT"
        else
            rm -rf "$TMP_EXTRACT"
            log "${C_RED}[ERRO] Falha ao extrair payload.zip.${C_RESET}"
            wait_and_finish 5 1
        fi
    elif [ -f "$DEVBRIDGE_DIR/r36s-agent" ]; then
        cp -f "$DEVBRIDGE_DIR/r36s-agent" /usr/local/bin/r36s-agent
        chmod 755 /usr/local/bin/r36s-agent
        log "${C_GREEN}[OK] Binário r36s-agent instalado a partir da pasta.${C_RESET}"
    else
        log "${C_RED}[ERRO] payload.zip ou r36s-agent não encontrado em $DEVBRIDGE_DIR.${C_RESET}"
        wait_and_finish 5 1
    fi

    log "\n${C_BOLD}[5/5] Ativando serviço r36s-agent...${C_RESET}"
    systemctl daemon-reload
    if ! systemctl enable r36s-agent.service 2>/dev/null; then
        log "${C_RED}[ERRO] Não foi possível habilitar o serviço no boot.${C_RESET}"
        wait_and_finish 8 1
    fi
    if ! systemctl --no-block restart r36s-agent.service 2>/dev/null; then
        log "${C_RED}[ERRO] Não foi possível solicitar o início do serviço.${C_RESET}"
        wait_and_finish 8 1
    fi
    # O restart com --no-block apenas enfileira o trabalho; confirme o listener.
    local attempt service_ready=0
    for attempt in {1..15}; do
        if systemctl is-active --quiet r36s-agent.service &&
            python3 -c 'import socket; socket.create_connection(("127.0.0.1", 7365), 0.3).close()' 2>/dev/null; then
            service_ready=1
            break
        fi
        sleep 1
    done
    if [ "$service_ready" -ne 1 ]; then
        log "${C_RED}[ERRO] O serviço não abriu a porta de controle 7365 após 15 segundos.${C_RESET}"
        local service_state recent_errors
        service_state=$(systemctl show r36s-agent.service -p ActiveState -p SubState -p Result -p ExecMainCode -p ExecMainStatus --no-pager 2>/dev/null || true)
        if [ -n "$service_state" ]; then
            log "${C_YELLOW}Estado do serviço:\n$service_state${C_RESET}"
        fi
        recent_errors=$(tail -n 8 /var/log/r36s-agent.log 2>/dev/null || true)
        if [ -n "$recent_errors" ]; then
            log "${C_YELLOW}Últimos registros do agente:\n$recent_errors${C_RESET}"
        fi
        wait_and_finish 8 1
    fi
    log "${C_GREEN}[OK] Serviço ativo e porta 7365 (Controle) respondendo localmente.${C_RESET}"

    # Validação e orientação final
    if [ "$REBOOT_REQUIRED" -eq 1 ]; then
        log "\n${C_YELLOW}==========================================================${C_RESET}"
        log "${C_YELLOW}          [AVISO] REINÍCIO DO CONSOLE NECESSÁRIO!         ${C_RESET}"
        log "${C_YELLOW}==========================================================${C_RESET}"
        log "O Device Tree foi alterado para modo periférico (USB OTG)."
        log "${C_BOLD}O kernel Linux exige um reinício completo para carregar o DTB.${C_RESET}"
        log "\n1. Desligue ou reinicie o R36S pelo menu do ArkOS."
        log "2. Conecte o cabo USB na porta OTG e no seu PC."
        log "3. No computador, abra o: ${C_CYAN}./app${C_RESET}"
        log "=========================================================="
        wait_and_finish 8 0
    else
        if ip link show dev usb0 >/dev/null 2>&1; then
            ip link set dev usb0 up 2>/dev/null || true
            ip -4 address replace 192.168.7.2/24 dev usb0 2>/dev/null || true
            log "${C_GREEN}[OK] Interface usb0 ativa com IP 192.168.7.2/24.${C_RESET}"
        else
            log "${C_YELLOW}[!] Interface usb0 aguardando conexão do cabo USB OTG.${C_RESET}"
        fi

        log "\n${C_GREEN}==========================================================${C_RESET}"
        log "${C_GREEN}         [PRONTO] R36S PREPARADO PARA O DEVBRIDGE         ${C_RESET}"
        log "${C_GREEN}==========================================================${C_RESET}"
        log "IP do Portátil:   ${C_BOLD}192.168.7.2${C_RESET}"
        log "Porta Controle:   ${C_BOLD}7365${C_RESET}"
        log "Porta Vídeo:      ${C_BOLD}7366${C_RESET}"
        log "\n1. Conecte o cabo USB na porta OTG e no seu PC."
        log "2. No computador, abra: ${C_CYAN}./app${C_RESET}"
        log "3. Pode iniciar qualquer jogo; o agente roda em segundo plano."
        log "=========================================================="
        wait_and_finish 5 0
    fi
}

# --- Ponto de Entrada / Menu Interativo ---
if [ "${1:-}" = "--uninstall" ]; then
    uninstall_and_restore
elif systemctl is-active --quiet r36s-agent.service 2>/dev/null || [ -f /usr/local/bin/r36s-agent ]; then
    print_header
    log "${C_GREEN}[OK] R36S DevBridge já instalado neste console!${C_RESET}"
    if systemctl is-active --quiet r36s-agent.service; then
        log "Status do serviço: ${C_GREEN}Ativo e rodando${C_RESET}"
    else
        log "Status do serviço: ${C_YELLOW}Inativo${C_RESET}"
    fi

    if ip link show dev usb0 >/dev/null 2>&1; then
        ip_info=""
        ip_info=$(ip -4 -o addr show dev usb0 2>/dev/null | awk '{print $4}' | head -n1)
        log "Rede usb0: ${C_GREEN}Conectado ($ip_info)${C_RESET}"
    else
        log "Rede usb0: ${C_YELLOW}Aguardando cabo USB OTG${C_RESET}"
    fi

    log "\n${C_BOLD}Selecione uma opção:${C_RESET}"
    log "  ${C_CYAN}[X / 1]${C_RESET} Reaplicar rede USB e reiniciar serviço"
    log "  ${C_CYAN}[A / 2]${C_RESET} Reinstalar / Atualizar componentes"
    log "  ${C_CYAN}[Y / 3]${C_RESET} Desinstalar (pressione duas vezes)"
    log "  ${C_CYAN}[B / 4]${C_RESET} Sair (Padrão)"
    log "\nPressione um botão [auto sair em 30s]: "

    choice="4"
    if [ -f "$MENU_HELPER" ]; then
        choice="$(python3 "$MENU_HELPER" 30 2>/dev/tty1)" || choice="4"
    else
        read -t 30 -n 1 choice < /dev/tty1 2>/dev/null || choice="4"
    fi
    echo ""

    case "$choice" in
        2)
            install_or_update
            ;;
        3)
            uninstall_and_restore
            ;;
        4)
            log "Saindo..."
            printf "\033c" > /dev/tty1 2>/dev/null || true
            exit 0
            ;;
        1)
            log "Reaplicando configuração de rede USB..."
            if ip link show dev usb0 >/dev/null 2>&1; then
                ip link set dev usb0 up 2>/dev/null || true
                ip -4 address replace 192.168.7.2/24 dev usb0 2>/dev/null || true
                log "${C_GREEN}[OK] Interface usb0 reconfigurada em 192.168.7.2/24.${C_RESET}"
            else
                log "${C_YELLOW}[!] usb0 ainda não está disponível; o serviço tentará carregar os módulos.${C_RESET}"
            fi
            if systemctl --no-block restart r36s-agent.service 2>/dev/null; then
                log "${C_YELLOW}[!] Reinício do serviço solicitado. Aguarde a rede USB aparecer no PC.${C_RESET}"
            else
                log "${C_RED}[ERRO] Não foi possível solicitar reinício do serviço.${C_RESET}"
                wait_and_finish 8 1
            fi
            wait_and_finish 4 0
            ;;
        *)
            log "${C_YELLOW}[!] Escolha inválida; saindo sem alterar a conexão.${C_RESET}"
            wait_and_finish 4 1
            ;;
    esac
else
    install_or_update
fi
