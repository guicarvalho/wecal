# WeCal — Sua agenda a dois

Aplicativo voltado para casais que resolve o problema de agendas isoladas
em ecossistemas diferentes (ex.: um parceiro usa Google Calendar no trabalho
e o outro Apple Calendar/iCloud no pessoal), fazendo o merge inteligente
desses calendários em um **calendário único** (mês/semana/dia/lista).

**Stack:** Rust — Axum 0.8, SQLx 0.8, Askama 0.14, HTMX 2 + Tailwind via CDN,
**FullCalendar 7.1.3** (MIT, CDN com globals, sem bundler), reqwest 0.13 (sync),
ical 0.11 (feed Apple). Docker multi-stage com cargo-chef.

## Como rodar

```bash
cp .env.example .env
# Preencha APP_ACCESS_KEY e API_ACCESS_KEYS com chaves aleatórias (openssl rand -hex 32)
docker compose up --build
```

- App: http://localhost:3001 — pede a chave definida em `APP_ACCESS_KEY`
- API JSON: `GET /api/events` com `Authorization: Bearer <chave>` de `API_ACCESS_KEYS`
- Banco Postgres: localhost:5432 (usuario/senha/banco `wecal`)

As chaves ficam apenas no `.env`; o login usa um cookie HttpOnly e a API aceita
várias chaves Bearer, separadas por vírgula. `/health` permanece público para o
healthcheck do serviço. Configure `APP_BASE_URL` com HTTPS em produção para que
o cookie também receba o atributo `Secure`.

O banco sobe com o schema (`migrations/0001_init.sql`) e o seed mock
estruturado (`db/seed.sql`) no primeiro boot do volume, seguidos das
migrations do SQLx na inicialização da app.

## Arquitetura

```
wecal/
├── Cargo.toml            # Axum 0.8 + SQLx 0.8 + Askama 0.14 + reqwest/ical
├── Dockerfile            # cargo-chef → debian-slim
├── docker-compose.yml    # app + postgres com healthcheck
├── .env.example          # credenciais OAuth e chaves de acesso (copie para .env)
├── migrations/
│   ├── 0001_init.sql     # tabela wecal_events
│   ├── 0002_connections.sql  # wecal_connections + external_id/synced_at
│   ├── 0003_import_source.sql # coluna source_filename (importação)
│   └── 0004_import_provider.sql # CHECK provider estendido p/ 'import'
├── db/seed.sql               # mock estruturado (agenda do casal)
├── fixtures/agenda-teste.ics # feed p/ teste do parser iCalendar
├── templates/
│   ├── index.html        # FullCalendar v7 + popover + sync + logout
│   ├── login.html        # acesso por chave
│   ├── events.html       # fragmento parcial da timeline (rota /events)
│   ├── settings.html     # conexões (Google OAuth + link .ics Apple)
│   ├── import.html       # upload .ics/.zip + instruções de exportação
│   ├── import_result.html # resultado da importação (fragmento HTMX)
│   └── sync_status.html  # resultado da sync (retornado via HTMX)
└── src/
    ├── main.rs           # Axum + pool SQLx + handlers HTMX + auth + import
    ├── views.rs          # templates Askama
    ├── config.rs         # env vars e hashes de chaves (sem persistência no banco)
    ├── auth.rs           # OAuth 2.0 Google: PKCE, code→token, refresh
    ├── sync.rs           # Calendar API v3 + parser .ics + upsert + import .ics/.zip
    └── merge_engine.rs   # normalização, detecção de conflito, máscara de privacidade
```

## Interface — calendário (FullCalendar v7)

A página `/` renderiza o **FullCalendar v7** carregado via CDN (globals, sem
bundler/npm), com locale **pt-br** oficial:

- **Views:** Mês, Semana (time-grid), Dia e Lista — navegação `‹ › Hoje`
- **Fonte de dados:** `events: '/api/events'` — o FullCalendar envia
  `?start=&end=` (RFC3339) e o backend devolve só a janela pedida
- **Cores** (via `eventDataTransform`): 🟣 Google · 🔵 Apple · 🔴 conflito (⚠️) ·
  ⚪ privado (🔒, mascarado como "Ocupado")
- **Popover:** clique num evento mostra título, período, origem e descrição
  (renderizado com `textContent` — seguro contra HTML do banco)
