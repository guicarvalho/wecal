// src/sync.rs
// Sincronização das agendas REAIS para o banco local:
//   - Google Calendar API v3 (JSON, paginado, via OAuth access token)
//   - Apple/iCloud via feed público .ics (parse iCalendar)
//   - Importação manual de arquivos .ics ou .zip (via upload)
// Estratégia comum: numa transação, zera `synced_at` do provedor, faz upsert
// por (provider, external_id) e remove o que deixou de existir no provedor.

use std::collections::HashMap;
use std::io::{BufReader, Cursor};

use anyhow::{bail, Context};
use chrono::{DateTime, Duration, NaiveDate, NaiveDateTime, TimeZone, Utc};
use chrono_tz::Tz;
use serde::Deserialize;
use sqlx::{PgPool, Postgres, Transaction};

use crate::auth;
use crate::config::Config;

// ------------------------------------------------------------------
// Relatório de sync (renderizado no HTMX)
// ------------------------------------------------------------------

#[derive(Debug)]
pub struct SyncOutcome {
    pub provider: String,
    pub message: String,
    pub ok: bool,
}

/// Sincroniza todas as conexões ativas e devolve um relatório por provedor.
pub async fn sync_all(pool: &PgPool, cfg: &Config) -> Vec<SyncOutcome> {
    let mut report = Vec::new();

    match sqlx::query_as::<_, (uuid::Uuid, Option<String>)>("SELECT id, calendar_name FROM wecal_connections WHERE provider = 'google' ORDER BY created_at").fetch_all(pool).await {
        Ok(connections) => for (id, name) in connections {
            let label = name.unwrap_or_else(|| "Google".into());
            match sync_google(pool, cfg, id).await {
                Ok(n) => report.push(ok(&label, format!("{n} eventos sincronizados"))),
                Err(e) => {
                    if let Err(update_error) = sqlx::query("UPDATE wecal_connections SET last_error = 'Falha na sincronização' WHERE id = $1")
                        .bind(id).execute(pool).await { eprintln!("falha ao registrar erro de sincronização: {update_error}"); }
                    report.push(err(&label, e));
                },
            }
        },
        Err(e) => report.push(err("Google", e)),
    }

    match sync_apple_ics(pool).await {
        Ok(n) => report.push(ok("Apple/iCloud", format!("{n} eventos sincronizados"))),
        Err(e) => report.push(err("Apple/iCloud", e)),
    }

    report
}

fn ok(provider: &str, message: String) -> SyncOutcome {
    SyncOutcome {
        provider: provider.into(),
        message,
        ok: true,
    }
}
fn err(provider: &str, e: impl std::fmt::Display) -> SyncOutcome {
    SyncOutcome {
        provider: provider.into(),
        message: e.to_string(),
        ok: false,
    }
}

// ------------------------------------------------------------------
// Google Calendar API v3
// ------------------------------------------------------------------

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GEvent {
    id: String,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    summary: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    visibility: Option<String>,
    #[serde(default)]
    transparency: Option<String>,
    start: GTime,
    end: Option<GTime>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GTime {
    #[serde(default)]
    date: Option<String>,
    #[serde(default)]
    date_time: Option<String>,
}

#[derive(Deserialize)]
struct GList {
    #[serde(default)]
    items: Vec<GEvent>,
    #[serde(default)]
    next_page_token: Option<String>,
}

async fn sync_google(pool: &PgPool, cfg: &Config, connection_id: uuid::Uuid) -> anyhow::Result<usize> {
    if !cfg.google_configured() {
        bail!("GOOGLE_CLIENT_ID/GOOGLE_CLIENT_SECRET não configurados");
    }
    let token = auth::ensure_access_token(pool, cfg, connection_id).await?;
    let client = reqwest::Client::new();
    let calendar_id = auth::primary_calendar_id(&token).await?;
    sqlx::query("UPDATE wecal_connections SET google_calendar_id = $1, calendar_name = $1 WHERE id = $2 AND google_calendar_id IS NULL")
        .bind(&calendar_id).bind(connection_id).execute(pool).await?;

    // Janela: 90 dias no passado até 1 ano no futuro (fase 1 — sem syncToken)
    let time_min = (Utc::now() - Duration::days(90)).to_rfc3339();
    let time_max = (Utc::now() + Duration::days(365)).to_rfc3339();

    let mut events: Vec<GEvent> = Vec::new();
    let mut page_token: Option<String> = None;

    loop {
        let mut req = client
            .get("https://www.googleapis.com/calendar/v3/calendars/primary/events")
            .bearer_auth(&token)
            .query(&[
                ("singleEvents", "true"),
                ("showDeleted", "false"),
                ("orderBy", "startTime"),
                ("maxResults", "250"),
                ("timeMin", time_min.as_str()),
                ("timeMax", time_max.as_str()),
            ]);
        if let Some(t) = &page_token {
            req = req.query(&[("pageToken", t.as_str())]);
        }

        let resp = req
            .send()
            .await
            .context("falha ao contatar a Calendar API")?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            bail!("Calendar API recusou ({status}): {body}");
        }
        let page: GList = resp
            .json()
            .await
            .context("resposta inválida da Calendar API")?;
        let next = page.next_page_token;
        events.extend(page.items);
        match next {
            Some(t) => page_token = Some(t),
            None => break,
        }
    }

    // Converte para linhas prontas (id do Google = external_id)
    let mut rows: Vec<EventRow> = Vec::new();
    for ev in events {
        if ev.status.as_deref() == Some("cancelled") {
            continue;
        }
        let Some((start, end)) = event_window(&ev) else {
            continue;
        };
        let mut title = ev.summary.unwrap_or_else(|| "Sem título".into());
        if ev.status.as_deref() == Some("tentative") {
            title = format!("⚠ Provisório · {title}");
        }
        rows.push((
            format!("{connection_id}::{}", ev.id),
            title,
            ev.description,
            start,
            end,
            ev.visibility.as_deref() == Some("private"),
            ev.transparency.as_deref() == Some("transparent"),
        ));
    }

    commit_sync(pool, "google", Some(connection_id), rows).await
}

