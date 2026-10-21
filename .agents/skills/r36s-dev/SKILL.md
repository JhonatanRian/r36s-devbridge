---
name: r36s-dev
description: Guia de engenharia, ciclo de desenvolvimento automatizado (AI DevLoop), cross-compilação e diagnóstico seguro para Rockchip RK3326 e ArkOS no portátil R36S.
---

# R36S DevBridge: Engenharia & AI DevLoop no RK3326

Este guia orienta agentes de IA e desenvolvedores a compilar, implantar, executar, inspecionar e depurar código no portátil **R36S** rodando **ArkOS** com segurança operacional absoluta no hardware real.

---

## 1. O Ciclo de Vida: AI DevLoop

Para iterar com máxima velocidade e risco zero de danificar o cartão microSD ou travar o kernel:

```
[PC Host] Compilar ELF (aarch64)
    │
    ▼
[Deploy] r36s_deploy / cli deploy para @sandbox/ (tmpfs em RAM)
    │
    ▼
[Start] r36s_sandbox(start) / cli run (com handoff de DRM Master)
    │
    ▼
[Visual] r36s_screenshot / cli screenshot (validação do frame 640x480)
    │
    ▼
[Input] r36s_input / cli input (START, WAIT:500, A)
    │
    ▼
[Audit] r36s_status_and_metrics & r36s_logs (auditar CPU0, CMA, Temp, stderr)
    │
    ▼
[Stop] r36s_sandbox(stop) / cli stop (restauração garantida do EmulationStation)
```

### Regra de Ouro do Cartão SD
Binários de teste de desenvolvimento repetido **DEVEM** ser implantados em `@sandbox/` (ex: `@sandbox/meu_binario`).
- O prefixo `@sandbox/` é mapeado pelo agente para um diretório temporário montado em **RAM (`tmpfs`)** com permissão de execução (`0755`).
- Isso evita desgaste das células de memória flash do cartão microSD durante ciclos frequentes de compilação e teste.

---

## 2. Tabela de Equivalência: CLI vs Ferramentas MCP

O DevBridge oferece paridade total entre o servidor MCP nativo e os subcomandos CLI:

| Operação | Ferramenta MCP (`./app --mcp`) | Subcomando CLI (`./app cli ...`) |
| :--- | :--- | :--- |
| **Status & Métricas** | `r36s_status_and_metrics` | `./app cli status` |
| **Captura de Tela** | `r36s_screenshot` | `./app cli screenshot <caminho.png>` |
| **Entrada Gamepad** | `r36s_input` | `./app cli input <sequência>` |
| **Deploy Transacional** | `r36s_deploy` | `./app cli deploy <origem_pc> <destino_r36s> [--executable]` |
| **Iniciar Teste** | `r36s_sandbox(start)` | `./app cli run <comando_r36s>` |
| **Encerrar Teste** | `r36s_sandbox(stop)` | `./app cli stop` |
| **Logs em RAM** | `r36s_logs` | `./app cli logs [sandbox\|dmesg\|retroarch\|es\|agent]` |

---

## 3. Matriz de Compilação & Cross-Compilação RK3326

O SoC **Rockchip RK3326** possui 4 núcleos **ARM Cortex-A35** (arquitetura `aarch64` *in-order* de baixo consumo).

### Escolha do Target de Compilação

1. **Target Estático Padrão (Recomendado):** `aarch64-unknown-linux-musl`
   - Gera executáveis estáticos sem dependência de bibliotecas dinâmicas do sistema.
   - Imune a incompatibilidades de versão de glibc.
   - Comando em Rust: `cargo build --target aarch64-unknown-linux-musl --release`

2. **Target Dinâmico:** `aarch64-unknown-linux-gnu`
   - O ArkOS baseia-se no **Ubuntu 19.10 (Eoan)** com **glibc 2.30**.
   - Se compilar no host com glibc mais recente (ex: 2.35+ do Ubuntu 22.04+), o binário **não inicializará** no R36S com erro `version 'GLIBC_2.3x' not found`.
   - Só use GNU dinâmico caso necessite linkar contra bibliotecas já existentes no sistema do ArkOS (como `/usr/lib/aarch64-linux-gnu/libSDL2-2.0.so.0`), garantindo que o toolchain use a glibc 2.30 ou via container do Ubuntu 19.10.

### Flags de Otimização Cortex-A35
Para compiladores C/C++ ou Rust (via `RUSTFLAGS`):
```bash
-mcpu=cortex-a35 -O2
```
Ou flags equivalentes: `-march=armv8-a+crc+crypto -mtune=cortex-a35`.

