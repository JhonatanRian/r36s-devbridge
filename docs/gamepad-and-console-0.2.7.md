# Controles e console integrado — 0.2.7

## Alterações

- Backend evdev mantém propriedade dos arquivos, descobre controles por capacidades, consulta calibração real e publica relatórios completos (`SYN_REPORT`).
- Estado inicial e perda de eventos (`SYN_DROPPED`) usam consultas ao kernel. Transições de botões são preservadas numa fila de 256 entradas, com indicação de transbordamento.
- Monitoração somente para leitura, sem `EVIOCGRAB`; escrita aberta separadamente ao habilitar envio. Sem controle físico, não há simulação automática.
- Capacidade `gamepad-v2` negociada no handshake, mantendo o protocolo de vídeo. Clientes sem a capacidade continuam usando vídeo e métricas.
- Envio requer sessão explícita, renovada a cada 250 ms, com expiração de 1 s. Perda de foco, troca de aba e desconexão desativam envio; agente libera os comandos pendentes.
- Console vetorial na área principal, com vídeo na tela 4:3, LED da conexão, brilho e deslocamento dos analógicos. Sidebar mantém métricas e switches, com rolagem.
- Sondagem TCP/Ping não assume a sessão do Studio. Arquivos de vídeo antigos são descartados na reconexão.
- Pacote informa versão real e hashes de binário/serviço. Instalador compartilhado verifica, substitui atomicamente e mantém uma versão anterior. Ativação verifica o hash do processo e resposta Ping; falha tenta restaurar a versão anterior.

## Instalação

Execute `./build.sh`. No PC, abra `./app`. Para instalação offline, continue usando `sudo ./install_to_sd.sh`. O pacote completo de `dist/ports/` inclui agora `install_payload.py`; copie também esse arquivo ao atualizar o Port.

Não é necessário alterar novamente o DTB de um console com USB funcional. A instalação offline confirma arquivos; conexão só pode ser confirmada após boot e cabo OTG conectado.

## Limites e validação

A injeção usa o mesmo dispositivo evdev do controle, sem criar outro jogador. Eventos físicos e remotos para **o mesmo botão/eixo ao mesmo tempo** compartilham o estado do kernel; não há arbitragem independente entre esses dois emissores. Para controle remoto previsível, não opere o mesmo comando físico simultaneamente. Monitoração isolada não interfere com o jogo.

Testes automatizados cobrem relatórios incompletos, toque rápido, fila limitada, perda de eventos, calibração assinada, liberação de teclas, rejeição de envio sem sessão e instalação atômica/rollback. A prévia gráfica pode ser gerada com `cargo run --manifest-path r36s-studio/Cargo.toml --example preview` (arquivo `/tmp/devbridge-preview.ppm`).

Validação física necessária para aprovação visual final: cada botão e analógico; menu → jogo → menu; pausa/retomada; suspensão/reconexão; envio no EmulationStation e RetroArch. Medidas de CPU/FPS de uma cena estática não substituem comparação de carga e latência em jogos.
