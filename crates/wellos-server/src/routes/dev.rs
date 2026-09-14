//! Development-only discovery of synthetic sign-in identities.
//!
//! Compiled only with the `dev-fixtures` feature and served only while
//! development authentication is enabled (which itself requires
//! `WELLOS_ENV=development|test`). Everywhere else the route does not exist,
//! so a deployed build can never enumerate or offer development credentials.
//! The listed users are the seeded synthetic humans (OIDC subject
//! `synthetic|<username>`) of synthetic tenants; test-created users, machine
//! principals and any non-synthetic tenant are never exposed.

use crate::error::ApiError;
use crate::state::AppState;
use axum::extract::State;
use axum::Json;
use serde_json::{json, Value};
use sqlx::Row;

pub async fn users(State(state): State<AppState>) -> Result<Json<Value>, ApiError> {
    if !state.auth.dev_auth_enabled || !state.runtime.env.is_local() {
        return Err(ApiError::not_found());
    }
    let rows = sqlx::query(
        "SELECT u.username, u.display_name, t.name AS tenant_name,
                COALESCE(array_agg(DISTINCT ra.role ORDER BY ra.role)
                         FILTER (WHERE ra.role IS NOT NULL), '{}') AS roles
         FROM users u
         JOIN tenants t ON t.id = u.tenant_id
         LEFT JOIN role_assignments ra ON ra.user_id = u.id
         WHERE t.data_class = 'synthetic'
           AND u.is_service = false
           AND u.oidc_subject LIKE 'synthetic|%'
         GROUP BY u.id, u.username, u.display_name, t.name
         HAVING count(ra.role) > 0
         ORDER BY t.name, u.username",
    )
    .fetch_all(&state.pool)
    .await?;
    let users = rows
        .iter()
        .map(|r| {
            json!({
                "username": r.get::<String, _>("username"),
                "display_name": r.get::<String, _>("display_name"),
                "tenant_name": r.get::<String, _>("tenant_name"),
                "roles": r.get::<Vec<String>, _>("roles"),
            })
        })
        .collect::<Vec<_>>();
    Ok(Json(json!({
        "environment": state.runtime.env.as_str(),
        "synthetic": true,
        "users": users,
    })))
}
