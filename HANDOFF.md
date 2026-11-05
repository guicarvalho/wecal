# WeCal — Handoff para o próximo agente

## 0. STATUS: ✅ APP NO AR + ✅ AGENDAS REAIS + ✅ UI DE CALENDÁRIO (FullCalendar v7) + ✅ IMPORTAÇÃO .ICS/.ZIP

**Fase 1 (base):** `docker compose up --build -d` funciona de ponta a ponta.
Checklist verificado:
- `curl http://localhost:3001/health` → **200**
- `curl http://localhost:3001/api/events` → JSON com **5 eventos**, sendo **4 com
  `is_conflict: true`** (dois pares sobrepostos: Daily×Pediatra e Entrevista×Reunião;
  o merge marca **os dois lados** de cada par — por isso 4 e não 2)
- `curl http://localhost:3001/` → 200 (página HTMX)
- `curl http://localhost:3001/events` → fragmento da timeline renderizado
- `docker compose logs app` → só `WeCal ouvindo em http://0.0.0.0:3000` (sem erros)

**Fase 2 (agendas reais):** Google OAuth (PKCE + refresh) e Apple/iCloud (.ics)
implementados e **testados end-to-end** com feed ICS local:
- Casos do parser validados: UTC (`Z`), `TZID=America/Sao_Paulo` (via chrono-tz),
  all-day (`VALUE=DATE`), `DURATION:PT45M`, `CLASS:PRIVATE` → máscara "Ocupado"
- Sync: upsert por `(provider, external_id)` + **stale delete** (evento removido
  do feed sumiu do banco) + disconnect preservando o seed (external_id NULL)
- OAuth: redirect para `accounts.google.com` com todos os parâmetros (state,
  PKCE S256, `access_type=offline`, `prompt=consent`), cookies HttpOnly,
  callback sem cookie → `google_state_invalido`, cancelado → `google_cancelado`
- Sync automática a cada 15 min (1º tick no boot) — logs em `[sync automática]`
- **Fluxo completo code→token NÃO testado** (exige Client ID real do usuário)

**Fase 3 (UI de calendário):** a timeline antiga foi substituída pelo
**FullCalendar v7.1.3 (MIT, CDN com globals — sem bundler)** em `templates/index.html`:
- Views **Mês / Semana / Dia / Lista** (time-grid estilo Google Calendar),
  locale **pt-br** oficial (`locales/pt-br/global.js`), indicador de "agora"
- JSON feed `/api/events?start=&end=` (janela enviada pelo FullCalendar; sem
  params retorna tudo — compat com o consumo antigo)
- Cores por proveniência/conflito/privacidade via `eventDataTransform`
  (campo `color` da v7) — dots/pills rosa (conflito), indigo (Apple),
  violeta (Google), cinza (🔒 privado)
- Popover de detalhes no clique do evento (`eventClick`, `textContent` — sem
  injeção de HTML), botão "⟳ Sincronizar" refaz `refetchEvents()` via
  `hx-on::after-request`
- Dark mode via `data-color-scheme="dark"` + vars `--fc-monarch-*` em `#calendar`
- **Validado visualmente** com Chrome headless (screenshot das 4 views + popover,
  zero erros de console) — ver lições 15–20 do §4

**Fase 4 (importação de arquivos):** rota `GET/POST /import` (página + handler HTMX)
aceita **um arquivo `.ics` OU um `.zip`** com múltiplos `.ics` (pedido do usuário):
- Upload multipart via HTMX (`hx-encoding="multipart/form-data"` no form — obrigatório;
  o `enctype` sozinho NÃO basta) → fragmento `import_result.html` no `#import-result`
- `provider='import'`, `external_id=UID`, upsert **aditivo** (sem stale delete);
  coluna `source_filename` rastreia a origem (migration 0003) e o CHECK de provider
  foi estendido para incluir `'import'` (migration 0004 — 0001 é sagrado)
