//! Clinical documents and imaging references.
//!
//! Document bytes live in the configured object store under a
//! tenant/patient/document key; PostgreSQL keeps checksum, size, MIME type,
//! quarantine state and release flag. Registration pre-signs an upload pinned
//! to the declared SHA-256; completion verifies the stored object against
//! the registration; only `clean` documents can be downloaded and only
//! released ones reach the patient. Imaging studies are controlled
//! references (Study/Series UIDs and a configured PACS endpoint code), never
//! pixels and never caller-supplied URLs. A disabled store fails closed.

use super::{check_len, guard_order, load_order, OrderRow, MAX_SHORT};
use crate::audit;
use crate::auth::AuthContext;
use crate::error::ApiError;
use crate::objectstore::{
    document_key, sha256_hex_valid, ObjectStoreError, PresignedUrl, ALLOWED_MIME_TYPES,
};
use crate::policy::actions;
use crate::state::AppState;
#[cfg(feature = "dev-fixtures")]
use axum::body::Bytes;
#[cfg(feature = "dev-fixtures")]
use axum::extract::Query;
use axum::extract::{Path, State};
use axum::http::StatusCode;
#[cfg(feature = "dev-fixtures")]
use axum::http::{header, HeaderMap};
#[cfg(feature = "dev-fixtures")]
use axum::response::{IntoResponse, Response};
use axum::Json;
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::postgres::PgRow;
use sqlx::{PgConnection, Row};
#[cfg(feature = "dev-fixtures")]
use std::collections::BTreeMap;
use uuid::Uuid;

pub const DOCUMENT_KINDS: &[&str] = &[
    "report_pdf",
    "image",
    "tracing",
    "referral",
    "consent",
    "other",
];
pub const IMAGING_STATUSES: &[&str] = &["registered", "available", "cancelled", "entered_in_error"];
const MAX_SERIES: usize = 200;
const MAX_UID: usize = 64;

pub fn store_error(e: ObjectStoreError) -> ApiError {
    match e {
        ObjectStoreError::Unavailable => ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "object_store_unavailable",
            "document storage is not configured; results and reports remain available",
        ),
        ObjectStoreError::InvalidKey => ApiError::internal("object key rejected by the store"),
        ObjectStoreError::Upstream(detail) => {
            tracing::warn!(error = %detail, "object store request failed");
            ApiError::new(
                StatusCode::BAD_GATEWAY,
                "object_store_error",
                "document storage did not accept the request; retry later",
            )
        }
        ObjectStoreError::Mismatch => ApiError::conflict(
            "object_mismatch",
            "stored object does not match the registered checksum or size",
        ),
    }
}

fn presigned_json(p: &PresignedUrl) -> Value {
    json!({
        "method": p.method,
        "url": p.url,
        "headers": p.headers.iter().map(|(k, v)| json!({ "name": k, "value": v })).collect::<Vec<_>>(),
        "expires_at": p.expires_at,
    })
}

// ---------------------------------------------------------------------------
// Documents
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct DocumentRow {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub patient_id: Uuid,
    pub service_request_id: Option<Uuid>,
    pub diagnostic_report_id: Option<Uuid>,
    pub kind: String,
    pub title: String,
    pub mime_type: String,
    pub size_bytes: i64,
    pub checksum_sha256: String,
    pub object_key: String,
    pub store_kind: String,
    pub status: String,
    pub scan_verdict: Option<String>,
    pub scanned_at: Option<DateTime<Utc>>,
    pub released: bool,
    pub uploaded_by: Option<Uuid>,
    pub source_system: String,
    pub created_at: DateTime<Utc>,
}

const DOC_COLUMNS: &str = "d.id, d.tenant_id, d.patient_id, d.service_request_id, d.diagnostic_report_id, d.kind, d.title,
    d.mime_type, d.size_bytes, d.checksum_sha256, d.object_key, d.store_kind, d.status, d.scan_verdict, d.scanned_at,
    d.released, d.uploaded_by, d.source_system, d.created_at";

