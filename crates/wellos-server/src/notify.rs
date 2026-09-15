//! Durable scheduling notifications: enqueueing inside business
//! transactions, PostgreSQL-claimed delivery with bounded retries and
//! dead-lettering, and the in-app / SMTP / signed-webhook adapters.
//!
//! Payloads and message bodies carry scheduling facts only (times, service
//! and facility names, appointment identifiers) — never the patient's name,
//! diagnosis or free text. Logs mention notification ids and kinds only.

use crate::audit;
use crate::auth::AuthContext;
use crate::error::ApiError;
use crate::runtime::{NotificationConfig, SmtpConfig, SmtpTls, WebhookConfig};
use crate::scheduling::{self, AppointmentRow, OfferRow, Policy, ServiceEntry};
use crate::state::AppState;
use chrono::{DateTime, Duration, NaiveTime, TimeZone, Utc};
use hmac::{Hmac, Mac};
use serde_json::{json, Value};
use sha2::Sha256;
use sqlx::{PgConnection, Row};
use uuid::Uuid;
use wellos_domain::access::in_quiet_hours;

pub const CHANNELS: &[&str] = &["in_app", "email", "webhook"];
const CLAIM_MINUTES: i64 = 5;

/// Who receives a patient-facing notification and how.
#[derive(Debug, Clone)]
pub struct Recipient {
    pub language: String,
    pub time_zone: String,
    pub channels: Vec<String>,
    pub quiet_start: NaiveTime,
    pub quiet_end: NaiveTime,
}

pub async fn recipient_for_patient(
    conn: &mut PgConnection,
    tenant_id: Uuid,
    patient_id: Uuid,
    policy: &Policy,
    fallback_tz: &str,
) -> Result<Recipient, ApiError> {
    let prefs = scheduling::load_preferences(conn, tenant_id, patient_id).await?;
    let language = prefs
        .language
        .as_deref()
        .map(|l| if l.starts_with("es") { "es" } else { "en" })
        .unwrap_or("en")
        .to_string();
    let mut channels: Vec<String> = prefs
        .channels
        .iter()
        .filter(|c| CHANNELS.contains(&c.as_str()))
        .cloned()
        .collect();
    if !channels.iter().any(|c| c == "in_app") {
        channels.insert(0, "in_app".into());
    }
    Ok(Recipient {
        language,
        time_zone: prefs.time_zone.unwrap_or_else(|| fallback_tz.to_string()),
        channels,
        quiet_start: prefs.quiet_hours_start.unwrap_or(policy.quiet_hours_start),
        quiet_end: prefs.quiet_hours_end.unwrap_or(policy.quiet_hours_end),
    })
}

/// Shift a delivery out of the recipient's quiet hours to their end.
pub fn outside_quiet_hours(at: DateTime<Utc>, r: &Recipient) -> DateTime<Utc> {
    let tz: chrono_tz::Tz = r.time_zone.parse().unwrap_or(chrono_tz::UTC);
    let local = at.with_timezone(&tz);
    if !in_quiet_hours(local.time(), r.quiet_start, r.quiet_end) {
        return at;
    }
    let mut date = local.date_naive();
    if r.quiet_start > r.quiet_end && local.time() >= r.quiet_start {
        date += Duration::days(1);
    }
    let candidate = date.and_time(r.quiet_end);
    match tz.from_local_datetime(&candidate).earliest() {
        Some(t) => t.with_timezone(&Utc),
        None => at,
    }
}

fn respects_quiet_hours(kind: &str) -> bool {
    matches!(
        kind,
        "reminder"
            | "preparation"
            | "confirmation_request"
            | "confirmation_follow_up"
            | "no_response_follow_up"
    )
}

pub struct Enqueue<'a> {
    pub tenant_id: Uuid,
    pub patient_id: Option<Uuid>,
    pub user_id: Option<Uuid>,
    pub kind: &'a str,
    pub appointment_id: Option<Uuid>,
    pub offer_id: Option<Uuid>,
    pub transport_request_id: Option<Uuid>,
    pub payload: Value,
    pub dedupe_key: String,
    pub scheduled_for: DateTime<Utc>,
    pub recipient: &'a Recipient,
}

