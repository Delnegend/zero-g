//! Dashboard authentication. Set `ZERO_G_DASHBOARD_AUTH` to `basic` or `oidc`.
//! Basic requires `ZERO_G_DASHBOARD_USERNAME` and `ZERO_G_DASHBOARD_PASSWORD`.
//! OIDC requires `ZERO_G_OIDC_ISSUER`,
//! `ZERO_G_OIDC_CLIENT_ID`, `ZERO_G_OIDC_CLIENT_SECRET`, `ZERO_G_OIDC_REDIRECT_URL`
//! and at least one of `ZERO_G_OIDC_ALLOWED_EMAILS` or `ZERO_G_OIDC_ALLOWED_GROUPS`.
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
use openidconnect::{
    AdditionalClaims, AuthorizationCode, ClientId, ClientSecret, CsrfToken, EmptyExtraTokenFields,
    EndpointMaybeSet, EndpointNotSet, EndpointSet, IdTokenFields, IssuerUrl, JsonWebKeySetUrl,
    Nonce, OAuth2TokenResponse, PkceCodeChallenge, PkceCodeVerifier, RedirectUrl, Scope,
    StandardErrorResponse, StandardTokenResponse, TokenResponse,
    core::{
        CoreAuthDisplay, CoreAuthPrompt, CoreAuthenticationFlow, CoreErrorResponseType,
        CoreGenderClaim, CoreIdTokenVerifier, CoreJsonWebKey, CoreJsonWebKeySet,
        CoreJweContentEncryptionAlgorithm, CoreJwsSigningAlgorithm, CoreProviderMetadata,
        CoreRevocableToken, CoreRevocationErrorResponse, CoreTokenIntrospectionResponse,
        CoreTokenType,
    },
};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

/// OIDC HTTP client that validates every requested endpoint before sending.
struct SafeHttp(reqwest::Client);

impl<'a> openidconnect::AsyncHttpClient<'a> for SafeHttp {
    type Error = std::io::Error;
    type Future = std::pin::Pin<
        Box<
            dyn Future<Output = Result<openidconnect::HttpResponse, Self::Error>>
                + Send
                + Sync
                + 'a,
        >,
    >;

    fn call(&'a self, request: openidconnect::HttpRequest) -> Self::Future {
        Box::pin(async move {
            safe_url(&request.uri().to_string()).map_err(std::io::Error::other)?;
            openidconnect::AsyncHttpClient::call(&self.0, request)
                .await
                .map_err(std::io::Error::other)
        })
    }
}

/// OIDC client configured with the provider's discovered endpoints and group claims.
type OidcClient = openidconnect::Client<
    ExtraClaims,
    CoreAuthDisplay,
    CoreGenderClaim,
    CoreJweContentEncryptionAlgorithm,
    CoreJsonWebKey,
    CoreAuthPrompt,
    StandardErrorResponse<CoreErrorResponseType>,
    StandardTokenResponse<
        IdTokenFields<
            ExtraClaims,
            EmptyExtraTokenFields,
            CoreGenderClaim,
            CoreJweContentEncryptionAlgorithm,
            CoreJwsSigningAlgorithm,
        >,
        CoreTokenType,
    >,
    CoreTokenIntrospectionResponse,
    CoreRevocableToken,
    CoreRevocationErrorResponse,
    EndpointSet,
    EndpointNotSet,
    EndpointNotSet,
    EndpointNotSet,
    EndpointMaybeSet,
    EndpointMaybeSet,
>;

/// Additional identity-token claims used for group authorization.
#[derive(Clone, Debug, Deserialize, Serialize)]
struct ExtraClaims {
    /// Group memberships asserted by the identity provider.
    #[serde(default)]
    groups: Vec<String>,
}
impl AdditionalClaims for ExtraClaims {}

