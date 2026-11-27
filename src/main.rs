// src/main.rs
// Ponto de entrada do servidor Axum + HTMX do WeCal.
// Lê DATABASE_URL, sobe o pool SQLx, aplica migrations, expõe as rotas
// da timeline (HTML parcial via HTMX), a API JSON de eventos e o fluxo de
// conexão das agendas reais (Google OAuth + feed .ics da Apple).

use std::{net::SocketAddr, str::FromStr, sync::Arc};

use askama::Template;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use axum::{
    extract::{Form, Multipart, Query, Request, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    middleware::{self, Next},
    response::{Html, IntoResponse, Redirect, Response},
    routing::{get, post},
    Json, Router,
};
use chrono::{DateTime, Utc};
use hmac::{Hmac, KeyInit, Mac};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use sqlx::{postgres::PgPoolOptions, PgPool};
use tower_http::compression::CompressionLayer;

use wecal::auth::{self, Pkce};
use wecal::config::Config;
use wecal::merge_engine::{merge_events, AppleEvent, EventSource, GoogleEvent, MergedEvent};
use wecal::sync::{import_ics_file, import_zip_file, is_supported_calendar_file, sync_all, ImportOutcome};
use wecal::views::{
    CalendarLegend, EventListTemplate, IndexTemplate, LoginTemplate, SettingsTemplate, SyncOutcomeView,
    PresentationTemplate, SyncStatusTemplate, GoogleAccount, ImportedCalendar,
};

#[derive(Clone)]
pub struct AppState {
    pub pool: PgPool,
    pub config: Arc<Config>,
}

// ------------------------------------------------------------------
// Erro de aplicação — vira resposta HTTP amigável no Axum
// ------------------------------------------------------------------

pub struct AppError(anyhow::Error);

impl IntoResponse for AppError {
    fn into_response(self) -> axum::response::Response {
        eprintln!("erro interno: {:?}", self.0);
        (StatusCode::INTERNAL_SERVER_ERROR, "erro interno").into_response()
    }
}

impl<E> From<E> for AppError
where
    E: Into<anyhow::Error>,
{
    fn from(err: E) -> Self {
        Self(err.into())
    }
}

type AppResult<T> = Result<T, AppError>;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // 1. Ambiente ----------------------------------------------------
    let database_url = std::env::var("DATABASE_URL")
        .expect("DATABASE_URL não definida. Ex.: postgres://wecal:wecal@localhost:5432/wecal");
    let port = std::env::var("PORT").unwrap_or_else(|_| "3000".into());
    let addr: SocketAddr = format!("0.0.0.0:{port}").parse().expect("PORT inválida");
    let config = Arc::new(Config::from_env());

    // 2. Pool de conexões SQLx (Postgres) ----------------------------
    let pool = PgPoolOptions::new()
        .max_connections(10)
        .connect_with(
            sqlx::postgres::PgConnectOptions::from_str(&database_url)?
                .ssl_mode(sqlx::postgres::PgSslMode::Prefer),
        )
        .await?;

    // 3. Migrations embutidas em tempo de compilação (migrations/) --
    sqlx::migrate!().run(&pool).await?;

    let state = AppState {
        pool: pool.clone(),
        config: config.clone(),
    };

    let app = Router::new()
        .route("/login", get(login_page_handler).post(login_post_handler))
        .route("/logout", post(logout_handler))
        .route("/", get(index_handler))
        .route("/apresentacao", get(presentation_handler))
        .route("/assets/presentation-week.png", get(presentation_week_asset))
        .route(
            "/assets/presentation-week-mobile.png",
            get(presentation_week_mobile_asset),
        )
        .route("/assets/presentation-month.png", get(presentation_month_asset))
        .route("/assets/presentation-event.png", get(presentation_event_asset))
        .route("/events", get(events_fragment_handler))
        .route("/api/events", get(events_json_handler))
        .route("/health", get(|| async { StatusCode::OK }))
        // Conexão de agendas reais
        .route("/settings", get(settings_handler))
        .route("/auth/google/login", get(google_login_handler))
        .route("/auth/google/callback", get(google_callback_handler))
        .route("/auth/apple", post(apple_connect_handler))
        .route("/disconnect", post(disconnect_handler))
        .route("/import/delete", post(delete_import_handler))
        .route("/sync", post(sync_handler))
        // Importação de arquivos .ics / .zip
        .route(
            "/import",
            get(import_page_handler).post(import_post_handler),
        )
        .with_state(state.clone())
        .layer(CompressionLayer::new())
        .layer(middleware::from_fn_with_state(state.clone(), auth_middleware));

    // 4. Sync automático a cada 15 minutos (1º tick é imediato) ------
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(15 * 60));
        loop {
            interval.tick().await;
            for outcome in sync_all(&pool, &config).await {
                eprintln!(
                    "[sync automática] {} — {}",
                    outcome.provider, outcome.message
                );
            }
        }
    });

    let listener = tokio::net::TcpListener::bind(addr).await?;
    println!("WeCal ouvindo em http://{}", listener.local_addr()?);

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