fn document_from_row(r: &PgRow) -> DocumentRow {
    DocumentRow {
        id: r.get("id"),
        tenant_id: r.get("tenant_id"),
        patient_id: r.get("patient_id"),
        service_request_id: r.get("service_request_id"),
        diagnostic_report_id: r.get("diagnostic_report_id"),
        kind: r.get("kind"),
        title: r.get("title"),
        mime_type: r.get("mime_type"),
        size_bytes: r.get("size_bytes"),
        checksum_sha256: r.get("checksum_sha256"),
        object_key: r.get("object_key"),
        store_kind: r.get("store_kind"),
        status: r.get("status"),
        scan_verdict: r.get("scan_verdict"),
        scanned_at: r.get("scanned_at"),
        released: r.get("released"),
        uploaded_by: r.get("uploaded_by"),
        source_system: r.get("source_system"),
        created_at: r.get("created_at"),
    }
}

/// Staff view. The object key is internal and never serialized.
pub fn document_json(d: &DocumentRow) -> Value {
    json!({
        "id": d.id,
        "patient_id": d.patient_id,
        "service_request_id": d.service_request_id,
        "diagnostic_report_id": d.diagnostic_report_id,
        "kind": d.kind,
        "title": d.title,
        "mime_type": d.mime_type,
        "size_bytes": d.size_bytes,
        "checksum_sha256": d.checksum_sha256,
        "store_kind": d.store_kind,
        "status": d.status,
        "scan_verdict": d.scan_verdict,
        "scanned_at": d.scanned_at,
        "released": d.released,
        "uploaded_by": d.uploaded_by,
        "source_system": d.source_system,
        "created_at": d.created_at,
        "downloadable": d.status == "clean",
    })
}

/// Patient view: no checksum, store or uploader details.
pub fn document_patient_json(d: &DocumentRow) -> Value {
    json!({
        "id": d.id,
        "diagnostic_report_id": d.diagnostic_report_id,
        "kind": d.kind,
        "title": d.title,
        "mime_type": d.mime_type,
        "size_bytes": d.size_bytes,
        "created_at": d.created_at,
    })
}

pub async fn load_document(
    conn: &mut PgConnection,
    tenant_id: Uuid,
    id: Uuid,
    for_update: bool,
) -> Result<DocumentRow, ApiError> {
    let lock = if for_update { "FOR UPDATE" } else { "" };
    let row = sqlx::query(&format!(
        "SELECT {DOC_COLUMNS} FROM clinical_documents d WHERE d.id = $1 AND d.tenant_id = $2 {lock}"
    ))
    .bind(id)
    .bind(tenant_id)
    .fetch_optional(&mut *conn)
    .await?
    .ok_or_else(ApiError::not_found)?;
    Ok(document_from_row(&row))
}

pub async fn list_for_order_json(
    conn: &mut PgConnection,
    order_id: Uuid,
    released_only: bool,
) -> Result<Vec<Value>, ApiError> {
    let rows = sqlx::query(&format!(
        "SELECT {DOC_COLUMNS} FROM clinical_documents d
         WHERE d.service_request_id = $1 AND d.status <> 'deleted'
           AND (NOT $2::boolean OR (d.released AND d.status = 'clean'))
         ORDER BY d.created_at, d.id"
    ))
    .bind(order_id)
    .bind(released_only)
    .fetch_all(&mut *conn)
    .await?;
    Ok(rows
        .iter()
        .map(document_from_row)
        .map(|d| {
            if released_only {
                document_patient_json(&d)
            } else {
                document_json(&d)
            }
        })
        .collect())
}

pub async fn list_for_report_json(
    conn: &mut PgConnection,
    report_id: Uuid,
    released_only: bool,
) -> Result<Vec<Value>, ApiError> {
    let rows = sqlx::query(&format!(
        "SELECT {DOC_COLUMNS} FROM clinical_documents d
         WHERE d.diagnostic_report_id = $1 AND d.status <> 'deleted'
           AND (NOT $2::boolean OR (d.released AND d.status = 'clean'))
         ORDER BY d.created_at, d.id"
    ))
    .bind(report_id)
    .bind(released_only)
    .fetch_all(&mut *conn)
    .await?;
    Ok(rows
        .iter()
        .map(document_from_row)
        .map(|d| {
            if released_only {
                document_patient_json(&d)
            } else {
                document_json(&d)
            }
        })
        .collect())
}