/// Path receiving the identity provider's authorization response.
const CALLBACK: &str = "/auth/callback";
/// Cookie carrying the opaque dashboard session token.
const SESSION_COOKIE: &str = "zero_g_dashboard_session";
/// Cookie binding an OIDC callback to its initiating browser.
const STATE_COOKIE: &str = "zero_g_oidc_state";
/// Maximum age of a pending authorization flow.
const LOGIN_TTL: Duration = Duration::from_mins(10);
/// Lifetime of an authenticated dashboard session.
const SESSION_TTL: Duration = Duration::from_hours(8);

/// Shared authentication configuration and trusted public origin.
#[derive(Clone)]
pub(crate) struct DashboardAuth(Arc<Mode>, Arc<reqwest::Url>);

/// Dashboard authentication mechanism selected at startup.
enum Mode {
    /// HTTP Basic credentials configured for the dashboard.
    Basic {
        /// Expected Basic username.
        username: String,
        /// Expected Basic password.
        password: String,
    },
    /// OIDC authorization-code flow and session state.
    Oidc(Box<Oidc>),
}

/// Discovered OIDC provider configuration and local login state.
struct Oidc {
    /// Validating HTTP client for discovery and token requests.
    http_client: SafeHttp,
    /// Configured authorization and token endpoint client.
    client: OidcClient,
    /// Expected issuer of identity tokens.
    issuer: IssuerUrl,
    /// Endpoint supplying identity-token verification keys.
    jwks_uri: JsonWebKeySetUrl,
    /// Provider-advertised signing algorithms accepted for identity tokens.
    signing_algorithms: Vec<CoreJwsSigningAlgorithm>,
    /// Email addresses permitted when verified by the provider.
    allowed_emails: Vec<String>,
    /// Group memberships permitted to access the dashboard.
    allowed_groups: Vec<String>,
    /// Whether authentication cookies require HTTPS.
    secure: bool,
    /// Pending login flows and active sessions protected by a mutex.
    state: Mutex<Store>,
}

/// In-memory login and session records.
#[derive(Default)]
struct Store {
    /// Authorization state tokens mapped to their pending flow.
    pending: HashMap<String, Pending>,
    /// Session tokens mapped to their expiration instants.
    sessions: HashMap<String, Instant>,
}

/// Data retained between authorization redirect and callback.
struct Pending {
    /// PKCE verifier used when exchanging the authorization code.
    verifier: String,
    /// Nonce expected in the identity token.
    nonce: String,
    /// Local path to visit after successful login.
    return_to: String,
    /// Instant at which the login flow began.
    created: Instant,
}

/// Query parameters returned by the OIDC authorization endpoint.
#[derive(Deserialize)]
struct Callback {
    /// CSRF state returned by the provider.
    state: Option<String>,
    /// Authorization code to exchange for tokens.
    code: Option<String>,
    /// Provider-reported authorization error, if any.
    error: Option<String>,
}

/// Reads a required nonblank environment variable.
fn required(name: &str) -> Result<String, String> {
    std::env::var(name)
        .ok()
        .filter(|v| !v.trim().is_empty())
        .ok_or_else(|| format!("{name} must be set"))
}

/// Authorizes a verified allowlisted email or an allowlisted group membership.
fn authorized(
    email: Option<&str>,
    verified: bool,
    membership: &[String],
    emails: &[String],
    groups: &[String],
) -> bool {
    (verified && email.is_some_and(|value| emails.iter().any(|entry| entry == value)))
        || membership.iter().any(|group| groups.contains(group))
}

