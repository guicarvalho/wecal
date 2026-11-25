// src/views.rs
// Templates Askama (HTML gerado no servidor, parcial via HTMX).

use askama::Template;

use crate::merge_engine::{EventSource, MergedEvent};

// ------------------------------------------------------------------
// Templates (Askama tem acesso aos campos brutos do MergedEvent)
// ------------------------------------------------------------------

#[derive(Template)]
#[template(path = "login.html")]
pub struct LoginTemplate {
    pub error: Option<String>,
}

#[derive(Template)]
#[template(path = "presentation.html")]
pub struct PresentationTemplate;

#[derive(Template)]
#[template(path = "index.html")]
pub struct IndexTemplate {
    pub events: Vec<MergedEvent>,
    pub calendars: Vec<CalendarLegend>,
}

pub struct CalendarLegend {
    pub name: String,
    pub color: String,
}

#[derive(Template)]
#[template(path = "events.html")]
pub struct EventListTemplate {
    pub events: Vec<MergedEvent>,
}

// ------------------------------------------------------------------
// Página de conexões (Google OAuth + Apple .ics)
// ------------------------------------------------------------------

#[derive(Template)]
#[template(path = "settings.html")]
pub struct SettingsTemplate {
    pub google_configured: bool,
    pub google_connected: bool,
    pub google_last_sync: Option<String>,
    pub apple_connected: bool,
    pub apple_url: Option<String>,
    pub apple_last_sync: Option<String>,
    /// Flash da última ação (query string /settings?status=...)
    pub status: Option<String>,
    pub app_base_url: String,
}

// ------------------------------------------------------------------
// Fragmento de resultado da sincronização (retornado via HTMX)
// ------------------------------------------------------------------

pub struct SyncOutcomeView {
    pub provider: String,
    pub message: String,
    pub ok: bool,
}

#[derive(Template)]
#[template(path = "sync_status.html")]
pub struct SyncStatusTemplate {
    pub outcomes: Vec<SyncOutcomeView>,
}

// Helpers usados dentro dos templates Askama
pub trait EventViewHelpers {
    fn source_label(&self) -> &str;
    fn time_label(&self) -> String;
    fn date_label(&self) -> String;
}

impl EventViewHelpers for MergedEvent {
    fn source_label(&self) -> &str {
        self.calendar_name.as_deref().unwrap_or(match self.source {
            EventSource::Google => "Google",
            EventSource::Apple => "Apple",
            EventSource::Import => "Importado",
        })
    }
    fn time_label(&self) -> String {
        self.start.format("%H:%M").to_string()
    }
    fn date_label(&self) -> String {
        self.start.format("%d/%m").to_string()
    }
}