#[derive(Debug, Deserialize)]
pub struct RegisterBody {
    pub kind: String,
    pub title: String,
    pub mime_type: String,
    pub size_bytes: i64,
    pub checksum_sha256: String,
    pub diagnostic_report_id: Option<Uuid>,
    pub source_system: Option<String>,
}

fn require_store(state: &AppState) -> Result<(), ApiError> {
    if state.object_store.kind() == "disabled" {
        return Err(store_error(ObjectStoreError::Unavailable));
    }
    Ok(())
}

/// `POST /api/v1/diagnostics/orders/:id/documents` — register metadata and
/// obtain a pre-signed upload pinned to the declared checksum.
pub async fn register(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(order_id): Path<Uuid>,
    Json(body): Json<RegisterBody>,
) -> Result<Json<Value>, ApiError> {
    let kind = body.kind.trim();
    if !DOCUMENT_KINDS.contains(&kind) {
        return Err(ApiError::bad_request(
            "validation_failed",
            "kind must be report_pdf, image, tracing, referral, consent or other",
        ));
    }
    let title = check_len("title", Some(&body.title), MAX_SHORT)?
        .ok_or_else(|| ApiError::bad_request("validation_failed", "title is required"))?;
    let mime = body.mime_type.trim().to_ascii_lowercase();
    if !ALLOWED_MIME_TYPES.contains(&mime.as_str()) {
        return Err(ApiError::bad_request(
            "unsupported_media_type",
            "mime_type is not accepted for clinical documents (DICOM is referenced through imaging studies)",
        ));
    }
    let max_bytes = state.runtime.object_store.max_bytes;
    if body.size_bytes <= 0 || body.size_bytes > max_bytes {
        return Err(ApiError::bad_request(
            "validation_failed",
            format!("size_bytes must be between 1 and {max_bytes}"),
        ));
    }
    let checksum = body.checksum_sha256.trim().to_ascii_lowercase();
    if !sha256_hex_valid(&checksum) {
        return Err(ApiError::bad_request(
            "validation_failed",
            "checksum_sha256 must be 64 hexadecimal characters",
        ));
    }
    let source_system = check_len("source_system", body.source_system.as_deref(), 128)?
        .unwrap_or_else(|| "wellos-upload".to_string());
    require_store(&state)?;

    let mut conn = state.pool.acquire().await?;
    let o = load_order(&mut conn, ctx.tenant_id, order_id).await?;
    drop(conn);
    let allowed = guard_order(&state, &ctx, actions::DIAGNOSTIC_REPORT_WRITE, &o).await?;
    let mut tx = state.pool.begin().await?;
    allowed.record(&mut tx, &ctx, &state.cell).await?;
    if let Some(rid) = body.diagnostic_report_id {
        let ok: Option<Uuid> = sqlx::query_scalar(
            "SELECT id FROM diagnostic_reports WHERE id = $1 AND service_request_id = $2",
        )
        .bind(rid)
        .bind(o.id)
        .fetch_optional(&mut *tx)
        .await?;
        if ok.is_none() {
            return Err(ApiError::bad_request(
                "validation_failed",
                "diagnostic_report_id does not belong to this order",
            ));
        }
    }
    let id = Uuid::now_v7();
    let key = document_key(o.tenant_id, o.patient_id, id);
    let upload = state
        .object_store
        .presign_upload(&key, &mime, &checksum, state.runtime.object_store.url_ttl)
        .await
        .map_err(store_error)?;
    sqlx::query(
        "INSERT INTO clinical_documents
         (id, tenant_id, patient_id, service_request_id, diagnostic_report_id, kind, title, mime_type, size_bytes,
          checksum_sha256, object_key, store_kind, status, uploaded_by, source_system)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,'quarantined',$13,$14)",
    )
    .bind(id)
    .bind(o.tenant_id)
    .bind(o.patient_id)
    .bind(o.id)
    .bind(body.diagnostic_report_id)
    .bind(kind)
    .bind(&title)
    .bind(&mime)
    .bind(body.size_bytes)
    .bind(&checksum)
    .bind(&key)
    .bind(state.object_store.kind())
    .bind(ctx.user_id)
    .bind(&source_system)
    .execute(&mut *tx)
    .await?;
    audit::emit(
        &mut *tx,
        &ctx,
        "clinical_document.registered",
        &state.cell,
        json!({ "document_id": id, "service_request_id": o.id, "diagnostic_report_id": body.diagnostic_report_id,
                "kind": kind, "mime_type": mime, "size_bytes": body.size_bytes, "store_kind": state.object_store.kind() }),
        None,
    )
    .await
    .map_err(ApiError::internal)?;
    tx.commit().await?;
    let mut conn = state.pool.acquire().await?;
    let d = load_document(&mut conn, ctx.tenant_id, id, false).await?;
    Ok(Json(
        json!({ "document": document_json(&d), "upload": presigned_json(&upload) }),
    ))
}