- Página traz instruções de exportação do Google (zip direto), Outlook, Apple e genérico
- Testado E2E: import direto (+4), re-import idempotente (⟳4 atualizado), zip com
  2 `.ics` + 1 `.txt` (2 processados, 1 ignorado), `source_filename` conferido no banco
- **Lição grande:** a lib `zip 0.6` tem tipos `!Send` (`ZipFile`/`dyn AesCipher`) —
  ver lição 21 do §4

**Porta:** a app usa **3001** no host (a 3000 estava ocupada pelo dev server Nuxt do
projeto `postura-pro`, pid local). Ajuste com `WECAL_PORT=3000 docker compose up -d`
quando a 3000 estiver livre.

## 1. Onde paramos (estado atual)

Migração completa de **Leptos 0.5 + cargo-leptos** → **Axum 0.8 + HTMX 2 + Askama 0.14 +
SQLx 0.8**, com rebuild "do zero" (deps e Dockerfile modernos, legado Leptos removido),
**mais a fase de conexão real de agendas** (Google OAuth + Apple .ics).
Objetivos cumpridos (detalhes no §0). Lições de build no §4.

## 2. Arquitetura atual

```
wecal/
├── Cargo.toml            # Axum 0.8 + SQLx 0.8 + Askama 0.14 + reqwest 0.13
│                         #   + ical 0.11 + chrono-tz + sha2/base64 (PKCE)
├── Dockerfile            # chef → planner → builder → debian-slim (cargo-chef, rust 1.89)
│                         #   builder instala: pkg-config libssl-dev cmake (aws-lc-rs)
├── docker-compose.yml    # app (WECAL_PORT, padrão 3001) + postgres:16-alpine
│                         #   env: APP_BASE_URL, GOOGLE_CLIENT_ID/SECRET (via .env)
├── .env.example          # template das credenciais OAuth
├── migrations/
│   ├── 0001_init.sql     # wecal_events (provider IN ('google','apple'))
│   ├── 0002_connections.sql  # wecal_connections + external_id/synced_at + índice único
│   ├── 0003_import_source.sql # coluna source_filename (importação .ics/.zip)
│   └── 0004_import_provider.sql # CHECK provider estendido p/ 'import'
├── db/seed.sql               # 5 eventos mock (2 pares sobrepostos + 1 privado)
├── fixtures/agenda-teste.ics # feed de teste do parser (TZID, all-day, DURATION, PRIVATE)
├── templates/
│   ├── index.html        # FullCalendar v7 (CDN globals + locale pt-br) + nav
│   │                     #   "⚙️ Conexões" + Sincronizar (hx-post /sync → refetch)
│   ├── events.html       # fragmento parcial da timeline (rota /events ainda existe)
│   ├── settings.html     # conexões Google/Apple + instruções + sync manual
│   ├── import.html       # upload .ics/.zip + instruções de export (Google/Outlook/Apple)
│   ├── import_result.html # fragmento HTMX do resultado (badges novo/atualizado/erro)
│   └── sync_status.html  # relatório de sync (retornado via HTMX)
└── src/
    ├── lib.rs            # mods: auth, config, merge_engine, sync, views
    ├── main.rs           # rotas (abaixo) + AppError/AppResult + sync periódica
    ├── config.rs         # Config::from_env (APP_BASE_URL, GOOGLE_CLIENT_*)
    ├── auth.rs           # OAuth Google: Pkce, build_auth_url, exchange_code,
    │                     #   ensure_access_token (refresh automático)
    ├── sync.rs           # sync_all → sync_google (API v3 paginada) +
    │                     #   sync_apple_ics (reqwest + ical) + commit_sync transacional
    │                     #   + import_ics_file/import_zip_file/commit_import (adição)
    ├── views.rs          # Index/EventList/Settings/SyncStatus templates
    └── merge_engine.rs   # lógica de negócio — NÃO mexer (completa e testada)
```

Rotas: `GET /`, `/events`, `/api/events`, `/health`, `/settings`,
`/auth/google/login`, `/auth/google/callback`, `/import`; `POST /auth/apple`,
`/disconnect`, `/sync`, `/import`.