const SESSION_COOKIE: &str = "wecal_session";
const SESSION_MAX_AGE: u32 = 60 * 60 * 24 * 30;
type HmacSha256 = Hmac<Sha256>;

#[derive(Deserialize)]
struct LoginForm {
    access_key: String,
}

async fn login_page_handler(State(state): State<AppState>, headers: HeaderMap) -> AppResult<Response> {
    if session_is_valid(&headers, &state.config) {
        return Ok(Redirect::to("/").into_response());
    }
    let html = LoginTemplate { error: None }.render()?;
    let mut response = Html(html).into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(response)
}

async fn login_post_handler(
    State(state): State<AppState>,
    Form(form): Form<LoginForm>,
) -> AppResult<Response> {
    if !secret_matches(&form.access_key, &state.config.app_access_key_hash) {
        let html = LoginTemplate { error: Some("A chave informada é inválida.".into()) }.render()?;
        let mut response = (StatusCode::UNAUTHORIZED, Html(html)).into_response();
        response
            .headers_mut()
            .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
        return Ok(response);
    }

    let mut response = Redirect::to("/").into_response();
    response
        .headers_mut()
        .insert(
            header::SET_COOKIE,
            session_cookie(&state.config, Some(&new_session_token(&state.config))),
        );
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(response)
}

async fn logout_handler(State(state): State<AppState>) -> Response {
    let mut response = Redirect::to("/login").into_response();
    response
        .headers_mut()
        .insert(header::SET_COOKIE, session_cookie(&state.config, None));
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

async fn auth_middleware(State(state): State<AppState>, request: Request, next: Next) -> Response {
    let path = request.uri().path();
    if path == "/"
        || path == "/apresentacao"
        || path.starts_with("/assets/presentation-")
        || path == "/login"
        || path == "/health"
    {
        return next.run(request).await;
    }

    let is_api = path == "/api" || path.starts_with("/api/");
    let authenticated = session_is_valid(request.headers(), &state.config)
        || (is_api && api_key_is_valid(request.headers(), &state.config));
    if !authenticated {
        if is_api {
            let mut response = (
                StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({"error": "unauthorized"})),
            )
                .into_response();
            response.headers_mut().insert(
                header::WWW_AUTHENTICATE,
                HeaderValue::from_static("Bearer realm=\"wecal\""),
            );
            return response;
        }
        if request.headers().contains_key("hx-request") {
            let mut response = (StatusCode::UNAUTHORIZED, "Sessão expirada").into_response();
            response.headers_mut().insert("HX-Redirect", HeaderValue::from_static("/login"));
            return response;
        }
        return Redirect::to("/login").into_response();
    }

    let mut response = next.run(request).await;
    response.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

fn secret_matches(secret: &str, expected_hash: &[u8; 32]) -> bool {
    let actual_hash = Sha256::digest(secret.as_bytes());
    expected_hash
        .iter()
        .zip(actual_hash.iter())
            .fold(0u8, |difference, (expected, actual)| difference | (*expected ^ *actual))
        == 0
}

fn session_is_valid(headers: &HeaderMap, config: &Config) -> bool {
    let Some(cookie_header) = headers.get(header::COOKIE).and_then(|value| value.to_str().ok()) else {
        return false;
    };
    let Some(token) = cookie_header
        .split(';')
        .filter_map(|cookie| cookie.trim().split_once('='))
        .find_map(|(name, value)| (name == SESSION_COOKIE).then_some(value))
    else {
        return false;
    };
    let mut parts = token.split('.');
    let (Some(issued_at), Some(nonce), Some(signature), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return false;
    };
    let Ok(issued_at) = issued_at.parse::<i64>() else {
        return false;
    };
    let now = Utc::now().timestamp();
    if nonce.len() != 32
        || !nonce.bytes().all(|byte| byte.is_ascii_hexdigit())
        || now.saturating_sub(issued_at) > SESSION_MAX_AGE as i64
        || issued_at > now
    {
        return false;
    }
    let Ok(signature) = URL_SAFE_NO_PAD.decode(signature) else {
        return false;
    };
    let Ok(mut mac) = HmacSha256::new_from_slice(&config.app_access_key_hash) else {
        return false;
    };
    mac.update(format!("{issued_at}.{nonce}").as_bytes());
    mac.verify_slice(&signature).is_ok()
}

fn new_session_token(config: &Config) -> String {
    let issued_at = Utc::now().timestamp();
    let nonce = uuid::Uuid::new_v4().simple().to_string();
    let payload = format!("{issued_at}.{nonce}");
    let mut mac = HmacSha256::new_from_slice(&config.app_access_key_hash)
        .expect("hash SHA-256 válido como chave HMAC");
    mac.update(payload.as_bytes());
    format!("{payload}.{}", URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes()))
}