/// Resolve a janela (start, end) de um evento Google, incluindo all-day (date).
fn event_window(ev: &GEvent) -> Option<(DateTime<Utc>, DateTime<Utc>)> {
    let start = parse_gtime(&ev.start)?;
    let end = ev
        .end
        .as_ref()
        .and_then(parse_gtime)
        .unwrap_or_else(|| start + Duration::hours(1));
    Some((start, end.max(start)))
}

fn parse_gtime(t: &GTime) -> Option<DateTime<Utc>> {
    if let Some(dt) = &t.date_time {
        // RFC3339 com offset ("2026-11-05T09:00:00-03:00" ou "...Z")
        return DateTime::parse_from_rfc3339(dt)
            .ok()
            .map(|d| d.with_timezone(&Utc));
    }
    if let Some(d) = &t.date {
        // All-day: preserva meia-noite da data civil em São Paulo.
        let local = NaiveDate::parse_from_str(d, "%Y-%m-%d")
            .ok()?
            .and_hms_opt(0, 0, 0)?;
        return chrono_tz::America::Sao_Paulo
            .from_local_datetime(&local)
            .earliest()
            .map(|date| date.with_timezone(&Utc));
    }
    None
}

// ------------------------------------------------------------------
// Apple/iCloud — feed .ics público
// ------------------------------------------------------------------

async fn sync_apple_ics(pool: &PgPool) -> anyhow::Result<usize> {
    let ics_url: String =
        sqlx::query_scalar("SELECT ics_url FROM wecal_connections WHERE provider = 'apple_ics'")
            .fetch_optional(pool)
            .await?
            .flatten()
            .context("Apple não está conectado (cole o link .ics do iCloud)")?;

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()?;
    let resp = client
        .get(&ics_url)
        .header(reqwest::header::USER_AGENT, "WeCal/0.2 (+wecal.app)")
        .send()
        .await
        .context("falha ao baixar o feed .ics")?;
    let status = resp.status();
    if !status.is_success() {
        bail!("feed .ics respondeu {status}");
    }
    let body = resp.bytes().await.context("corpo do .ics ilegível")?;

    let rows = parse_ics(&body)?;
    commit_sync(pool, "apple", None, rows).await
}