Fluxo OAuth: login gera `state` + PKCE (cookies HttpOnly, 10 min) → Google →
callback valida state, troca code, salva `wecal_connections` (provider `google`),
synca e redireciona `/settings?status=...`. Apple: form POSTa a URL `.ics`,
salva conexão (`apple_ics`) e synca — eventos entram com `provider='apple'`
(mesmo código de merge, zero mudança lá).

Rotas: `GET /` (HTML), `GET /events` (fragmento HTMX), `GET /api/events` (JSON), `GET /health`.

## 3. O que foi removido (legado Leptos)

- `src/app.rs` (componentes Leptos) — REMOVIDO
- `index.html` (template cargo-leptos), `tailwind.config.js`, `style/main.scss`, `.cargo/config.toml` — REMOVIDOS
- `src/lib.rs` antigo (re-exportava app) — reescrito para `pub mod merge_engine; pub mod views;`

## 4. Erros já resolvidos (lições — não recair nessas)

Todos estes já estão **corrigidos**; aqui fica o histórico para o próximo agente:

1. **`askama` não tem feature `axum`** (é framework-agnostic; `askama_axum` é crate
   separado e desnecessário — usamos `Template::render()` + `Html<String>`).
   ```toml
   askama = { version = "0.14" }   # NUNCA features = ["axum"]
   ```
2. **`uuid` não tem feature `sqlx`** — a integração vem do lado do SQLx
   (`sqlx` feature `uuid`); `uuid = { features = ["serde"] }` chega.
3. **`sqlx` feature `macros` removida** — `query!` exige DB em build time;
   usamos `sqlx::query()` em runtime de propósito (build Docker sem DB).
4. **Handlers Axum não podem retornar `anyhow::Result`** (não implementa
   `IntoResponse`) — `src/main.rs` tem `AppError` (wrap de `anyhow::Error` com
   `IntoResponse`) e o alias `type AppResult<T> = Result<T, AppError>`.
5. **`tokio::signal::ctrl_c()` retorna Future — precisa de `.await`** (em `async fn`
   mas fora de `select!`, o `?` não resolve sozinho).
6. **rustc antigo quebra deps transitivas** — `home`/`icu_*` exigem edition2024
   (Rust ≥1.85/1.88). Solução final: imagem **`cargo-chef:latest-rust-1.89`**.
7. **Seed vs. migration na ordem do initdb do Postgres** — o `docker-entrypoint-initdb.d`
   roda **antes** da app, então a migration tem que ser montada junto:
   `./migrations/0001_init.sql:/docker-entrypoint-initdb.d/01_init.sql:ro` antes do
   `02_seed.sql`. Sem isso: `relation "wecal_events" does not exist`.
8. **Porta 3000 do host ocupada** (dev server Nuxt do projeto `postura-pro`) —
   compose expõe `${WECAL_PORT:-3001}:3000`.
9. **`reqwest 0.13` mudou as features de TLS** — não existe mais `rustls-tls`;
   a feature correta é `rustls` (usa **aws-lc-rs**, que exige **`cmake`** no builder —
   já está no Dockerfile). E `form`/`query` **não são default** — precisam estar
   explícitos em `features = [...]`.
10. **API do crate `ical` 0.11**: import é `ical::IcalParser` (não
    `parser::ical::component::IcalParser`) e o construtor é `IcalParser::new(reader)`
    (não `::read`); `Property` fica em `ical::property::Property`.
11. **`uuid` precisa da feature `v4`** para `Uuid::new_v4()` (só `serde` não basta).
12. **Askama 0.14**: o filtro `default_if_none` no `settings.html` gerou código com
    `filters::` não resolvido — evite o filtro; use `{% if let Some(x) = campo %}`.
13. **`Redirect::to()` do Axum 0.8 não aceita `String`** (só `&str`/`Uri`) —
    use `redirect_with_cookies(&format!(...), &[])` do `main.rs`.
