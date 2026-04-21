// cache.rs — Cache-Control header middleware.
//
// Block explorer + wallet UIs hammer read endpoints every few seconds.
// A lot of what they fetch is immutable (confirmed block at height N,
// confirmed tx by id) or only slowly changing (token metadata). Without
// Cache-Control, every request hits MDBX; with the right headers, the
// browser + any upstream CDN short-circuits most of it.
//
// Rules applied (by matched URI path):
//
// | Endpoint                        | Cache-Control                               |
// |---------------------------------|----------------------------------------------|
// | /chain/blocks/{height}          | public, max-age=3600, immutable             |
// | /blocks/{height}                | public, max-age=3600, immutable             |
// | /transactions/{txid}            | public, max-age=3600, immutable             |
// | /tokens/{contract}              | public, max-age=60                          |
// | /chain/info                     | no-store                                    |
// | /sentrix_status                 | no-store                                    |
// | /mempool                        | no-store                                    |
// | /chain/performance              | no-store                                    |
// | /chain/blocks (list)            | public, max-age=5                           |
// | /blocks (list)                  | public, max-age=5                           |
// | everything else                 | unchanged                                   |
//
// Only applied to successful (2xx) GET responses — 4xx/5xx skip it so
// error responses aren't cached by a browser that later sees the real
// data.

use axum::{
    body::Body,
    http::{HeaderValue, Method, Request, header::CACHE_CONTROL},
    middleware::Next,
    response::Response,
};

const CACHE_IMMUTABLE: &str = "public, max-age=3600, immutable";
const CACHE_TOKEN_META: &str = "public, max-age=60";
const CACHE_LIVE_LIST: &str = "public, max-age=5";
const CACHE_NO_STORE: &str = "no-store";

/// Decide the Cache-Control value for a given path. Returns `None` if
/// the path has no cache policy (leave headers untouched).
fn cache_policy_for(path: &str) -> Option<&'static str> {
    // Live data: must always fetch fresh.
    match path {
        "/chain/info" | "/sentrix_status" | "/mempool" | "/chain/performance" => {
            return Some(CACHE_NO_STORE);
        }
        "/chain/blocks" | "/blocks" => return Some(CACHE_LIVE_LIST),
        _ => {}
    }

    // Immutable block / tx resources: have a trailing path segment that
    // names the resource, never mutate once confirmed.
    //
    // Match `/chain/blocks/<seg>`, `/blocks/<seg>`, `/transactions/<seg>`
    // strictly — so the LIST endpoints above and any nested children do
    // NOT match.
    if let Some(rest) = path.strip_prefix("/chain/blocks/")
        && is_single_segment(rest)
    {
        return Some(CACHE_IMMUTABLE);
    }
    if let Some(rest) = path.strip_prefix("/blocks/")
        && is_single_segment(rest)
    {
        return Some(CACHE_IMMUTABLE);
    }
    if let Some(rest) = path.strip_prefix("/transactions/")
        && is_single_segment(rest)
    {
        return Some(CACHE_IMMUTABLE);
    }

    // Token metadata — `/tokens/{contract}` only. Excludes balance /
    // holders / transfers / trades / etc which live under the same
    // prefix but mutate more often.
    if let Some(rest) = path.strip_prefix("/tokens/")
        && is_single_segment(rest)
    {
        return Some(CACHE_TOKEN_META);
    }

    None
}

/// True when `s` looks like a single non-empty URL path segment (no `/`,
/// no query). Used to distinguish `/chain/blocks/{height}` (cacheable)
/// from `/chain/blocks` (list) and from any deeper nested route.
fn is_single_segment(s: &str) -> bool {
    !s.is_empty() && !s.contains('/')
}

/// Axum middleware. Only applies to GET responses with 2xx status — we
/// don't want a browser caching a 404 or 500.
pub(super) async fn cache_control_middleware(request: Request<Body>, next: Next) -> Response {
    let path = request.uri().path().to_string();
    let is_get = request.method() == Method::GET;

    let mut response = next.run(request).await;

    if !is_get || !response.status().is_success() {
        return response;
    }

    if let Some(value) = cache_policy_for(&path)
        && let Ok(header) = HeaderValue::from_str(value)
    {
        // Don't overwrite a handler that already set its own Cache-Control.
        response
            .headers_mut()
            .entry(CACHE_CONTROL)
            .or_insert(header);
    }

    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_live_endpoints_get_no_store() {
        assert_eq!(cache_policy_for("/chain/info"), Some(CACHE_NO_STORE));
        assert_eq!(cache_policy_for("/sentrix_status"), Some(CACHE_NO_STORE));
        assert_eq!(cache_policy_for("/mempool"), Some(CACHE_NO_STORE));
        assert_eq!(cache_policy_for("/chain/performance"), Some(CACHE_NO_STORE));
    }

    #[test]
    fn test_block_list_vs_specific_block() {
        // Lists: short cache so UI picks up new blocks quickly.
        assert_eq!(cache_policy_for("/chain/blocks"), Some(CACHE_LIVE_LIST));
        assert_eq!(cache_policy_for("/blocks"), Some(CACHE_LIVE_LIST));

        // Specific block at a committed height: immutable.
        assert_eq!(cache_policy_for("/chain/blocks/100"), Some(CACHE_IMMUTABLE));
        assert_eq!(cache_policy_for("/blocks/100"), Some(CACHE_IMMUTABLE));
    }

    #[test]
    fn test_transaction_by_id_immutable() {
        assert_eq!(
            cache_policy_for("/transactions/abcdef0123"),
            Some(CACHE_IMMUTABLE)
        );
    }

    #[test]
    fn test_token_metadata_vs_token_children() {
        // `/tokens/{contract}` → token metadata, 60s cache.
        assert_eq!(
            cache_policy_for("/tokens/SRC20_abcdef"),
            Some(CACHE_TOKEN_META)
        );
        // `/tokens` list itself is not cached (metadata count churns).
        assert_eq!(cache_policy_for("/tokens"), None);
        // Children (holders, balance, etc) must NOT pick up token meta.
        assert_eq!(cache_policy_for("/tokens/SRC20_abcdef/holders"), None);
        assert_eq!(cache_policy_for("/tokens/SRC20_abcdef/balance/0xdead"), None);
    }

    #[test]
    fn test_unmatched_paths_return_none() {
        assert_eq!(cache_policy_for("/health"), None);
        assert_eq!(cache_policy_for("/metrics"), None);
        assert_eq!(cache_policy_for("/validators"), None);
        assert_eq!(cache_policy_for("/accounts/0xdead/balance"), None);
    }

    #[test]
    fn test_is_single_segment() {
        assert!(is_single_segment("100"));
        assert!(is_single_segment("abcdef"));
        assert!(!is_single_segment(""));
        assert!(!is_single_segment("100/extra"));
        assert!(!is_single_segment("a/b"));
    }
}
