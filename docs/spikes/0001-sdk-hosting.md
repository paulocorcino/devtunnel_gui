# Spike #2 — Hospedagem nativa via SDK `tunnels` (HITL)

Valida a aposta do [ADR-0001](../adr/0001-split-cli-management-sdk-hosting-rust.md):
o SDK Rust `tunnels` (feature `connections`) consegue hospedar um túnel **em
processo**, autorizado por um host token emitido via `devtunnel token`.

## API confirmada (lendo o source do SDK)

- `new_tunnel_management("ua").into()` → `TunnelManagementClient` (auth `Anonymous` default).
- `TunnelLocator::ID { cluster, id }` — o id do CLI `nome.cluster` separa no último `.`.
- `RelayTunnelHost::new(locator, mgmt)` → `connect(host_token)` → `RelayHandle`
  (Future; completa ao desconectar) → `add_port(&TunnelPort)`.
- `add_port` chama `create_tunnel_port` com auth anônima mas **trata 409 (já existe)
  como OK** → não precisa de token de management se a porta já foi criada via CLI.
- Host token: `devtunnel token <id> --scopes host -j` → campo `token`.

## Achados de risco

### 1. Toolchain de build pesado no Windows (vendored OpenSSL)
O SDK fixa `russh`/`russh-keys` com feature **openssl**. Para não depender de
OpenSSL do sistema, usamos `vendored-openssl`, que compila OpenSSL do zero e exige:
- **NASM** (instalado via `winget install NASM.NASM`) — senão falha o assembly.
- **Strawberry Perl** (instalado via `winget`) — o perl do msys2 não tem
  `Locale::Maketext::Simple`, quebrando o `./Configure` do OpenSSL.
- Toolchain **MSVC** (já presente).

Implicação: build a partir do fonte no Windows é não-trivial. Para *distribuição*
é um custo único de dev (gera binário pronto), mas é um ponto contra a leveza.

### 2. Host token expira em ~24h
`devtunnel token --scopes host` retorna `lifeTime: 1.00:00:01`. Keep-alive de longa
duração precisa **re-emitir o token e reconectar** periodicamente (≤ 24h).

### 3. Hospedar precisa de DOIS tokens, não um
Um host token sozinho **não basta**: `connect()` autoriza o endpoint de relay com o
host token (ok), mas `add_port()` chama `create_tunnel_port` no mgmt client, que com
auth anônima retorna **401** (mesmo a porta já existindo — o 401 vem antes do 409).
Solução: dois tunnel tokens:
- `host` → passado em `host.connect(host_token)`.
- `manage:ports` → default auth do `TunnelManagementClient` (`add_port`).

### 4. Bug do CLI ao repetir `--scopes`
`devtunnel token ... --scopes host --scopes manage:ports` corrompe o 1º escopo para
`shost` (inválido). **Mintar um escopo por token** (chamadas separadas).

## Resultado em runtime — ✅ SUCESSO

```
relay conectado ✓
porta 3000 encaminhada ✓
Public URL: https://9nfm43tl-3000.brs.devtunnels.ms/
curl → DEVTUNNEL_SPIKE_OK  (HTTP 200)
```

Tráfego público chegou ao servidor local **sem processo `devtunnel host` externo**.
O SDK hospeda em processo. **ADR-0001 confirmado.**

## Recomendação

Seguir com hospedagem via SDK (ADR-0001). Encapsular atrás de um trait `TunnelHost`
(implementação SDK), mantendo a porta de fuga para fallback `devtunnel host` apenas se
a manutenção (token refresh / reconexão / build OpenSSL) se provar custosa demais.

### Implicações para as próximas fatias
- **#4 (hospedar):** mintar 2 tokens por grupo (`host` + `manage:ports`); reconectar
  e re-mintar antes de ~24h; "parar" = dropar o `RelayHandle`.
- **Build/CI:** documentar/automatizar NASM + Strawberry Perl + `vendored-openssl`
  (ou produzir binário pré-compilado para o usuário final).