14. **Seed vs. migration na ordem do initdb do Postgres** — o `docker-entrypoint-initdb.d`
    roda **antes** da app, então a migration tem que ser montada junto:
    `./migrations/0001_init.sql:/docker-entrypoint-initdb.d/01_init.sql:ro` antes do
    `02_seed.sql`. Sem isso: `relation "wecal_events" does not exist`.

**FullCalendar v7 (UI de calendário) — lições, não recair:**

15. **`buttonText` NÃO existe mais na v7** (console: `Unknown option buttonText`).
    Os textos dos botões vêm do **locale**: inclua
    `<script src=".../fullcalendar@7.1.3/locales/pt-br/global.js">` após o bundle e
    use `locale: 'pt-br'` — o arquivo define `monthText:'Mês'`, `weekTextLong:'Semana'`,
    `dayText:'Dia'`, `listText:'Lista'`, `todayText:'Hoje'`, `prevText/nextText`.
16. **A v7 removeu `backgroundColor`/`borderColor`/`textColor`** (0 ocorrências no
    bundle) — o campo do evento é **`color`** + `contrastColor` (alimenta a CSS var
    `--fc-event-color`: dot, borda e fundo a 15%). Use no `eventDataTransform`.
17. **Classes do DOM são 100% hashadas** (`fc-monarch-*`, `fc-XX`) — seletores tipo
    `.fc .fc-button`, `.fc-daygrid-day-number` **não casam nada** (CSS morto silencioso).
    Customização correta: variáveis **`--fc-monarch-*`** em `#calendar { ... }`
    (primary, now, highlight, selected...) — é o que deixa o botão de view ativo indigo.
18. **Dark mode da paleta**: `palettes/purple.css` já traz o bloco
    `[data-color-scheme=dark]` — basta `data-color-scheme="dark"` no `<html>`
    (junto com `class="dark"` do Tailwind).
19. **`calendar.render()` dispara `datesSet` síncrono** — callbacks referenciam o DOM;
    declare (`var popover = document.getElementById(...)`) **antes** de `render()`.
    Sem isso: `Cannot read properties of undefined (reading 'classList')` e **nenhum
    evento renderiza** (o render aborta). `hidePopover` deve ter guarda (`if (popover)`).
20. **Clique em evento borbulha até `document`** — o listener global que fecha o
    popover rodaria no mesmo clique que o abre. Solução: `info.jsEvent.stopPropagation()`
    no `eventClick` + checagem `e.target.closest(...)` no listener global.
    *Nota:* na v7 **não existe classe `fc-event`** no DOM (é hashada) — para testar,
    selecione `#calendar [role="button"]`.

**Importação .ics/.zip — lições, não recair:**

21. **Tipos da lib `zip 0.6` NÃO são `Send`** (`ZipFile` contém `Box<dyn AesCipher>`) —
    mantê-los vivos **através de um `await`** num handler torna o future `!Send` e o
    Axum recusa com `E0277: Handler<_, _> is not satisfied` (o `#[axum::debug_handler]`
    aponta a culpa). **Correção:** extrair tudo em função **síncrona** (`extract_zip_entries`
    retorna `Vec<(nome, bytes)>`) e só depois fazer os `.await` de importação.
    O `Multipart` em si é `Send` e funciona normal com `State` como 1º extractor.
22. **`SELECT 1` retorna `INT4`** — `sqlx::query_scalar::<_, i64>` dá
    `mismatched types; Rust type i64 (INT8) not compatible with INT4`. Use `i32`
    (afetava `commit_import` e o `google_connected` do settings).
23. **CHECK constraint do `0001_init.sql`** só aceitava `('google','apple')` —
    provider `'import'` precisa de migration **nova** (`0004_import_provider.sql`
    faz DROP + ADD do constraint; nunca editar 0001, é sagrado).
24. **HTMX + upload de arquivo exige `hx-encoding="multipart/form-data"`** no form —
    o `enctype` HTML sozinho é ignorado pelo htmx (o POST chega sem o campo de arquivo).