/// Parseia um feed iCalendar em linhas de evento (external_id = UID).
fn parse_ics(bytes: &[u8]) -> anyhow::Result<Vec<EventRow>> {
    use ical::IcalParser;

    let parser = IcalParser::new(BufReader::new(bytes));
    let mut rows = HashMap::new();
    let mut overrides = Vec::new();
    let mut cancelled_occurrences = Vec::new();
    let mut idx = 0usize;

    for calendar in parser {
        let calendar = calendar.map_err(|e| anyhow::anyhow!("ICS malformado: {e}"))?;
        for event in calendar.events {
            idx += 1;
            let mut uid = None;
            let mut summary = None;
            let mut description = None;
            let mut class = None;
            let mut start = None;
            let mut end = None;
            let mut duration = None;
            let mut cancelled = false;
            let mut is_transparent = false;
            let mut tentative = false;
            let mut rrule = None;
            let mut exdates: Vec<(Option<String>, String)> = Vec::new();
            let mut rdates: Vec<(Option<String>, String)> = Vec::new();
            let mut recurrence_id = None;

            for prop in &event.properties {
                let value = prop.value.as_deref().unwrap_or_default().trim().to_string();
                let tzid = param_value(prop, "TZID");
                match prop.name.to_ascii_uppercase().as_str() {
                    "UID" => uid = Some(value),
                    "SUMMARY" => summary = Some(unescape_ical_text(&value)),
                    "DESCRIPTION" => description = Some(unescape_ical_text(&value)),
                    "CLASS" => class = Some(value.to_ascii_uppercase()),
                    "STATUS" => {
                        cancelled = value.eq_ignore_ascii_case("CANCELLED");
                        tentative = value.eq_ignore_ascii_case("TENTATIVE");
                    }
                    "TRANSP" => is_transparent = value.eq_ignore_ascii_case("TRANSPARENT"),
                    "DTSTART" => start = parse_ics_datetime(&value, tzid.as_deref()),
                    "DTEND" => end = parse_ics_datetime(&value, tzid.as_deref()),
                    "DURATION" => duration = parse_ics_duration(&value),
                    "RRULE" => rrule = Some(value),
                    "EXDATE" => exdates.push((tzid, value)),
                    "RDATE" => rdates.push((tzid, value)),
                    "RECURRENCE-ID" => recurrence_id = parse_ics_datetime(&value, tzid.as_deref()),
                    _ => {}
                }
            }

            if cancelled {
                if let (Some(uid), Some(date)) = (uid.as_ref(), recurrence_id) {
                    cancelled_occurrences.push(format!("{uid}@{}", date.format("%Y%m%dT%H%M%SZ")));
                }
                continue;
            }

            let Some(start) = start else { continue };
            let end = end.unwrap_or_else(|| {
                duration
                    .map(|d| start + d)
                    .unwrap_or_else(|| start + Duration::hours(1))
                    .max(start)
            });

            let uid = uid.unwrap_or_else(|| format!("ical-{idx}"));
            let mut title = summary.unwrap_or_else(|| "Sem título".into());
            if tentative {
                title = format!("⚠ Provisório · {title}");
            }
            let is_private = matches!(class.as_deref(), Some("PRIVATE") | Some("CONFIDENTIAL"));
            if rrule.is_some() || !rdates.is_empty() {
                let starts =
                    expand_ics_recurrence(&event.properties, rrule.as_deref(), &exdates, &rdates)?;
                for occurrence in starts {
                    let occurrence_end = occurrence + (end - start);
                    let id = format!("{uid}@{}", occurrence.format("%Y%m%dT%H%M%SZ"));
                    rows.insert(
                        id.clone(),
                        (
                            id,
                            title.clone(),
                            description.clone(),
                            occurrence,
                            occurrence_end,
                            is_private,
                            is_transparent,
                        ),
                    );
                }
            } else {
                let external_id = recurrence_id
                    .map(|date| format!("{uid}@{}", date.format("%Y%m%dT%H%M%SZ")))
                    .unwrap_or(uid);
                let row = (
                    external_id.clone(),
                    title,
                    description,
                    start,
                    end,
                    is_private,
                    is_transparent,
                );
                if recurrence_id.is_some() {
                    overrides.push(row);
                } else {
                    rows.insert(external_id, row);
                }
            }
        }
    }
    for row in overrides {
        rows.insert(row.0.clone(), row);
    }
    for id in cancelled_occurrences {
        rows.remove(&id);
    }
    let mut rows = rows.into_values().collect::<Vec<_>>();
    rows.sort_by_key(|row| row.3);
    Ok(rows)
}

fn expand_ics_recurrence(
    properties: &[ical::property::Property],
    rule: Option<&str>,
    exdates: &[(Option<String>, String)],
    rdates: &[(Option<String>, String)],
) -> anyhow::Result<Vec<DateTime<Utc>>> {
    let dtstart = properties
        .iter()
        .find(|p| p.name.eq_ignore_ascii_case("DTSTART"))
        .and_then(|p| p.value.as_deref())
        .context("RRULE sem DTSTART")?;
    let dtstart_prop = properties
        .iter()
        .find(|p| p.name.eq_ignore_ascii_case("DTSTART"))
        .unwrap();
    let tzid = param_value(dtstart_prop, "TZID");
    let start_utc =
        parse_ics_datetime(dtstart, tzid.as_deref()).context("DTSTART inválido em RRULE")?;
    if let Some(until) =
        rule.and_then(|rule| rule.split(';').find_map(|part| part.strip_prefix("UNTIL=")))
    {
        if parse_ics_datetime(until, None).is_some_and(|until| until < start_utc) {
            let excluded = exdates
                .iter()
                .flat_map(|(_, values)| values.split(','))
                .filter_map(|value| parse_ics_datetime(value, tzid.as_deref()))
                .any(|date| date == start_utc);
            return Ok(if excluded {
                Vec::new()
            } else {
                vec![start_utc]
            });
        }
    }
    let canonical_tz = tzid.as_deref().map(canonical_ical_tzid);
    let mut input = if let Some(tz) = canonical_tz {
        format!("DTSTART;TZID={tz}:{dtstart}")
    } else {
        // Floating local times follow the parser's established UTC fallback.
        let utc = start_utc;
        format!("DTSTART:{}", utc.format("%Y%m%dT%H%M%SZ"))
    };
    if let Some(rule) = rule {
        input.push_str(&format!("\nRRULE:{rule}"));
    } else {
        input.push_str("\nRRULE:FREQ=DAILY;COUNT=1");
    }
    for (tzid, values) in exdates {
        let values = values.trim_end_matches(',');
        if values.is_empty() {
            continue;
        }
        if let Some(tz) = tzid.as_deref().map(canonical_ical_tzid) {
            input.push_str(&format!("\nEXDATE;TZID={tz}:{values}"));
        } else {
            let utc_values = values
                .split(',')
                .filter_map(|value| parse_ics_datetime(value, None))
                .map(|date| date.format("%Y%m%dT%H%M%SZ").to_string())
                .collect::<Vec<_>>()
                .join(",");
            if !utc_values.is_empty() {
                input.push_str(&format!("\nEXDATE:{utc_values}"));
            }
        }
    }
    for (tzid, values) in rdates {
        let values = values.trim_end_matches(',');
        if values.is_empty() {
            continue;
        }
        if let Some(tz) = tzid.as_deref().map(canonical_ical_tzid) {
            input.push_str(&format!("\nRDATE;TZID={tz}:{values}"));
        } else {
            let utc_values = values
                .split(',')
                .filter_map(|value| parse_ics_datetime(value, None))
                .map(|date| date.format("%Y%m%dT%H%M%SZ").to_string())
                .collect::<Vec<_>>()
                .join(",");
            if !utc_values.is_empty() {
                input.push_str(&format!("\nRDATE:{utc_values}"));
            }
        }
    }
    let set: rrule::RRuleSet = input
        .parse()
        .with_context(|| format!("RRULE/RDATE inválido: {}", rule.unwrap_or("(sem RRULE)")))?;
    // shortcut: importa até 5 anos à frente; troque por expansão por janela se agendas exigirem horizonte maior.
    let horizon =
        Utc::now().with_timezone(&set.get_dt_start().timezone()) + Duration::days(365 * 5);
    let set = set.before(horizon);
    let result = set.all(20_000);
    if result.limited {
        anyhow::bail!("RRULE excede o limite seguro de 20.000 ocorrências");
    }
    Ok(result
        .dates
        .into_iter()
        .map(|date| date.with_timezone(&Utc))
        .collect())
}

