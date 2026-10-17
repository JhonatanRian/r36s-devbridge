# Recuperação de transmissão 0.2.9

A falha de NES relatada ocorre no stream: o jogo continua funcionando no portátil.
A captura externa com shaders ainda depende da sincronização fornecida pelo driver do
kernel 4.4.189. Nenhum filtro reconstrói linhas da imagem com pixels de quadros antigos.

## Alterações

- Captura nativa RGB565/BGRA8888/RGBA8888, sem quantização obrigatória. O DRM fecha em cada
  captura, incluindo erros; exportações DMA-BUF usam close-on-exec. Poll e cache sync
  verificam retorno, com espera/tentativas limitadas. Copy e compressão são fases separadas.
- Uma captura completa por vez, sem fila de quadros na aplicação. Antes de capturar novamente,
  o agente verifica a fila TCP. Cada envio possui prazo total de 250 ms; um envio parcial que
  falha encerra a sessão. O receptor substitui o último quadro completo. Cadência adaptativa
  de 5–30 FPS preserva as cores e resolução quando o envio fica lento.
- Studio compartilha quadros imutáveis por Arc. Upload e screenshots não bloqueiam o mutex
  do receptor; screenshot executa em outra thread. Reconexão mantém brevemente a última
  imagem; falta de quadros é sinalizada após um segundo. UNCHANGED renova a validade de
  cenas estáticas sem copiar seus pixels. O instante local identifica quadros entre sessões.
- Consulta de teclado fora de ui.input; mapa único para ambos os modos; troca de modo,
  aba ou foco libera botões/analógicos. O agente também impõe exclusividade entre escuta
  e injeção. Reconexão descarta comandos pendentes e nunca restaura envio.
- Ping mede sua própria solicitação, separado do tempo de coleta de métricas.
- O PC cria um perfil NetworkManager dedicado, vinculado ao MAC e à interface USB do
  R36S identificado por VID/PID. O endereço persiste após reconexão; outras placas USB
  de rede não são reconfiguradas.
- Instalador único para SD/Port/atualização remota: verifica hashes, troca por rename,
  inclui configurações no rollback e remove hook antigo concorrente. Atualização remota
  roda em unidade systemd separada do agente substituído.
- Novas instalações geram MAC local persistente por aparelho; pares já configurados são
  preservados. O MAC persistente não substitui a configuração do IP no PC.
- Suspensão para o agente e desvincula g_ether antes dos hooks do ArkOS. ExecStopPost
  recupera gadget, interface e agente depois de systemd-sleep terminar. Não há disputa entre
  um hook chamado 99 e o hook do fabricante. O watchdog do agente não recarrega módulos.
- Instalação por SD exige EASYROMS/BOOT/root no mesmo disco identificado. Ambiguidade,
  somente leitura e EIO interrompem a operação. Não há varredura para desmontar outros discos.

## Verificação reproduzível

```
cargo test --manifest-path r36s-agent/Cargo.toml
cargo test --manifest-path r36s-studio/Cargo.toml
python3 -m unittest discover -s r36s-port/tests -v
./build.sh
# Feche o Studio para este diagnóstico ocupar uma única sessão de vídeo:
python3 tools/verify_stream.py --seconds 30 --check-controls
```

Diagnóstico do agente pode ser habilitado temporariamente com `DEVBRIDGE_DIAG=1` no
ambiente do serviço. Os logs mostram espera/sincronização, cópia, compressão, envio,
payload, saltos por fila TCP e cadência. Não subtraia relógios do PC e do console para
estimar latência. O intervalo entre quadros e Ping não medem a latência física de apresentação.

## Recuperação e instalação

`sudo ./install_to_sd.sh` instala no cartão. Se houver mais de um cartão compatível,
selecione explicitamente: `sudo DEVBRIDGE_SD_DEVICE=/dev/sdX ./install_to_sd.sh`.
Nunca use o disco do sistema.

O Port e a atualização de `r36s-devbridge/payload.zip` usam o instalador transacional.
No console, rollback explícito: `sudo python3 /usr/local/share/devbridge/install_payload.py
--rollback --activate` (na mesma linha). As configurações alteradas também são restauradas.
A desinstalação do Port remove a integração de suspensão e preserva o arquivo de MAC
compartilhado com possíveis configurações anteriores de USB SSH.

## Limites de aceite

Testes de código não comprovam fluidez de uma ROM específica. Comparar PSP e NES com/sem
shader na mesma cena; medir apresentação, FPS, banda e impacto no jogo. Se o produtor reutiliza
um buffer durante a leitura e o driver não oferece fence suficiente, a solução fiel exige
captura integrada ao RetroArch após renderizar e antes de reutilizar esse buffer. Essa
integração permanece condicionada à evidência; a versão não promete corrigir todos os shaders.

Os screenshots existentes, recomprimidos com lz4_flex, geraram aproximadamente 585 KB
em BGRA e 243 KB em RGB565. Isso não comprova capacidade USB nem os 40–70 KB prometidos
na implementação anterior. Medições de cena estática também não representam gameplay.

## Evidência no aparelho desta sessão

- Agente instalado e hash do processo conferido. Modos escutar/enviar confirmados pelo
  protocolo sem injetar botões. Testes de captura validaram a descompressão completa LZ4.
- PSP: amostra de 15 s com 376 quadros diferentes (~25,1 FPS), sem erro de vídeo;
  intervalo p95 de 50,2 ms e Ping p95 de 4,94 ms. Uma segunda amostra de 10 s, em outra
  cena, recebeu ~20,2 FPS (182 quadros diferentes), sem erro de vídeo. Isso não mede
  atraso físico de apresentação. Relatórios brutos estão em `validation-0.2.9/`.
- NES 1943 com shader: amostra de 15 s com 71 quadros diferentes (~4,7 FPS),
  sem erro de descompressão. Diagnóstico registrou captura média de ~140 ms e envio de
  ~62 ms em um intervalo, incluindo a compressão. Não foi declarado suporte
  fluido a shaders; o usuário aceitou documentar essa limitação.
- Primeiro ensaio de suspensão não recuperou USB. A recuperação foi ajustada para
  desvincular g_ether antes da remoção do controlador. Depois do ajuste, um teste de
  remoção/reinserção de dwc2 e um ciclo completo de suspensão recuperaram USB, IP e
  agente automaticamente, com o mesmo MAC. A série de dez ciclos não foi executada.
- Não houve medição controlada de atraso físico antes/depois nem a bateria de vinte
  lançamentos de ROM. Os números acima são amostras, não garantias de desempenho.

O prazo de 250 ms limita envio de rede, não uma chamada bloqueante interna do driver.
Nesse kernel, a thread de captura também foi observada em estado D durante a carga com shader.
A indicação de indisponibilidade evita apresentar imagem antiga como vídeo atual; não corrige
um bloqueio dentro do driver.
