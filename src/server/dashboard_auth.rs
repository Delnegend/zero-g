//! Dashboard authentication. Set `ZERO_G_DASHBOARD_AUTH` to `basic`, `oidc`, or
//! explicitly to `disabled`. Basic requires `ZERO_G_DASHBOARD_USERNAME` and
//! `ZERO_G_DASHBOARD_PASSWORD`. OIDC requires `ZERO_G_OIDC_ISSUER`,
//! `ZERO_G_OIDC_CLIENT_ID`, `ZERO_G_OIDC_CLIENT_SECRET`, `ZERO_G_OIDC_REDIRECT_URL`
//! and `ZERO_G_OIDC_ALLOWED_EMAILS` (comma-separated exact email addresses).
//! Register the redirect URL as the exact public HTTPS URL ending in `/auth/callback`;
//! an HTTP loopback URL is allowed for local development. The configured URL,
//! rather than untrusted forwarded headers, determines cookie security.
//! Set `ZERO_G_DASHBOARD_ORIGIN` to the public origin (scheme and authority
//! only); reverse proxies must preserve the public Host header. Browser writes
//! require a matching Origin; authenticated non-browser clients may omit it.
//!
//! Apply `protect` only to dashboard and management API routes, not inference
//! routes. It adds its own callback endpoint. Sessions are local to this process.

use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};

use axum::{
    Router,
    extract::{Query, Request, State},
    http::{HeaderMap, HeaderValue, Method, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Redirect, Response},
    routing::get,
};
use base64::{
    Engine,
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
};
use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode, decode_header, jwk::JwkSet};
use rand::RngCore;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tokio::sync::Mutex;

const CALLBACK: &str = "/auth/callback";
const SESSION_COOKIE: &str = "zero_g_dashboard_session";
const STATE_COOKIE: &str = "zero_g_oidc_state";
const LOGIN_TTL: Duration = Duration::from_mins(10);
const SESSION_TTL: Duration = Duration::from_hours(8);

#[derive(Clone)]
pub(crate) struct DashboardAuth(Arc<Mode>, Arc<reqwest::Url>);

enum Mode {
    Disabled,
    Basic { username: String, password: String },
    Oidc(Box<Oidc>),
}

struct Oidc {
    client: reqwest::Client,
    issuer: String,
    client_id: String,
    client_secret: String,
    allowed_emails: Vec<String>,
    redirect_url: String,
    authorization_endpoint: String,
    token_endpoint: String,
    jwks_uri: String,
    secure: bool,
    state: Mutex<Store>,
}

#[derive(Default)]
struct Store {
    pending: HashMap<String, Pending>,
    sessions: HashMap<String, Instant>,
}

struct Pending {
    verifier: String,
    nonce: String,
    created: Instant,
}

#[derive(Deserialize)]
struct Discovery {
    issuer: String,
    authorization_endpoint: String,
    token_endpoint: String,
    jwks_uri: String,
}

#[derive(Deserialize)]
struct Callback {
    state: Option<String>,
    code: Option<String>,
    error: Option<String>,
}

#[derive(Deserialize)]
struct TokenResponse {
    id_token: String,
}

#[derive(Deserialize)]
struct Claims {
    nonce: String,
    sub: String,
    email: Option<String>,
    email_verified: Option<bool>,
}

fn required(name: &str) -> Result<String, String> {
    std::env::var(name)
        .ok()
        .filter(|v| !v.trim().is_empty())
        .ok_or_else(|| format!("{name} must be set"))
}

fn allowed_email(claims: &Claims, allowed: &[String]) -> bool {
    claims.email_verified == Some(true)
        && claims
            .email
            .as_ref()
            .is_some_and(|email| allowed.iter().any(|entry| entry == email))
}

// Only explicitly configured loopback HTTP is permitted. All other endpoints
// must use TLS; never derive the public callback URL from request headers.
fn safe_url(value: &str) -> Result<reqwest::Url, String> {
    let url = reqwest::Url::parse(value).map_err(|e| format!("invalid URL: {e}"))?;
    let loopback = matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"));
    if !(url.scheme() == "https" || url.scheme() == "http" && loopback)
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err(
            "URL must use HTTPS (or HTTP on loopback), without credentials or fragment".into(),
        );
    }
    Ok(url)
}