- **Sync:** botão "⟳ Sincronizar" faz `POST /sync` (HTMX) e o calendário
  refaz o fetch automaticamente (`refetchEvents`)
- **Dark mode:** `data-color-scheme="dark"` + variáveis `--fc-monarch-*` em
  `#calendar` (as classes do tema são hashadas na v7 — não usar seletores
  `.fc .fc-*`, ver HANDOFF.md §4 lições 15–20)

## Conectar agendas reais

Tudo em **http://localhost:3001/settings** (botão "⚙️ Conexões" no topo da timeline).

### 📅 Google Calendar (OAuth 2.0 + PKCE)
1. Crie um projeto em [console.cloud.google.com](https://console.cloud.google.com)
2. Ative a **Google Calendar API** (APIs & Services → Library)
3. Preencha o **OAuth consent screen** e adicione seu e-mail como usuário de teste
4. Crie uma **OAuth client ID** (tipo *Web application*) com redirect URI:
   `http://localhost:3001/auth/google/callback`
5. Copie para o `.env` na raiz (veja `.env.example`) e reinicie:
   ```bash
   cp .env.example .env   # preencha GOOGLE_CLIENT_ID e GOOGLE_CLIENT_SECRET
   docker compose up -d
   ```
6. Clique em **Conectar** na página de conexões — pronto, sync automático a cada 15 min.

### 🍎 Apple / iCloud (link público .ics)
1. Abra [icloud.com/calendar](https://www.icloud.com/calendar)
2. Selecione o calendário → compartilhar (🔗) → **Public Calendar** → copie o link
3. Cole na página de conexões trocando `webcal://` por `https://` e clique **Conectar**

O sync busca eventos de −90 dias a +1 ano, faz upsert por `external_id`
(eventos atualizados mudam no lugar) e **remove** os que você apagar no provedor.
Eventos privados (`CLASS:PRIVATE` / `visibility: private`) chegam mascarados.

> **Limitação atual:** eventos recorrentes (RRULE) aparecem apenas como ocorrência
> inicial — expansão de recorrência é pendência futura (ver HANDOFF.md).

## Importar arquivos (.ics / .zip)

**http://localhost:3001/import** (link "📥 Importar agora" em Conexões) aceita
**um único `.ics` ou um `.zip`** contendo vários `.ics` (como o que o Google
Calendar baixa na exportação):

- Envia o arquivo e o resultado aparece na mesma página (HTMX): quantos eventos
  foram **novos** vs. **atualizados**, e entradas ignoradas (arquivo que não é `.ics`)
- A importação é **aditiva**: usa `provider='import'` com upsert por UID (não apaga
  nada dos seus eventos Google/Apple/seed); `source_filename` rastreia a origem
- A própria página traz o passo a passo de exportação do Google Calendar,
  Outlook/Microsoft 365 e Apple Calendar

## Rotas

| Método | Rota                     | Descrição                                  |
|--------|--------------------------|--------------------------------------------|
| GET    | `/`                      | Página principal com o calendário          |
| GET    | `/events`                | Fragmento da timeline (HTMX)              |
| GET    | `/api/events`            | Lista mesclada em JSON (`?start=&end=`)   |
| GET    | `/health`                | Health check                               |
| GET    | `/settings`              | Página de conexões das agendas             |
| GET    | `/auth/google/login`     | Inicia o OAuth do Google (redirect)        |
| GET    | `/auth/google/callback`  | Volta do Google — troca code por tokens    |
| POST   | `/auth/apple`            | Conecta o feed .ics do iCloud              |
| POST   | `/disconnect`            | Remove conexão e eventos sincronizados     |
| POST   | `/sync`                  | Sincroniza agora (retorna fragmento HTMX)  |
| GET    | `/import`                | Página de importação .ics/.zip             |
| POST   | `/import`                | Envia arquivo e retorna resultado (HTMX)   |

## Regras de negócio (merge_engine)

- **Normalização:** fim nunca anterior ao início (invariante garantida).
- **Privacidade:** eventos `is_private` viram `"Ocupado"` sem descrição.
- **Conflito:** sobreposição de intervalos marca `is_conflict = true` nos
  dois lados; eventos encostados (fim == início) não são conflito.
- **Ordenação:** linha do tempo ordenada por `start`, desempate por `end`.
