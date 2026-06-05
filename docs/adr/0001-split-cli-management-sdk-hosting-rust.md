# Rust + CLI para management, SDK nativo para hospedagem

## Status
accepted — **confirmado** pelo spike #2 (2026-06-05): o SDK hospeda em processo
end-to-end (ver [docs/spikes/0001-sdk-hosting.md](../spikes/0001-sdk-hosting.md)).
A hospedagem fica atrás de um trait `TunnelHost`, deixando `devtunnel host` como
fallback barato caso a manutenção (token refresh / build OpenSSL) incomode.

## Decisão

O app é escrito em **Rust** e divide a integração com o Microsoft Dev Tunnels em
duas camadas: operações de **management de conta** (create/list/delete/port/access/
update/renew) são feitas chamando o binário `devtunnel.exe` como **subprocesso
one-shot** (`std::process::Command`, com saída `-j/--json`); a **hospedagem** de
longa duração (keep-alive) usa o **SDK Rust oficial `tunnels`** (feature
`connections`) rodando **em processo**, autorizado por um token host-scoped emitido
com `devtunnel token <id> --scopes host`.

## Contexto e motivação

O objetivo é um app de tray que mantenha URLs públicas vivas de forma confiável, com
experiência próxima a soluções pagas, sem PowerShell e sem overengineering. A dor do
script atual (PowerShell) é a hospedagem: disparar `devtunnel host` como processo
externo e raspar a URL do stdout, um processo por porta.

## Considered Options

- **Go:** descartado — o SDK oficial em Go tem Management API mas **não** tem Tunnel
  Host Connections; não consegue hospedar em processo, que é justamente o ganho-chave.
- **SDK nativo puro (inclusive auth):** mais elegante, mas o serviço Dev Tunnels é
  first-party da Microsoft; obter token de usuário (nível conta) exigiria ou ler o
  cache não-documentado do keychain, ou auto-registrar um app OAuth cujo escopo
  first-party pode **não ser concedido**. Risco alto para o caso comum (criar/listar).
- **CLI/PowerShell puro (status quo):** mantém a dor de N processos `devtunnel host`
  de longa duração + scraping de URL; é o que estamos saindo.

## Consequences

- O nativo (SDK) fica concentrado onde dá retorno: hospedagem em processo, uma
  conexão de host por grupo, sem scraping de stdout.
- Depende do `devtunnel.exe` instalado e logado. O token de login do CLI vale só
  alguns dias → existe um teto natural de keep-alive; o app trata isso com um estado
  explícito de **re-login** (ver CONTEXT.md).
- O SDK `tunnels` é v0.1.0 (preview, ativo em 2026) — risco de maturidade na camada
  de hospedagem; a camada de hospedagem deve ficar isolada atrás de uma interface
  para permitir fallback a `devtunnel host` se necessário.
- Token host-scoped tem vida limitada; keep-alive longo pode exigir re-emissão
  periódica + reconexão.