async fn order_of_document(conn: &mut PgConnection, d: &DocumentRow) -> Result<OrderRow, ApiError> {
    let order_id = d
        .service_request_id
        .ok_or_else(|| ApiError::internal("clinical document without order"))?;
    load_order(conn, d.tenant_id, order_id).await
}

/// `POST /api/v1/diagnostics/documents/:id/complete` — verify the uploaded
/// object against the registration. A verified checksum clears the
/// quarantine; a store that cannot attest the checksum leaves the document
/// `scanning` until a scanner verdict is recorded.
pub async fn complete(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let mut conn = state.pool.acquire().await?;
    let d = load_document(&mut conn, ctx.tenant_id, id, false).await?;
    let o = order_of_document(&mut conn, &d).await?;
    drop(conn);
    let allowed = guard_order(&state, &ctx, actions::DIAGNOSTIC_REPORT_WRITE, &o).await?;
    require_store(&state)?;
    let stat = state
        .object_store
        .stat(&d.object_key)
        .await
        .map_err(store_error)?;
    let mut tx = state.pool.begin().await?;
    let d = load_document(&mut tx, ctx.tenant_id, id, true).await?;
    allowed.record(&mut tx, &ctx, &state.cell).await?;
    if d.status != "quarantined" {
        return Err(ApiError::conflict(
            "document_not_pending",
            format!(
                "the document is {}; only quarantined uploads can be completed",
                d.status
            ),
        ));
    }
    let Some(stat) = stat else {
        return Err(ApiError::conflict(
            "object_missing",
            "the upload has not reached the store yet",
        ));
    };
    let verdict = if stat.size_bytes != d.size_bytes {
        Some("size_mismatch")
    } else if stat
        .sha256_hex
        .as_deref()
        .is_some_and(|s| !s.eq_ignore_ascii_case(&d.checksum_sha256))
    {
        Some("checksum_mismatch")
    } else {
        None
    };
    let (status, scan_verdict) = match (verdict, stat.sha256_hex.is_some()) {
        (Some(v), _) => ("rejected", Some(v)),
        (None, true) => ("clean", Some("checksum_verified")),
        (None, false) => ("scanning", None),
    };
    sqlx::query(
        "UPDATE clinical_documents SET status = $2, scan_verdict = $3,
                scanned_at = CASE WHEN $2 IN ('clean','rejected') THEN now() ELSE NULL END
         WHERE id = $1",
    )
    .bind(id)
    .bind(status)
    .bind(scan_verdict)
    .execute(&mut *tx)
    .await?;
    audit::emit(
        &mut *tx,
        &ctx,
        "clinical_document.completed",
        &state.cell,
        json!({ "document_id": id, "service_request_id": o.id, "status": status, "scan_verdict": scan_verdict,
                "stored_size": stat.size_bytes, "checksum_attested": stat.sha256_hex.is_some() }),
        None,
    )
    .await
    .map_err(ApiError::internal)?;
    tx.commit().await?;
    if status == "rejected" {
        return Err(ApiError::conflict(
            "object_mismatch",
            "the stored object does not match the registered size or checksum; the document was rejected",
        ));
    }
    let mut conn = state.pool.acquire().await?;
    let d = load_document(&mut conn, ctx.tenant_id, id, false).await?;
    Ok(Json(document_json(&d)))
}

