use axum::body::Body;
use axum::extract::State;
use axum::http::{Request, StatusCode};
use axum::middleware::Next;
use axum::response::Response;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

use crate::state::AppState;

pub async fn require_token(
    State(state): State<AppState>,
    req: Request<Body>,
    next: Next,
) -> Result<Response, StatusCode> {
    let Some(token) = req
        .headers()
        .get("Authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
    else {
        return Err(StatusCode::UNAUTHORIZED);
    };

    if token_matches(token, state.api_token_hash.as_ref()) {
        Ok(next.run(req).await)
    } else {
        Err(StatusCode::UNAUTHORIZED)
    }
}

/// Constant-time сравнение через SHA256 + subtle.
///
/// Оба входа хешируются — результирующие срезы всегда 32 байта,
/// length-leak невозможен в принципе. `subtle::ConstantTimeEq`
/// компилируется в сравнение без ветвлений (защищено от оптимизаций).
#[inline]
fn token_matches(candidate: &str, expected_hash: &[u8; 32]) -> bool {
    let mut hasher = Sha256::new();
    hasher.update(candidate.as_bytes());
    let candidate_hash: [u8; 32] = hasher.finalize().into();

    candidate_hash
        .as_slice()
        .ct_eq(expected_hash.as_slice())
        .into()
}