/// Idempotently enqueue one notification (duplicate dedupe keys are
/// no-ops) and audit the schedule.
pub async fn enqueue(
    conn: &mut PgConnection,
    ctx: &AuthContext,
    cell: &str,
    e: Enqueue<'_>,
) -> Result<Option<Uuid>, ApiError> {
    let when = if respects_quiet_hours(e.kind) {
        outside_quiet_hours(e.scheduled_for, e.recipient)
    } else {
        e.scheduled_for
    };
    let id = Uuid::now_v7();
    let inserted = sqlx::query(
        "INSERT INTO notifications (id, tenant_id, patient_id, user_id, kind, appointment_id, offer_id,
             transport_request_id, language, time_zone, channels, payload, dedupe_key, scheduled_for, status)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,'scheduled')
         ON CONFLICT (tenant_id, dedupe_key) DO NOTHING",
    )
    .bind(id)
    .bind(e.tenant_id)
    .bind(e.patient_id)
    .bind(e.user_id)
    .bind(e.kind)
    .bind(e.appointment_id)
    .bind(e.offer_id)
    .bind(e.transport_request_id)
    .bind(&e.recipient.language)
    .bind(&e.recipient.time_zone)
    .bind(&e.recipient.channels)
    .bind(&e.payload)
    .bind(&e.dedupe_key)
    .bind(when)
    .execute(&mut *conn)
    .await?;
    if inserted.rows_affected() == 0 {
        return Ok(None);
    }
    audit::emit(
        &mut *conn,
        ctx,
        "notification.scheduled",
        cell,
        json!({ "notification_id": id, "kind": e.kind, "appointment_id": e.appointment_id,
                "offer_id": e.offer_id, "scheduled_for": when }),
        None,
    )
    .await
    .map_err(ApiError::internal)?;
    Ok(Some(id))
}

async fn facility_name(conn: &mut PgConnection, facility_id: Uuid) -> Result<String, ApiError> {
    Ok(
        sqlx::query_scalar("SELECT name FROM facilities WHERE id = $1")
            .bind(facility_id)
            .fetch_optional(&mut *conn)
            .await?
            .unwrap_or_default(),
    )
}

fn appointment_payload(a: &AppointmentRow, service: &ServiceEntry, facility: &str) -> Value {
    json!({
        "appointment_id": a.id,
        "starts_at": a.starts_at,
        "ends_at": a.ends_at,
        "time_zone": a.time_zone,
        "service_code": a.service_code,
        "service_name_en": service.name_en,
        "service_name_es": service.name_es,
        "modality_code": a.modality_code,
        "facility_id": a.facility_id,
        "facility_name": facility,
        "preparation_en": service.config.preparation_en,
        "preparation_es": service.config.preparation_es,
        "confirmation_required": a.confirmation_required,
        "confirmation_due_at": a.confirmation_due_at,
    })
}

/// Confirmation (or reschedule) notice, configured reminders, preparation
/// instructions and the confirmation request/follow-up for a new
/// appointment.
pub async fn schedule_for_confirmed(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    ctx: &AuthContext,
    state: &AppState,
    a: &AppointmentRow,
    policy: &Policy,
    service: &ServiceEntry,
    is_reschedule: bool,
) -> Result<(), ApiError> {
    let recipient =
        recipient_for_patient(tx, a.tenant_id, a.patient_id, policy, &a.time_zone).await?;
    let facility = facility_name(tx, a.facility_id).await?;
    let payload = appointment_payload(a, service, &facility);
    let now = Utc::now();
    let base = |kind: &'static str, key: String, when: DateTime<Utc>| Enqueue {
        tenant_id: a.tenant_id,
        patient_id: Some(a.patient_id),
        user_id: None,
        kind,
        appointment_id: Some(a.id),
        offer_id: None,
        transport_request_id: None,
        payload: payload.clone(),
        dedupe_key: key,
        scheduled_for: when,
        recipient: &recipient,
    };
    let first_kind = if is_reschedule {
        "reschedule"
    } else {
        "booking_confirmation"
    };
    enqueue(
        tx,
        ctx,
        &state.cell,
        base(first_kind, format!("appt:{}:{first_kind}", a.id), now),
    )
    .await?;
    for lead in policy.reminder_lead_hours.iter().filter(|h| **h > 0) {
        let when = a.starts_at - Duration::hours(*lead as i64);
        if when > now {
            enqueue(
                tx,
                ctx,
                &state.cell,
                base("reminder", format!("appt:{}:reminder:{lead}", a.id), when),
            )
            .await?;
        }
    }
    if service.config.preparation_en.is_some() || service.config.preparation_es.is_some() {
        let when = (a.starts_at - Duration::hours(24)).max(now);
        enqueue(
            tx,
            ctx,
            &state.cell,
            base("preparation", format!("appt:{}:preparation", a.id), when),
        )
        .await?;
    }
    if a.confirmation_required {
        enqueue(
            tx,
            ctx,
            &state.cell,
            base(
                "confirmation_request",
                format!("appt:{}:confirmation_request", a.id),
                now,
            ),
        )
        .await?;
        if let Some(due) = a.confirmation_due_at {
            let follow = (due - Duration::hours(12)).max(now + Duration::hours(1));
            if follow < a.starts_at {
                enqueue(
                    tx,
                    ctx,
                    &state.cell,
                    base(
                        "confirmation_follow_up",
                        format!("appt:{}:confirmation_follow_up", a.id),
                        follow,
                    ),
                )
                .await?;
            }
            if due > now && due < a.starts_at {
                enqueue(
                    tx,
                    ctx,
                    &state.cell,
                    base(
                        "no_response_follow_up",
                        format!("appt:{}:no_response_follow_up", a.id),
                        due,
                    ),
                )
                .await?;
            }
        }
    }
    Ok(())
}

