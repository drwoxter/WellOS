//! Object storage for clinical documents (report PDFs, tracings, images).
//!
//! Document bytes never enter PostgreSQL: the database keeps a tenant-scoped
//! object key, checksum, size, MIME type and quarantine state, and the bytes
//! live in the configured store. Two adapters exist:
//!
//! * `s3`: any S3-compatible service (AWS S3, MinIO, Ceph RGW, ...) addressed
//!   with SigV4 pre-signed URLs. Uploads are pinned to the declared SHA-256
//!   (`x-amz-checksum-sha256`) so the store itself rejects tampered bytes;
//!   downloads are short-lived GET URLs. HTTPS is mandatory outside local
//!   environments.
//! * `fixture`: an in-process store served by a `dev-fixtures`-only route.
//!   It is refused in staging/production and absent from production
//!   binaries; there is no silent fallback from a misconfigured `s3` to it.
//!
//! Keys are always `tenants/<tenant>/patients/<patient>/documents/<id>` so a
//! bucket policy can scope credentials, and no URL, filename or report text
//! is ever logged.

use crate::runtime::{fixtures_allowed, parse_bool, parse_positive_i64, RuntimeEnv};
use async_trait::async_trait;
use base64::Engine;
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use uuid::Uuid;

type HmacSha256 = Hmac<Sha256>;

pub const MAX_URL_TTL_SECS: i64 = 900;
pub const DEFAULT_URL_TTL_SECS: i64 = 300;
pub const DEFAULT_MAX_BYTES: i64 = 50 * 1024 * 1024;

/// MIME types a clinical document may carry. DICOM objects are referenced
/// through `imaging_studies` + a configured PACS endpoint, never uploaded.
pub const ALLOWED_MIME_TYPES: &[&str] = &[
    "application/pdf",
    "image/jpeg",
    "image/png",
    "text/plain",
    "application/xml",
];

#[derive(Debug, thiserror::Error)]
pub enum ObjectStoreError {
    #[error("object store is not configured")]
    Unavailable,
    #[error("object key is malformed")]
    InvalidKey,
    #[error("object store request failed: {0}")]
    Upstream(String),
    #[error("stored object does not match the registered checksum or size")]
    Mismatch,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PresignedUrl {
    pub method: &'static str,
    pub url: String,
    /// Headers the client must send verbatim (they are part of the signature).
    pub headers: Vec<(String, String)>,
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectStat {
    pub size_bytes: i64,
    /// Hex SHA-256 when the store can attest it.
    pub sha256_hex: Option<String>,
}

#[async_trait]
pub trait ObjectStore: Send + Sync {
    fn kind(&self) -> &'static str;
    async fn presign_upload(
        &self,
        key: &str,
        content_type: &str,
        sha256_hex: &str,
        ttl: Duration,
    ) -> Result<PresignedUrl, ObjectStoreError>;
    async fn presign_download(
        &self,
        key: &str,
        ttl: Duration,
    ) -> Result<PresignedUrl, ObjectStoreError>;
    async fn stat(&self, key: &str) -> Result<Option<ObjectStat>, ObjectStoreError>;
    /// The in-process fixture store behind this adapter, when it is one. Only
    /// the development transfer routes use it; production adapters return
    /// `None`.
    fn fixture(&self) -> Option<&FixtureStore> {
        None
    }
}

/// Build the tenant-scoped key for a clinical document.
pub fn document_key(tenant_id: Uuid, patient_id: Uuid, document_id: Uuid) -> String {
    format!("tenants/{tenant_id}/patients/{patient_id}/documents/{document_id}")
}

/// Keys are restricted to the shape produced by [`document_key`] so no
/// caller-controlled path fragments reach the store.
pub fn validate_key(key: &str) -> Result<(), ObjectStoreError> {
    let parts: Vec<&str> = key.split('/').collect();
    let ok = parts.len() == 6
        && parts[0] == "tenants"
        && parts[2] == "patients"
        && parts[4] == "documents"
        && [parts[1], parts[3], parts[5]]
            .iter()
            .all(|p| Uuid::parse_str(p).is_ok());
    if ok {
        Ok(())
    } else {
        Err(ObjectStoreError::InvalidKey)
    }
}

pub fn sha256_hex_valid(s: &str) -> bool {
    s.len() == 64 && s.chars().all(|c| c.is_ascii_hexdigit())
}

fn clamp_ttl(ttl: Duration) -> i64 {
    (ttl.as_secs() as i64).clamp(1, MAX_URL_TTL_SECS)
}

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObjectStoreKind {
    Disabled,
    Fixture,
    S3,
}

impl ObjectStoreKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::Fixture => "fixture",
            Self::S3 => "s3",
        }
    }
}