fn api_key_is_valid(headers: &HeaderMap, config: &Config) -> bool {
    let Some(value) = headers.get(header::AUTHORIZATION).and_then(|value| value.to_str().ok()) else {
        return false;
    };
    let Some((scheme, token)) = value.split_once(' ') else {
        return false;
    };
    if !scheme.eq_ignore_ascii_case("Bearer") {
        return false;
    }
    let token = token.trim();
    config.api_access_key_hashes.iter().fold(false, |matched, expected| {
        secret_matches(token, expected) | matched
    })
}

fn session_cookie(config: &Config, token: Option<&str>) -> HeaderValue {
    let (token, max_age) = token.map_or(("", 0), |token| (token, SESSION_MAX_AGE));
    let mut value = format!(
        "{SESSION_COOKIE}={token}; Path=/; HttpOnly; SameSite=Lax; Max-Age={max_age}"
    );
    if config.secure_cookie {
        value.push_str("; Secure");
    }
    HeaderValue::from_str(&value).expect("cookie de sessão válido")
}

// ------------------------------------------------------------------
// Dados da timeline
// ------------------------------------------------------------------

/// Busca os eventos brutos (com filtro opcional de janela), agrupa por provedor
/// e aplica o merge. O FullCalendar envia ?start=&end= em RFC3339.
async fn fetch_merged_events(
    pool: &PgPool,
    range: Option<(DateTime<Utc>, DateTime<Utc>)>,
) -> anyhow::Result<Vec<MergedEvent>> {
    type EventRow = (
        String,
        String,
        Option<String>,
        DateTime<Utc>,
        DateTime<Utc>,
        bool,
        bool,
        Option<String>,
        Option<String>,
        Option<String>,
    );

    let rows: Vec<EventRow> = match range {
        // Overlap com a janela pedida (feed do calendário)
        Some((from, to)) => {
            sqlx::query_as(
                r#"
                SELECT e.provider, e.title, e.description, e.start_at, e.end_at, e.is_private, e.is_transparent,
                       e.source_filename, COALESCE(c.calendar_name, g.calendar_name), c.calendar_color
                FROM wecal_events e
                LEFT JOIN wecal_import_calendars c ON e.provider = 'import' AND e.source_filename = c.source_filename
                LEFT JOIN wecal_connections g ON e.connection_id = g.id
                WHERE e.end_at > $1 AND e.start_at < $2
                ORDER BY e.start_at ASC
                "#,
            )
            .bind(from)
            .bind(to)
            .fetch_all(pool)
            .await?
        }
        None => {
            sqlx::query_as(
                r#"
                SELECT e.provider, e.title, e.description, e.start_at, e.end_at, e.is_private, e.is_transparent,
                       e.source_filename, COALESCE(c.calendar_name, g.calendar_name), c.calendar_color
                FROM wecal_events e
                LEFT JOIN wecal_import_calendars c ON e.provider = 'import' AND e.source_filename = c.source_filename
                LEFT JOIN wecal_connections g ON e.connection_id = g.id
                ORDER BY e.start_at ASC
                "#,
            )
            .fetch_all(pool)
            .await?
        }
    };

    let mut google = Vec::new();
    let mut apple = Vec::new();
    let mut transparent = Vec::new();
    let mut import_calendars = std::collections::HashMap::new();

    // Marcador p/ identificar os eventos de import após o merge:
    // Google/Apple entram com id vazio, então qualquer valor aqui é exclusivo.
    const IMPORT_MARK: &str = "__wecal_import__";
    const GOOGLE_MARK: &str = "__wecal_google__";

    for (provider, title, description, start_at, end_at, is_private, is_transparent, source_filename, calendar_name, calendar_color) in rows {
        if is_transparent {
            transparent.push((
                provider.clone(),
                title.clone(),
                description.clone(),
                start_at,
                end_at,
                is_private,
                calendar_name,
                calendar_color,
            ));
            continue;
        }
        match provider.as_str() {
            "google" => {
                let key = format!("{GOOGLE_MARK}{}", calendar_name.as_deref().unwrap_or("Google"));
                import_calendars.insert(key.clone(), (calendar_name, calendar_color));
                google.push(GoogleEvent {
                id: key,
                summary: title,
                description,
                start: start_at,
                end: end_at,
                is_private,
            })},
            "apple" => apple.push(AppleEvent {
                id: String::new(),
                title,
                notes: description,
                start: start_at,
                end: end_at,
                is_private,
            }),
            // Importados entram na fila da Apple (mesma forma) para
            // participar do merge e preservar ordenação e normalização.
            "import" => {
                let key = format!("{IMPORT_MARK}{}", source_filename.as_deref().unwrap_or_default());
                import_calendars.insert(key.clone(), (calendar_name, calendar_color));
                apple.push(AppleEvent {
                    id: key,
                    title,
                    notes: description,
                    start: start_at,
                    end: end_at,
                    is_private,
                });
            }
            _ => {}
        }
    }

    let mut merged = merge_events(google, apple);
    // Re-rotula os que entraram com o marcador: a proveniência real é Import.
    for ev in &mut merged {
        if ev.id.starts_with(IMPORT_MARK) || ev.id.starts_with(GOOGLE_MARK) {
            if ev.id.starts_with(IMPORT_MARK) { ev.source = EventSource::Import; }
            if let Some((name, color)) = import_calendars.get(&ev.id) {
                ev.calendar_name = name.clone();
                ev.calendar_color = color.clone();
            }
        }
    }
    merged.extend(transparent.into_iter().map(
        |(provider, title, description, start, end, is_private, calendar_name, calendar_color)| MergedEvent {
            id: String::new(),
            title: if is_private { "Ocupado".into() } else { title },
            description: if is_private { None } else { description },
            start,
            end: end.max(start),
            is_private,
            source: match provider.as_str() {
                "google" => EventSource::Google,
                "import" => EventSource::Import,
                _ => EventSource::Apple,
            },
            calendar_name,
            calendar_color,
        },
    ));
    merged.sort_by_key(|event| event.start);
    Ok(merged)
}