fn canonical_ical_tzid(tz: &str) -> &str {
    if tz.eq_ignore_ascii_case("E. South America Standard Time") {
        "America/Sao_Paulo"
    } else if tz.eq_ignore_ascii_case("Romance Standard Time") {
        "Europe/Paris"
    } else {
        tz
    }
}

fn param_value(prop: &ical::property::Property, name: &str) -> Option<String> {
    prop.params.as_ref()?.iter().find_map(|(k, values)| {
        if k.eq_ignore_ascii_case(name) {
            values.first().cloned()
        } else {
            None
        }
    })
}

/// Converte data/hora iCalendar para UTC.
/// Formatos: `20261105` (all-day), `20261105T090000Z` (UTC),
/// `20261105T090000` + TZID (fuso IANA via chrono-tz; fallback UTC).
fn parse_ics_datetime(value: &str, tzid: Option<&str>) -> Option<DateTime<Utc>> {
    let v = value.trim();

    if let Ok(date) = NaiveDate::parse_from_str(v, "%Y%m%d") {
        let local = date.and_hms_opt(0, 0, 0)?;
        let zone = tzid
            .and_then(|tz| canonical_ical_tzid(tz).parse::<Tz>().ok())
            .unwrap_or(chrono_tz::America::Sao_Paulo);
        return zone
            .from_local_datetime(&local)
            .earliest()
            .map(|date| date.with_timezone(&Utc));
    }

    let fmts = ["%Y%m%dT%H%M%SZ", "%Y%m%dT%H%M%S"];
    for (i, fmt) in fmts.iter().enumerate() {
        if let Ok(naive) = NaiveDateTime::parse_from_str(v, fmt) {
            if i == 0 {
                return Some(naive.and_utc()); // sufixo Z = UTC
            }
            if let Some(tz) = tzid {
                if !tz.eq_ignore_ascii_case("UTC") {
                    let canonical = canonical_ical_tzid(tz);
                    if let Ok(zone) = canonical.parse::<Tz>() {
                        return Some(
                            zone.from_local_datetime(&naive)
                                .earliest()
                                .map(|d| d.with_timezone(&Utc))?
                                .into(),
                        );
                    }
                }
            }
            let zone = tzid
                .and_then(|tz| canonical_ical_tzid(tz).parse::<Tz>().ok())
                .unwrap_or(chrono_tz::America::Sao_Paulo);
            return zone
                .from_local_datetime(&naive)
                .earliest()
                .map(|date| date.with_timezone(&Utc));
        }
    }
    None
}

fn unescape_ical_text(value: &str) -> String {
    let mut result = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            match chars.next() {
                Some('n' | 'N') => result.push('\n'),
                Some(',') => result.push(','),
                Some(';') => result.push(';'),
                Some('\\') => result.push('\\'),
                Some(other) => {
                    result.push('\\');
                    result.push(other);
                }
                None => result.push('\\'),
            }
        } else {
            result.push(ch);
        }
    }
    result
}