---

## 4. Ambiente Gráfico SDL2 & Handoff DRM Master

No ArkOS, não existe servidor gráfico X11 ou Wayland. As aplicações gráficas renderizam diretamente no controlador KMS/DRM (`/dev/dri/card0`) ou via Framebuffer (`/dev/fb0`).

### Handoff do EmulationStation
- Quando o console está no menu principal, o processo `emulationstation` detém o **DRM Master** exclusivo.
- Para rodar um teste gráfico via `cli run` ou `r36s_sandbox(start)`, o DevBridge suspende temporariamente o EmulationStation (`exclusive_drm: true`) para ceder o controle do hardware ao binário em teste.
- Ao chamar `cli stop` ou quando o teste termina, o DevBridge restaura o EmulationStation automaticamente.

### Variáveis de Ambiente Obrigatórias para SDL2 no ArkOS
Ao rodar binários gráficos SDL2, o DevBridge injeta automaticamente no ambiente do sandbox:
```bash
SDL_VIDEODRIVER=KMSDRM
SDL_GAMECONTROLLERCONFIG_FILE=/opt/inttools/gamecontrollerdb.txt
XDG_RUNTIME_DIR=/run/user/1002
HOME=/home/ark
```

---

## 5. Playbook de Diagnóstico e Sintomas

### Sintoma 1: Tela Preta ou Falha de Inicialização com `EACCES` / Permission Denied
- **Causa:** O processo tentou abrir `/dev/dri/card0` enquanto outro processo (ex: EmulationStation) ainda era DRM Master.
- **Solução:** Certifique-se de executar via `cli run` ou `r36s_sandbox(action="start")`, que realizam o handoff automático coordenado. Se o menu estiver travado, chame `cli stop` para restabelecer o estado.

### Sintoma 2: Jogo fecha repentinamente sem mensagem de Out of Memory (RAM livre > 300 MB)
- **Causa:** Esgotamento do **CMA (*Contiguous Memory Allocator*)**.
- **Explicação:** A GPU ARM Mali-G31 e os planos do controlador DRM exigem blocos contíguos de memória física reservada (`cma_total_mb`). Se `cma_free_mb` zerar (consulte via `cli status`), a criação de janelas ou buffers DRM falha fatalmente.
- **Solução:** Feche shaders pesados do RetroArch ou reduza a quantidade de buffers e resolução da aplicação.

### Sintoma 3: Stutter / Áudio estalando com CPU global baixa (< 40%)
- **Causa:** **Saturação do núcleo `CPU0`**.
- **Explicação:** No RK3326, todas as interrupções de hardware (IRQs) da controladora USB `dwc2` e do subsistema de áudio convergem no núcleo 0. Um jogo pode parecer leve na média dos 4 núcleos, mas travar porque a `CPU0` está em 100%.
- **Solução:** Audite `cpu_core_usage` no `cli status`. Otimize loops de eventos e evite syscalls desnecessárias.

### Sintoma 4: Queda progressiva de performance após alguns minutos de teste
- **Causa:** **Thermal Throttling**.
- **Explicação:** Em temperaturas elevadas (> 75°C), o governor do kernel do RK3326 reduz os clocks da CPU e GPU para evitar superaquecimento.
- **Solução:** Monitore o campo `thermal_throttling` no `cli status`.

---

## 6. Proibições Críticas de Engenharia (Leis de Hardware)

1. **JAMAIS use `libc::mmap()` em `/dev/fb0`:**
   - No kernel Rockchip 4.4 do ArkOS, transições de display causam deadlock no mutex de memória, colocando o processo em **Uninterruptible Sleep (`STAT = D`)**. Utilize exclusivamente leituras síncronas `pread` (`read_exact_at`).
2. **JAMAIS utilize aceleradores de hardware VPU/MPP/RGA no daemon:**
   - O uso de encoders de vídeo por hardware ou conversões RGA no agente consome a reserva de memória CMA, quebrando os emuladores.
3. **NUNCA confie em `journalctl`:**
   - O serviço `systemd-journald` é mascarado para `/dev/null` no ArkOS por padrão para preservar o cartão SD. Os logs devem ser lidos de buffers de RAM (`cli logs`).
4. **NUNCA abra uma segunda conexão TCP direta para `192.168.7.2` enquanto a GUI `./app` estiver aberta:**
   - O agente opera com sessão única preemptiva. Uma nova conexão TCP derruba imediatamente a GUI do desenvolvedor. Sempre utilize o IPC local do PC quando a GUI estiver em execução.