/// Parses a comma-separated allowlist, dropping blank entries.
fn allowlist(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Validates an endpoint URL, allowing plaintext HTTP only on loopback.
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
    /// Builds isolated Basic authentication for provider integration tests.
    #[cfg(test)]
    pub(crate) fn test() -> Result<Self, String> {
        Ok(Self(
            Arc::new(Mode::Basic {
                username: "test".into(),
                password: "test".into(),
            }),
            Arc::new(reqwest::Url::parse("http://127.0.0.1:8080").map_err(|e| e.to_string())?),
        ))
    }

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
                let allowed_emails =
                    allowlist(&std::env::var("ZERO_G_OIDC_ALLOWED_EMAILS").unwrap_or_default());
                let allowed_groups =
                    allowlist(&std::env::var("ZERO_G_OIDC_ALLOWED_GROUPS").unwrap_or_default());
                if allowed_emails.is_empty() && allowed_groups.is_empty() {
                    return Err("Configure at least one OIDC allowed email or group".into());
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
                let http_client = SafeHttp(
                    reqwest::Client::builder()
                        .redirect(reqwest::redirect::Policy::none())
                        .timeout(Duration::from_secs(10))
                        .build()
                        .map_err(|e| format!("creating OIDC client: {e}"))?,
                );
                let metadata = CoreProviderMetadata::discover_async(
                    IssuerUrl::new(issuer).map_err(|e| format!("invalid OIDC issuer: {e}"))?,
                    &http_client,
                )
                .await
                .map_err(|e| format!("OIDC discovery: {e}"))?;
                safe_url(metadata.authorization_endpoint().url().as_str())?;
                if let Some(endpoint) = metadata.token_endpoint() {
                    safe_url(endpoint.url().as_str())?;
                }
                let issuer = metadata.issuer().clone();
                let jwks_uri = metadata.jwks_uri().clone();
                let signing_algorithms = metadata.id_token_signing_alg_values_supported().clone();
                let client = OidcClient::from_provider_metadata(
                    metadata,
                    ClientId::new(client_id),
                    Some(ClientSecret::new(client_secret)),
                )
                .set_redirect_uri(RedirectUrl::new(redirect_url).map_err(|e| e.to_string())?);
                Mode::Oidc(Box::new(Oidc {
                    http_client,
                    client,
                    issuer,
                    jwks_uri,
                    signing_algorithms,
                    allowed_emails,
                    allowed_groups,
                    secure: redirect.scheme() == "https",
                    state: Mutex::new(Store::default()),
                }))
            }
            _ => return Err("ZERO_G_DASHBOARD_AUTH must be basic or oidc".into()),
        };
        Ok(Self(Arc::new(mode), Arc::new(origin)))
    }

    /// Adds authentication to an already assembled dashboard router. Merge the
    /// result with the separately assembled inference router afterwards.
    pub(super) fn protect(&self, router: Router) -> Router {
        router
            .merge(
                Router::new()
                    .route(CALLBACK, get(callback))
                    .with_state(self.clone()),
            )
            .layer(middleware::from_fn_with_state(self.clone(), authenticate))
    }
}