/// Duração iCalendar básica: `PT1H30M`, `PT45M`, `P1DT2H`, `P2D`...
fn parse_ics_duration(value: &str) -> Option<Duration> {
    let v = value.trim().trim_start_matches('P').to_uppercase();
    let (date_part, time_part) = match v.split_once('T') {
        Some((d, t)) => (d, Some(t)),
        None => (v.as_str(), None),
    };

    let mut secs: i64 = 0;
    let mut num = String::new();
    for ch in date_part.chars() {
        if ch.is_ascii_digit() {
            num.push(ch);
        } else if !num.is_empty() {
            let n: i64 = num.parse().ok()?;
            secs += match ch {
                'W' => n * 7 * 86400,
                'D' => n * 86400,
                _ => return None,
            };
            num.clear();
        }
    }
    if let Some(t) = time_part {
        let mut num = String::new();
        for ch in t.chars() {
            if ch.is_ascii_digit() {
                num.push(ch);
            } else if !num.is_empty() {
                let n: i64 = num.parse().ok()?;
                secs += match ch {
                    'H' => n * 3600,
                    'M' => n * 60,
                    'S' => n,
                    _ => return None,
                };
                num.clear();
            }
        }
    }
    (secs > 0).then(|| Duration::seconds(secs))
}

#[cfg(test)]
mod ics_tests {
    use super::parse_ics;

    #[test]
    fn expands_series_exdates_and_replaces_exception() {
        let ics = b"BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:series\r\nDTSTART;TZID=America/Sao_Paulo:20260105T090000\r\nDTEND;TZID=America/Sao_Paulo:20260105T100000\r\nRRULE:FREQ=DAILY;COUNT=3\r\nEXDATE;TZID=America/Sao_Paulo:20260107T090000\r\nSUMMARY:Daily\\, sync\r\nEND:VEVENT\r\nBEGIN:VEVENT\r\nUID:series\r\nRECURRENCE-ID;TZID=America/Sao_Paulo:20260106T090000\r\nDTSTART;TZID=America/Sao_Paulo:20260106T110000\r\nDTEND;TZID=America/Sao_Paulo:20260106T120000\r\nSUMMARY:Moved\\ninstance\r\nSTATUS:TENTATIVE\r\nTRANSP:TRANSPARENT\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
        let rows = parse_ics(ics).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].1, "Daily, sync");
        assert_eq!(rows[1].1, "⚠ Provisório · Moved\ninstance");
        assert!(rows[1].6);
        assert_eq!(rows[1].3.format("%H:%M").to_string(), "14:00");
    }

    #[test]
    fn converts_outlook_timezone_and_drops_cancelled_events() {
        let ics = b"BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:outlook\r\nDTSTART;TZID=E. South America Standard Time:20261105T090000\r\nDTEND;TZID=E. South America Standard Time:20261105T100000\r\nSUMMARY:Outlook\r\nEND:VEVENT\r\nBEGIN:VEVENT\r\nUID:cancelled\r\nDTSTART:20261105T090000Z\r\nSTATUS:CANCELLED\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
        let rows = parse_ics(ics).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].3.format("%H:%M").to_string(), "12:00");
    }

    #[test]
    fn all_day_values_keep_the_brazilian_calendar_date() {
        let date = super::parse_ics_datetime("20261105", None).unwrap();
        assert_eq!(
            date.format("%Y-%m-%dT%H:%MZ").to_string(),
            "2026-11-05T03:00Z"
        );
    }

    #[test]
    fn rdate_adds_an_occurrence_without_rrule() {
        let ics = b"BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:extra\r\nDTSTART;TZID=America/Sao_Paulo:20260105T090000\r\nDTEND;TZID=America/Sao_Paulo:20260105T100000\r\nRDATE;TZID=America/Sao_Paulo:20260107T090000\r\nSUMMARY:Extra\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
        let rows = parse_ics(ics).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(
            rows[1].3.format("%Y%m%dT%H%MZ").to_string(),
            "20260107T1200Z"
        );
    }

    #[test]
    fn cancelled_recurrence_exception_removes_generated_occurrence() {
        let ics = b"BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:cancel-series\r\nDTSTART:20260105T090000Z\r\nDTEND:20260105T100000Z\r\nRRULE:FREQ=DAILY;COUNT=2\r\nSUMMARY:Daily\r\nEND:VEVENT\r\nBEGIN:VEVENT\r\nUID:cancel-series\r\nRECURRENCE-ID:20260106T090000Z\r\nSTATUS:CANCELLED\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
        let rows = parse_ics(ics).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].3.format("%Y%m%dT%H%MZ").to_string(),
            "20260105T0900Z"
        );
    }
}

// ------------------------------------------------------------------
// Commit transacional comum (upsert + remoção do que saiu do provedor)
// ------------------------------------------------------------------

type EventRow = (
    String,
    String,
    Option<String>,
    DateTime<Utc>,
    DateTime<Utc>,
    bool,
    bool,
);