impl DashboardAuth {
    /// Loads configuration and discovers the OIDC provider before serving traffic.
    /// Invalid configuration or failed discovery is a startup error.
    pub(crate) async fn from_env() -> Result<Self, String> {
        let mode = required("ZERO_G_DASHBOARD_AUTH")?;
        let public_origin = required("ZERO_G_DASHBOARD_ORIGIN")?;
        let origin = safe_url(&public_origin)?;
        if origin.path() != "/" || origin.query().is_some() {
            return Err("ZERO_G_DASHBOARD_ORIGIN must contain only scheme and authority".into());
        }
        let mode = match mode.as_str() {
            "disabled" => Mode::Disabled,
            "basic" => {
                let username = required("ZERO_G_DASHBOARD_USERNAME")?;
                let password = required("ZERO_G_DASHBOARD_PASSWORD")?;
                if username.contains(':') {
                    return Err("dashboard username must not contain ':'".into());
                }
                Mode::Basic { username, password }
            }
            "oidc" => {
                let issuer = required("ZERO_G_OIDC_ISSUER")?;
                let issuer_url = safe_url(&issuer)?;
                if issuer_url.query().is_some() {
                    return Err("OIDC issuer must not contain a query".into());
                }
                let client_id = required("ZERO_G_OIDC_CLIENT_ID")?;
                let client_secret = required("ZERO_G_OIDC_CLIENT_SECRET")?;
                let allowed_emails: Vec<String> = required("ZERO_G_OIDC_ALLOWED_EMAILS")?
                    .split(',')
                    .map(str::trim)
                    .filter(|entry| !entry.is_empty())
                    .map(str::to_owned)
                    .collect();
                if allowed_emails.is_empty() {
                    return Err("ZERO_G_OIDC_ALLOWED_EMAILS must contain at least one email".into());
                }
                let redirect_url = required("ZERO_G_OIDC_REDIRECT_URL")?;
                let redirect = safe_url(&redirect_url)?;
                if redirect.origin() != origin.origin() {
                    return Err("OIDC redirect URL must match ZERO_G_DASHBOARD_ORIGIN".into());
                }
                if redirect.path() != CALLBACK || redirect.query().is_some() {
                    return Err(format!(
                        "OIDC redirect URL must end in {CALLBACK} without a query"
                    ));
                }
                let client = reqwest::Client::builder()
                    .redirect(reqwest::redirect::Policy::none())
                    .timeout(Duration::from_secs(10))
                    .build()
                    .map_err(|e| format!("creating OIDC client: {e}"))?;
                let discovery: Discovery = client
                    .get(format!(
                        "{}/.well-known/openid-configuration",
                        issuer.trim_end_matches('/')
                    ))
                    .send()
                    .await
                    .map_err(|e| format!("OIDC discovery: {e}"))?
                    .error_for_status()
                    .map_err(|e| format!("OIDC discovery: {e}"))?
                    .json()
                    .await
                    .map_err(|e| format!("OIDC discovery: {e}"))?;
                if discovery.issuer != issuer {
                    return Err("OIDC discovery issuer does not match configured issuer".into());
                }
                for endpoint in [
                    &discovery.authorization_endpoint,
                    &discovery.token_endpoint,
                    &discovery.jwks_uri,
                ] {
                    safe_url(endpoint)?;
                }
                Mode::Oidc(Box::new(Oidc {
                    client,
                    issuer,
                    client_id,
                    client_secret,
                    allowed_emails,
                    redirect_url,
                    authorization_endpoint: discovery.authorization_endpoint,
                    token_endpoint: discovery.token_endpoint,
                    jwks_uri: discovery.jwks_uri,
                    secure: redirect.scheme() == "https",
                    state: Mutex::new(Store::default()),
                }))
            }
            _ => return Err("ZERO_G_DASHBOARD_AUTH must be basic, oidc, or disabled".into()),
        };
        Ok(Self(Arc::new(mode), Arc::new(origin)))
    }

    /// Adds authentication to an already assembled dashboard router. Merge the
    /// result with the separately assembled inference router afterwards.
    pub(crate) fn protect(&self, router: Router) -> Router {
        router
            .merge(
                Router::new()
                    .route(CALLBACK, get(callback))
                    .with_state(self.clone()),
            )
            .layer(middleware::from_fn_with_state(self.clone(), authenticate))
    }
}