// ------------------------------------------------------------------
// Handlers da timeline
// ------------------------------------------------------------------

async fn index_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> AppResult<Html<String>> {
    if !session_is_valid(&headers, &state.config) {
        return Ok(Html(PresentationTemplate.render()?));
    }
    let events = fetch_merged_events(&state.pool, None).await?;
    let calendars = sqlx::query_as::<_, (String, String)>(
        "SELECT calendar_name, calendar_color FROM wecal_import_calendars ORDER BY calendar_name",
    )
    .fetch_all(&state.pool)
    .await?
    .into_iter()
    .map(|(name, color)| CalendarLegend { name, color })
    .collect();
    let html = IndexTemplate { events, calendars }.render()?;
    Ok(Html(html))
}

async fn presentation_handler() -> AppResult<Html<String>> {
    Ok(Html(PresentationTemplate.render()?))
}

async fn presentation_week_asset() -> Response {
    presentation_asset(include_bytes!("../assets/presentation-week.png"))
}

async fn presentation_week_mobile_asset() -> Response {
    presentation_asset(include_bytes!("../assets/presentation-week-mobile.png"))
}

async fn presentation_month_asset() -> Response {
    presentation_asset(include_bytes!("../assets/presentation-month.png"))
}

async fn presentation_event_asset() -> Response {
    presentation_asset(include_bytes!("../assets/presentation-event.png"))
}