async fn commit_sync(pool: &PgPool, provider: &str, connection_id: Option<uuid::Uuid>, rows: Vec<EventRow>) -> anyhow::Result<usize> {
    let mut tx: Transaction<'_, Postgres> = pool.begin().await?;

    if let Some(id) = connection_id {
        sqlx::query("SELECT id FROM wecal_connections WHERE id = $1 AND provider = 'google' FOR UPDATE")
            .bind(id).fetch_one(&mut *tx).await?;
    }

    // Marca tudo do provedor como "não visto nesta rodada"
    sqlx::query(
        "UPDATE wecal_events SET synced_at = NULL WHERE provider = $1 AND external_id IS NOT NULL AND connection_id IS NOT DISTINCT FROM $2",
    )
    .bind(provider)
    .bind(connection_id)
    .execute(&mut *tx)
    .await?;

    for (external_id, title, description, start, end, is_private, is_transparent) in &rows {
        sqlx::query(
            r#"
            INSERT INTO wecal_events
                (provider, external_id, title, description, start_at, end_at, is_private, is_transparent, synced_at, connection_id)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, now(), $9)
            ON CONFLICT (provider, external_id) DO UPDATE SET
                title       = EXCLUDED.title,
                description = EXCLUDED.description,
                start_at    = EXCLUDED.start_at,
                end_at      = EXCLUDED.end_at,
                is_private  = EXCLUDED.is_private,
                is_transparent = EXCLUDED.is_transparent,
                synced_at   = now(),
                updated_at  = now()
            "#,
        )
        .bind(provider)
        .bind(&external_id)
        .bind(title)
        .bind(description)
        .bind(start)
        .bind(end)
        .bind(is_private)
        .bind(is_transparent)
        .bind(connection_id)
        .execute(&mut *tx)
        .await?;
    }

    // Remove eventos que o provedor não devolveu mais
    let stale = sqlx::query(
        "DELETE FROM wecal_events WHERE provider = $1 AND external_id IS NOT NULL AND synced_at IS NULL AND connection_id IS NOT DISTINCT FROM $2",
    )
    .bind(provider)
    .bind(connection_id)
    .execute(&mut *tx)
    .await?
    .rows_affected();

    // Sucesso: registra o horário e limpa o último erro
    sqlx::query(
        "UPDATE wecal_connections SET last_sync_at = now(), last_error = NULL, updated_at = now() WHERE provider = $1 AND ($2::uuid IS NULL OR id = $2)",
    )
    .bind(if provider == "apple" { "apple_ics" } else { provider })
    .bind(connection_id)
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;
    let _ = stale; // remoções não contam no total exibido
    Ok(rows.len())
}

// ------------------------------------------------------------------
// Importação manual de arquivos .ics ou .zip (público)
// ------------------------------------------------------------------

/// Resultado da importação de arquivo(s)
#[derive(Debug)]
pub struct ImportOutcome {
    pub filename: String,
    pub calendar_name: Option<String>,
    pub calendar_color: Option<String>,
    pub events_imported: usize,
    pub events_updated: usize,
    pub events_removed: usize,
    pub ok: bool,
    pub error: Option<String>,
}

/// Importa um arquivo .ics (bytes brutos) para o banco.
/// Usa provider = "import" e external_id = UID do evento.
/// Retorna contagem de eventos importados.
pub async fn import_ics_file(
    pool: &PgPool,
    filename: &str,
    bytes: &[u8],
    calendar_name: Option<&str>,
    calendar_color: Option<&str>,
) -> anyhow::Result<ImportOutcome> {
    let rows = parse_ics(bytes)?;
    let total = rows.len();
    let (updated, removed, name, color) =
        commit_import(pool, "import", filename, rows, calendar_name, calendar_color).await?;
    // "novos" = total processado menos os que já existiam (atualizados)
    let imported = total.saturating_sub(updated);
    Ok(ImportOutcome {
        filename: filename.to_string(),
        calendar_name: Some(name),
        calendar_color: Some(color),
        events_imported: imported,
        events_updated: updated,
        events_removed: removed,
        ok: true,
        error: None,
    })
}

