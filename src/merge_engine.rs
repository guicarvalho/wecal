// src/merge_engine.rs
// Lógica pura de negócios do WeCal: normalização e merge inteligente
// de eventos de provedores (Google / Apple) em uma linha do tempo única.

use chrono::{DateTime, TimeZone, Utc};
use serde::{Deserialize, Serialize};

// ------------------------------------------------------------------
// Structs de entrada — eventos brutos dos provedores
// ------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoogleEvent {
    pub id: String,
    pub summary: String,
    pub description: Option<String>,
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    pub is_private: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppleEvent {
    pub id: String,
    pub title: String,
    pub notes: Option<String>,
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    pub is_private: bool,
}

// ------------------------------------------------------------------
// Struct de saída — evento unificado exibido no WeCal
// ------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MergedEvent {
    pub id: String,
    pub title: String,
    pub description: Option<String>,
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    pub is_private: bool,
    pub source: EventSource,
    pub calendar_name: Option<String>,
    pub calendar_color: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum EventSource {
    Google,
    Apple,
    /// Evento vindo de upload manual de arquivo .ics/.zip (rota /import)
    Import,
}

// ------------------------------------------------------------------
// Normalização
// ------------------------------------------------------------------

impl From<GoogleEvent> for MergedEvent {
    fn from(ev: GoogleEvent) -> Self {
        Self::from_normalized(
            ev.id,
            ev.summary,
            ev.description,
            ev.start,
            ev.end,
            ev.is_private,
            EventSource::Google,
        )
    }
}

impl From<AppleEvent> for MergedEvent {
    fn from(ev: AppleEvent) -> Self {
        Self::from_normalized(
            ev.id,
            ev.title,
            ev.notes,
            ev.start,
            ev.end,
            ev.is_private,
            EventSource::Apple,
        )
    }
}

impl MergedEvent {
    /// Construtor central que garante a máscara de privacidade e a
    /// normalização de horários (fim nunca anterior ao início).
    fn from_normalized(
        id: String,
        title: String,
        description: Option<String>,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
        is_private: bool,
        source: EventSource,
    ) -> Self {
        let (start, end) = if end < start {
            (start, start)
        } else {
            (start, end)
        };
        let (title, description) = if is_private {
            ("Ocupado".to_string(), None)
        } else {
            (title, description)
        };
        Self {
            id,
            title,
            description,
            start,
            end,
            is_private,
            source,
            calendar_name: None,
            calendar_color: None,
        }
    }
}

// ------------------------------------------------------------------
// Merge principal
// ------------------------------------------------------------------

/// Mescla listas do Google e da Apple e ordena por início.
pub fn merge_events(google: Vec<GoogleEvent>, apple: Vec<AppleEvent>) -> Vec<MergedEvent> {
    let mut merged: Vec<MergedEvent> = google
        .into_iter()
        .map(MergedEvent::from)
        .chain(apple.into_iter().map(MergedEvent::from))
        .collect();

    merged.sort_by(|a, b| a.start.cmp(&b.start).then(a.end.cmp(&b.end)));

    merged
}

// ------------------------------------------------------------------
// Helpers de exibição (back-end seguro para o front-end)
// ------------------------------------------------------------------

/// Filtra eventos cujo intervalo intersecta [day_start, day_end).
pub fn events_for_day(
    events: &[MergedEvent],
    day_start: DateTime<Utc>,
    day_end: DateTime<Utc>,
) -> Vec<MergedEvent> {
    events
        .iter()
        .filter(|e| e.start < day_end && e.end > day_start)
        .cloned()
        .collect()
}

/// Retorna o meio-dia UTC de uma data (ano, mês, dia).
pub fn day_bounds(year: i32, month: u32, day: u32) -> Option<(DateTime<Utc>, DateTime<Utc>)> {
    let start = Utc.with_ymd_and_hms(year, month, day, 0, 0, 0).single()?;
    let end = start + chrono::Duration::days(1);
    Some((start, end))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn dt(hour: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 4, hour, 0, 0).unwrap()
    }

    #[test]
    fn private_events_are_masked() {
        let ev = GoogleEvent {
            id: "1".into(),
            summary: "Entrevista sigilosa".into(),
            description: Some("detalhe secreto".into()),
            start: dt(9),
            end: dt(10),
            is_private: true,
        };
        let merged: MergedEvent = ev.into();
        assert_eq!(merged.title, "Ocupado");
        assert_eq!(merged.description, None);
        assert!(merged.is_private);
    }

}