fn presentation_asset(bytes: &'static [u8]) -> Response {
    (
        [
            (header::CONTENT_TYPE, "image/png"),
            (header::CACHE_CONTROL, "public, max-age=86400"),
        ],
        bytes,
    )
        .into_response()
}

async fn events_fragment_handler(State(state): State<AppState>) -> AppResult<Html<String>> {
    let events = fetch_merged_events(&state.pool, None).await?;
    let html = EventListTemplate { events }.render()?;
    Ok(Html(html))
}

/// JSON feed do FullCalendar: aceita ?start=&end= (RFC3339) e devolve só a janela.
async fn events_json_handler(
    State(state): State<AppState>,
    Query(params): Query<EventRange>,
) -> AppResult<Json<Vec<MergedEvent>>> {
    let range = parse_range(params.start.as_deref(), params.end.as_deref());
    let mut events = fetch_merged_events(&state.pool, range).await?;
    // Ids estáveis para o FullCalendar (o merge não gera id)
    for (i, ev) in events.iter_mut().enumerate() {
        ev.id = format!("ev-{i}");
    }
    Ok(Json(events))
}

#[derive(Deserialize, Default)]
struct EventRange {
    #[serde(default)]
    start: Option<String>,
    #[serde(default)]
    end: Option<String>,
}

fn parse_range(start: Option<&str>, end: Option<&str>) -> Option<(DateTime<Utc>, DateTime<Utc>)> {
    let from = DateTime::parse_from_rfc3339(start?)
        .ok()?
        .with_timezone(&Utc);
    let to = DateTime::parse_from_rfc3339(end?).ok()?.with_timezone(&Utc);
    (from < to).then_some((from, to))
}

// ------------------------------------------------------------------
// Página de conexões
// ------------------------------------------------------------------

#[derive(Deserialize, Default)]
struct StatusParams {
    #[serde(default)]
    status: Option<String>,
}

async fn settings_handler(
    State(state): State<AppState>,
    Query(params): Query<StatusParams>,
) -> AppResult<Html<String>> {
    let google_accounts = sqlx::query_as::<_, (uuid::Uuid, Option<String>, Option<DateTime<Utc>>, bool)>(
        "SELECT id, calendar_name, last_sync_at, last_error IS NOT NULL FROM wecal_connections WHERE provider = 'google' ORDER BY created_at"
    ).fetch_all(&state.pool).await?.into_iter().map(|(id, name, last, has_error)| GoogleAccount {
        id, has_error, name: name.unwrap_or_else(|| "Conta Google existente".into()),
        last_sync: last.map(|t| t.format("%d/%m %H:%M").to_string()),
    }).collect();
    let imported_calendars = sqlx::query_as::<_, (String, String)>(
        "SELECT source_filename, calendar_name FROM wecal_import_calendars ORDER BY calendar_name, source_filename"
    ).fetch_all(&state.pool).await?.into_iter().map(|(filename, name)| ImportedCalendar { filename, name }).collect();

    let apple: Option<(Option<String>, Option<DateTime<Utc>>)> = sqlx::query_as(
        "SELECT ics_url, last_sync_at FROM wecal_connections WHERE provider = 'apple_ics'",
    )
    .fetch_optional(&state.pool)
    .await?;

    let (apple_url, apple_last) = apple.map(|(u, s)| (u, s)).unwrap_or((None, None));

    let fmt = |t: Option<DateTime<Utc>>| t.map(|t| t.format("%d/%m %H:%M").to_string());

    let html = SettingsTemplate {
        google_configured: state.config.google_configured(),
        google_accounts,
        imported_calendars,
        apple_connected: apple_url.is_some(),
        apple_url: apple_url.clone(),
        apple_last_sync: fmt(apple_last),
        status: params.status,
        app_base_url: state.config.app_base_url.clone(),
    }
    .render()?;
    Ok(Html(html))
}