/// Importa um arquivo .zip contendo um ou mais arquivos .ics.
/// Cada .ics dentro vira uma importação separada (mesmo provider "import").
///
/// A extração do zip é síncrona e acontece *antes* de qualquer await:
/// os tipos da lib `zip` (ex.: `ZipFile`) não são `Send`, e o Axum exige
/// futures `Send` — mantê-los vivos através de um await quebra o Handler.
pub async fn import_zip_file(
    pool: &PgPool,
    filename: &str,
    bytes: &[u8],
) -> anyhow::Result<Vec<ImportOutcome>> {
    let entries = extract_zip_entries(filename, bytes)?;

    let mut outcomes = Vec::new();

    for (name, contents) in entries {
        // Só processa arquivos .ics
        if !is_supported_calendar_file(&name) {
            outcomes.push(ImportOutcome {
                filename: name,
                calendar_name: None,
                calendar_color: None,
                events_imported: 0,
                events_updated: 0,
                events_removed: 0,
                ok: false,
                error: Some("não é um arquivo iCalendar (ignorado)".into()),
            });
            continue;
        }

        match import_ics_file(pool, &name, &contents, None, None).await {
            Ok(outcome) => outcomes.push(outcome),
            Err(e) => outcomes.push(ImportOutcome {
                filename: name,
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

    Ok(outcomes)
}

pub fn is_supported_calendar_file(filename: &str) -> bool {
    let lower = filename.to_ascii_lowercase();
    [".ics", ".ical", ".ifb", ".vcs"].iter().any(|ext| lower.ends_with(ext))
}

/// Lê todas as entradas do zip de uma vez (síncrono, sem await).
/// Retorna (nome, bytes) de cada entrada, incluindo as que não são .ics.
fn extract_zip_entries(filename: &str, bytes: &[u8]) -> anyhow::Result<Vec<(String, Vec<u8>)>> {
    let mut cursor = Cursor::new(bytes);
    let mut archive = zip::ZipArchive::new(&mut cursor)
        .with_context(|| format!("{filename}: arquivo .zip inválido ou corrompido"))?;

    let mut entries = Vec::new();
    for i in 0..archive.len() {
        let mut file = archive
            .by_index(i)
            .with_context(|| format!("{filename}: falha ao ler entrada do zip"))?;
        let name = file.name().to_string();
        let mut contents = Vec::new();
        std::io::copy(&mut file, &mut contents)
            .with_context(|| format!("{filename}: falha ao extrair {name}"))?;
        entries.push((name, contents));
    }
    Ok(entries)
}

/// Commit da importação (sem stale delete — import é aditivo, não substitui a fonte).
/// Retorna quantos eventos foram *atualizados* (já existiam pelo mesmo UID).
async fn commit_import(
    pool: &PgPool,
    provider: &str,
    source_filename: &str,
    rows: Vec<EventRow>,
    requested_name: Option<&str>,
    requested_color: Option<&str>,
) -> anyhow::Result<(usize, usize, String, String)> {
    let mut tx: Transaction<'_, Postgres> = pool.begin().await?;

    sqlx::query("LOCK TABLE wecal_import_calendars IN EXCLUSIVE MODE")
        .execute(&mut *tx)
        .await?;
    let existing: Option<(String, String)> = sqlx::query_as(
        "SELECT calendar_name, calendar_color FROM wecal_import_calendars WHERE source_filename = $1",
    )
    .bind(source_filename)
    .fetch_optional(&mut *tx)
    .await?;
    let name = requested_name
        .filter(|name| !name.trim().is_empty())
        .map(str::trim)
        .map(str::to_owned)
        .unwrap_or_else(|| calendar_name_from_filename(source_filename));
    if name.chars().count() > 120 {
        bail!("o nome da agenda deve ter no máximo 120 caracteres");
    }
    let requested_color = requested_color.map(str::trim).map(str::to_ascii_lowercase);
    if let Some(color) = requested_color.as_deref() {
        if !valid_calendar_color(color) {
            bail!("cor inválida: use o formato #RRGGBB");
        }
    }
    let used_colors = sqlx::query_scalar::<_, String>(
        "SELECT calendar_color FROM wecal_import_calendars WHERE source_filename <> $1",
    )
    .bind(source_filename)
    .fetch_all(&mut *tx)
    .await?;
    let color = if let Some(color) = requested_color.as_deref() {
        if !color_is_bright(color) || !color_is_distinct(color, &used_colors) {
            bail!("escolha uma cor clara e afastada das cores já usadas no calendário");
        }
        color.to_owned()
    } else if let Some((_, color)) = &existing {
        color.clone()
    } else {
        let mut generated = None;
        for _ in 0..100_000 {
            let bytes = *uuid::Uuid::new_v4().as_bytes();
            let color = format!("#{:02x}{:02x}{:02x}", 128 + bytes[0] / 2, 128 + bytes[1] / 2, 128 + bytes[2] / 2);
            if color_is_distinct(&color, &used_colors) {
                generated = Some(color);
                break;
            }
        }
        generated.ok_or_else(|| anyhow::anyhow!("não há mais cores claras suficientemente diferentes para esta agenda"))?
    };
    sqlx::query(
        r#"INSERT INTO wecal_import_calendars (source_filename, calendar_name, calendar_color)
           VALUES ($1, $2, $3)
           ON CONFLICT (source_filename) DO UPDATE SET
             calendar_name = EXCLUDED.calendar_name,
             calendar_color = EXCLUDED.calendar_color"#,
    )
    .bind(source_filename)
    .bind(&name)
    .bind(&color)
    .execute(&mut *tx)
    .await?;

    sqlx::query(
        "UPDATE wecal_events SET synced_at = NULL WHERE provider = $1 AND source_filename = $2",
    )
    .bind(provider)
    .bind(source_filename)
    .execute(&mut *tx)
    .await?;

    let mut updated_count = 0usize;

    for (external_id, title, description, start, end, is_private, is_transparent) in &rows {
        let external_id = format!("{source_filename}::{external_id}");
        // Verifica se já existe para contar updates
        // (SELECT 1 retorna INT4 — por isso i32, não i64)
        let exists = sqlx::query_scalar::<_, i32>(
            "SELECT 1 FROM wecal_events WHERE provider = $1 AND external_id = $2",
        )
        .bind(provider)
        .bind(&external_id)
        .fetch_optional(&mut *tx)
        .await?
        .is_some();

        if exists {
            updated_count += 1;
        }

        sqlx::query(
            r#"
            INSERT INTO wecal_events
                (provider, external_id, title, description, start_at, end_at, is_private, is_transparent, synced_at, source_filename)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, now(), $9)
            ON CONFLICT (provider, external_id) DO UPDATE SET
                title       = EXCLUDED.title,
                description = EXCLUDED.description,
                start_at    = EXCLUDED.start_at,
                end_at      = EXCLUDED.end_at,
                is_private  = EXCLUDED.is_private,
                is_transparent = EXCLUDED.is_transparent,
                synced_at   = now(),
                source_filename = EXCLUDED.source_filename,
                updated_at  = now()
            "#,
        )
        .bind(provider)
        .bind(&external_id)
        .bind(title)
        .bind(description)
        .bind(start)
        .bind(end)
        .bind(is_private)
        .bind(is_transparent)
        .bind(source_filename)
        .execute(&mut *tx)
        .await?;
    }

    let removed = sqlx::query("DELETE FROM wecal_events WHERE provider = $1 AND source_filename = $2 AND synced_at IS NULL")
        .bind(provider).bind(source_filename).execute(&mut *tx).await?.rows_affected() as usize;

    tx.commit().await?;
    Ok((updated_count, removed, name, color))
}

fn valid_calendar_color(color: &str) -> bool {
    color.len() == 7
        && color.starts_with('#')
        && color[1..].bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn color_rgb(color: &str) -> Option<[i32; 3]> {
    Some([
        i32::from(u8::from_str_radix(&color[1..3], 16).ok()?),
        i32::from(u8::from_str_radix(&color[3..5], 16).ok()?),
        i32::from(u8::from_str_radix(&color[5..7], 16).ok()?),
    ])
}

fn color_is_bright(color: &str) -> bool {
    let Some([red, green, blue]) = color_rgb(color) else {
        return false;
    };
    let linear = |channel: i32| {
        let channel = channel as f64 / 255.0;
        if channel <= 0.04045 {
            channel / 12.92
        } else {
            ((channel + 0.055) / 1.055).powf(2.4)
        }
    };
    linear(red) * 0.2126 + linear(green) * 0.7152 + linear(blue) * 0.0722 >= 0.18
}

fn color_is_distinct(color: &str, existing: &[String]) -> bool {
    let Some(candidate) = color_rgb(color) else {
        return false;
    };
    let distance_ok = |other: [i32; 3]| {
        (0..3).map(|index| (candidate[index] - other[index]).pow(2)).sum::<i32>() >= 10_000
    };
    ["#8b5cf6", "#6366f1", "#64748b"]
        .iter()
        .all(|color| distance_ok(color_rgb(color).unwrap()))
        && existing
            .iter()
            .filter_map(|color| color_rgb(color))
            .all(|other| distance_ok(other))
}

fn calendar_name_from_filename(filename: &str) -> String {
    let basename = filename.rsplit('/').next().unwrap_or(filename);
    let stem = basename.rsplit_once('.').map_or(basename, |(stem, _)| stem);
    if stem.trim().is_empty() {
        "Agenda importada".into()
    } else {
        stem.into()
    }
}

#[cfg(test)]
mod import_calendar_tests {
    use super::*;

    #[test]
    fn derives_agenda_name_from_path_and_extension() {
        assert_eq!(calendar_name_from_filename("export/Viagens.ical"), "Viagens");
        assert_eq!(calendar_name_from_filename("Aulas.ics"), "Aulas");
        assert_eq!(calendar_name_from_filename("Aulas  .ics"), "Aulas  ");
    }

    #[test]
    fn validates_optional_hex_color() {
        assert!(valid_calendar_color("#aBc123"));
        assert!(!valid_calendar_color("#abc"));
        assert!(!valid_calendar_color("aabbcc"));
    }

    #[test]
    fn imported_colors_are_bright_and_separate_from_fixed_calendar_colors() {
        assert!(color_is_bright("#c080e0"));
        assert!(!color_is_bright("#64748b"));
        assert!(!color_is_distinct("#64748b", &[]));
        assert!(!color_is_distinct("#858080", &["#808080".into()]));
        assert!(color_is_distinct("#ffff00", &[]));
    }

    #[test]
    fn recognizes_common_icalendar_extensions() {
        assert!(is_supported_calendar_file("events.ICAL"));
        assert!(is_supported_calendar_file("events.vcs"));
        assert!(!is_supported_calendar_file("events.txt"));
    }
}