pub async fn schedule_cancellation(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    ctx: &AuthContext,
    state: &AppState,
    a: &AppointmentRow,
    policy: &Policy,
) -> Result<(), ApiError> {
    let recipient =
        recipient_for_patient(tx, a.tenant_id, a.patient_id, policy, &a.time_zone).await?;
    let facility = facility_name(tx, a.facility_id).await?;
    let service = scheduling::load_service(tx, a.tenant_id, &a.service_code)
        .await
        .unwrap_or(ServiceEntry {
            code: a.service_code.clone(),
            name_en: a.service_code.clone(),
            name_es: a.service_code.clone(),
            config: Default::default(),
        });
    enqueue(
        tx,
        ctx,
        &state.cell,
        Enqueue {
            tenant_id: a.tenant_id,
            patient_id: Some(a.patient_id),
            user_id: None,
            kind: "cancellation",
            appointment_id: Some(a.id),
            offer_id: None,
            transport_request_id: None,
            payload: appointment_payload(a, &service, &facility),
            dedupe_key: format!("appt:{}:cancellation", a.id),
            scheduled_for: Utc::now(),
            recipient: &recipient,
        },
    )
    .await?;
    Ok(())
}

/// Pending reminders of a closed appointment are withdrawn.
pub async fn cancel_pending_for_appointment(
    conn: &mut PgConnection,
    tenant_id: Uuid,
    appointment_id: Uuid,
) -> Result<u64, ApiError> {
    let r = sqlx::query(
        "UPDATE notifications SET status = 'cancelled', updated_at = now()
         WHERE tenant_id = $1 AND appointment_id = $2 AND status IN ('scheduled','failed')
           AND kind IN ('reminder','preparation','confirmation_request','confirmation_follow_up','no_response_follow_up')",
    )
    .bind(tenant_id)
    .bind(appointment_id)
    .execute(&mut *conn)
    .await?;
    Ok(r.rows_affected())
}