// ------------------------------------------------------------------
// Google OAuth — login (redirect para o Google)
// ------------------------------------------------------------------

async fn google_login_handler(State(state): State<AppState>) -> AppResult<Response> {
    if !state.config.google_configured() {
        return Ok(redirect_with_cookies(
            "/settings?status=google_nao_configurado",
            &[],
        ));
    }

    let state_token = uuid::Uuid::new_v4().to_string();
    let pkce = Pkce::generate();
    let auth_url = auth::build_auth_url(&state.config, &state_token, &pkce);

    let cookies = [
        format!("wecal_oauth_state={state_token}; Path=/; HttpOnly; SameSite=Lax; Max-Age=600"),
        format!(
            "wecal_oauth_verifier={}; Path=/; HttpOnly; SameSite=Lax; Max-Age=600",
            pkce.verifier
        ),
    ];
    Ok(redirect_with_cookies(&auth_url, &cookies))
}

// ------------------------------------------------------------------
// Google OAuth — callback (volta do Google)
// ------------------------------------------------------------------

#[derive(Deserialize, Default)]
struct CallbackParams {
    #[serde(default)]
    code: Option<String>,
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    error: Option<String>,
}

async fn google_callback_handler(
    State(state): State<AppState>,
    Query(params): Query<CallbackParams>,
    headers: HeaderMap,
) -> AppResult<Response> {
    // Usuário cancelou na tela do Google
    if params.error.is_some() {
        return Ok(redirect_with_cookies(
            "/settings?status=google_cancelado",
            &[
                clear_cookie("wecal_oauth_state"),
                clear_cookie("wecal_oauth_verifier"),
            ],
        ));
    }

    // Valida o state devolvido contra o cookie guardado no login
    let expected = cookie_header(&headers, "wecal_oauth_state");
    let verifier = cookie_header(&headers, "wecal_oauth_verifier");
    let state_ok = matches!(&expected, Some(e) if Some(e.as_str()) == params.state.as_deref());
    let Some(verifier) = verifier else {
        return Ok(redirect_with_cookies(
            "/settings?status=google_state_invalido",
            &[clear_cookie("wecal_oauth_state")],
        ));
    };
    if !state_ok {
        return Ok(redirect_with_cookies(
            "/settings?status=google_state_invalido",
            &[
                clear_cookie("wecal_oauth_state"),
                clear_cookie("wecal_oauth_verifier"),
            ],
        ));
    }
    let Some(code) = params.code else {
        return Ok(redirect_with_cookies("/settings?status=google_falha", &[]));
    };

    // Troca o code por tokens e salva a conexão
    match auth::exchange_code(&state.pool, &state.config, &code, &verifier).await {
        Ok(_) => {
            for outcome in sync_all(&state.pool, &state.config).await {
                eprintln!("[sync pós-auth] {} — {}", outcome.provider, outcome.message);
            }
            Ok(redirect_with_cookies(
                "/settings?status=google_ok",
                &[
                    clear_cookie("wecal_oauth_state"),
                    clear_cookie("wecal_oauth_verifier"),
                ],
            ))
        }
        Err(e) => {
            eprintln!("falha no exchange do Google: {e:#}");
            Ok(redirect_with_cookies(
                "/settings?status=google_falha",
                &[
                    clear_cookie("wecal_oauth_state"),
                    clear_cookie("wecal_oauth_verifier"),
                ],
            ))
        }
    }
}

// ------------------------------------------------------------------
// Apple/iCloud — conexão por link .ics
// ------------------------------------------------------------------

#[derive(Deserialize)]
struct AppleForm {
    ics_url: String,
}

