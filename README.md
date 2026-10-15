# 🕹 R36S DevBridge (ArkOS Dev Toolkit)

Kit de ferramentas para desenvolvedores do console portátil **R36S rodando ArkOS** (Rockchip RK3326, 640x480 IPS, Linux 4.4.189). 

Com o **DevBridge**, você conecta o console diretamente ao computador através da porta USB OTG para ter:
- **Espelhamento de Tela (640x480):** Viewport com o jogo ou emulador rodando no hardware real do console a 30 FPS sob demanda.
- **Telemetria de Hardware:** Monitoramento em tempo real de carga de CPU, frequências dos 4 núcleos Cortex-A35, temperatura térmica (°C), uso de memória RAM e GPU Mali.
- **Deploy Direto de Jogos:** Envio de ROMs, JARs, Ports e scripts `.sh` para as pastas do console sem precisar remover o cartão MicroSD.
- **Streaming de Logs:** Leitura ao vivo de `retroarch.log`, `java.log` (FreeJ2ME), `dmesg` do kernel e journal do sistema.
- **Gerenciador de Processos:** Encerramento de jogos travados (`SIGTERM`/`SIGKILL`) e reinício da EmulationStation à distância.

---

## 📁 Estrutura do Projeto

```
DevBridge/
├── app                               <-- Binário desktop executável para o PC Host
├── build.sh                          <-- Script mestre de compilação cruzada e release
├── plan.md                           <-- Especificação de arquitetura e decisões (/grill-me)
│
├── r36s-agent/                       <-- Daemon em Rust para o R36S (AArch64)
│   ├── Cargo.toml
│   ├── src/
│   │   ├── main.rs                   <-- Entrypoint do daemon
│   │   ├── server.rs                 <-- TCP listeners (7365 controle, 7366 vídeo)
│   │   ├── framebuffer.rs            <-- Captura /dev/fb0 via mmap com compressão LZ4
│   │   ├── metrics.rs                <-- Coleta de CPU, RAM, GPU Mali, temps e PIDs
│   │   ├── deploy.rs                 <-- Gravação atômica com verificação de SHA-256
│   │   └── process.rs                <-- Controle de processos e streaming de logs
│   └── systemd/
│       └── r36s-agent.service        <-- Arquivo de serviço para inicialização em background
│
├── r36s-studio/                      <-- Aplicativo Desktop com interface gráfica egui (PC)
│   ├── Cargo.toml
│   └── src/
│       ├── main.rs                   <-- Entrypoint do app desktop
│       ├── app.rs                    <-- Layout unificado com abas
│       ├── connection.rs             <-- Descoberta automática USB e sockets
│       ├── viewport.rs               <-- Renderizador 4:3 com filtro pixel-art
│       ├── telemetry.rs              <-- Gauges de hardware
│       ├── deploy_tab.rs             <-- Seletor de arquivos e envio de jogos
│       ├── logs_tab.rs               <-- Visualizador de logs com busca e auto-refresh
│       └── process_tab.rs            <-- Gerenciador de PIDs
│
├── r36s-port/                        <-- Arquivos de empacotamento para o console
│   ├── R36S DevBridge.sh             <-- Script de instalação e menu para /roms/ports/
│   ├── patch_dtb_usb.py              <-- Patch seguro do DTB com backup automático
│   └── build_payload.sh              <-- Gerador do payload.zip
│
└── dist/                             <-- Pacote pronto para copiar no cartão SD
    └── ports/
        ├── R36S DevBridge.sh
        └── r36s-devbridge/
            ├── patch_dtb_usb.py
            └── payload.zip
```

---

## 🚀 Como Usar

### 1. Preparar o R36S (Apenas na 1ª vez)
1. Insira o cartão MicroSD do R36S no computador (ou acesse a partição de ROMs).
2. Copie o conteúdo da pasta `dist/ports/` para `/roms/ports/` (ou `EASYROMS/ports/`).
3. Insira o cartão no R36S e ligue o console.
4. No menu principal da EmulationStation, acesse **Ports** e abra **R36S DevBridge**.
5. O console exibirá um checklist no terminal:
   - Remonta partições RW caso estejam em modo somente leitura.
   - Aplica o patch USB no DTB para habilitar modo periférico (com backup automático).
   - Carrega os módulos de rede USB Gadget (`dwc2`, `g_ether`).
   - Extrai o `r36s-agent` e ativa o serviço em segundo plano via `systemd`.
6. Quando exibir a mensagem de sucesso, pressione qualquer botão para retornar ao ArkOS.

### 2. Conectar e Desenvolver no PC
1. Conecte um cabo USB de dados na porta **OTG (porta da esquerda)** do R36S e a outra ponta no PC.
2. No seu computador, dentro desta pasta, execute:
   ```bash
   ./app
   ```
3. O aplicativo detectará o R36S automaticamente e conectará:
   - Status muda para: `🟢 Conectado em 192.168.7.2 (< 2ms)`.
4. Clique em **▶ Iniciar Stream (640x480)** para visualizar a tela ao vivo.
5. Você pode abrir qualquer jogo no R36S (RetroArch, Ports, etc.) e ele continuará sendo transmitido e monitorado!

---

## 🛠 Compilação e Testes

Para recompilar ambos os componentes (agente ARM64 e aplicativo desktop do PC):
```bash
./build.sh
```

Para rodar a suíte de testes automatizados:
```bash
# Testes do r36s-agent
cargo test --manifest-path r36s-agent/Cargo.toml

# Testes do r36s-studio
cargo test --manifest-path r36s-studio/Cargo.toml
```

---

## 🛡 Recuperação e Reversão
Se reabrir o atalho **R36S DevBridge** no menu Ports do console, ele exibirá um menu interativo com opções:
- **[1] Reiniciar Serviço & Rede USB:** recarrega a rede e o agente.
- **[2] Reinstalar / Atualizar Binários:** reinstala caso você tenha atualizado o `payload.zip`.
- **[3] Desinstalar e Restaurar DTB Original:** remove o serviço e restaura o arquivo `.dtb` original a partir do backup seguro.
