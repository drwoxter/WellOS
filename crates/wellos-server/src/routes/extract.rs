//! Request extractors shared by the route modules.

use crate::error::ApiError;
use axum::async_trait;
use axum::body::Bytes;
use axum::extract::{FromRequest, Request};
use axum::http::header::CONTENT_TYPE;
use axum::http::StatusCode;
use serde::de::DeserializeOwned;

/// A JSON body that may be omitted entirely. Unlike `Option<Json<T>>`, a
/// body that *is* sent but is malformed, has the wrong media type or
/// carries fields the contract does not accept is rejected instead of
/// being silently treated as absent.
#[derive(Debug)]
pub struct OptionalJson<T>(pub Option<T>);

#[async_trait]
impl<S, T> FromRequest<S> for OptionalJson<T>
where
    S: Send + Sync,
    T: DeserializeOwned,
{
    type Rejection = ApiError;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        let is_json = req.headers().get(CONTENT_TYPE).map(|v| {
            v.to_str().is_ok_and(|s| {
                let media = s
                    .split(';')
                    .next()
                    .unwrap_or("")
                    .trim()
                    .to_ascii_lowercase();
                media == "application/json"
                    || (media.starts_with("application/") && media.ends_with("+json"))
            })
        });
        let bytes = Bytes::from_request(req, state)
            .await
            .map_err(|e| ApiError::new(e.status(), "invalid_body", e.body_text()))?;
        if bytes.is_empty() {
            return Ok(Self(None));
        }
        if is_json == Some(false) {
            return Err(ApiError::new(
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "unsupported_media_type",
                "expected an application/json body",
            ));
        }
        let value: T = serde_json::from_slice(&bytes).map_err(|e| {
            ApiError::bad_request("invalid_json", format!("invalid JSON body: {e}"))
        })?;
        Ok(Self(Some(value)))
    }
}
