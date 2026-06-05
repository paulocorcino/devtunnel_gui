# CONTEXT — DevTunnel GUI

Aplicativo desktop (Windows, traybar) para gerenciar Microsoft Dev Tunnels com
experiência próxima a soluções pagas (estilo ngrok/expose), de forma pragmática e
sem overengineering.

## Glossário

### Tunnel (Túnel)
Recurso lógico no serviço Microsoft Dev Tunnels, identificado por um **Tunnel ID**.
Pode conter uma ou mais **Ports**. Tem expiração e regras de acesso.

### Requested Tunnel ID vs Real Tunnel ID
Ao criar um túnel com um nome desejado (ex.: `paulo-desktop-3000`), o serviço
devolve um **Real Tunnel ID** com sufixo gerado + região (ex.:
`paulo-desktop-diad0dn-3000.brs`). O nome pedido (Requested) ≠ o ID real. O app
precisa guardar o mapeamento.

### Group (Grupo)
Conceito de organização do app que mapeia **1:1 para um Dev Tunnel**. O usuário cria
grupos nomeados (ex.: `frontend`, `apis`) e aloca portas neles. Um grupo = um Tunnel
ID = uma conexão de host. Deletar o grupo deleta o túnel e todas as portas dele;
deletar uma porta não afeta as outras.

### Port (Porta)
Porta local exposta dentro de um túnel/grupo, com um **Protocol** (http/https/auto).
**Adicionar porta sempre pede o grupo de destino** (dropdown com default selecionado +
opção "novo grupo" inline). O caminho rápido é: número da porta → Enter.

### Host (Hospedar)
Ato de manter o túnel ativo servindo o tráfego da porta local para a URL pública.
No modelo nativo, a hospedagem roda **dentro do processo do app** via SDK
(Tunnel Host Connections), e não como processo externo.

### Public URL
URL pública gerada pelo serviço para acessar a porta hospedada
(ex.: `https://<tunnel>.<region>.devtunnels.ms`).

### Keep-alive (Liveness)
Manter a conexão de host de um grupo ativa, reconectando se o relay cair.
Responsabilidade do SDK rodando em processo. Distinto do Health probe.

### Health probe (Operacional)
Sonda HTTP periódica sobre as Public URLs dos grupos **hospedados**, classificando
em **3 estados**:
- **Operacional:** relay roteando E o serviço local respondeu.
- **Túnel ok, serviço caído:** relay responde mas upstream morto (ex.: 502/503 com
  página de erro do devtunnels). *Assinatura exata a confirmar na implementação.*
- **Fora:** URL inalcançável / grupo não hospedado.

Método: GET `/`, timeout ~5s, intervalo **configurável, default conservador 60s**,
só nos grupos ativos. Funciona porque os túneis usam **acesso anônimo** (URL
autenticada redirecionaria a sonda para a página de login).

## Decisões resolvidas

- **Arquitetura (sweet spot nativo):** o nativo é concentrado onde dá ganho real
  (hospedar). (ver ADR-0001)
  - **Management de conta** (create/list/delete/port/access/update) → subprocesso
    one-shot do `devtunnel.exe` direto (NÃO PowerShell), via `std::process::Command`.
    Operações rápidas, sem dor de processo longo. Necessário porque o token de
    *usuário* (nível conta) só existe no system secure key chain, sem comando
    documentado para extraí-lo.
  - **Hospedagem** (keep-alive, longa duração) → SDK Rust `tunnels` em processo
    (Tunnel Host Connections), autorizado por um token host-scoped emitido com
    `devtunnel token <id> --scopes host`.
- **Linguagem:** Rust — única opção do SDK oficial que tem Management API **e**
  Tunnel Host Connections (Go não tem hosting). Crate `tunnels` do repo
  `microsoft/dev-tunnels`, ativamente mantido (2026), v0.1.0, feature `connections`.
- **PowerShell:** eliminado. O CLI é chamado como binário direto, não via scripts PS.