#[derive(Debug, Deserialize)]
pub struct ScanVerdictBody {
    /// `clean` or `rejected`.
    pub verdict: String,
    pub detail: Option<String>,
}

/// `POST /api/v1/diagnostics/documents/:id/scan` — scanner verdict for a
/// document left `scanning` by completion (service credential or staff).
pub async fn scan_verdict(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
    Json(body): Json<ScanVerdictBody>,
) -> Result<Json<Value>, ApiError> {
    let status = match body.verdict.trim() {
        "clean" => "clean",
        "rejected" => "rejected",
        _ => {
            return Err(ApiError::bad_request(
                "validation_failed",
                "verdict must be clean or rejected",
            ))
        }
    };
    let detail = check_len("detail", body.detail.as_deref(), MAX_SHORT)?;
    let mut conn = state.pool.acquire().await?;
    let d = load_document(&mut conn, ctx.tenant_id, id, false).await?;
    let o = order_of_document(&mut conn, &d).await?;
    drop(conn);
    let allowed = guard_order(&state, &ctx, actions::DIAGNOSTIC_REPORT_WRITE, &o).await?;
    let mut tx = state.pool.begin().await?;
    let d = load_document(&mut tx, ctx.tenant_id, id, true).await?;
    allowed.record(&mut tx, &ctx, &state.cell).await?;
    if d.status != "scanning" {
        return Err(ApiError::conflict(
            "document_not_scanning",
            format!(
                "the document is {}; only scanning documents accept a verdict",
                d.status
            ),
        ));
    }
    let verdict = detail.unwrap_or_else(|| format!("scanner_{status}"));
    sqlx::query(
        "UPDATE clinical_documents SET status = $2, scan_verdict = $3, scanned_at = now() WHERE id = $1",
    )
    .bind(id)
    .bind(status)
    .bind(&verdict)
    .execute(&mut *tx)
    .await?;
    audit::emit(
        &mut *tx,
        &ctx,
        "clinical_document.scanned",
        &state.cell,
        json!({ "document_id": id, "service_request_id": o.id, "status": status }),
        None,
    )
    .await
    .map_err(ApiError::internal)?;
    tx.commit().await?;
    let mut conn = state.pool.acquire().await?;
    let d = load_document(&mut conn, ctx.tenant_id, id, false).await?;
    Ok(Json(document_json(&d)))
}

/// Pre-signed download for a clean document; the caller is already
/// authorized for the document's order/patient.
pub async fn presign_download_in(
    conn: &mut PgConnection,
    ctx: &AuthContext,
    state: &AppState,
    d: &DocumentRow,
    surface: &str,
) -> Result<Value, ApiError> {
    if d.status != "clean" {
        return Err(ApiError::conflict(
            "document_not_available",
            format!(
                "the document is {}; only clean documents can be downloaded",
                d.status
            ),
        ));
    }
    require_store(state)?;
    let url = state
        .object_store
        .presign_download(&d.object_key, state.runtime.object_store.url_ttl)
        .await
        .map_err(store_error)?;
    audit::emit(
        &mut *conn,
        ctx,
        "clinical_document.downloaded",
        &state.cell,
        json!({ "document_id": d.id, "patient_id": d.patient_id, "service_request_id": d.service_request_id,
                "diagnostic_report_id": d.diagnostic_report_id, "surface": surface, "expires_at": url.expires_at }),
        None,
    )
    .await
    .map_err(ApiError::internal)?;
    Ok(json!({ "document": document_json(d), "download": presigned_json(&url) }))
}

/// `GET /api/v1/diagnostics/documents/:id/download`
pub async fn download(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let mut conn = state.pool.acquire().await?;
    let d = load_document(&mut conn, ctx.tenant_id, id, false).await?;
    let o = order_of_document(&mut conn, &d).await?;
    let allowed = guard_order(&state, &ctx, actions::DIAGNOSTIC_READ, &o).await?;
    allowed.record(&mut conn, &ctx, &state.cell).await?;
    let v = presign_download_in(&mut conn, &ctx, &state, &d, "staff").await?;
    Ok(Json(v))
}