25. **BUG: eventos importados não apareciam no calendário** — o `fetch_merged_events`
    (`main.rs`) agrupava linhas por provider com `match { "google" => ..., "apple" => ...,
    _ => {} }`; o `_ => {}` **descartava silenciosamente `provider='import'`** (a query
    até buscava, mas o merge os jogava fora). Correção: empurrar `import` na fila da
    Apple (mesma forma) com `id = "__wecal_import__"`, passar pelo `merge_events`
    (conflito/ordenação/normalização) e **re-rotular** `source = EventSource::Import`
    pós-merge. `EventSource::Import` foi **adicionado ao enum** (1 linha aditiva no
    merge_engine — lógica intacta), `views.rs` ganhou o arm `"Importado"` e o
    `index.html` a cor âmbar `#f59e0b` + badge 📥 + item na legenda.
    *Ao adicionar provider novo: verificar TODOS os `match`/`if` de provider
    (fetch, views, templates) — omissão não gera erro de compilação.*

**Teste de UI com Chrome headless** (não há browser interativo neste ambiente):
```bash
google-chrome --headless=new --no-sandbox --disable-gpu --hide-scrollbars \
  --window-size=1440,1150 --virtual-time-budget=15000 \
  --screenshot=/tmp/x.png http://localhost:3001
# erros de JS:
google-chrome --headless=new --no-sandbox --enable-logging=stderr --v=0 \
  --virtual-time-budget=12000 --dump-dom URL 2>&1 >/dev/null | grep -i console
```
Páginas `file://` **não conseguem fetch cross-origin** (CORS) — em teste, gere páginas
temporárias e use `--disable-web-security --user-data-dir=/tmp/...` só no Chrome de teste.

## 5. Checklist de verificação (EXECUTADO — tudo OK)

**Base:**
1. `docker compose down -v && docker compose up --build -d` ✅
2. `docker compose logs app` → `WeCal ouvindo em http://0.0.0.0:3000` ✅
3. `curl http://localhost:3001/health` → `200` ✅
4. `curl http://localhost:3001/api/events` → 5 eventos, 4 com `is_conflict: true` ✅
5. `curl http://localhost:3001/` → 200; `/events` → fragmento HTMX ✅
6. `docker compose down` encerra; `docker compose down -v` zera o volume (reaplica seed) ✅

**Agendas reais:**
7. `/settings` → 200 com instruções de Google Cloud e iCloud ✅
8. `/auth/google/login` **sem** credenciais → 303 `/settings?status=google_nao_configurado` ✅
9. `/auth/google/login` **com** env fake → 303 `accounts.google.com/o/oauth2/v2/auth?...`
   com `response_type`, `redirect_uri`, `scope=calendar.readonly`, `state`,
   `code_challenge` (S256), `access_type=offline`, `prompt=consent` + 2 cookies
   HttpOnly ✅ (teste manual — credenciais fake removidas depois)
10. `/auth/google/callback?code=x&state=y` sem cookie → `google_state_invalido`; com
    `?error=access_denied` → `google_cancelado` ✅
11. `POST /auth/apple` com ICS de teste (container nginx temporário na rede) →
    `apple_ok`; 4 eventos novos no merge com fuso/all-day/DURATION/PRIVADO corretos ✅
12. Remover evento do feed + `POST /sync` → relatório HTMX e total 9→8 (stale delete) ✅
13. `POST /disconnect` → total volta a 5 e seed intacto (`external_id IS NULL` preservado) ✅
14. Sync automática no boot loga `[sync automática]` com estado correto ✅

**UI de calendário (FullCalendar v7) — Chrome headless:**
15. Página `/` → 200; screenshot: dark mode, mês de novembro, "hoje" destacado,
    botões em PT-BR (Mês/Semana/Dia/Lista, Hoje), zero erros de console ✅
16. `/api/events?start=2030-01-01..` → 0 eventos; janela atual → 5 com ids `ev-N`;
    sem params → 5 (compat) ✅
17. View **Semana**: time-grid com pills coloridas, eventos sobrepostos lado a lado
    (Ocupado × Reunião), indicador de "agora" ✅