/// Generates a random URL-safe token for an authenticated session.
fn random_token() -> String {
    let mut bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

/// Finds a named cookie across the request's Cookie headers.
fn cookie(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find_map(|(key, value)| (key == name).then(|| value.to_owned()))
}

/// Builds an HTTP-only, same-site cookie with an optional Secure attribute.
fn set_cookie(name: &str, value: &str, max_age: u64, secure: bool) -> String {
    format!(
        "{name}={value}; Path=/; HttpOnly; SameSite=Lax; Max-Age={max_age}{}",
        if secure { "; Secure" } else { "" }
    )
}

/// Appends a cookie and prevents caching of the response.
fn with_cookie(mut response: Response, value: &str) -> Response {
    if let Ok(value) = HeaderValue::from_str(value) {
        response.headers_mut().append(header::SET_COOKIE, value);
    }
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

/// Compares byte slices without early exit on differing bytes.
// Avoid early exit on differing bytes for credentials and state values.
fn equal(a: &[u8], b: &[u8]) -> bool {
    let mut difference = a.len() ^ b.len();
    for i in 0..a.len().max(b.len()) {
        difference |= usize::from(a.get(i).copied().unwrap_or(0) ^ b.get(i).copied().unwrap_or(0));
    }
    difference == 0
}

/// Checks request origin and authentication, disabling caching for API responses.
async fn authenticate(State(auth): State<DashboardAuth>, req: Request, next: Next) -> Response {
    let is_api = req.uri().path() == "/api" || req.uri().path().starts_with("/api/");
    let mut response = if valid_request_origin(&auth.1, &req) {
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

/// Enforces the configured host and same-origin rules for dashboard requests.
fn valid_request_origin(origin: &reqwest::Url, req: &Request) -> bool {
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
        && headers
            .keys()
            .any(|name| name.as_str().starts_with("sec-fetch-"))
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

/// Validates Basic credentials or an OIDC session, starting login when needed.
async fn authenticate_inner(auth: DashboardAuth, req: Request, next: Next) -> Response {
    match auth.0.as_ref() {
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
            let (challenge, verifier) = PkceCodeChallenge::new_random_sha256();
            let mut request = oidc
                .client
                .authorize_url(
                    CoreAuthenticationFlow::AuthorizationCode,
                    CsrfToken::new_random,
                    Nonce::new_random,
                )
                .add_scope(Scope::new("profile".into()))
                .add_scope(Scope::new("email".into()))
                .set_pkce_challenge(challenge);
            if !oidc.allowed_groups.is_empty() {
                request = request.add_scope(Scope::new("groups".into()));
            }
            let (url, csrf, nonce) = request.url();
            let state = csrf.secret().to_owned();
            store.pending.insert(
                state.clone(),
                Pending {
                    verifier: verifier.secret().to_owned(),
                    nonce: nonce.secret().to_owned(),
                    return_to: req
                        .uri()
                        .path_and_query()
                        .map(axum::http::uri::PathAndQuery::as_str)
                        .filter(|value| {
                            value.starts_with('/')
                                && !value.starts_with("//")
                                && !value.contains('\\')
                        })
                        .unwrap_or("/")
                        .to_owned(),
                    created: now,
                },
            );
            drop(store);
            with_cookie(
                Redirect::to(url.as_str()).into_response(),
                &set_cookie(STATE_COOKIE, &state, LOGIN_TTL.as_secs(), oidc.secure),
            )
        }
    }
}

/// Consumes OIDC callback state and creates a session after successful verification.
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
    let Some(expires) = Instant::now().checked_add(SESSION_TTL) else {
        return with_cookie(StatusCode::INTERNAL_SERVER_ERROR.into_response(), &clear);
    };
    let session = random_token();
    oidc.state
        .lock()
        .await
        .sessions
        .insert(session.clone(), expires);
    let response = with_cookie(Redirect::to(&pending.return_to).into_response(), &clear);
    with_cookie(
        response,
        &set_cookie(SESSION_COOKIE, &session, SESSION_TTL.as_secs(), oidc.secure),
    )
}

/// Exchanges a code and verifies the identity token and authorization claims.
async fn verify_login(oidc: &Oidc, code: &str, pending: &Pending) -> Result<(), String> {
    let tokens = oidc
        .client
        .exchange_code(AuthorizationCode::new(code.to_owned()))
        .map_err(|e| e.to_string())?
        .set_pkce_verifier(PkceCodeVerifier::new(pending.verifier.clone()))
        .request_async(&oidc.http_client)
        .await
        .map_err(|e| e.to_string())?;
    let id_token = tokens.id_token().ok_or("OIDC response missing ID token")?;
    if !matches!(
        id_token.signing_alg().map_err(|e| e.to_string())?,
        CoreJwsSigningAlgorithm::RsaSsaPkcs1V15Sha256
            | CoreJwsSigningAlgorithm::RsaSsaPkcs1V15Sha384
            | CoreJwsSigningAlgorithm::RsaSsaPkcs1V15Sha512
            | CoreJwsSigningAlgorithm::RsaSsaPssSha256
            | CoreJwsSigningAlgorithm::RsaSsaPssSha384
            | CoreJwsSigningAlgorithm::RsaSsaPssSha512
            | CoreJwsSigningAlgorithm::EcdsaP256Sha256
            | CoreJwsSigningAlgorithm::EcdsaP384Sha384
            | CoreJwsSigningAlgorithm::EdDsa
    ) {
        return Err("unsupported ID token algorithm".into());
    }
    let keys = CoreJsonWebKeySet::fetch_async(&oidc.jwks_uri, &oidc.http_client)
        .await
        .map_err(|e| e.to_string())?;
    let verifier = CoreIdTokenVerifier::new_public_client(
        oidc.client.client_id().clone(),
        oidc.issuer.clone(),
        keys,
    )
    .set_allowed_algs(oidc.signing_algorithms.clone());
    let claims = id_token
        .claims(&verifier, &Nonce::new(pending.nonce.clone()))
        .map_err(|e| e.to_string())?;
    if claims.subject().as_str().is_empty() {
        return Err("invalid ID token subject".into());
    }
    if let Some(expected) = claims.access_token_hash() {
        let actual = openidconnect::AccessTokenHash::from_token(
            tokens.access_token(),
            id_token.signing_alg().map_err(|e| e.to_string())?,
            id_token.signing_key(&verifier).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        if actual != *expected {
            return Err("invalid access token hash".into());
        }
    }
    if !authorized(
        claims.email().map(|email| email.as_str()),
        claims.email_verified().unwrap_or(false),
        &claims.additional_claims().groups,
        &oidc.allowed_emails,
        &oidc.allowed_groups,
    ) {
        return Err("OIDC identity is not authorized".into());
    }
    Ok(())
}

#[cfg(test)]
/// Exercises dashboard authentication, origin checks, and OIDC login.
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use axum::{body::Body, http::Request};
    use tower::ServiceExt;
    /// Builds a test OIDC configuration with supplied in-memory state.
    fn test_oidc(store: Store) -> Oidc {
        let metadata: CoreProviderMetadata = serde_json::from_value(serde_json::json!({
            "issuer": "https://issuer.example",
            "authorization_endpoint": "https://issuer.example/authorize",
            "token_endpoint": "https://issuer.example/token",
            "jwks_uri": "https://issuer.example/jwks",
            "response_types_supported": ["code"],
            "subject_types_supported": ["public"],
            "id_token_signing_alg_values_supported": ["RS256"]
        }))
        .unwrap();
        let issuer = metadata.issuer().clone();
        let jwks_uri = metadata.jwks_uri().clone();
        let signing_algorithms = metadata.id_token_signing_alg_values_supported().clone();
        let client = OidcClient::from_provider_metadata(
            metadata,
            ClientId::new("client".into()),
            Some(ClientSecret::new("secret".into())),
        )
        .set_redirect_uri(
            RedirectUrl::new("https://dashboard.example/auth/callback".into()).unwrap(),
        );
        Oidc {
            http_client: SafeHttp(reqwest::Client::new()),
            client,
            issuer,
            jwks_uri,
            signing_algorithms,
            allowed_emails: vec!["admin@example.com".into()],
            allowed_groups: vec![],
            secure: true,
            state: Mutex::new(store),
        }
    }

    /// Verified email or exact group membership grants access.
    #[test]
    fn oidc_allows_verified_emails_or_exact_group_membership() {
        let emails = vec!["admin@example.com".to_owned()];
        let groups = vec!["admins".to_owned()];
        assert!(!authorized(None, false, &[], &emails, &groups));
        assert!(!authorized(
            Some("admin@example.com"),
            false,
            &[],
            &emails,
            &groups
        ));
        assert!(authorized(
            Some("admin@example.com"),
            true,
            &[],
            &emails,
            &groups
        ));
        assert!(!authorized(
            None,
            false,
            &["admin".into()],
            &emails,
            &groups
        ));
        assert!(authorized(
            None,
            false,
            &["admins".into()],
            &emails,
            &groups
        ));
        assert!(!authorized(None, false, &["admins".into()], &[], &[]));
    }

    /// Both authentication modes enforce host and origin rules for writes.
    #[tokio::test]
    async fn writes_require_trusted_origin_and_host_in_both_modes() {
        let mut store = Store::default();
        store.sessions.insert(
            "session".into(),
            Instant::now().checked_add(SESSION_TTL).unwrap(),
        );
        let oidc = Mode::Oidc(Box::new(test_oidc(store)));
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

    /// Basic authentication protects both dashboard and management API routes.
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

    /// Callback state must match the browser and be fresh and single-use.
    #[tokio::test]
    async fn oidc_callback_requires_matching_unexpired_single_use_state() {
        let mut store = Store::default();
        store.pending.insert(
            "valid".into(),
            Pending {
                verifier: "verifier".into(),
                nonce: "nonce".into(),
                return_to: "/".into(),
                created: Instant::now(),
            },
        );
        store.pending.insert(
            "expired".into(),
            Pending {
                verifier: "verifier".into(),
                nonce: "nonce".into(),
                return_to: "/".into(),
                created: Instant::now().checked_sub(LOGIN_TTL).unwrap(),
            },
        );
        let auth = DashboardAuth(
            Arc::new(Mode::Oidc(Box::new(test_oidc(store)))),
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
    #[allow(clippy::too_many_lines)]
    /// Signed OIDC login validates nonce and group before creating a session.
    #[tokio::test]
    async fn oidc_signed_login_checks_nonce_group_and_session() {
        use ring::signature::{RSA_PKCS1_SHA256, RsaKeyPair};
        use serde_json::{Value, json};
        use std::sync::Arc;
        use tokio::sync::Mutex;

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let issuer = format!("http://{}", listener.local_addr().unwrap());
        let fixture: Value =
            serde_json::from_str(include_str!("../../tests/fixtures/oidc-test-key.json")).unwrap();
        let signing_key = Arc::new(
            RsaKeyPair::from_pkcs8(
                &STANDARD
                    .decode(fixture["privateKey"].as_str().unwrap())
                    .unwrap(),
            )
            .unwrap(),
        );
        let public_key = fixture["publicKey"].clone();
        let publish_key = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let provider_key = publish_key.clone();
        let scenario = Arc::new(Mutex::new((String::new(), true, true, true)));
        let discovery_issuer = issuer.clone();
        let keys = public_key.clone();
        let token_issuer = issuer.clone();
        let token_scenario = scenario.clone();
        let provider = Router::new()
            .route("/.well-known/openid-configuration", get(move || {
                let base = discovery_issuer.clone();
                async move { axum::Json(json!({"issuer":base,"authorization_endpoint":format!("{base}/authorize"),"token_endpoint":format!("{base}/token"),"jwks_uri":format!("{base}/keys"),"response_types_supported":["code"],"subject_types_supported":["public"],"id_token_signing_alg_values_supported":["RS256"]})) }
            }))
            .route("/keys", get(move || {
                let mut key = keys.clone();
                let published = provider_key.load(std::sync::atomic::Ordering::Relaxed);
                async move {
                    key["kid"] = json!("test-key");
                    key["alg"] = json!("RS256");
                    key["use"] = json!("sig");
                    axum::Json(json!({"keys": if published { vec![key] } else { vec![] }}))
                }
            }))
            .route("/token", axum::routing::post(move || {
                let base = token_issuer.clone();
                let scenario = token_scenario.clone();
                let key = signing_key.clone();
                async move {
                    let (nonce, matching_nonce, allowed_group, valid_signature) = scenario.lock().await.clone();
                    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
                    let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"RS256","kid":"test-key","typ":"JWT"}"#);
                    let claims = json!({"iss":base,"aud":"client","sub":"subject","iat":now,"exp":now.saturating_add(300),"nonce":if matching_nonce {nonce} else {"wrong".into()},"groups":if allowed_group {vec!["admins"]} else {vec!["users"]}});
                    let payload = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).unwrap());
                    let message = format!("{header}.{payload}");
                    let mut signature = vec![0; key.public().modulus_len()];
                    key.sign(&RSA_PKCS1_SHA256, &ring::rand::SystemRandom::new(), message.as_bytes(), &mut signature).unwrap();
                    if !valid_signature { signature[0] ^= 1; }
                    axum::Json(json!({"access_token":"test-access","token_type":"Bearer","id_token":format!("{message}.{}", URL_SAFE_NO_PAD.encode(signature))}))
                }
            }));
        let task = tokio::spawn(async move {
            axum::serve(listener, provider).await.unwrap();
        });
        let http_client = SafeHttp(
            reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .unwrap(),
        );
        let metadata =
            CoreProviderMetadata::discover_async(IssuerUrl::new(issuer).unwrap(), &http_client)
                .await
                .unwrap();
        publish_key.store(true, std::sync::atomic::Ordering::Relaxed);
        let issuer = metadata.issuer().clone();
        let jwks_uri = metadata.jwks_uri().clone();
        let signing_algorithms = metadata.id_token_signing_alg_values_supported().clone();
        let client = OidcClient::from_provider_metadata(
            metadata,
            ClientId::new("client".into()),
            Some(ClientSecret::new("secret".into())),
        )
        .set_redirect_uri(RedirectUrl::new("http://127.0.0.1:8080/auth/callback".into()).unwrap());
        let oidc = Oidc {
            http_client,
            client,
            issuer,
            jwks_uri,
            signing_algorithms,
            allowed_emails: vec![],
            allowed_groups: vec!["admins".into()],
            secure: false,
            state: Mutex::new(Store::default()),
        };
        let auth = DashboardAuth(
            Arc::new(Mode::Oidc(Box::new(oidc))),
            Arc::new(reqwest::Url::parse("http://127.0.0.1:8080").unwrap()),
        );
        let app = auth.protect(
            Router::new()
                .route("/", get(|| async { "dashboard" }))
                .route("/api/settings", get(|| async { "settings" })),
        );
        for (valid_nonce, allowed_group, valid_signature, expected) in [
            (true, true, true, StatusCode::SEE_OTHER),
            (false, true, true, StatusCode::UNAUTHORIZED),
            (true, false, true, StatusCode::UNAUTHORIZED),
            (true, true, false, StatusCode::UNAUTHORIZED),
        ] {
            let start = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri("/llm-logs?search=test")
                        .header(header::HOST, "127.0.0.1:8080")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            let url = reqwest::Url::parse(
                start
                    .headers()
                    .get(header::LOCATION)
                    .unwrap()
                    .to_str()
                    .unwrap(),
            )
            .unwrap();
            let query: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
            assert!(
                query["scope"]
                    .split_whitespace()
                    .any(|scope| scope == "groups")
            );
            *scenario.lock().await = (
                query["nonce"].clone(),
                valid_nonce,
                allowed_group,
                valid_signature,
            );
            let cookie = start
                .headers()
                .get(header::SET_COOKIE)
                .unwrap()
                .to_str()
                .unwrap()
                .split(';')
                .next()
                .unwrap();
            let callback = format!("/auth/callback?code=test-code&state={}", query["state"]);
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(callback)
                        .header(header::HOST, "127.0.0.1:8080")
                        .header(header::COOKIE, cookie)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), expected);
            if expected == StatusCode::SEE_OTHER {
                assert_eq!(
                    response.headers()[header::LOCATION],
                    "/llm-logs?search=test"
                );
                let session = response
                    .headers()
                    .get_all(header::SET_COOKIE)
                    .iter()
                    .filter_map(|v| v.to_str().ok())
                    .find(|v| v.starts_with("zero_g_dashboard_session="))
                    .unwrap()
                    .split(';')
                    .next()
                    .unwrap();
                let result = app
                    .clone()
                    .oneshot(
                        Request::builder()
                            .uri("/api/settings")
                            .header(header::HOST, "127.0.0.1:8080")
                            .header(header::COOKIE, session)
                            .body(Body::empty())
                            .unwrap(),
                    )
                    .await
                    .unwrap();
                assert_eq!(result.status(), StatusCode::OK);
            }
        }
        task.abort();
    }
}