async fn apple_connect_handler(
    State(state): State<AppState>,
    Form(form): Form<AppleForm>,
) -> AppResult<Response> {
    let url = form.ics_url.trim().to_string();
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Ok(Redirect::to("/settings?status=apple_url_invalida").into_response());
    }

    sqlx::query(
        r#"
        INSERT INTO wecal_connections (provider, ics_url, calendar_name)
        VALUES ('apple_ics', $1, 'iCloud')
        ON CONFLICT (provider) WHERE provider = 'apple_ics' DO UPDATE SET
            ics_url      = EXCLUDED.ics_url,
            calendar_name = EXCLUDED.calendar_name,
            updated_at   = now()
        "#,
    )
    .bind(&url)
    .execute(&state.pool)
    .await?;

    let report = sync_all(&state.pool, &state.config).await;
    let apple = report
        .iter()
        .find(|o| o.provider == "Apple/iCloud")
        .map(|o| (o.ok, o.message.clone()));
    let apple_ok = apple.as_ref().is_some_and(|(ok, _)| *ok);
    if let Some((false, msg)) = apple {
        eprintln!("sync Apple falhou: {msg}");
    }

    let status = if apple_ok { "apple_ok" } else { "apple_falha" };
    Ok(redirect_with_cookies(
        &format!("/settings?status={status}"),
        &[],
    ))
}

// ------------------------------------------------------------------
// Desconectar provedor (remove conexão + eventos sincronizados dele)
// ------------------------------------------------------------------

#[derive(Deserialize)]
struct DisconnectForm {
    provider: String,
    connection_id: Option<uuid::Uuid>,
}

async fn disconnect_handler(State(state): State<AppState>, Form(form): Form<DisconnectForm>) -> AppResult<Response> {
    let mut tx = state.pool.begin().await?;
    match (form.provider.as_str(), form.connection_id) {
        ("google", Some(id)) => {
            sqlx::query("DELETE FROM wecal_connections WHERE provider = 'google' AND id = $1")
                .bind(id).execute(&mut *tx).await?;
        }
        ("apple_ics", _) => {
            sqlx::query("DELETE FROM wecal_events WHERE provider = 'apple' AND external_id IS NOT NULL")
                .execute(&mut *tx).await?;
            sqlx::query("DELETE FROM wecal_connections WHERE provider = 'apple_ics'")
                .execute(&mut *tx).await?;
        }
        _ => return Ok(StatusCode::BAD_REQUEST.into_response()),
    }
    tx.commit().await?;
    Ok(Redirect::to("/settings?status=desconectado").into_response())
}

#[derive(Deserialize)]
struct DeleteImportForm { source_filename: String }

async fn delete_import_handler(State(state): State<AppState>, Form(form): Form<DeleteImportForm>) -> AppResult<Response> {
    let mut tx = state.pool.begin().await?;
    sqlx::query("LOCK TABLE wecal_import_calendars IN EXCLUSIVE MODE").execute(&mut *tx).await?;
    sqlx::query("DELETE FROM wecal_events WHERE provider = 'import' AND source_filename = $1")
        .bind(&form.source_filename).execute(&mut *tx).await?;
    sqlx::query("DELETE FROM wecal_import_calendars WHERE source_filename = $1")
        .bind(&form.source_filename).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(Redirect::to("/settings?status=desconectado").into_response())
}

// ------------------------------------------------------------------
// Sync manual (botão HTMX)
// ------------------------------------------------------------------

async fn sync_handler(State(state): State<AppState>) -> AppResult<Html<String>> {
    let report = sync_all(&state.pool, &state.config).await;
    let outcomes: Vec<SyncOutcomeView> = report
        .into_iter()
        .map(|o| SyncOutcomeView {
            provider: o.provider,
            message: o.message,
            ok: o.ok,
        })
        .collect();
    let html = SyncStatusTemplate { outcomes }.render()?;
    Ok(Html(html))
}

// ------------------------------------------------------------------
// Importação de arquivos .ics / .zip (página + handler)
// ------------------------------------------------------------------

#[derive(Template)]
#[template(path = "import.html")]
struct ImportTemplate {
    status: Option<String>,
}

