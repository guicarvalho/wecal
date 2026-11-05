// src/auth.rs
// Fluxo OAuth 2.0 do Google Calendar (Authorization Code + PKCE + refresh token).
// Sem crate de terceiros: o fluxo é pequeno e controlado — monta a URL de
// consentimento, troca o code por tokens no Google e persiste em wecal_connections.

use anyhow::{bail, Context};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use chrono::{Duration, Utc};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use sqlx::PgPool;

use crate::config::Config;

pub const GOOGLE_SCOPE: &str = "https://www.googleapis.com/auth/calendar.readonly";
const AUTH_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";
const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";

// ------------------------------------------------------------------
// PKCE (RFC 7636) — protege a troca do code
// ------------------------------------------------------------------

#[derive(Clone)]
pub struct Pkce {
    pub verifier: String,
    pub challenge: String,
}

impl Pkce {
    /// Gera um par verifier/challenge novo a cada tentativa de login.
    pub fn generate() -> Self {
        let verifier = uuid::Uuid::new_v4().simple().to_string(); // 32 chars
        let digest = Sha256::digest(verifier.as_bytes());
        let challenge = URL_SAFE_NO_PAD.encode(digest);
        Self {
            verifier,
            challenge,
        }
    }
}

// ------------------------------------------------------------------
// URL de consentimento
// ------------------------------------------------------------------

pub fn build_auth_url(cfg: &Config, state: &str, pkce: &Pkce) -> String {
    let client_id = cfg
        .google_client_id
        .as_deref()
        .expect("build_auth_url só deve ser chamado com Google configurado");
    format!(
        "{AUTH_URL}?response_type=code\
         &client_id={client_id}\
         &redirect_uri={redirect}\
         &scope={scope}\
         &state={state}\
         &code_challenge={challenge}\
         &code_challenge_method=S256\
         &access_type=offline\
         &prompt=consent\
         &include_granted_scopes=true",
        redirect = urlencoding(&cfg.google_redirect_uri()),
        scope = urlencoding(GOOGLE_SCOPE),
        challenge = pkce.challenge,
    )
}

fn urlencoding(s: &str) -> String {
    // Escopo contém "/" e ":" — precisa de percent-encoding mínimo e previsível.
    s.replace(':', "%3A").replace('/', "%2F")
}

// ------------------------------------------------------------------
// Troca de code por tokens
// ------------------------------------------------------------------

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: Option<i64>,
}

/// Troca o `code` do callback por access/refresh token e salva (ou atualiza)
/// a conexão Google no banco. Retorna true se veio refresh_token novo.
pub async fn exchange_code(
    pool: &PgPool,
    cfg: &Config,
    code: &str,
    code_verifier: &str,
) -> anyhow::Result<bool> {
    let client = reqwest::Client::new();
    let resp = client
        .post(TOKEN_URL)
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", cfg.google_redirect_uri().as_str()),
            (
                "client_id",
                cfg.google_client_id.as_deref().unwrap_or_default(),
            ),
            (
                "client_secret",
                cfg.google_client_secret.as_deref().unwrap_or_default(),
            ),
            ("code_verifier", code_verifier),
        ])
        .send()
        .await
        .context("falha ao contatar o Google (token)")?;

    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        bail!("Google recusou o code ({status}): {body}");
    }
    let tokens: TokenResponse =
        serde_json::from_str(&body).context("resposta inválida do Google")?;

    let expires_at = Utc::now() + Duration::seconds(tokens.expires_in.unwrap_or(3600));
    let has_new_refresh = tokens.refresh_token.is_some();

    sqlx::query(
        r#"
        INSERT INTO wecal_connections (provider, access_token, refresh_token, expires_at, calendar_name, last_error)
        VALUES ('google', $1, $2, $3, 'Google Calendar', NULL)
        ON CONFLICT (provider) DO UPDATE SET
            access_token  = EXCLUDED.access_token,
            refresh_token = COALESCE(EXCLUDED.refresh_token, wecal_connections.refresh_token),
            expires_at    = EXCLUDED.expires_at,
            last_error    = NULL,
            updated_at    = now()
        "#,
    )
    .bind(&tokens.access_token)
    .bind(&tokens.refresh_token)
    .bind(expires_at)
    .execute(pool)
    .await
    .context("falha ao salvar conexão Google")?;

    Ok(has_new_refresh)
}

// ------------------------------------------------------------------
// Refresh do access token (expira em ~1h)
// ------------------------------------------------------------------

/// Garante um access token válido para a conexão Google; renova via refresh
/// token quando necessário. Retorna o access_token atual.
pub async fn ensure_access_token(pool: &PgPool, cfg: &Config) -> anyhow::Result<String> {
    let row: (Option<String>, Option<chrono::DateTime<Utc>>) = sqlx::query_as(
        "SELECT access_token, expires_at FROM wecal_connections WHERE provider = 'google'",
    )
    .fetch_optional(pool)
    .await?
    .context("Google não está conectado")?;

    let (token, expires_at) = row;
    let still_valid = matches!(expires_at, Some(t) if t > Utc::now() + Duration::minutes(5));
    if let (Some(t), true) = (token, still_valid) {
        return Ok(t);
    }

    let refresh_token = sqlx::query_scalar::<_, Option<String>>(
        "SELECT refresh_token FROM wecal_connections WHERE provider = 'google'",
    )
    .fetch_optional(pool)
    .await?
    .flatten()
    .context("sem refresh_token salvo — reconecte o Google")?;

    let client = reqwest::Client::new();
    let resp = client
        .post(TOKEN_URL)
        .form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token.as_str()),
            (
                "client_id",
                cfg.google_client_id.as_deref().unwrap_or_default(),
            ),
            (
                "client_secret",
                cfg.google_client_secret.as_deref().unwrap_or_default(),
            ),
        ])
        .send()
        .await
        .context("falha ao contatar o Google (refresh)")?;

    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        bail!("refresh token falhou ({status}): {body}");
    }
    let tokens: TokenResponse = serde_json::from_str(&body)?;
    let expires_at = Utc::now() + Duration::seconds(tokens.expires_in.unwrap_or(3600));

    sqlx::query(
        "UPDATE wecal_connections SET access_token = $1, expires_at = $2, updated_at = now() WHERE provider = 'google'",
    )
    .bind(&tokens.access_token)
    .bind(expires_at)
    .execute(pool)
    .await?;

    Ok(tokens.access_token)
}