18. View **Dia**: time-grid + "QUARTA-FEIRA 4" + indicador de agora ✅
19. View **Lista**: agrupada por dia (5 NOV QUI / 7 NOV SÁB) com horários e ⚠️ ✅
20. Clique num evento → **popover** com título, período, badges (📅 Google /
    ⚠️ Conflito) e descrição; fecha com Esc/clique fora ✅

**Importação de arquivos (.ics / .zip):**
21. `GET /import` → 200; screenshot Chrome headless: upload + cards de instrução
    (Google/Outlook/Apple/genérico) no dark mode ✅
22. `curl -F ics_file=@fixtures/agenda-teste.ics /import` → **+4 novo(s)** e
    fragmento HTMX com "✅ Importação concluída" ✅
23. Re-import do mesmo arquivo → **⟳ 4 atualizado(s)** (upsert aditivo, idempotente) ✅
24. Zip com 2 `.ics` + 1 `.txt` → `agenda1.ics` e `agenda2.ics` processados
    (`+4` para o segundo, UIDs distintos) e `leia-me.txt` → "não é um arquivo .ics
    (ignorado)" ✅
25. Banco: `SELECT provider, source_filename, count(*)` → `import` com
    `agenda1.ics` (4) e `agenda2.ics` (4); google/apple/seed intactos ✅
26. `cargo check` → **zero warnings zero erros** (tzid_start e i64 limpos); migration
    0004 aplicada (constraint com `('google','apple','import')`) ✅
27. **Bug do loop corrigido**: `/api/events` sem params → 466 eventos (461 `Import`,
    3 Apple, 2 Google — antes só retornava 5); janela de novembro → 27 (22 `Import`);
    screenshot: eventos âmbar visíveis no mês + legenda com "Importado" + zero erros
    de console ✅

## 6. Instruções de modo de trabalho (acordadas com o usuário)

- **Corrigir em loop:** corrigir → `docker compose up --build` → ler erro → corrigir →
  repetir **só parar quando a app subir** (`curl /health` = 200).
- **Comunicação:** responder sempre em **pt-BR**; explicações e comentários de código em pt-BR.
- **Sem perguntas repetidas:** não confirmar o que já está decidido (stack = Axum + HTMX).
- **Não adicionar features** (auth, CRUD de eventos, OAuth Google/Apple) sem ser pedido —
  são fases futuras.

## 7. Pendências futuras (não fazer agora)

- **Testar o fluxo OAuth de ponta a ponta com um Client ID real** (único passo
  não validado da fase 2 — depende de credenciais do usuário no `.env`)
- Auth de casal (login/sessão) — hoje qualquer um acessa `/settings`
- Expandir **recorrências (RRULE)** do ICS/Google — hoje só a ocorrência inicial
- Sync incremental com `syncToken` (Google) em vez de janela −90d/+365d
- CRUD de eventos (criar/editar/excluir) via HTMX
- Testes automatizados do `sync.rs` (parser ICS tem fixture em `fixtures/`)
- Ajuste de timezone do usuário (sync converte para UTC; exibição em UTC)

## 8. Resumo técnico para o próximo agente

- **Stack final:** Rust 1.89 + Axum 0.8 + SQLx 0.8 + Askama 0.14 + HTMX 2.0.4 +
  Tailwind CDN + **FullCalendar 7.1.3 (MIT, CDN globals + locale pt-br)** +
  PostgreSQL 16 + reqwest 0.13 + ical 0.11 + chrono-tz + Docker (cargo-chef).
- **Estado:** ✅ no ar com agendas reais e UI de calendário validada — ver §0/§5.
- **Próximo passo natural (se pedido):** Client ID real no `.env` e teste E2E do
  Google OAuth (§7), depois auth de casal, RRULE e CRUD (gancho `eventDrop`/`eventClick`
  do FullCalendar já existe).
- **Arquivos sagrados (não mexer sem necessidade):** `src/merge_engine.rs`, `migrations/0001_init.sql`, `db/seed.sql`.