// ---------------------------------------------------------------------------
// Imaging studies (controlled references)
// ---------------------------------------------------------------------------

fn imaging_json(r: &PgRow) -> Value {
    json!({
        "id": r.get::<Uuid, _>("id"),
        "service_request_id": r.get::<Uuid, _>("service_request_id"),
        "diagnostic_report_id": r.get::<Option<Uuid>, _>("diagnostic_report_id"),
        "study_instance_uid": r.get::<String, _>("study_instance_uid"),
        "accession_number": r.get::<Option<String>, _>("accession_number"),
        "modality_code": r.get::<String, _>("modality_code"),
        "description": r.get::<Option<String>, _>("description"),
        "series": r.get::<Value, _>("series"),
        "number_of_series": r.get::<i32, _>("number_of_series"),
        "number_of_instances": r.get::<i32, _>("number_of_instances"),
        "pacs_endpoint_code": r.get::<Option<String>, _>("pacs_endpoint_code"),
        "status": r.get::<String, _>("status"),
        "started_at": r.get::<Option<DateTime<Utc>>, _>("started_at"),
        "source_system": r.get::<String, _>("source_system"),
        "created_at": r.get::<DateTime<Utc>, _>("created_at"),
    })
}

const IMAGING_COLUMNS: &str = "id, service_request_id, diagnostic_report_id, study_instance_uid, accession_number,
    modality_code, description, series, number_of_series, number_of_instances, pacs_endpoint_code, status,
    started_at, source_system, created_at";

pub async fn imaging_for_order_json(
    conn: &mut PgConnection,
    order_id: Uuid,
) -> Result<Vec<Value>, ApiError> {
    let rows = sqlx::query(&format!(
        "SELECT {IMAGING_COLUMNS} FROM imaging_studies WHERE service_request_id = $1 ORDER BY created_at, id"
    ))
    .bind(order_id)
    .fetch_all(&mut *conn)
    .await?;
    Ok(rows.iter().map(imaging_json).collect())
}

pub async fn imaging_for_report_json(
    conn: &mut PgConnection,
    report_id: Uuid,
) -> Result<Vec<Value>, ApiError> {
    let rows = sqlx::query(&format!(
        "SELECT {IMAGING_COLUMNS} FROM imaging_studies WHERE diagnostic_report_id = $1 ORDER BY created_at, id"
    ))
    .bind(report_id)
    .fetch_all(&mut *conn)
    .await?;
    Ok(rows.iter().map(imaging_json).collect())
}

fn valid_uid(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= MAX_UID
        && s.split('.')
            .all(|part| !part.is_empty() && part.chars().all(|c| c.is_ascii_digit()))
}

fn valid_code(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
}