### Risco em aberto (hospedagem)
Token host-scoped do `devtunnel token` tem vida limitada. Keep-alive de longa
duração pode exigir re-emissão periódica do token + reconexão. A confirmar no SDK.

## Decisões de modelo

- **Modelo de túnel:** agrupamento manual. Grupo = Tunnel (N portas). Há uma
  conexão de host por grupo. Adicionar porta sempre escolhe o grupo de destino.

## Decisões de UI/runtime

- **GUI:** Slint (declarativo, Rust, binário único). Tray via crate `tray-icon`
  (integra no event loop winit do Slint).
- **Runtime:** SDK `tunnels` é async/tokio → roda em thread de fundo (host
  connections + health probes); comunica com a UI Slint via canais. Management de
  conta (subprocesso `devtunnel.exe`) também fora da thread de UI.

## Decisões de ciclo de vida

- **Auto-start:** inicia com o Windows (registry HKCU Run), minimizado no tray.
- **Auto-resume:** re-hospeda os grupos que estavam ativos antes do último encerramento.
- **Fechar (X):** vai para o tray; encerrar de verdade só pelo menu do tray.
- **Expiração de login (limite imposto pela Microsoft):** o token de login do CLI
  vale só alguns dias. Quando o auto-host falha por login expirado, o app entra em
  estado **"Re-login"**: ícone de alerta no tray + notificação do Windows + botão
  "Entrar" que dispara `devtunnel user login` (abre navegador). Não há keep-alive
  verdadeiramente eterno sem re-autenticação periódica.

## Decisões de estado

- **Fonte da verdade:** o serviço Dev Tunnels. No startup e periodicamente,
  `devtunnel list -j` + `devtunnel port list -j` reconstroem a visão. JSON local
  (`%APPDATA%\devtunnel-gui\`) guarda só: conjunto de grupos a auto-hospedar +
  configurações (intervalo da sonda, auto-start). Cache leve para pintar a UI
  instantaneamente antes da reconciliação.
- **Parsing robusto (confirmado):** o CLI suporta `-j/--json` em list/show/port,
  então o subprocesso devolve JSON estruturado — fim do regex-scraping do
  Real Tunnel ID que causava dor no script atual. Comandos também aceitam
  `--access-token`.

## Escopo do MVP (v1)

**Núcleo:** criar/listar/deletar grupos · adicionar/remover portas (sempre escolhendo
grupo) · hospedar/parar grupo · auto-start + auto-resume · sonda 3-estados @60s ·
tray com toggle de janela · fluxo de re-login.

**Extras incluídos:** copiar URL + abrir no navegador (app e tray) · auto-renovação
de expiração (renova túnel **E** reaplica ACE de acesso anônimo — ambos expiram) ·
painel de configurações (intervalo da sonda, auto-start, expiração padrão).

**Notificações:** apenas o caso crítico de **re-login** (toast + alerta no tray).
Sem toasts de mudança de status comum.

**Fora do v1:** QR code, inspeção de tráfego (Inspect URL), multi-conta,
labels, controle de acesso por tenant/org.

### Opções nos diálogos de criação (mínimo + "Avançado" recolhível)
- **Grupo:** nome, expiração (default 30d), anônimo (default on), descrição.
- **Porta:** número, protocolo (default `auto`), descrição.
- **Avançado (recolhido):** `host-header`/`origin-header` (valor `unchanged` —
  resolve quebra de dev servers tipo Vite/HMR, webpack, virtual hosts/CORS, que o
  Dev Tunnels causa ao reescrever Host→localhost), `request-timeout`
  (0=desabilitado, útil p/ SSE/long-polling/uploads grandes).

## Notas de implementação
- **"Parar hospedagem":** derruba a conexão de host do SDK mas mantém o
  grupo/portas definidos no serviço (persistem, só não são servidos).
- **Nome do grupo = Tunnel ID:** sanitizar para o formato aceito (minúsculas,
  alfanumérico + hífen). Guardar mapeamento nome→Real Tunnel ID (com sufixo/região).
- **Auto-renovação:** atualizar expiração do túnel e recriar a ACE anônima, como o
  script atual já faz.
