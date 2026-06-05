# Design da UI — DevTunnel GUI

Direção visual: **ferramenta de dev SaaS, calma e status-forward** (vibe ngrok /
Tailscale / Linear). Compacta, neutra, com um único acento e cores de status fortes.
Pragmática: sem ilustrações, sem animações pesadas. O `Theme` global
([ui/theme.slint](../../ui/theme.slint)) é a fonte única de cores/espaçamento.

## Princípios

1. **Status em primeiro lugar.** O usuário abre o app para saber "minhas URLs estão no ar?". Cada porta tem um ponto de status sempre visível.
2. **URL é o produto.** A Public URL é destaque (fonte mono), copiável em 1 clique.
3. **Grupo organiza, porta é a unidade.** Lista agrupada por Grupo (=Tunnel); a linha acionável é a porta.
4. **Cru = bug.** Nada de widget default sem tema. Tudo passa pelo `Theme`.
5. **Dark mode de primeira classe** (dev usa à noite). Alterna via `Theme.dark`.

## Paleta (`Theme`)

| Token | Light | Dark | Uso |
|---|---|---|---|
| `bg` | `#ffffff` | `#0f1117` | fundo da janela |
| `surface` | `#f7f8fa` | `#171a21` | cartões de grupo, barras |
| `surface-2` | `#ffffff` | `#1e222b` | linha em hover / inputs |
| `border` | `#e5e7eb` | `#262b36` | divisores, contornos |
| `text` | `#1f2330` | `#e6e8ee` | texto principal |
| `muted` | `#6b7280` | `#9aa3b2` | legendas, metadados |
| `accent` | `#4f46e5` | `#6366f1` | ação primária, foco |
| `ok` | `#16a34a` | `#22c55e` | Operacional |
| `warn` | `#d97706` | `#f59e0b` | Túnel ok, serviço caído |
| `down` | `#dc2626` | `#ef4444` | Fora |
| `idle` | `#9ca3af` | `#9ca3af` | não hospedado / parado |

Métricas: `radius 8px` · `gap 8px` · `pad 12px` · fonte mono `Consolas` (URLs).
Tipografia: system UI (Segoe UI). Título 16–18 · seção 13 semibold · corpo 12–13 ·
legenda 11 muted.

## Estados de status (ponto + rótulo)

| Estado | Cor | Quando | Origem |
|---|---|---|---|
| Operacional | `ok` | relay roteia + serviço local responde | sonda (#4/#5) |
| Serviço caído | `warn` | relay responde, upstream morto (502/503) | sonda |
| Fora | `down` | URL inalcançável | sonda |
| Parado | `idle` | grupo não hospedado | estado local |
| Hospedando… | `accent` (pulsante leve) | conectando ao relay | transição |

## Layout (mockup)

```
┌────────────────────────────────────────────────────────────────────┐
│ Dev Tunnels            ● Conectado: paulo@…      [ Atualizar ] [ ⚙ ] │  ← top bar (surface)
├────────────────────────────────────────────────────────────────────┤
│  ┌──────────────────────────────────────────────────────────────┐  │
│  │ frontend                       expira em 29d        [ ●○ Host ]│  │  ← group card header
│  │ ───────────────────────────────────────────────────────────── │  │
│  │  ● 3000  http   https://9nfm43tl-3000.brs.devtunnels.ms  ⧉  ↗ │  │  ← port row (status·porta·proto·url·copiar·abrir)
│  │  ● 5173  auto   https://9nfm43tl-5173.brs.devtunnels.ms  ⧉  ↗ │  │
│  └──────────────────────────────────────────────────────────────┘  │
│  ┌──────────────────────────────────────────────────────────────┐  │
│  │ apis                           expira em 12d        [ ○● Host ]│  │
│  │  ● 8049  http   https://…-8049.brs.devtunnels.ms         ⧉  ↗ │  │
│  └──────────────────────────────────────────────────────────────┘  │
│                                                       [ + Novo grupo ]│
└────────────────────────────────────────────────────────────────────┘
```

Estado vazio: cartão central "Nenhum grupo ainda" + botão `+ Criar grupo`.

## Inventário de componentes (Slint)

Arquivos sob `ui/`, consumidos por `app-window.slint`:

- `theme.slint` — global `Theme` (✅ criado).
- `StatusDot` — círculo colorido por estado (+ tooltip com o rótulo).
- `Pill` — chip pequeno (expiração, protocolo, status do login).
- `IconButton` — botão fantasma para ações de linha (copiar ⧉, abrir ↗).
- `PrimaryButton` / `GhostButton` — ações filled / outline.
- `Toggle` — switch Host/Parar do cabeçalho do grupo.
- `GroupCard` — cartão: header (nome, expiração, toggle) + lista de `PortRow`.
- `PortRow` — status · porta · protocolo · URL mono · ações em hover.
- `Field` / `Select` / `Collapsible` — para os diálogos (#3): porta, grupo, "Avançado".
- `Toast` — confirmação efêmera ("URL copiada").

## Interações

- **Clique na URL** copia (toast "copiada"). Botões ⧉/↗ explícitos também.
- **Ações da linha** aparecem em hover (em telas pequenas, sempre visíveis).
- **Enter** confirma diálogos; **Esc** cancela.
- **Tray**: clique abre/fecha; menu Abrir/Sair (estado re-login muda o ícone — #5).
- Dark mode: seguir o tema do Windows quando viável; senão toggle no ⚙.

## Mapeamento para as fatias

| Slice | Entrega de UI no estilo acima |
|---|---|
| #1 (feito) | top bar + lista; agora repintada com `Theme` (base do sistema) |
| #3 | `GroupCard` real, diálogos com `Field/Select/Collapsible`, estado vazio, `+ Novo grupo` |
| #4 | `Toggle` Host, `StatusDot` 3-estados, `Toast` |
| #5 | estado/ícone de **re-login** no tray + pill de login na top bar |
| #6 | tela de ⚙ Configurações (intervalo da sonda, auto-start, expiração padrão, dark mode) |

> Regra para as próximas fatias: **nenhuma cor/medida hard-coded** — sempre `Theme.*`.
> Componentes novos viram arquivo em `ui/` e são reutilizados, não copiados.