async fn import_page_handler(Query(params): Query<StatusParams>) -> AppResult<Html<String>> {
    let html = ImportTemplate {
        status: params.status,
    }
    .render()?;
    Ok(Html(html))
}

// Template para resultado da importação (retornado via HTMX)
#[derive(Template)]
#[template(path = "import_result.html")]
struct ImportResultTemplate {
    outcomes: Vec<ImportOutcome>,
    any_ok: bool,
}

/// POST /import — recebe um arquivo .ics ou .zip via multipart/form-data,
/// importa e devolve o fragmento HTMX com o resultado de cada arquivo.
#[axum::debug_handler]
async fn import_post_handler(
    State(state): State<AppState>,
    mut multipart: Multipart,
) -> AppResult<Html<String>> {
    let mut outcomes: Vec<ImportOutcome> = Vec::new();
    let mut calendar_name = None;
    let mut calendar_color = None;
    let mut uploaded = None;

    while let Some(field) = multipart.next_field().await? {
        match field.name() {
            Some("calendar_name") => {
                let value = field.text().await?;
                calendar_name = (!value.trim().is_empty()).then_some(value);
            }
            Some("calendar_color") => {
                let value = field.text().await?;
                calendar_color = (!value.trim().is_empty()).then_some(value);
            }
            Some("ics_file") => {
                let filename = field.file_name().map(str::to_owned).unwrap_or_else(|| "arquivo".into());
                uploaded = Some((filename, field.bytes().await?));
            }
            _ => {}
        }
    }

    if let Some((filename, data)) = uploaded {
        let lower = filename.to_ascii_lowercase();

        let result = if lower.ends_with(".zip") {
            import_zip_file(&state.pool, &filename, &data).await
        } else if is_supported_calendar_file(&lower) {
            import_ics_file(&state.pool, &filename, &data, calendar_name.as_deref(), calendar_color.as_deref())
                .await
                .map(|o| vec![o])
        } else {
            Err(anyhow::anyhow!("arquivo não é .ics nem .zip"))
        };

        match result {
            Ok(mut os) => outcomes.append(&mut os),
            Err(e) => outcomes.push(ImportOutcome {
                filename,
                calendar_name: None,
                calendar_color: None,
                events_imported: 0,
                events_updated: 0,
                events_removed: 0,
                ok: false,
                error: Some(e.to_string()),
            }),
        }
    }

    if outcomes.is_empty() {
        outcomes.push(ImportOutcome {
            filename: "nenhum arquivo".into(),
            calendar_name: None,
            calendar_color: None,
            events_imported: 0,
            events_updated: 0,
            events_removed: 0,
            ok: false,
            error: Some("nenhum arquivo recebido no formulário".into()),
        });
    }

    let any_ok = outcomes.iter().any(|o| o.ok);
    let html = ImportResultTemplate { outcomes, any_ok }.render()?;
    Ok(Html(html))
}

// ------------------------------------------------------------------
// Cookies e redirects utilitários
// ------------------------------------------------------------------

fn cookie_header(headers: &HeaderMap, name: &str) -> Option<String> {
    let raw = headers.get(header::COOKIE)?.to_str().ok()?;
    raw.split(';')
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(k, _)| *k == name)
        .map(|(_, v)| v.to_string())
}

fn clear_cookie(name: &str) -> String {
    format!("{name}=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0")
}

/// Redirect 303 com cookies extras (Set-Cookie).
fn redirect_with_cookies(location: &str, cookies: &[String]) -> Response {
    let mut resp = Redirect::to(location).into_response();
    for c in cookies {
        if let Ok(value) = HeaderValue::from_str(c) {
            resp.headers_mut().append(header::SET_COOKIE, value);
        }
    }
    resp
}

// ------------------------------------------------------------------
// Shutdown gracioso
// ------------------------------------------------------------------

async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("falha ao instalar handler de Ctrl+C");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("falha ao instalar handler de SIGTERM")
            .recv()
            .await
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => println!("recebido Ctrl+C, encerrando..."),
        _ = terminate => println!("recebido SIGTERM, encerrando..."),
    }
}
