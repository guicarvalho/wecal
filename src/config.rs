// src/config.rs
// Configuração lida do ambiente (12-factor): base URL pública e credenciais
// do OAuth do Google. Segredo SEM versionado — só via env do docker-compose/.env.

use sha2::{Digest, Sha256};

#[derive(Clone, Debug)]
pub struct Config {
    /// URL pública da app (ex.: http://localhost:3001) — usada na redirect URI do OAuth.
    pub app_base_url: String,
    pub google_client_id: Option<String>,
    pub google_client_secret: Option<String>,
    pub app_access_key_hash: [u8; 32],
    pub api_access_key_hashes: Vec<[u8; 32]>,
    pub secure_cookie: bool,
}

impl Config {
    pub fn from_env() -> Self {
        let app_access_key = required_secret("APP_ACCESS_KEY");
        let api_access_keys = std::env::var("API_ACCESS_KEYS")
            .expect("API_ACCESS_KEYS não definida; informe uma ou mais chaves separadas por vírgula")
            .split(',')
            .map(str::trim)
            .filter(|key| !key.is_empty())
            .map(|key| {
                assert!(
                    key.len() >= 32,
                    "cada chave em API_ACCESS_KEYS deve ter ao menos 32 caracteres"
                );
                Sha256::digest(key.as_bytes()).into()
            })
            .collect::<Vec<[u8; 32]>>();
        assert!(!api_access_keys.is_empty(), "API_ACCESS_KEYS precisa conter ao menos uma chave");
        let app_base_url = std::env::var("APP_BASE_URL")
            .unwrap_or_else(|_| "http://localhost:3001".into());
        Self {
            secure_cookie: app_base_url.starts_with("https://"),
            app_base_url,
            google_client_id: non_empty(std::env::var("GOOGLE_CLIENT_ID").ok()),
            google_client_secret: non_empty(std::env::var("GOOGLE_CLIENT_SECRET").ok()),
            app_access_key_hash: Sha256::digest(app_access_key.as_bytes()).into(),
            api_access_key_hashes: api_access_keys,
        }
    }

    /// OAuth do Google só é habilitado se ID e secret estiverem definidos.
    pub fn google_configured(&self) -> bool {
        self.google_client_id.is_some() && self.google_client_secret.is_some()
    }

    /// Redirect URI registrada no Google Cloud Console.
    pub fn google_redirect_uri(&self) -> String {
        format!("{}/auth/google/callback", self.app_base_url)
    }
}

fn required_secret(name: &str) -> String {
    let secret = std::env::var(name).unwrap_or_else(|_| panic!("{name} não definida no ambiente"));
    let secret = secret.trim().to_owned();
    assert!(secret.len() >= 32, "{name} deve ter ao menos 32 caracteres");
    secret
}

fn non_empty(v: Option<String>) -> Option<String> {
    v.filter(|s| !s.trim().is_empty())
}