fn random_token() -> String {
    let mut bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

fn cookie(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find_map(|(key, value)| (key == name).then(|| value.to_owned()))
}

fn set_cookie(name: &str, value: &str, max_age: u64, secure: bool) -> String {
    format!(
        "{name}={value}; Path=/; HttpOnly; SameSite=Lax; Max-Age={max_age}{}",
        if secure { "; Secure" } else { "" }
    )
}

fn with_cookie(mut response: Response, value: &str) -> Response {
    if let Ok(value) = HeaderValue::from_str(value) {
        response.headers_mut().append(header::SET_COOKIE, value);
    }
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

// Avoid early exit on differing bytes for credentials and state values.
fn equal(a: &[u8], b: &[u8]) -> bool {
    let mut difference = a.len() ^ b.len();
    for i in 0..a.len().max(b.len()) {
        difference |= usize::from(a.get(i).copied().unwrap_or(0) ^ b.get(i).copied().unwrap_or(0));
    }
    difference == 0
}

async fn authenticate(State(auth): State<DashboardAuth>, req: Request, next: Next) -> Response {
    let is_api = req.uri().path() == "/api" || req.uri().path().starts_with("/api/");
    let mut response =
        if valid_request_origin(&auth.1, &req, !matches!(auth.0.as_ref(), Mode::Disabled)) {
            authenticate_inner(auth, req, next).await
        } else {
            (StatusCode::FORBIDDEN, "Invalid request origin").into_response()
        };
    if is_api {
        response
            .headers_mut()
            .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    }
    response
}

fn valid_request_origin(origin: &reqwest::Url, req: &Request, authenticated_mode: bool) -> bool {
    let headers = req.headers();
    let mut hosts = headers.get_all(header::HOST).iter();
    let Some(host) = hosts.next().and_then(|v| v.to_str().ok()) else {
        return false;
    };
    if hosts.next().is_some() || host.contains(['/', '?', '#', '@', '\\']) {
        return false;
    }
    let Ok(host_url) = reqwest::Url::parse(&format!("{}://{host}/", origin.scheme())) else {
        return false;
    };
    if host_url.origin() != origin.origin() {
        return false;
    }
    if req.uri().path() == CALLBACK && req.method() == Method::GET {
        return true;
    }

    let safe = matches!(*req.method(), Method::GET | Method::HEAD | Method::OPTIONS);
    let mut origins = headers.get_all(header::ORIGIN).iter();
    let request_origin = origins.next();
    if origins.next().is_some() {
        return false;
    }
    if let Some(value) = request_origin {
        if value.to_str().ok() != Some(origin.origin().ascii_serialization().as_str()) {
            return false;
        }
    } else if !safe
        && (!authenticated_mode
            || headers
                .keys()
                .any(|name| name.as_str().starts_with("sec-fetch-")))
    {
        return false;
    }
    if !safe {
        let mut sites = headers.get_all("sec-fetch-site").iter();
        if let Some(site) = sites.next()
            && (sites.next().is_some() || site.to_str().ok() != Some("same-origin"))
        {
            return false;
        }
    }
    true
}

async fn authenticate_inner(auth: DashboardAuth, req: Request, next: Next) -> Response {
    match auth.0.as_ref() {
        Mode::Disabled => next.run(req).await,
        Mode::Basic { username, password } => {
            let valid = req
                .headers()
                .get(header::AUTHORIZATION)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.split_once(' '))
                .filter(|(scheme, _)| scheme.eq_ignore_ascii_case("basic"))
                .and_then(|(_, value)| STANDARD.decode(value).ok())
                .is_some_and(|decoded| {
                    let expected = format!("{username}:{password}");
                    equal(&decoded, expected.as_bytes())
                });
            if valid {
                return next.run(req).await;
            }
            (
                StatusCode::UNAUTHORIZED,
                [(
                    header::WWW_AUTHENTICATE,
                    "Basic realm=\"zero-g dashboard\", charset=\"UTF-8\"",
                )],
                "Authentication required",
            )
                .into_response()
        }
        Mode::Oidc(oidc) => {
            if req.uri().path() == CALLBACK {
                return next.run(req).await;
            }
            let session = cookie(req.headers(), SESSION_COOKIE);
            let now = Instant::now();
            let mut store = oidc.state.lock().await;
            store.sessions.retain(|_, expiry| *expiry > now);
            store
                .pending
                .retain(|_, pending| now.duration_since(pending.created) < LOGIN_TTL);
            if session
                .as_ref()
                .is_some_and(|s| store.sessions.contains_key(s))
            {
                drop(store);
                return next.run(req).await;
            }
            if req.method() != Method::GET
                || req.uri().path().starts_with("/api/")
                || req.uri().path() == "/api"
            {
                return (StatusCode::UNAUTHORIZED, "Authentication required").into_response();
            }
            let state = random_token();
            let verifier = random_token();
            let nonce = random_token();
            let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
            store.pending.insert(
                state.clone(),
                Pending {
                    verifier,
                    nonce: nonce.clone(),
                    created: now,
                },
            );
            drop(store);
            let Ok(mut url) = reqwest::Url::parse(&oidc.authorization_endpoint) else {
                return StatusCode::INTERNAL_SERVER_ERROR.into_response();
            };
            url.query_pairs_mut()
                .append_pair("response_type", "code")
                .append_pair("client_id", &oidc.client_id)
                .append_pair("redirect_uri", &oidc.redirect_url)
                .append_pair("scope", "openid profile email")
                .append_pair("state", &state)
                .append_pair("nonce", &nonce)
                .append_pair("code_challenge", &challenge)
                .append_pair("code_challenge_method", "S256");
            with_cookie(
                Redirect::to(url.as_str()).into_response(),
                &set_cookie(STATE_COOKIE, &state, LOGIN_TTL.as_secs(), oidc.secure),
            )
        }
    }
}

async fn callback(
    State(auth): State<DashboardAuth>,
    headers: HeaderMap,
    Query(params): Query<Callback>,
) -> Response {
    let Mode::Oidc(oidc) = auth.0.as_ref() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let clear = set_cookie(STATE_COOKIE, "", 0, oidc.secure);
    let Some(state) = params.state else {
        return with_cookie(StatusCode::BAD_REQUEST.into_response(), &clear);
    };
    let Some(browser_state) = cookie(&headers, STATE_COOKIE) else {
        return with_cookie(StatusCode::BAD_REQUEST.into_response(), &clear);
    };
    if !equal(state.as_bytes(), browser_state.as_bytes()) {
        return with_cookie(StatusCode::BAD_REQUEST.into_response(), &clear);
    }
    let pending = oidc.state.lock().await.pending.remove(&state);
    let Some(pending) = pending.filter(|p| p.created.elapsed() < LOGIN_TTL) else {
        return with_cookie(StatusCode::BAD_REQUEST.into_response(), &clear);
    };
    let Some(code) = params.code.filter(|_| params.error.is_none()) else {
        return with_cookie(StatusCode::UNAUTHORIZED.into_response(), &clear);
    };
    if verify_login(oidc, &code, &pending).await.is_err() {
        return with_cookie(StatusCode::UNAUTHORIZED.into_response(), &clear);
    }
    let session = random_token();
    oidc.state
        .lock()
        .await
        .sessions
        .insert(session.clone(), Instant::now() + SESSION_TTL);
    let response = with_cookie(Redirect::to("/").into_response(), &clear);
    with_cookie(
        response,
        &set_cookie(SESSION_COOKIE, &session, SESSION_TTL.as_secs(), oidc.secure),
    )
}

async fn verify_login(oidc: &Oidc, code: &str, pending: &Pending) -> Result<(), String> {
    let tokens: TokenResponse = oidc
        .client
        .post(&oidc.token_endpoint)
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", &oidc.redirect_url),
            ("client_id", &oidc.client_id),
            ("client_secret", &oidc.client_secret),
            ("code_verifier", &pending.verifier),
        ])
        .send()
        .await
        .map_err(|e| e.to_string())?
        .error_for_status()
        .map_err(|e| e.to_string())?
        .json()
        .await
        .map_err(|e| e.to_string())?;
    let header = decode_header(&tokens.id_token).map_err(|e| e.to_string())?;
    // Reject symmetric or unsigned algorithms: provider JWKS must contain a public key.
    if !matches!(
        header.alg,
        Algorithm::RS256
            | Algorithm::RS384
            | Algorithm::RS512
            | Algorithm::PS256
            | Algorithm::PS384
            | Algorithm::PS512
            | Algorithm::ES256
            | Algorithm::ES384
            | Algorithm::EdDSA
    ) {
        return Err("unsupported ID token algorithm".into());
    }
    let kid = header.kid.ok_or("ID token missing key ID")?;
    let keys: JwkSet = oidc
        .client
        .get(&oidc.jwks_uri)
        .send()
        .await
        .map_err(|e| e.to_string())?
        .error_for_status()
        .map_err(|e| e.to_string())?
        .json()
        .await
        .map_err(|e| e.to_string())?;
    let key = keys.find(&kid).ok_or("OIDC signing key not found")?;
    let key = DecodingKey::from_jwk(key).map_err(|e| e.to_string())?;
    let mut validation = Validation::new(header.alg);
    validation.set_issuer(&[&oidc.issuer]);
    validation.set_audience(&[&oidc.client_id]);
    validation.set_required_spec_claims(&["exp", "iss", "aud", "sub"]);
    let claims = decode::<Claims>(&tokens.id_token, &key, &validation)
        .map_err(|e| e.to_string())?
        .claims;
    if claims.sub.is_empty() || !equal(claims.nonce.as_bytes(), pending.nonce.as_bytes()) {
        return Err("invalid ID token claims".into());
    }
    if !allowed_email(&claims, &oidc.allowed_emails) {
        return Err("OIDC email is not verified or authorized".into());
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use axum::{body::Body, http::Request};
    use tower::ServiceExt;

    #[test]
    fn oidc_requires_verified_allowlisted_email() {
        let allowed = vec!["admin@example.com".to_owned()];
        let mut claims = Claims {
            nonce: "nonce".into(),
            sub: "subject".into(),
            email: None,
            email_verified: None,
        };
        assert!(!allowed_email(&claims, &allowed));
        claims.email = Some("admin@example.com".into());
        assert!(!allowed_email(&claims, &allowed));
        claims.email_verified = Some(false);
        assert!(!allowed_email(&claims, &allowed));
        claims.email_verified = Some(true);
        assert!(allowed_email(&claims, &allowed));
        claims.email = Some("other@example.com".into());
        assert!(!allowed_email(&claims, &allowed));
    }

    #[tokio::test]
    async fn writes_require_trusted_origin_and_host_in_both_modes() {
        let mut store = Store::default();
        store
            .sessions
            .insert("session".into(), Instant::now() + SESSION_TTL);
        let oidc = Mode::Oidc(Box::new(Oidc {
            client: reqwest::Client::new(),
            issuer: "https://issuer.example".into(),
            client_id: "client".into(),
            client_secret: "secret".into(),
            allowed_emails: vec!["admin@example.com".into()],
            redirect_url: "https://dashboard.example/auth/callback".into(),
            authorization_endpoint: "https://issuer.example/authorize".into(),
            token_endpoint: "https://issuer.example/token".into(),
            jwks_uri: "https://issuer.example/jwks".into(),
            secure: true,
            state: Mutex::new(store),
        }));
        for mode in [
            Mode::Basic {
                username: "admin".into(),
                password: "secret".into(),
            },
            oidc,
        ] {
            let auth = DashboardAuth(
                Arc::new(mode),
                Arc::new(reqwest::Url::parse("https://dashboard.example").unwrap()),
            );
            let app = auth.protect(
                Router::new().route("/api/settings", axum::routing::post(|| async { "ok" })),
            );
            for (host, origin, fetch, expected) in [
                (
                    "dashboard.example",
                    Some("https://dashboard.example"),
                    Some("same-origin"),
                    StatusCode::OK,
                ),
                ("dashboard.example", None, None, StatusCode::OK),
                (
                    "dashboard.example",
                    Some("https://evil.example"),
                    Some("cross-site"),
                    StatusCode::FORBIDDEN,
                ),
                (
                    "dashboard.example",
                    None,
                    Some("cross-site"),
                    StatusCode::FORBIDDEN,
                ),
                (
                    "evil.example",
                    Some("https://evil.example"),
                    Some("same-origin"),
                    StatusCode::FORBIDDEN,
                ),
            ] {
                let mut request = Request::builder()
                    .method(Method::POST)
                    .uri("/api/settings")
                    .header(header::HOST, host)
                    .header(header::AUTHORIZATION, "Basic YWRtaW46c2VjcmV0")
                    .header(header::COOKIE, "zero_g_dashboard_session=session");
                if let Some(value) = origin {
                    request = request.header(header::ORIGIN, value);
                }
                if let Some(value) = fetch {
                    request = request.header("sec-fetch-site", value);
                }
                let response = app
                    .clone()
                    .oneshot(request.body(Body::empty()).unwrap())
                    .await
                    .unwrap();
                assert_eq!(response.status(), expected);
                assert_eq!(
                    response.headers().get(header::CACHE_CONTROL).unwrap(),
                    "no-store"
                );
            }
        }
    }

    #[tokio::test]
    async fn basic_auth_protects_dashboard_and_api() {
        let auth = DashboardAuth(
            Arc::new(Mode::Basic {
                username: "admin".into(),
                password: "secret".into(),
            }),
            Arc::new(reqwest::Url::parse("https://dashboard.example").unwrap()),
        );
        let app = auth.protect(
            Router::new()
                .route("/", get(|| async { "dashboard" }))
                .route("/api/observability", get(|| async { "logs" })),
        );
        for path in ["/", "/api/observability"] {
            let request = Request::builder()
                .uri(path)
                .header(header::HOST, "dashboard.example")
                .body(Body::empty());
            let response = app.clone().oneshot(request.unwrap()).await.unwrap();
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
            assert!(response.headers().contains_key(header::WWW_AUTHENTICATE));

            let request = Request::builder()
                .uri(path)
                .header(header::HOST, "dashboard.example")
                .header(header::AUTHORIZATION, "Basic YWRtaW46d3Jvbmc=")
                .body(Body::empty());
            assert_eq!(
                app.clone()
                    .oneshot(request.unwrap())
                    .await
                    .unwrap()
                    .status(),
                StatusCode::UNAUTHORIZED
            );

            let request = Request::builder()
                .uri(path)
                .header(header::HOST, "dashboard.example")
                .header(header::AUTHORIZATION, "Basic YWRtaW46c2VjcmV0")
                .body(Body::empty());
            assert_eq!(
                app.clone()
                    .oneshot(request.unwrap())
                    .await
                    .unwrap()
                    .status(),
                StatusCode::OK
            );
        }
    }

    #[tokio::test]
    async fn oidc_callback_requires_matching_unexpired_single_use_state() {
        let mut store = Store::default();
        store.pending.insert(
            "valid".into(),
            Pending {
                verifier: "verifier".into(),
                nonce: "nonce".into(),
                created: Instant::now(),
            },
        );
        store.pending.insert(
            "expired".into(),
            Pending {
                verifier: "verifier".into(),
                nonce: "nonce".into(),
                created: Instant::now().checked_sub(LOGIN_TTL).unwrap(),
            },
        );
        let auth = DashboardAuth(
            Arc::new(Mode::Oidc(Box::new(Oidc {
                client: reqwest::Client::new(),
                issuer: "https://issuer.example".into(),
                client_id: "client".into(),
                client_secret: "secret".into(),
                allowed_emails: vec!["admin@example.com".into()],
                redirect_url: "https://dashboard.example/auth/callback".into(),
                authorization_endpoint: "https://issuer.example/authorize".into(),
                token_endpoint: "https://issuer.example/token".into(),
                jwks_uri: "https://issuer.example/jwks".into(),
                secure: true,
                state: Mutex::new(store),
            }))),
            Arc::new(reqwest::Url::parse("https://dashboard.example").unwrap()),
        );
        let app = auth.protect(Router::new().route("/", get(|| async { "dashboard" })));
        for (uri, cookie_value, expected) in [
            ("/auth/callback?state=valid", None, StatusCode::BAD_REQUEST),
            (
                "/auth/callback?state=valid",
                Some("zero_g_oidc_state=wrong"),
                StatusCode::BAD_REQUEST,
            ),
            (
                "/auth/callback?state=expired",
                Some("zero_g_oidc_state=expired"),
                StatusCode::BAD_REQUEST,
            ),
            (
                "/auth/callback?state=valid&error=access_denied",
                Some("zero_g_oidc_state=valid"),
                StatusCode::UNAUTHORIZED,
            ),
            (
                "/auth/callback?state=valid&error=access_denied",
                Some("zero_g_oidc_state=valid"),
                StatusCode::BAD_REQUEST,
            ),
        ] {
            let mut request = Request::builder()
                .uri(uri)
                .header(header::HOST, "dashboard.example");
            if let Some(value) = cookie_value {
                request = request.header(header::COOKIE, value);
            }
            let response = app
                .clone()
                .oneshot(request.body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), expected);
            let cookie = response
                .headers()
                .get(header::SET_COOKIE)
                .unwrap()
                .to_str()
                .unwrap();
            assert!(cookie.contains("Max-Age=0") && cookie.contains("Secure"));
        }
    }
}