#[derive(Clone)]
pub struct S3Config {
    pub endpoint: url::Url,
    pub bucket: String,
    pub region: String,
    pub access_key_id: String,
    pub secret_access_key: String,
    pub path_style: bool,
}

impl std::fmt::Debug for S3Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("S3Config")
            .field("endpoint", &self.endpoint.as_str())
            .field("bucket", &self.bucket)
            .field("region", &self.region)
            .field("path_style", &self.path_style)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone)]
pub struct ObjectStoreConfig {
    pub kind: ObjectStoreKind,
    pub s3: Option<S3Config>,
    pub url_ttl: Duration,
    pub max_bytes: i64,
}

impl ObjectStoreConfig {
    pub fn disabled() -> Self {
        Self {
            kind: ObjectStoreKind::Disabled,
            s3: None,
            url_ttl: Duration::from_secs(DEFAULT_URL_TTL_SECS as u64),
            max_bytes: DEFAULT_MAX_BYTES,
        }
    }

    pub fn fixture() -> Self {
        Self {
            kind: ObjectStoreKind::Fixture,
            ..Self::disabled()
        }
    }

    /// `WELLOS_OBJECT_STORE=disabled|fixture|s3`. Unset means disabled:
    /// document upload/download answer `503 object_store_unavailable` while
    /// every other diagnostic workflow continues. `fixture` is subject to
    /// the same refusal as every other fixture (local env + fixture build);
    /// `s3` requires the full credential set and an HTTPS endpoint outside
    /// local environments.
    pub fn from_env(env: RuntimeEnv) -> anyhow::Result<Self> {
        let raw = std::env::var("WELLOS_OBJECT_STORE").unwrap_or_else(|_| "disabled".into());
        let ttl = parse_positive_i64("WELLOS_OBJECT_URL_TTL_SECS", DEFAULT_URL_TTL_SECS)?;
        if ttl > MAX_URL_TTL_SECS {
            anyhow::bail!("WELLOS_OBJECT_URL_TTL_SECS must be at most {MAX_URL_TTL_SECS}");
        }
        let max_bytes = parse_positive_i64("WELLOS_OBJECT_MAX_BYTES", DEFAULT_MAX_BYTES)?;
        let base = Self {
            kind: ObjectStoreKind::Disabled,
            s3: None,
            url_ttl: Duration::from_secs(ttl as u64),
            max_bytes,
        };
        match raw.trim() {
            "disabled" => Ok(base),
            "fixture" => {
                fixtures_allowed(env, "WELLOS_OBJECT_STORE=fixture")?;
                Ok(Self {
                    kind: ObjectStoreKind::Fixture,
                    ..base
                })
            }
            "s3" => {
                let endpoint = std::env::var("WELLOS_S3_ENDPOINT")
                    .map_err(|_| anyhow::anyhow!("WELLOS_S3_ENDPOINT is required for s3"))?;
                let endpoint = url::Url::parse(endpoint.trim())
                    .map_err(|_| anyhow::anyhow!("WELLOS_S3_ENDPOINT must be an absolute URL"))?;
                if endpoint.host_str().is_none() {
                    anyhow::bail!("WELLOS_S3_ENDPOINT must include a host");
                }
                if endpoint.scheme() != "https" && !(env.is_local() && endpoint.scheme() == "http")
                {
                    anyhow::bail!(
                        "WELLOS_S3_ENDPOINT must use https (http is permitted only in local environments)"
                    );
                }
                let bucket = std::env::var("WELLOS_S3_BUCKET")
                    .ok()
                    .map(|b| b.trim().to_string())
                    .filter(|b| !b.is_empty())
                    .ok_or_else(|| anyhow::anyhow!("WELLOS_S3_BUCKET is required for s3"))?;
                if !bucket
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '.')
                {
                    anyhow::bail!(
                        "WELLOS_S3_BUCKET contains characters outside the S3 naming rules"
                    );
                }
                let region = std::env::var("WELLOS_S3_REGION")
                    .ok()
                    .map(|r| r.trim().to_string())
                    .filter(|r| !r.is_empty())
                    .ok_or_else(|| anyhow::anyhow!("WELLOS_S3_REGION is required for s3"))?;
                let access_key_id = std::env::var("WELLOS_S3_ACCESS_KEY_ID")
                    .ok()
                    .filter(|v| !v.trim().is_empty())
                    .ok_or_else(|| anyhow::anyhow!("WELLOS_S3_ACCESS_KEY_ID is required for s3"))?;
                let secret_access_key = std::env::var("WELLOS_S3_SECRET_ACCESS_KEY")
                    .ok()
                    .filter(|v| !v.trim().is_empty())
                    .ok_or_else(|| {
                        anyhow::anyhow!("WELLOS_S3_SECRET_ACCESS_KEY is required for s3")
                    })?;
                let path_style = parse_bool("WELLOS_S3_PATH_STYLE")?.unwrap_or(true);
                Ok(Self {
                    kind: ObjectStoreKind::S3,
                    s3: Some(S3Config {
                        endpoint,
                        bucket,
                        region,
                        access_key_id,
                        secret_access_key,
                        path_style,
                    }),
                    ..base
                })
            }
            other => anyhow::bail!(
                "WELLOS_OBJECT_STORE must be 'disabled', 'fixture' or 's3' (got '{other}')"
            ),
        }
    }

    /// Instantiate the configured adapter. Never substitutes one adapter for
    /// another.
    pub fn build(&self) -> anyhow::Result<Arc<dyn ObjectStore>> {
        match self.kind {
            ObjectStoreKind::Disabled => Ok(Arc::new(DisabledStore)),
            ObjectStoreKind::Fixture => Ok(Arc::new(FixtureStore::new(self.max_bytes))),
            ObjectStoreKind::S3 => {
                let cfg = self.s3.clone().ok_or_else(|| {
                    anyhow::anyhow!("s3 object store selected without configuration")
                })?;
                Ok(Arc::new(S3Store::new(cfg)?))
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Disabled
// ---------------------------------------------------------------------------

pub struct DisabledStore;

#[async_trait]
impl ObjectStore for DisabledStore {
    fn kind(&self) -> &'static str {
        "disabled"
    }
    async fn presign_upload(
        &self,
        _key: &str,
        _content_type: &str,
        _sha256_hex: &str,
        _ttl: Duration,
    ) -> Result<PresignedUrl, ObjectStoreError> {
        Err(ObjectStoreError::Unavailable)
    }
    async fn presign_download(
        &self,
        _key: &str,
        _ttl: Duration,
    ) -> Result<PresignedUrl, ObjectStoreError> {
        Err(ObjectStoreError::Unavailable)
    }
    async fn stat(&self, _key: &str) -> Result<Option<ObjectStat>, ObjectStoreError> {
        Err(ObjectStoreError::Unavailable)
    }
}

// ---------------------------------------------------------------------------
// Fixture (in-process, dev-fixtures route)
// ---------------------------------------------------------------------------

/// Path prefix of the fixture transfer route (mounted by the server only in
/// `dev-fixtures` builds whose runtime selected the fixture store).
pub const FIXTURE_ROUTE_PREFIX: &str = "/api/v1/dev/objects";

#[derive(Clone)]
struct FixtureObject {
    bytes: Vec<u8>,
    content_type: String,
    sha256_hex: String,
}

pub struct FixtureStore {
    signing_key: [u8; 32],
    max_bytes: i64,
    objects: Mutex<HashMap<String, FixtureObject>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FixtureGrant {
    pub method: &'static str,
    pub key: String,
    pub content_type: Option<String>,
    pub sha256_hex: Option<String>,
}

impl FixtureStore {
    pub fn new(max_bytes: i64) -> Self {
        let mut signing_key = [0u8; 32];
        rand::RngCore::fill_bytes(&mut rand::thread_rng(), &mut signing_key);
        Self {
            signing_key,
            max_bytes,
            objects: Mutex::new(HashMap::new()),
        }
    }

    pub fn max_bytes(&self) -> i64 {
        self.max_bytes
    }

    fn sign(&self, method: &str, key: &str, exp: i64, ct: &str, sha: &str) -> String {
        let mut mac = HmacSha256::new_from_slice(&self.signing_key).expect("hmac key");
        mac.update(format!("{method}\n{key}\n{exp}\n{ct}\n{sha}").as_bytes());
        hex::encode(mac.finalize().into_bytes())
    }

    fn grant_url(
        &self,
        method: &'static str,
        key: &str,
        ct: &str,
        sha: &str,
        ttl: Duration,
    ) -> PresignedUrl {
        let exp = Utc::now() + ChronoDuration::seconds(clamp_ttl(ttl));
        let sig = self.sign(method, key, exp.timestamp(), ct, sha);
        let mut url = format!(
            "{FIXTURE_ROUTE_PREFIX}/{}?exp={}&sig={sig}",
            uri_encode(key, false),
            exp.timestamp()
        );
        let mut headers = Vec::new();
        if method == "PUT" {
            url.push_str(&format!("&ct={}&sha={sha}", uri_encode(ct, true)));
            headers.push(("content-type".to_string(), ct.to_string()));
        }
        PresignedUrl {
            method,
            url,
            headers,
            expires_at: exp,
        }
    }

    /// Validate a transfer URL's query (`exp`, `sig`, and for PUT `ct`, `sha`).
    pub fn verify(
        &self,
        method: &str,
        key: &str,
        query: &BTreeMap<String, String>,
        now: DateTime<Utc>,
    ) -> Option<FixtureGrant> {
        validate_key(key).ok()?;
        let exp: i64 = query.get("exp")?.parse().ok()?;
        if now.timestamp() > exp {
            return None;
        }
        let sig = query.get("sig")?;
        let (ct, sha) = if method == "PUT" {
            (query.get("ct")?.as_str(), query.get("sha")?.as_str())
        } else {
            ("", "")
        };
        let expected = self.sign(method, key, exp, ct, sha);
        if !constant_time_eq(expected.as_bytes(), sig.as_bytes()) {
            return None;
        }
        Some(FixtureGrant {
            method: if method == "PUT" { "PUT" } else { "GET" },
            key: key.to_string(),
            content_type: (!ct.is_empty()).then(|| ct.to_string()),
            sha256_hex: (!sha.is_empty()).then(|| sha.to_string()),
        })
    }

    /// Store an uploaded body under a verified PUT grant. The declared
    /// checksum must match the bytes, mirroring S3's checksum enforcement.
    pub fn put(&self, grant: &FixtureGrant, bytes: Vec<u8>) -> Result<(), ObjectStoreError> {
        if bytes.len() as i64 > self.max_bytes {
            return Err(ObjectStoreError::Mismatch);
        }
        let actual = hex::encode(Sha256::digest(&bytes));
        if grant.sha256_hex.as_deref() != Some(actual.as_str()) {
            return Err(ObjectStoreError::Mismatch);
        }
        let ct = grant
            .content_type
            .clone()
            .ok_or(ObjectStoreError::InvalidKey)?;
        self.objects.lock().expect("fixture store").insert(
            grant.key.clone(),
            FixtureObject {
                bytes,
                content_type: ct,
                sha256_hex: actual,
            },
        );
        Ok(())
    }

    pub fn get(&self, key: &str) -> Option<(Vec<u8>, String)> {
        self.objects
            .lock()
            .expect("fixture store")
            .get(key)
            .map(|o| (o.bytes.clone(), o.content_type.clone()))
    }
}

#[async_trait]
impl ObjectStore for FixtureStore {
    fn kind(&self) -> &'static str {
        "fixture"
    }
    async fn presign_upload(
        &self,
        key: &str,
        content_type: &str,
        sha256_hex: &str,
        ttl: Duration,
    ) -> Result<PresignedUrl, ObjectStoreError> {
        validate_key(key)?;
        Ok(self.grant_url("PUT", key, content_type, sha256_hex, ttl))
    }
    async fn presign_download(
        &self,
        key: &str,
        ttl: Duration,
    ) -> Result<PresignedUrl, ObjectStoreError> {
        validate_key(key)?;
        Ok(self.grant_url("GET", key, "", "", ttl))
    }
    async fn stat(&self, key: &str) -> Result<Option<ObjectStat>, ObjectStoreError> {
        validate_key(key)?;
        Ok(self
            .objects
            .lock()
            .expect("fixture store")
            .get(key)
            .map(|o| ObjectStat {
                size_bytes: o.bytes.len() as i64,
                sha256_hex: Some(o.sha256_hex.clone()),
            }))
    }
    fn fixture(&self) -> Option<&FixtureStore> {
        Some(self)
    }
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

// ---------------------------------------------------------------------------
// S3-compatible (SigV4)
// ---------------------------------------------------------------------------

pub struct S3Store {
    cfg: S3Config,
    client: reqwest::Client,
}

impl S3Store {
    pub fn new(cfg: S3Config) -> anyhow::Result<Self> {
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(15))
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        Ok(Self { cfg, client })
    }

    fn object_url(&self, key: &str) -> (String, String) {
        let host = self.cfg.endpoint.host_str().unwrap_or_default().to_string();
        let port = self
            .cfg
            .endpoint
            .port()
            .map(|p| format!(":{p}"))
            .unwrap_or_default();
        if self.cfg.path_style {
            (
                format!("{host}{port}"),
                format!("/{}/{}", self.cfg.bucket, uri_encode(key, false)),
            )
        } else {
            (
                format!("{}.{host}{port}", self.cfg.bucket),
                format!("/{}", uri_encode(key, false)),
            )
        }
    }

    fn signing_key(&self, date: &str) -> Vec<u8> {
        let mut k = hmac_sha256(
            format!("AWS4{}", self.cfg.secret_access_key).as_bytes(),
            date.as_bytes(),
        );
        for part in [self.cfg.region.as_str(), "s3", "aws4_request"] {
            k = hmac_sha256(&k, part.as_bytes());
        }
        k
    }

    /// SigV4 query-string presigning (payload unsigned; the checksum header
    /// is part of the signed headers for uploads).
    fn presign(
        &self,
        method: &'static str,
        key: &str,
        extra_headers: &[(String, String)],
        ttl: Duration,
        now: DateTime<Utc>,
    ) -> PresignedUrl {
        let (host, path) = self.object_url(key);
        let amz_date = now.format("%Y%m%dT%H%M%SZ").to_string();
        let date = now.format("%Y%m%d").to_string();
        let scope = format!("{date}/{}/s3/aws4_request", self.cfg.region);
        let ttl_secs = clamp_ttl(ttl);

        let mut headers: BTreeMap<String, String> = BTreeMap::new();
        headers.insert("host".into(), host.clone());
        for (k, v) in extra_headers {
            headers.insert(k.to_ascii_lowercase(), v.trim().to_string());
        }
        let signed_headers = headers.keys().cloned().collect::<Vec<_>>().join(";");
        let canonical_headers = headers
            .iter()
            .map(|(k, v)| format!("{k}:{v}\n"))
            .collect::<String>();

        let mut query: BTreeMap<String, String> = BTreeMap::new();
        query.insert("X-Amz-Algorithm".into(), "AWS4-HMAC-SHA256".into());
        query.insert(
            "X-Amz-Credential".into(),
            format!("{}/{scope}", self.cfg.access_key_id),
        );
        query.insert("X-Amz-Date".into(), amz_date.clone());
        query.insert("X-Amz-Expires".into(), ttl_secs.to_string());
        query.insert("X-Amz-SignedHeaders".into(), signed_headers.clone());
        let canonical_query = query
            .iter()
            .map(|(k, v)| format!("{}={}", uri_encode(k, true), uri_encode(v, true)))
            .collect::<Vec<_>>()
            .join("&");

        let canonical_request = format!(
            "{method}\n{path}\n{canonical_query}\n{canonical_headers}\n{signed_headers}\nUNSIGNED-PAYLOAD"
        );
        let string_to_sign = format!(
            "AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n{}",
            hex::encode(Sha256::digest(canonical_request.as_bytes()))
        );
        let signature = hex::encode(hmac_sha256(
            &self.signing_key(&date),
            string_to_sign.as_bytes(),
        ));
        let url = format!(
            "{}://{host}{path}?{canonical_query}&X-Amz-Signature={signature}",
            self.cfg.endpoint.scheme()
        );
        PresignedUrl {
            method,
            url,
            headers: extra_headers.to_vec(),
            expires_at: now + ChronoDuration::seconds(ttl_secs),
        }
    }

    /// Header-signed request (used for HEAD). Payload is empty.
    fn signed_headers_for(
        &self,
        method: &str,
        key: &str,
        extra: &[(String, String)],
        now: DateTime<Utc>,
    ) -> (String, Vec<(String, String)>) {
        let (host, path) = self.object_url(key);
        let amz_date = now.format("%Y%m%dT%H%M%SZ").to_string();
        let date = now.format("%Y%m%d").to_string();
        let scope = format!("{date}/{}/s3/aws4_request", self.cfg.region);
        let payload_hash = hex::encode(Sha256::digest(b""));
        let mut headers: BTreeMap<String, String> = BTreeMap::new();
        headers.insert("host".into(), host.clone());
        headers.insert("x-amz-date".into(), amz_date.clone());
        headers.insert("x-amz-content-sha256".into(), payload_hash.clone());
        for (k, v) in extra {
            headers.insert(k.to_ascii_lowercase(), v.trim().to_string());
        }
        let signed_headers = headers.keys().cloned().collect::<Vec<_>>().join(";");
        let canonical_headers = headers
            .iter()
            .map(|(k, v)| format!("{k}:{v}\n"))
            .collect::<String>();
        let canonical_request =
            format!("{method}\n{path}\n\n{canonical_headers}\n{signed_headers}\n{payload_hash}");
        let string_to_sign = format!(
            "AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n{}",
            hex::encode(Sha256::digest(canonical_request.as_bytes()))
        );
        let signature = hex::encode(hmac_sha256(
            &self.signing_key(&date),
            string_to_sign.as_bytes(),
        ));
        let authorization = format!(
            "AWS4-HMAC-SHA256 Credential={}/{scope}, SignedHeaders={signed_headers}, Signature={signature}",
            self.cfg.access_key_id
        );
        let mut out: Vec<(String, String)> =
            headers.into_iter().filter(|(k, _)| k != "host").collect();
        out.push(("authorization".into(), authorization));
        (
            format!("{}://{host}{path}", self.cfg.endpoint.scheme()),
            out,
        )
    }
}

#[async_trait]
impl ObjectStore for S3Store {
    fn kind(&self) -> &'static str {
        "s3"
    }
    async fn presign_upload(
        &self,
        key: &str,
        content_type: &str,
        sha256_hex: &str,
        ttl: Duration,
    ) -> Result<PresignedUrl, ObjectStoreError> {
        validate_key(key)?;
        if !sha256_hex_valid(sha256_hex) {
            return Err(ObjectStoreError::InvalidKey);
        }
        let raw = hex::decode(sha256_hex).map_err(|_| ObjectStoreError::InvalidKey)?;
        let checksum_b64 = base64::engine::general_purpose::STANDARD.encode(raw);
        let headers = vec![
            ("content-type".to_string(), content_type.to_string()),
            ("x-amz-checksum-sha256".to_string(), checksum_b64),
        ];
        Ok(self.presign("PUT", key, &headers, ttl, Utc::now()))
    }
    async fn presign_download(
        &self,
        key: &str,
        ttl: Duration,
    ) -> Result<PresignedUrl, ObjectStoreError> {
        validate_key(key)?;
        Ok(self.presign("GET", key, &[], ttl, Utc::now()))
    }
    async fn stat(&self, key: &str) -> Result<Option<ObjectStat>, ObjectStoreError> {
        validate_key(key)?;
        let (url, headers) = self.signed_headers_for(
            "HEAD",
            key,
            &[("x-amz-checksum-mode".to_string(), "ENABLED".to_string())],
            Utc::now(),
        );
        let mut req = self.client.head(&url);
        for (k, v) in headers {
            req = req.header(k, v);
        }
        let resp = req
            .send()
            .await
            .map_err(|_| ObjectStoreError::Upstream("head request failed".into()))?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !resp.status().is_success() {
            return Err(ObjectStoreError::Upstream(format!(
                "head returned status {}",
                resp.status().as_u16()
            )));
        }
        let size_bytes = resp
            .headers()
            .get(reqwest::header::CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<i64>().ok())
            .ok_or_else(|| ObjectStoreError::Upstream("head response without length".into()))?;
        let sha256_hex = resp
            .headers()
            .get("x-amz-checksum-sha256")
            .and_then(|v| v.to_str().ok())
            .and_then(|b64| base64::engine::general_purpose::STANDARD.decode(b64).ok())
            .map(hex::encode);
        Ok(Some(ObjectStat {
            size_bytes,
            sha256_hex,
        }))
    }
}

fn hmac_sha256(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut mac = HmacSha256::new_from_slice(key).expect("hmac accepts any key length");
    mac.update(data);
    mac.finalize().into_bytes().to_vec()
}

/// AWS-style URI encoding: unreserved characters pass through, `/` is kept
/// in paths and encoded in query components.
pub fn uri_encode(input: &str, encode_slash: bool) -> String {
    let mut out = String::with_capacity(input.len());
    for b in input.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            b'/' if !encode_slash => out.push('/'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> S3Config {
        S3Config {
            endpoint: url::Url::parse("https://s3.example.test").unwrap(),
            bucket: "wellos-docs".into(),
            region: "eu-west-1".into(),
            access_key_id: "AKIDEXAMPLE".into(),
            secret_access_key: "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY".into(),
            path_style: true,
        }
    }

    #[test]
    fn document_keys_are_tenant_scoped_and_validated() {
        let k = document_key(Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
        assert!(validate_key(&k).is_ok());
        assert!(validate_key("tenants/x/patients/y/documents/z").is_err());
        assert!(validate_key("../etc/passwd").is_err());
        assert!(validate_key(&format!("{k}/extra")).is_err());
    }

    #[test]
    fn s3_presign_is_deterministic_and_bounded() {
        let store = S3Store::new(cfg()).unwrap();
        let key = document_key(Uuid::nil(), Uuid::nil(), Uuid::nil());
        let now = DateTime::parse_from_rfc3339("2026-10-04T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let a = store.presign("GET", &key, &[], Duration::from_secs(3600), now);
        let b = store.presign("GET", &key, &[], Duration::from_secs(3600), now);
        assert_eq!(a, b);
        assert!(a
            .url
            .starts_with("https://s3.example.test/wellos-docs/tenants/"));
        assert!(
            a.url.contains("X-Amz-Expires=900"),
            "ttl clamped to 15 minutes"
        );
        assert!(a.url.contains("X-Amz-SignedHeaders=host"));
        assert!(a.url.contains("X-Amz-Signature="));
        assert_eq!(a.expires_at, now + ChronoDuration::seconds(900));
    }

    #[test]
    fn s3_presign_matches_reference_signature() {
        // Reference computed independently with the AWS SigV4 algorithm for
        // this exact canonical request; guards against regressions in the
        // canonicalisation.
        let store = S3Store::new(cfg()).unwrap();
        let key = document_key(Uuid::nil(), Uuid::nil(), Uuid::nil());
        let now = DateTime::parse_from_rfc3339("2026-10-04T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let p = store.presign("GET", &key, &[], Duration::from_secs(300), now);
        let (host, path) = store.object_url(&key);
        let canonical_query = "X-Amz-Algorithm=AWS4-HMAC-SHA256&X-Amz-Credential=AKIDEXAMPLE%2F20261004%2Feu-west-1%2Fs3%2Faws4_request&X-Amz-Date=20261004T120000Z&X-Amz-Expires=300&X-Amz-SignedHeaders=host";
        let canonical =
            format!("GET\n{path}\n{canonical_query}\nhost:{host}\n\nhost\nUNSIGNED-PAYLOAD");
        let sts = format!(
            "AWS4-HMAC-SHA256\n20261004T120000Z\n20261004/eu-west-1/s3/aws4_request\n{}",
            hex::encode(Sha256::digest(canonical.as_bytes()))
        );
        let expected = hex::encode(hmac_sha256(&store.signing_key("20261004"), sts.as_bytes()));
        assert!(p.url.ends_with(&format!("&X-Amz-Signature={expected}")));
    }

    #[tokio::test]
    async fn s3_upload_pins_checksum_header() {
        let store = S3Store::new(cfg()).unwrap();
        let key = document_key(Uuid::nil(), Uuid::nil(), Uuid::nil());
        let sha = hex::encode(Sha256::digest(b"report"));
        let p = store
            .presign_upload(&key, "application/pdf", &sha, Duration::from_secs(60))
            .await
            .unwrap();
        assert_eq!(p.method, "PUT");
        assert!(p.headers.iter().any(|(k, _)| k == "x-amz-checksum-sha256"));
        assert!(p
            .url
            .contains("X-Amz-SignedHeaders=content-type%3Bhost%3Bx-amz-checksum-sha256"));
        assert!(store
            .presign_upload(&key, "application/pdf", "nothex", Duration::from_secs(60))
            .await
            .is_err());
    }

    #[tokio::test]
    async fn fixture_store_round_trip_enforces_checksum_and_expiry() {
        let store = FixtureStore::new(1024);
        let key = document_key(Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
        let body = b"synthetic report".to_vec();
        let sha = hex::encode(Sha256::digest(&body));
        let up = store
            .presign_upload(&key, "application/pdf", &sha, Duration::from_secs(60))
            .await
            .unwrap();
        let url = url::Url::parse(&format!("http://localhost{}", up.url)).unwrap();
        let q: BTreeMap<String, String> = url
            .query_pairs()
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();
        let grant = store
            .verify("PUT", &key, &q, Utc::now())
            .expect("valid grant");
        assert!(store.put(&grant, b"tampered".to_vec()).is_err());
        store.put(&grant, body.clone()).unwrap();
        let stat = store.stat(&key).await.unwrap().unwrap();
        assert_eq!(stat.size_bytes, body.len() as i64);
        assert_eq!(stat.sha256_hex.as_deref(), Some(sha.as_str()));
        // Expired grant is refused; wrong method is refused.
        assert!(store
            .verify("PUT", &key, &q, Utc::now() + ChronoDuration::seconds(120))
            .is_none());
        assert!(store.verify("GET", &key, &q, Utc::now()).is_none());
        let down = store
            .presign_download(&key, Duration::from_secs(60))
            .await
            .unwrap();
        let url = url::Url::parse(&format!("http://localhost{}", down.url)).unwrap();
        let q: BTreeMap<String, String> = url
            .query_pairs()
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();
        assert!(store.verify("GET", &key, &q, Utc::now()).is_some());
        assert!(store.get(&key).is_some());
        assert!(store.stat("tenants/bad").await.is_err());
    }
}
