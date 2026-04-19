// auth.rs — API-key extractor for handlers that require authentication.
// Pulled out of the monolithic `routes.rs` during the backlog #12
// refactor.

use axum::{extract::FromRequestParts, http::StatusCode, http::request::Parts};

// ── API key extractor ─────────────────────────────────────
/// Add `_auth: ApiKey` as the first parameter of any handler that needs
/// auth. Returns 401 if `SENTRIX_API_KEY` is set and the request header
/// `X-API-Key` doesn't match.
pub struct ApiKey;

impl<S: Send + Sync> FromRequestParts<S> for ApiKey {
    type Rejection = StatusCode;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        // P1: require SENTRIX_API_KEY of at least MIN_API_KEY_LEN bytes
        // when set. The pre-fix behaviour accepted any non-empty key,
        // so an operator who accidentally set `SENTRIX_API_KEY=1`
        // effectively had no protection (trivially guessable). An
        // empty or too-short value now behaves as "not configured" —
        // endpoints stay open rather than silently trusting a
        // hopelessly weak secret.
        const MIN_API_KEY_LEN: usize = 16;
        let required = match std::env::var("SENTRIX_API_KEY") {
            Ok(k) if k.len() >= MIN_API_KEY_LEN => k,
            Ok(k) if !k.is_empty() => {
                tracing::warn!(
                    "SENTRIX_API_KEY is set but too short ({} chars, need ≥ {}); \
                     ignoring — endpoints running unauthenticated",
                    k.len(),
                    MIN_API_KEY_LEN
                );
                return Ok(ApiKey);
            }
            _ => return Ok(ApiKey), // no key set → always allow
        };
        let provided = parts
            .headers
            .get("X-API-Key")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        if constant_time_eq(provided, &required) {
            Ok(ApiKey)
        } else {
            Err(StatusCode::UNAUTHORIZED)
        }
    }
}

/// Constant-time comparison via the `subtle` crate — prevents
/// timing-based API-key leakage. Pads both inputs to `max(a.len, b.len)`
/// so comparison traverses equal bytes regardless of input length, and
/// folds a length-mismatch check in as a second constant-time gate.
pub fn constant_time_eq(a: &str, b: &str) -> bool {
    use subtle::ConstantTimeEq;
    let a_bytes = a.as_bytes();
    let b_bytes = b.as_bytes();
    let max_len = a_bytes.len().max(b_bytes.len());
    let mut a_padded = vec![0u8; max_len];
    let mut b_padded = vec![0u8; max_len];
    a_padded[..a_bytes.len()].copy_from_slice(a_bytes);
    b_padded[..b_bytes.len()].copy_from_slice(b_bytes);
    let len_eq: subtle::Choice = (a_bytes.len() as u64).ct_eq(&(b_bytes.len() as u64));
    let content_eq: subtle::Choice = a_padded.ct_eq(&b_padded);
    (len_eq & content_eq).into()
}