pub async fn schedule_offer(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    ctx: &AuthContext,
    state: &AppState,
    o: &OfferRow,
    policy: &Policy,
    kind: &str,
) -> Result<(), ApiError> {
    let recipient =
        recipient_for_patient(tx, o.tenant_id, o.patient_id, policy, &policy.time_zone).await?;
    let facility = facility_name(tx, o.facility_id).await?;
    let service = scheduling::load_service(tx, o.tenant_id, &o.service_code)
        .await
        .ok();
    enqueue(
        tx,
        ctx,
        &state.cell,
        Enqueue {
            tenant_id: o.tenant_id,
            patient_id: Some(o.patient_id),
            user_id: None,
            kind,
            appointment_id: None,
            offer_id: Some(o.id),
            transport_request_id: None,
            payload: json!({
                "offer_id": o.id,
                "starts_at": o.starts_at,
                "ends_at": o.ends_at,
                "service_code": o.service_code,
                "service_name_en": service.as_ref().map(|s| s.name_en.clone()),
                "service_name_es": service.as_ref().map(|s| s.name_es.clone()),
                "facility_id": o.facility_id,
                "facility_name": facility,
                "offer_expires_at": o.offer_expires_at,
            }),
            dedupe_key: format!("offer:{}:{kind}", o.id),
            scheduled_for: Utc::now(),
            recipient: &recipient,
        },
    )
    .await?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub async fn schedule_transport_status(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    ctx: &AuthContext,
    state: &AppState,
    tenant_id: Uuid,
    patient_id: Uuid,
    appointment_id: Uuid,
    transport_request_id: Uuid,
    status: &str,
    pickup_window_start: Option<DateTime<Utc>>,
    policy: &Policy,
) -> Result<(), ApiError> {
    let recipient =
        recipient_for_patient(tx, tenant_id, patient_id, policy, &policy.time_zone).await?;
    enqueue(
        tx,
        ctx,
        &state.cell,
        Enqueue {
            tenant_id,
            patient_id: Some(patient_id),
            user_id: None,
            kind: "transport_status",
            appointment_id: Some(appointment_id),
            offer_id: None,
            transport_request_id: Some(transport_request_id),
            payload: json!({
                "transport_request_id": transport_request_id,
                "appointment_id": appointment_id,
                "status": status,
                "pickup_window_start": pickup_window_start,
            }),
            dedupe_key: format!(
                "transport:{transport_request_id}:{status}:{}",
                Utc::now().timestamp()
            ),
            scheduled_for: Utc::now(),
            recipient: &recipient,
        },
    )
    .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Rendering (EN/ES, PHI-minimized)
// ---------------------------------------------------------------------------

fn local_time(payload: &Value, key: &str, tz: &str) -> String {
    let Some(raw) = payload.get(key).and_then(|v| v.as_str()) else {
        return String::new();
    };
    let Ok(t) = DateTime::parse_from_rfc3339(raw) else {
        return raw.to_string();
    };
    let zone: chrono_tz::Tz = tz.parse().unwrap_or(chrono_tz::UTC);
    t.with_timezone(&zone)
        .format("%Y-%m-%d %H:%M (%Z)")
        .to_string()
}

/// Subject and body for one notification; the subject never names the
/// patient or the clinical reason.
pub fn render(kind: &str, language: &str, time_zone: &str, payload: &Value) -> (String, String) {
    let es = language.starts_with("es");
    let service = payload
        .get(if es {
            "service_name_es"
        } else {
            "service_name_en"
        })
        .and_then(|v| v.as_str())
        .or_else(|| payload.get("service_code").and_then(|v| v.as_str()))
        .unwrap_or("");
    let facility = payload
        .get("facility_name")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let when = local_time(payload, "starts_at", time_zone);
    let where_ = if facility.is_empty() {
        String::new()
    } else if es {
        format!(" en {facility}")
    } else {
        format!(" at {facility}")
    };
    let (subject, body) = match (kind, es) {
        ("booking_confirmation", false) => ("Appointment confirmed", format!("Your {service} appointment is confirmed for {when}{where_}.")),
        ("booking_confirmation", true) => ("Cita confirmada", format!("Su cita de {service} está confirmada para el {when}{where_}.")),
        ("reschedule", false) => ("Appointment rescheduled", format!("Your {service} appointment now takes place on {when}{where_}.")),
        ("reschedule", true) => ("Cita reprogramada", format!("Su cita de {service} ahora será el {when}{where_}.")),
        ("cancellation", false) => ("Appointment cancelled", format!("Your {service} appointment on {when}{where_} has been cancelled.")),
        ("cancellation", true) => ("Cita cancelada", format!("Su cita de {service} del {when}{where_} ha sido cancelada.")),
        ("reminder", false) => ("Appointment reminder", format!("Reminder: {service} on {when}{where_}.")),
        ("reminder", true) => ("Recordatorio de cita", format!("Recordatorio: {service} el {when}{where_}.")),
        ("preparation", false) => (
            "How to prepare for your appointment",
            payload.get("preparation_en").and_then(|v| v.as_str()).unwrap_or("Please follow the preparation instructions provided by your facility.").to_string(),
        ),
        ("preparation", true) => (
            "Cómo prepararse para su cita",
            payload.get("preparation_es").and_then(|v| v.as_str()).unwrap_or("Siga las instrucciones de preparación indicadas por su centro.").to_string(),
        ),
        ("confirmation_request", false) => ("Please confirm your appointment", format!("Please confirm your {service} appointment on {when}{where_} in My appointments.")),
        ("confirmation_request", true) => ("Confirme su cita", format!("Confirme su cita de {service} del {when}{where_} en Mis citas.")),
        ("confirmation_follow_up", false) => ("Your appointment still needs confirmation", format!("Your {service} appointment on {when} is not confirmed yet.")),
        ("confirmation_follow_up", true) => ("Su cita aún no está confirmada", format!("Su cita de {service} del {when} todavía no está confirmada.")),
        ("no_response_follow_up", false) => ("We have not heard from you", format!("Your {service} appointment on {when} remains unconfirmed; the facility may contact you.")),
        ("no_response_follow_up", true) => ("No hemos recibido respuesta", format!("Su cita de {service} del {when} sigue sin confirmar; el centro podría contactarle.")),
        ("waitlist_offer", false) => ("An earlier appointment is available", format!("A {service} appointment on {when}{where_} is available to you until {}.", local_time(payload, "offer_expires_at", time_zone))),
        ("waitlist_offer", true) => ("Hay una cita antes disponible", format!("Una cita de {service} el {when}{where_} está disponible para usted hasta el {}.", local_time(payload, "offer_expires_at", time_zone))),
        ("waitlist_offer_expired", false) => ("Waitlist offer expired", format!("The {service} offer for {when} has expired. You remain on the waitlist.")),
        ("waitlist_offer_expired", true) => ("Oferta de lista de espera caducada", format!("La oferta de {service} para el {when} ha caducado. Sigue en la lista de espera.")),
        ("transport_status", false) => ("Transport update", format!("Your transport status is now '{}'.", payload.get("status").and_then(|v| v.as_str()).unwrap_or(""))),
        ("transport_status", true) => ("Actualización de transporte", format!("El estado de su transporte ahora es '{}'.", payload.get("status").and_then(|v| v.as_str()).unwrap_or(""))),
        (_, false) => ("Appointment update", "There is an update about your appointment.".to_string()),
        (_, true) => ("Actualización de cita", "Hay una actualización sobre su cita.".to_string()),
    };
    (subject.to_string(), body)
}

// ---------------------------------------------------------------------------
// Delivery worker
// ---------------------------------------------------------------------------

#[derive(Debug, Default, serde::Serialize)]
pub struct PassReport {
    pub claimed: usize,
    pub delivered: usize,
    pub failed: usize,
    pub dead: usize,
    pub skipped: usize,
}

/// Whether the notification is still relevant when it comes due.
async fn still_relevant(
    conn: &mut PgConnection,
    kind: &str,
    appointment_id: Option<Uuid>,
) -> Result<bool, ApiError> {
    let Some(id) = appointment_id else {
        return Ok(true);
    };
    let row = sqlx::query("SELECT status, patient_confirmed_at FROM appointments WHERE id = $1")
        .bind(id)
        .fetch_optional(&mut *conn)
        .await?;
    let Some(row) = row else { return Ok(false) };
    let status: String = row.get("status");
    let confirmed: Option<DateTime<Utc>> = row.get("patient_confirmed_at");
    Ok(match kind {
        "reminder" | "preparation" => status == "confirmed",
        "confirmation_request" | "confirmation_follow_up" | "no_response_follow_up" => {
            status == "confirmed" && confirmed.is_none()
        }
        _ => true,
    })
}

/// Claim due notifications with `FOR UPDATE SKIP LOCKED` so concurrent
/// replicas never pick the same row, then deliver each one. Lapsed claims
/// (`locked_until` in the past) are reclaimable.
pub async fn run_pass(state: &AppState, worker_id: &str) -> Result<PassReport, ApiError> {
    let cfg = &state.runtime.notifications;
    let claimed = sqlx::query(
        "UPDATE notifications n SET status = 'delivering', locked_by = $1,
                locked_until = now() + make_interval(mins => $3), attempts = attempts + 1, updated_at = now()
         WHERE id IN (
            SELECT id FROM notifications
            WHERE ((status IN ('scheduled','failed') AND scheduled_for <= now()
                    AND (next_attempt_at IS NULL OR next_attempt_at <= now()))
                   OR (status = 'delivering' AND locked_until < now()))
            ORDER BY scheduled_for
            LIMIT $2
            FOR UPDATE SKIP LOCKED)
         RETURNING id, tenant_id, patient_id, kind, appointment_id, offer_id, language, time_zone,
                   channels, payload, attempts",
    )
    .bind(worker_id)
    .bind(cfg.batch_size)
    .bind(CLAIM_MINUTES as i32)
    .fetch_all(&state.pool)
    .await?;
    let mut report = PassReport {
        claimed: claimed.len(),
        ..Default::default()
    };
    for row in claimed {
        let id: Uuid = row.get("id");
        let tenant_id: Uuid = row.get("tenant_id");
        let patient_id: Option<Uuid> = row.get("patient_id");
        let kind: String = row.get("kind");
        let appointment_id: Option<Uuid> = row.get("appointment_id");
        let language: String = row.get("language");
        let time_zone: String = row.get("time_zone");
        let channels: Vec<String> = row.get("channels");
        let payload: Value = row.get("payload");
        let attempts: i32 = row.get("attempts");
        let ctx = system_context(tenant_id, worker_id);
        let mut conn = state.pool.acquire().await?;
        if !still_relevant(&mut conn, &kind, appointment_id).await? {
            sqlx::query("UPDATE notifications SET status = 'cancelled', locked_by = NULL, locked_until = NULL, updated_at = now() WHERE id = $1")
                .bind(id)
                .execute(&mut *conn)
                .await?;
            report.skipped += 1;
            continue;
        }
        let (subject, body) = render(&kind, &language, &time_zone, &payload);
        let mut any_failed = false;
        let mut error_code: Option<&'static str> = None;
        for channel in &channels {
            let outcome = match channel.as_str() {
                "in_app" => Outcome::Delivered,
                "email" => deliver_email(state, cfg, tenant_id, patient_id, &subject, &body).await,
                "webhook" => {
                    deliver_webhook(
                        state, cfg, tenant_id, patient_id, id, &kind, &subject, &body,
                    )
                    .await
                }
                _ => Outcome::Skipped("unknown_channel"),
            };
            let (status, code) = match &outcome {
                Outcome::Delivered => ("delivered", None),
                Outcome::Skipped(c) => ("skipped", Some(*c)),
                Outcome::Failed(c) => {
                    any_failed = true;
                    error_code = Some(c);
                    ("failed", Some(*c))
                }
            };
            sqlx::query(
                "INSERT INTO notification_deliveries (id, tenant_id, notification_id, channel, attempt, status, error_code)
                 VALUES ($1,$2,$3,$4,$5,$6,$7) ON CONFLICT (notification_id, channel, attempt) DO NOTHING",
            )
            .bind(Uuid::now_v7())
            .bind(tenant_id)
            .bind(id)
            .bind(channel)
            .bind(attempts)
            .bind(status)
            .bind(code)
            .execute(&mut *conn)
            .await?;
        }
        if !any_failed {
            sqlx::query(
                "UPDATE notifications SET status = 'delivered', delivered_at = now(), locked_by = NULL,
                        locked_until = NULL, last_error_code = NULL, updated_at = now() WHERE id = $1",
            )
            .bind(id)
            .execute(&mut *conn)
            .await?;
            audit::emit(
                &mut *conn,
                &ctx,
                "notification.delivered",
                &state.cell,
                json!({"notification_id": id, "kind": kind}),
                None,
            )
            .await
            .map_err(ApiError::internal)?;
            report.delivered += 1;
        } else if attempts >= cfg.max_attempts {
            sqlx::query(
                "UPDATE notifications SET status = 'dead', locked_by = NULL, locked_until = NULL,
                        last_error_code = $2, updated_at = now() WHERE id = $1",
            )
            .bind(id)
            .bind(error_code)
            .execute(&mut *conn)
            .await?;
            audit::emit(
                &mut *conn,
                &ctx,
                "notification.dead_lettered",
                &state.cell,
                json!({"notification_id": id, "kind": kind, "attempts": attempts}),
                None,
            )
            .await
            .map_err(ApiError::internal)?;
            tracing::warn!(notification_id = %id, kind = %kind, attempts, "notification dead-lettered");
            report.dead += 1;
        } else {
            let backoff = backoff_for(cfg, attempts);
            sqlx::query(
                "UPDATE notifications SET status = 'failed', locked_by = NULL, locked_until = NULL,
                        next_attempt_at = now() + make_interval(secs => $2), last_error_code = $3, updated_at = now()
                 WHERE id = $1",
            )
            .bind(id)
            .bind(backoff.as_secs_f64())
            .bind(error_code)
            .execute(&mut *conn)
            .await?;
            audit::emit(
                &mut *conn,
                &ctx,
                "notification.failed",
                &state.cell,
                json!({"notification_id": id, "kind": kind, "attempt": attempts}),
                None,
            )
            .await
            .map_err(ApiError::internal)?;
            report.failed += 1;
        }
    }
    Ok(report)
}

/// Exponential backoff: base × 2^(attempt−1), capped.
pub fn backoff_for(cfg: &NotificationConfig, attempt: i32) -> std::time::Duration {
    let exp = attempt.saturating_sub(1).clamp(0, 20) as u32;
    let secs = cfg.base_backoff.as_secs().saturating_mul(1u64 << exp);
    std::time::Duration::from_secs(secs.min(cfg.max_backoff.as_secs()))
}

/// Audit identity for background delivery and sweeps.
pub fn system_context(tenant_id: Uuid, worker_id: &str) -> AuthContext {
    AuthContext {
        user_id: Uuid::nil(),
        tenant_id,
        username: format!("system:{worker_id}"),
        display_name: "WellOS scheduling worker".into(),
        is_service: true,
        roles: Vec::new(),
        assignments: Vec::new(),
        scopes: Vec::new(),
        purpose_of_use: crate::policy::Purpose::Operations,
        break_glass_reason: None,
        web_session_id: None,
        correlation_id: Uuid::now_v7(),
    }
}

enum Outcome {
    Delivered,
    Skipped(&'static str),
    Failed(&'static str),
}

async fn sealed_contact(
    state: &AppState,
    tenant_id: Uuid,
    patient_id: Option<Uuid>,
    column: &str,
) -> Result<Option<String>, &'static str> {
    let Some(patient_id) = patient_id else {
        return Ok(None);
    };
    let sql = format!(
        "SELECT {column} FROM patient_scheduling_preferences WHERE tenant_id = $1 AND patient_id = $2"
    );
    let sealed: Option<Option<Vec<u8>>> = sqlx::query_scalar(&sql)
        .bind(tenant_id)
        .bind(patient_id)
        .fetch_optional(&state.pool)
        .await
        .map_err(|_| "contact_lookup_failed")?;
    let Some(Some(sealed)) = sealed else {
        return Ok(None);
    };
    let keyring = state
        .runtime
        .location
        .keyring
        .as_ref()
        .ok_or("encryption_unavailable")?;
    let plain = keyring.open(&sealed).map_err(|_| "contact_undecryptable")?;
    String::from_utf8(plain)
        .map(Some)
        .map_err(|_| "contact_undecryptable")
}

async fn deliver_email(
    state: &AppState,
    cfg: &NotificationConfig,
    tenant_id: Uuid,
    patient_id: Option<Uuid>,
    subject: &str,
    body: &str,
) -> Outcome {
    let address = match sealed_contact(state, tenant_id, patient_id, "contact_email_enc").await {
        Ok(Some(a)) => a,
        Ok(None) => return Outcome::Skipped("no_email_address"),
        Err(c) => return Outcome::Failed(c),
    };
    if cfg.dev_sink {
        return Outcome::Delivered;
    }
    let Some(smtp) = cfg.smtp.as_ref() else {
        return Outcome::Skipped("email_delivery_disabled");
    };
    match send_smtp(smtp, &address, subject, body).await {
        Ok(()) => Outcome::Delivered,
        Err(code) => Outcome::Failed(code),
    }
}

async fn send_smtp(
    smtp: &SmtpConfig,
    to: &str,
    subject: &str,
    body: &str,
) -> Result<(), &'static str> {
    use lettre::message::header::ContentType;
    use lettre::transport::smtp::authentication::Credentials;
    use lettre::{AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};
    let email = Message::builder()
        .from(smtp.from.parse().map_err(|_| "smtp_from_invalid")?)
        .to(to.parse().map_err(|_| "smtp_recipient_invalid")?)
        .subject(subject)
        .header(ContentType::TEXT_PLAIN)
        .body(body.to_string())
        .map_err(|_| "smtp_build_failed")?;
    let builder = match smtp.tls {
        SmtpTls::StartTls => AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(&smtp.host)
            .map_err(|_| "smtp_transport_invalid")?,
        SmtpTls::Implicit => AsyncSmtpTransport::<Tokio1Executor>::relay(&smtp.host)
            .map_err(|_| "smtp_transport_invalid")?,
        SmtpTls::None => AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(&smtp.host),
    };
    let transport = builder
        .port(smtp.port)
        .credentials(Credentials::new(
            smtp.username.clone(),
            smtp.password.clone(),
        ))
        .timeout(Some(smtp.timeout))
        .build();
    transport
        .send(email)
        .await
        .map(|_| ())
        .map_err(|_| "smtp_send_failed")
}

#[allow(clippy::too_many_arguments)]
async fn deliver_webhook(
    state: &AppState,
    cfg: &NotificationConfig,
    tenant_id: Uuid,
    patient_id: Option<Uuid>,
    notification_id: Uuid,
    kind: &str,
    subject: &str,
    body: &str,
) -> Outcome {
    let endpoint = match sealed_contact(state, tenant_id, patient_id, "push_endpoint_enc").await {
        Ok(Some(e)) => e,
        Ok(None) => return Outcome::Skipped("no_push_endpoint"),
        Err(c) => return Outcome::Failed(c),
    };
    if cfg.dev_sink {
        return Outcome::Delivered;
    }
    let Some(hook) = cfg.webhook.as_ref() else {
        return Outcome::Skipped("webhook_delivery_disabled");
    };
    match send_webhook(hook, &endpoint, notification_id, kind, subject, body).await {
        Ok(()) => Outcome::Delivered,
        Err(code) => Outcome::Failed(code),
    }
}

/// HMAC-SHA256 over `"{timestamp}.{body}"`, hex-encoded, in
/// `X-WellOS-Signature`; the receiver rejects stale timestamps.
pub fn sign_webhook(secret: &str, timestamp: i64, body: &str) -> String {
    let mut mac =
        Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("hmac accepts any key length");
    mac.update(format!("{timestamp}.{body}").as_bytes());
    hex::encode(mac.finalize().into_bytes())
}

async fn send_webhook(
    hook: &WebhookConfig,
    device_endpoint: &str,
    notification_id: Uuid,
    kind: &str,
    subject: &str,
    body: &str,
) -> Result<(), &'static str> {
    let payload = json!({
        "notification_id": notification_id,
        "kind": kind,
        "device_endpoint": device_endpoint,
        "subject": subject,
        "body": body,
    })
    .to_string();
    let ts = Utc::now().timestamp();
    let signature = sign_webhook(&hook.secret, ts, &payload);
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(hook.timeout)
        .build()
        .map_err(|_| "webhook_client_failed")?;
    let resp = client
        .post(&hook.url)
        .header("content-type", "application/json")
        .header("x-wellos-timestamp", ts.to_string())
        .header("x-wellos-signature", signature)
        .body(payload)
        .send()
        .await
        .map_err(|_| "webhook_unreachable")?;
    if resp.status().is_success() {
        Ok(())
    } else {
        Err("webhook_rejected")
    }
}

/// Full scheduling tick: expire holds/offers per tenant, cascade waitlist
/// offers, purge live locations, deliver due notifications.
pub async fn scheduling_tick(state: &AppState, worker_id: &str) -> Result<Value, ApiError> {
    let tenants: Vec<Uuid> = sqlx::query_scalar("SELECT id FROM tenants")
        .fetch_all(&state.pool)
        .await?;
    let mut expired_offers = 0usize;
    let mut released_holds = 0u64;
    let mut cascaded = 0usize;
    for tenant_id in tenants {
        let ctx = system_context(tenant_id, worker_id);
        let mut tx = state.pool.begin().await?;
        let (released, expired) =
            scheduling::sweep_expired(&mut tx, &ctx, &state.cell, tenant_id).await?;
        released_holds += released;
        expired_offers += expired.len();
        for offer_id in expired {
            cascaded +=
                crate::recovery::on_offer_closed(&mut tx, &ctx, state, offer_id, "expired").await?;
        }
        tx.commit().await?;
    }
    let purged = crate::transport::purge_expired_locations(state, worker_id).await?;
    let pass = run_pass(state, worker_id).await?;
    Ok(json!({
        "released_holds": released_holds,
        "expired_offers": expired_offers,
        "recovery_cascaded": cascaded,
        "live_locations_purged": purged,
        "notifications": pass,
    }))
}
