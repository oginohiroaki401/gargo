//! Request validation + small response/HTML helpers.

use axum::{
    http::{HeaderValue, StatusCode, header},
    response::{IntoResponse, Json, Response},
};

pub(crate) fn parse_bool_param(value: Option<&String>, default: bool) -> bool {
    match value.map(|v| v.as_str()) {
        Some("true") => true,
        Some("false") => false,
        _ => default,
    }
}

pub(crate) fn html_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

/// Validate a relative path used as a git diff argument.
///
/// We always pass paths after `--` so flag injection is structurally blocked,
/// but we still reject control characters, path traversal, absolute paths,
/// and unreasonably long inputs to keep the API surface tight.
pub(crate) fn parse_diff_path(value: &str) -> Option<String> {
    if value.is_empty() || value.len() > 4096 {
        return None;
    }
    if value.starts_with('-') || value.starts_with('/') {
        return None;
    }
    if value.contains('\0') || value.contains('\n') || value.contains('\r') {
        return None;
    }
    for segment in value.split('/') {
        if segment == ".." {
            return None;
        }
    }
    Some(value.to_string())
}

/// Validate a git branch name to block flag injection and command injection.
///
/// Accepts the conservative subset `[A-Za-z0-9._/\-]`, rejects names that start
/// with `-` (so they can never be parsed as a git CLI flag), and caps the length
/// to bound the work git has to do on a malicious input.
pub(crate) fn parse_branch_name(value: &str) -> Option<String> {
    if value.is_empty() || value.len() > 256 {
        return None;
    }
    if value.starts_with('-') {
        return None;
    }
    let ok = value
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '/' | '-'));
    if !ok {
        return None;
    }
    Some(value.to_string())
}

pub(crate) fn bad_request(message: impl Into<String>) -> Response {
    let payload = serde_json::json!({ "error": message.into() });
    let mut response = (StatusCode::BAD_REQUEST, Json(payload)).into_response();
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("no-store, no-cache, must-revalidate"),
    );
    response
}

pub(crate) fn ok_json(payload: serde_json::Value) -> Response {
    let mut response = (StatusCode::OK, Json(payload)).into_response();
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("no-store, no-cache, must-revalidate"),
    );
    response
}

/// Reject a request that a browser tagged as coming from another site. This
/// guards the billed AI endpoints against CSRF / "wallet DoS": a page the user
/// visits could otherwise trigger `<img src=…/api/ai/summary>` or a cross-origin
/// POST and run up provider cost. The server is loopback-only, so any call the
/// editor itself makes is same-origin and passes; only cross-site browser
/// requests (identified by the browser-set `Sec-Fetch-Site` / `Origin` headers)
/// are blocked. Non-browser callers (curl, tests) send neither header and are
/// allowed — they carry no ambient credentials, so they aren't the CSRF threat.
///
/// Returns `Some(403)` to short-circuit with, or `None` when the request is OK.
pub(crate) fn forbid_cross_site(headers: &axum::http::HeaderMap) -> Option<Response> {
    let same_origin = match headers.get("sec-fetch-site").and_then(|v| v.to_str().ok()) {
        // Modern browsers tag every request; only same-origin / direct loads pass.
        Some(site) => matches!(site, "same-origin" | "none"),
        // Older browsers: fall back to Origin, which must be a loopback address.
        None => match headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) {
            Some(origin) => origin_is_loopback(origin),
            None => true,
        },
    };
    if same_origin {
        return None;
    }
    let payload = serde_json::json!({ "error": "cross-site request rejected" });
    let mut response = (StatusCode::FORBIDDEN, Json(payload)).into_response();
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("no-store, no-cache, must-revalidate"),
    );
    Some(response)
}

/// True when an `Origin` URL points at a loopback host (any scheme/port).
fn origin_is_loopback(origin: &str) -> bool {
    let after_scheme = origin.split("://").nth(1).unwrap_or(origin);
    let authority = after_scheme.split('/').next().unwrap_or(after_scheme);
    let host = if let Some(rest) = authority.strip_prefix('[') {
        // IPv6 literal, e.g. `[::1]:8080` — host is between the brackets.
        rest.split(']').next().unwrap_or(rest)
    } else {
        authority.rsplit_once(':').map_or(authority, |(h, _)| h)
    };
    matches!(host, "127.0.0.1" | "localhost" | "::1")
}

pub(crate) fn repo_ctx_script(
    ctx: &crate::command::gargo_preview_server::RepoUrlContext,
    github_base: Option<&str>,
    default_branch: Option<&str>,
) -> String {
    crate::command::server_shared::repo_ctx_script(
        &ctx.owner,
        &ctx.repo,
        &ctx.branch,
        github_base,
        default_branch,
    )
}

#[cfg(test)]
mod csrf_tests {
    use super::*;
    use axum::http::HeaderMap;

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.insert(
                axum::http::HeaderName::from_bytes(k.as_bytes()).unwrap(),
                HeaderValue::from_str(v).unwrap(),
            );
        }
        h
    }

    #[test]
    fn allows_same_origin_and_direct_requests() {
        assert!(forbid_cross_site(&headers(&[("sec-fetch-site", "same-origin")])).is_none());
        assert!(forbid_cross_site(&headers(&[("sec-fetch-site", "none")])).is_none());
        // Non-browser callers (curl, tests) send neither header.
        assert!(forbid_cross_site(&headers(&[])).is_none());
    }

    #[test]
    fn rejects_cross_site_requests() {
        assert!(forbid_cross_site(&headers(&[("sec-fetch-site", "cross-site")])).is_some());
        assert!(forbid_cross_site(&headers(&[("sec-fetch-site", "same-site")])).is_some());
    }

    #[test]
    fn origin_fallback_distinguishes_loopback_from_remote() {
        // No Sec-Fetch-Site (older browser): fall back to Origin.
        assert!(forbid_cross_site(&headers(&[("origin", "http://127.0.0.1:8080")])).is_none());
        assert!(forbid_cross_site(&headers(&[("origin", "http://localhost:3000")])).is_none());
        assert!(forbid_cross_site(&headers(&[("origin", "http://[::1]:9000")])).is_none());
        assert!(forbid_cross_site(&headers(&[("origin", "https://evil.example.com")])).is_some());
    }
}