#[derive(Debug, Deserialize)]
pub struct SeriesInput {
    pub series_instance_uid: String,
    pub modality: Option<String>,
    pub number_of_instances: Option<i32>,
    pub description: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ImagingBody {
    pub study_instance_uid: String,
    pub accession_number: Option<String>,
    pub modality_code: String,
    pub description: Option<String>,
    #[serde(default)]
    pub series: Vec<SeriesInput>,
    /// Code of a PACS/DICOMweb endpoint configured by the operator; the
    /// server never stores or follows sender-supplied URLs.
    pub pacs_endpoint_code: Option<String>,
    pub status: Option<String>,
    pub started_at: Option<DateTime<Utc>>,
    pub source_system: Option<String>,
    pub idempotency_key: String,
    pub diagnostic_report_id: Option<Uuid>,
}

/// `POST /api/v1/diagnostics/orders/:id/imaging-studies`
pub async fn register_imaging_study(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(order_id): Path<Uuid>,
    Json(body): Json<ImagingBody>,
) -> Result<Json<Value>, ApiError> {
    let study_uid = body.study_instance_uid.trim();
    if !valid_uid(study_uid) {
        return Err(ApiError::bad_request(
            "validation_failed",
            "study_instance_uid must be a DICOM UID (digits and dots, at most 64 characters)",
        ));
    }
    let modality = body.modality_code.trim().to_ascii_uppercase();
    if !valid_code(&modality) {
        return Err(ApiError::bad_request(
            "validation_failed",
            "modality_code is required",
        ));
    }
    let status = body.status.as_deref().map(str::trim).unwrap_or("available");
    if !IMAGING_STATUSES.contains(&status) {
        return Err(ApiError::bad_request(
            "validation_failed",
            "status must be registered, available, cancelled or entered_in_error",
        ));
    }
    let idem = body.idempotency_key.trim();
    if idem.is_empty() || idem.len() > 128 {
        return Err(ApiError::bad_request(
            "validation_failed",
            "idempotency_key is required (at most 128 characters)",
        ));
    }
    let accession = check_len("accession_number", body.accession_number.as_deref(), 64)?;
    let description = check_len("description", body.description.as_deref(), MAX_SHORT)?;
    let source_system = check_len("source_system", body.source_system.as_deref(), 128)?
        .unwrap_or_else(|| "wellos-manual".to_string());
    let pacs = match body.pacs_endpoint_code.as_deref().map(str::trim) {
        None | Some("") => None,
        Some(code) if valid_code(code) => Some(code.to_string()),
        Some(_) => {
            return Err(ApiError::bad_request(
                "validation_failed",
                "pacs_endpoint_code must be a configured endpoint code, not a URL",
            ))
        }
    };
    if body.series.len() > MAX_SERIES {
        return Err(ApiError::bad_request(
            "validation_failed",
            format!("at most {MAX_SERIES} series per study"),
        ));
    }
    let mut series = Vec::with_capacity(body.series.len());
    let mut instances: i64 = 0;
    for s in &body.series {
        let uid = s.series_instance_uid.trim();
        if !valid_uid(uid) {
            return Err(ApiError::bad_request(
                "validation_failed",
                "series_instance_uid must be a DICOM UID",
            ));
        }
        let n = s.number_of_instances.unwrap_or(0);
        if !(0..=100_000).contains(&n) {
            return Err(ApiError::bad_request(
                "validation_failed",
                "number_of_instances out of range",
            ));
        }
        instances += n as i64;
        series.push(json!({
            "series_instance_uid": uid,
            "modality": s.modality.as_deref().map(|m| m.trim().to_ascii_uppercase()).unwrap_or_else(|| modality.clone()),
            "number_of_instances": n,
            "description": check_len("series description", s.description.as_deref(), MAX_SHORT)?,
        }));
    }

    let mut conn = state.pool.acquire().await?;
    let o = load_order(&mut conn, ctx.tenant_id, order_id).await?;
    drop(conn);
    let allowed = guard_order(&state, &ctx, actions::DIAGNOSTIC_REPORT_WRITE, &o).await?;
    let mut tx = state.pool.begin().await?;
    allowed.record(&mut tx, &ctx, &state.cell).await?;
    let existing = sqlx::query(&format!(
        "SELECT {IMAGING_COLUMNS} FROM imaging_studies WHERE tenant_id = $1 AND idempotency_key = $2"
    ))
    .bind(o.tenant_id)
    .bind(idem)
    .fetch_optional(&mut *tx)
    .await?;
    if let Some(row) = existing {
        if row.get::<Uuid, _>("service_request_id") != o.id
            || row.get::<String, _>("study_instance_uid") != study_uid
        {
            return Err(ApiError::conflict(
                "idempotency_key_reuse",
                "idempotency_key was already used for a different study",
            ));
        }
        tx.commit().await?;
        let mut v = imaging_json(&row);
        v["duplicate"] = json!(true);
        return Ok(Json(v));
    }
    if let Some(rid) = body.diagnostic_report_id {
        let ok: Option<Uuid> = sqlx::query_scalar(
            "SELECT id FROM diagnostic_reports WHERE id = $1 AND service_request_id = $2",
        )
        .bind(rid)
        .bind(o.id)
        .fetch_optional(&mut *tx)
        .await?;
        if ok.is_none() {
            return Err(ApiError::bad_request(
                "validation_failed",
                "diagnostic_report_id does not belong to this order",
            ));
        }
    }
    let id = Uuid::now_v7();
    let inserted = sqlx::query(
        "INSERT INTO imaging_studies
         (id, tenant_id, patient_id, service_request_id, diagnostic_report_id, study_instance_uid, accession_number,
          modality_code, description, series, number_of_series, number_of_instances, pacs_endpoint_code, status,
          started_at, source_system, idempotency_key)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17)",
    )
    .bind(id)
    .bind(o.tenant_id)
    .bind(o.patient_id)
    .bind(o.id)
    .bind(body.diagnostic_report_id)
    .bind(study_uid)
    .bind(&accession)
    .bind(&modality)
    .bind(&description)
    .bind(Value::Array(series))
    .bind(body.series.len() as i32)
    .bind(instances.min(i32::MAX as i64) as i32)
    .bind(&pacs)
    .bind(status)
    .bind(body.started_at)
    .bind(&source_system)
    .bind(idem)
    .execute(&mut *tx)
    .await;
    if let Err(e) = inserted {
        if matches!(&e, sqlx::Error::Database(db) if db.is_unique_violation()) {
            return Err(ApiError::conflict(
                "study_exists",
                "a study with this Study Instance UID is already registered",
            ));
        }
        return Err(e.into());
    }
    audit::emit(
        &mut *tx,
        &ctx,
        "imaging_study.registered",
        &state.cell,
        json!({ "imaging_study_id": id, "service_request_id": o.id, "modality_code": modality,
                "series": body.series.len(), "instances": instances, "status": status, "pacs_endpoint_code": pacs }),
        None,
    )
    .await
    .map_err(ApiError::internal)?;
    tx.commit().await?;
    let row = sqlx::query(&format!(
        "SELECT {IMAGING_COLUMNS} FROM imaging_studies WHERE id = $1"
    ))
    .bind(id)
    .fetch_one(&state.pool)
    .await?;
    let mut v = imaging_json(&row);
    v["duplicate"] = json!(false);
    Ok(Json(v))
}

// ---------------------------------------------------------------------------
// Fixture transfer routes (dev-fixtures builds only)
// ---------------------------------------------------------------------------

#[cfg(feature = "dev-fixtures")]
fn fixture_store(state: &AppState) -> Result<&crate::objectstore::FixtureStore, ApiError> {
    if !state.runtime.env.is_local() {
        return Err(ApiError::not_found());
    }
    state.object_store.fixture().ok_or_else(ApiError::not_found)
}

/// `PUT /api/v1/dev/objects/*key` — pre-signed fixture upload.
#[cfg(feature = "dev-fixtures")]
pub async fn fixture_put(
    State(state): State<AppState>,
    Path(key): Path<String>,
    Query(query): Query<BTreeMap<String, String>>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, ApiError> {
    let store = fixture_store(&state)?;
    let key = key.trim_start_matches('/');
    let grant = store
        .verify("PUT", key, &query, Utc::now())
        .ok_or_else(|| ApiError::forbidden("upload grant is invalid or expired"))?;
    let ct = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if grant.content_type.as_deref() != Some(ct) {
        return Err(ApiError::bad_request(
            "validation_failed",
            "content-type must match the registered mime type",
        ));
    }
    if body.len() as i64 > store.max_bytes() {
        return Err(ApiError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "payload_too_large",
            "object exceeds the configured maximum size",
        ));
    }
    store.put(&grant, body.to_vec()).map_err(store_error)?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

/// `GET /api/v1/dev/objects/*key` — pre-signed fixture download.
#[cfg(feature = "dev-fixtures")]
pub async fn fixture_get(
    State(state): State<AppState>,
    Path(key): Path<String>,
    Query(query): Query<BTreeMap<String, String>>,
) -> Result<Response, ApiError> {
    let store = fixture_store(&state)?;
    let key = key.trim_start_matches('/');
    store
        .verify("GET", key, &query, Utc::now())
        .ok_or_else(|| ApiError::forbidden("download grant is invalid or expired"))?;
    let (bytes, content_type) = store.get(key).ok_or_else(ApiError::not_found)?;
    Ok((
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, content_type),
            (header::CACHE_CONTROL, "no-store".to_string()),
        ],
        bytes,
    )
        .into_response())
}
