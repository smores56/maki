use std::time::Duration;
use std::{env, thread};

use isahc::ReadResponseExt;
use isahc::config::Configurable;
use maki_storage::StateDir;
use maki_storage::auth::{OAuthTokens, delete_tokens, load_tokens, now_millis, save_tokens};
use serde::Deserialize;
use tracing::{debug, error};

use crate::AgentError;
use crate::providers::{ResolvedAuth, urlenc};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const POLL_SAFETY_MARGIN: Duration = Duration::from_secs(3);
const TOKEN_EXCHANGE_TIMEOUT: Duration = Duration::from_secs(30);
const POLL_TIMEOUT: Duration = Duration::from_secs(300);
/// Upper bound on the device-polling interval the auth server may request.
/// Without it a hostile/buggy `interval` could stall the login thread past
/// `POLL_TIMEOUT`, since `thread::sleep` runs before the deadline check.
const MAX_POLL_INTERVAL_SECS: u64 = 60;
/// Cap on server-controlled response text echoed into error messages / logs.
/// Provider 5xx bodies can be HTML pages; without a cap they bloat logs and the
/// user-facing error surface unboundedly.
const MAX_ERROR_BODY: usize = 500;

/// Header name OpenAI's OAuth sets from the JWT-derived account id. OAuth-only
/// routing decisions on `OpenAi` key off this header's presence, and the value
/// comes from `OAuthConfig.account_id_header_name` so a future OAuth provider
/// with a different (or no) account-id header stays self-consistent.
pub(crate) const ACCOUNT_ID_HEADER: &str = "chatgpt-account-id";

#[derive(Debug, Clone, Copy)]
pub struct OAuthEndpoints {
    pub device_code_url: &'static str,
    pub device_token_url: &'static str,
    pub token_url: &'static str,
    pub device_auth_url: &'static str,
    pub redirect_uri: &'static str,
}

#[derive(Debug, Clone, Copy)]
pub struct OAuthConfig {
    pub provider: &'static str,
    pub client_id: &'static str,
    pub endpoints: OAuthEndpoints,
    pub env_fallback: Option<&'static str>,
    pub header_name: &'static str,
    pub header_value_format: &'static str,
    pub account_id_from_jwt: bool,
    pub account_id_header_name: Option<&'static str>,
}

pub const OPENAI_OAUTH: OAuthConfig = OAuthConfig {
    provider: "openai",
    client_id: "app_EMoamEEZ73f0CkXaXp7hrann",
    endpoints: OAuthEndpoints {
        device_code_url: "https://auth.openai.com/api/accounts/deviceauth/usercode",
        device_token_url: "https://auth.openai.com/api/accounts/deviceauth/token",
        token_url: "https://auth.openai.com/oauth/token",
        device_auth_url: "https://auth.openai.com/codex/device",
        redirect_uri: "https://auth.openai.com/deviceauth/callback",
    },
    env_fallback: Some("OPENAI_API_KEY"),
    header_name: "authorization",
    header_value_format: "Bearer {access}",
    account_id_from_jwt: true,
    account_id_header_name: Some(ACCOUNT_ID_HEADER),
};

pub fn oauth_config(slug: &str) -> Option<&'static OAuthConfig> {
    match slug {
        "openai" => Some(&OPENAI_OAUTH),
        _ => None,
    }
}

#[derive(Deserialize)]
struct DeviceCodeResponse {
    device_auth_id: String,
    user_code: String,
    interval: String,
}

#[derive(Deserialize)]
struct DeviceTokenResponse {
    authorization_code: String,
    code_verifier: String,
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: String,
    #[serde(default)]
    id_token: Option<String>,
    expires_in: Option<u64>,
}

fn http_client(timeout: Duration) -> Result<isahc::HttpClient, AgentError> {
    isahc::HttpClient::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(timeout)
        .build()
        .map_err(|e| AgentError::Config {
            message: format!("http client: {e}"),
        })
}

/// Truncate server-controlled response text before it flows into error
/// messages and logs. Char-based to stay panic-free on UTF-8 boundaries.
fn body_excerpt(text: &str) -> String {
    let trimmed = text.trim();
    if trimmed.chars().count() <= MAX_ERROR_BODY {
        return trimmed.to_string();
    }
    let head: String = trimmed.chars().take(MAX_ERROR_BODY).collect();
    format!("{head}...[truncated]")
}

/// OAuth endpoints return `{"error":"invalid_grant",...}` when a refresh
/// token is genuinely revoked. Distinguish that (caller deletes stored
/// tokens to force re-login) from transient 5xx/network failures (caller
/// keeps the token file and the retry path tries again).
fn is_invalid_grant(body: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v.get("error").and_then(|e| e.as_str()).map(String::from))
        .is_some_and(|e| e == "invalid_grant")
}

/// `invalid_grant` (or 400/401 from the token endpoint) means the refresh
/// token is revoked: surface as an auth error so the caller can wipe stored
/// tokens. Other failures are transient and stay `Config` so the caller leaves
/// the token file in place for the next retry.
fn refresh_failure(cfg: &OAuthConfig, status: u16, body: String) -> AgentError {
    let excerpt = body_excerpt(&body);
    if status == 400 || status == 401 || is_invalid_grant(&body) {
        AgentError::Api {
            status: 401,
            message: format!("{} refresh token revoked: {excerpt}", cfg.provider),
        }
    } else {
        AgentError::Config {
            message: format!("{} token refresh failed ({status}): {excerpt}", cfg.provider),
        }
    }
}

fn extract_account_id(token: &str) -> Option<String> {
    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() != 3 {
        return None;
    }
    use base64::Engine;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    let payload = URL_SAFE_NO_PAD.decode(parts[1]).ok()?;
    let claims: serde_json::Value = serde_json::from_slice(&payload).ok()?;

    claims
        .get("chatgpt_account_id")
        .and_then(|v| v.as_str())
        .or_else(|| {
            claims
                .pointer("/https:~1~1api.openai.com~1auth/chatgpt_account_id")
                .and_then(|v| v.as_str())
        })
        .or_else(|| {
            claims
                .get("organizations")
                .and_then(|v| v.as_array())
                .and_then(|arr| arr.first())
                .and_then(|org| org.get("id"))
                .and_then(|v| v.as_str())
        })
        .map(String::from)
}

fn extract_account_id_from_tokens(resp: &TokenResponse, cfg: &OAuthConfig) -> Option<String> {
    if !cfg.account_id_from_jwt {
        return None;
    }
    if let Some(id_token) = &resp.id_token
        && let Some(id) = extract_account_id(id_token)
    {
        return Some(id);
    }
    extract_account_id(&resp.access_token)
}

fn request_device_code(cfg: &OAuthConfig) -> Result<DeviceCodeResponse, AgentError> {
    let client = http_client(TOKEN_EXCHANGE_TIMEOUT)?;
    let body = serde_json::json!({"client_id": cfg.client_id});
    let json_body = serde_json::to_vec(&body)?;

    let request = isahc::Request::builder()
        .method("POST")
        .uri(cfg.endpoints.device_code_url)
        .header("content-type", "application/json")
        .body(json_body)?;

    let mut resp = client.send(request).map_err(|e| AgentError::Config {
        message: format!("device code request: {e}"),
    })?;

    if resp.status().as_u16() != 200 {
        let body_text = resp.text().unwrap_or_else(|_| "unknown error".into());
        return Err(AgentError::Config {
            message: format!("device code request failed: {}", body_excerpt(&body_text)),
        });
    }

    let body_text = resp.text()?;
    serde_json::from_str(&body_text).map_err(Into::into)
}

fn poll_device_token(cfg: &OAuthConfig) -> Result<DeviceTokenResponse, AgentError> {
    let client = http_client(POLL_TIMEOUT)?;
    let device = request_device_code(cfg)?;
    println!("Open this URL in your browser:\n\n  {}\n", cfg.endpoints.device_auth_url);
    if !device
        .user_code
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-')
    {
        return Err(AgentError::Config {
            message: "device authorization returned an unexpected user code format".into(),
        });
    }
    println!("Enter code: {}\n", device.user_code);
    println!("Waiting for authorization...");
    let interval_secs = device
        .interval
        .parse::<u64>()
        .unwrap_or(5)
        .clamp(1, MAX_POLL_INTERVAL_SECS);
    let poll_interval = Duration::from_secs(interval_secs) + POLL_SAFETY_MARGIN;
    let deadline = std::time::Instant::now() + POLL_TIMEOUT;

    let body = serde_json::json!({
        "device_auth_id": device.device_auth_id,
        "user_code": device.user_code,
    });
    let json_body = serde_json::to_vec(&body)?;

    loop {
        if std::time::Instant::now() > deadline {
            return Err(AgentError::Config {
                message: "device authorization timed out".into(),
            });
        }

        thread::sleep(poll_interval);

        let request = isahc::Request::builder()
            .method("POST")
            .uri(cfg.endpoints.device_token_url)
            .header("content-type", "application/json")
            .body(json_body.clone())?;

        let mut resp = client.send(request).map_err(|e| AgentError::Config {
            message: format!("device token poll: {e}"),
        })?;

        if resp.status().as_u16() == 200 {
            let body_text = resp.text()?;
            return serde_json::from_str(&body_text).map_err(Into::into);
        }

        let status = resp.status().as_u16();
        if status != 403 && status != 404 {
            let body_text = resp.text().unwrap_or_else(|_| "unknown error".into());
            return Err(AgentError::Config {
                message: format!(
                    "device token poll failed ({status}): {}",
                    body_excerpt(&body_text)
                ),
            });
        }
    }
}

fn exchange_device_token(
    cfg: &OAuthConfig,
    device_token: &DeviceTokenResponse,
) -> Result<TokenResponse, AgentError> {
    let client = http_client(TOKEN_EXCHANGE_TIMEOUT)?;

    let form_body = format!(
        "grant_type=authorization_code\
         &code={}\
         &redirect_uri={}\
         &client_id={}\
         &code_verifier={}",
        urlenc(&device_token.authorization_code),
        urlenc(cfg.endpoints.redirect_uri),
        urlenc(cfg.client_id),
        urlenc(&device_token.code_verifier),
    );

    let request = isahc::Request::builder()
        .method("POST")
        .uri(cfg.endpoints.token_url)
        .header("content-type", "application/x-www-form-urlencoded")
        .body(form_body.into_bytes())?;

    let mut resp = client.send(request).map_err(|e| AgentError::Config {
        message: format!("token exchange: {e}"),
    })?;

    if resp.status().as_u16() != 200 {
        let body_text = resp.text().unwrap_or_else(|_| "unknown error".into());
        return Err(AgentError::Config {
            message: format!("token exchange failed: {}", body_excerpt(&body_text)),
        });
    }

    let body_text = resp.text()?;
    serde_json::from_str(&body_text).map_err(Into::into)
}

fn into_oauth_tokens(resp: TokenResponse, cfg: &OAuthConfig) -> OAuthTokens {
    let account_id = extract_account_id_from_tokens(&resp, cfg);
    let expires = now_millis() + resp.expires_in.unwrap_or(3600) * 1000;
    OAuthTokens {
        access: resp.access_token,
        refresh: resp.refresh_token,
        expires,
        account_id,
    }
}

pub fn refresh_tokens(cfg: &OAuthConfig, tokens: &OAuthTokens) -> Result<OAuthTokens, AgentError> {
    let expired = tokens.is_expired();
    debug!(provider = cfg.provider, expired, "refreshing OAuth tokens");

    let client = http_client(TOKEN_EXCHANGE_TIMEOUT)?;
    let form_body = format!(
        "grant_type=refresh_token&refresh_token={}&client_id={}",
        urlenc(&tokens.refresh),
        urlenc(cfg.client_id),
    );

    let request = isahc::Request::builder()
        .method("POST")
        .uri(cfg.endpoints.token_url)
        .header("content-type", "application/x-www-form-urlencoded")
        .body(form_body.into_bytes())?;

    let mut resp = client.send(request).map_err(|e| AgentError::Config {
        message: format!("{} token refresh: {e}", cfg.provider),
    })?;

    if resp.status().as_u16() != 200 {
        let body_text = resp.text().unwrap_or_else(|_| "unknown error".into());
        return Err(refresh_failure(cfg, resp.status().as_u16(), body_text));
    }

    let body_text = resp.text()?;
    let token_resp: TokenResponse = serde_json::from_str(&body_text)?;
    Ok(into_oauth_tokens(token_resp, cfg))
}

/// Build the per-request auth headers from `OAuthConfig`'s data fields. Both
/// the Bearer header and the optional JWT-derived account-id header land in
/// shared auth state, so engines read a single `Arc<Mutex<ResolvedAuth>>`
/// instead of re-reading tokens per request.
pub fn build_resolved(cfg: &OAuthConfig, tokens: &OAuthTokens) -> ResolvedAuth {
    let value = cfg.header_value_format.replace("{access}", &tokens.access);
    let mut headers = vec![(cfg.header_name.into(), value)];
    if let Some(header) = cfg.account_id_header_name
        && let Some(account_id) = &tokens.account_id
    {
        headers.push((header.into(), account_id.clone()));
    }
    ResolvedAuth {
        base_url: None,
        headers,
    }
}

pub fn is_oauth(cfg: &OAuthConfig, dir: &StateDir) -> bool {
    load_tokens(dir, cfg.provider).is_some()
}

/// Resolve auth from on-disk state without any HTTP. Expired OAuth tokens
/// are returned as-is; the centralized retry path in `ExternalProvider::
/// stream_message` refreshes them lazily on a 401. This keeps `resolve`
/// synchronous and off the executor.
pub fn resolve(cfg: &OAuthConfig, dir: &StateDir) -> Result<ResolvedAuth, AgentError> {
    if let Some(tokens) = load_tokens(dir, cfg.provider) {
        debug!(provider = cfg.provider, expired = tokens.is_expired(), "using OAuth authentication");
        return Ok(build_resolved(cfg, &tokens));
    }

    if let Some(env_var) = cfg.env_fallback
        && let Ok(key) = env::var(env_var)
    {
        debug!(provider = cfg.provider, "using API key from {env_var}");
        return Ok(ResolvedAuth {
            base_url: None,
            headers: vec![(cfg.header_name.into(), format!("Bearer {key}"))],
        });
    }

    if let Some(creds) = maki_storage::auth::load_provider_credentials(dir, cfg.provider) {
        debug!(provider = cfg.provider, "using saved API key");
        return Ok(ResolvedAuth {
            base_url: None,
            headers: vec![(cfg.header_name.into(), format!("Bearer {}", creds.api_key))],
        });
    }

    Err(AgentError::Config {
        message: format!(
            "not authenticated for '{}', run `maki auth login {}` or set {}",
            cfg.provider,
            cfg.provider,
            cfg.env_fallback.unwrap_or("the provider's API key"),
        ),
    })
}

pub fn login(cfg: &OAuthConfig, dir: &StateDir) -> Result<(), AgentError> {
    let device_token = poll_device_token(cfg).map_err(|e| {
        error!(provider = cfg.provider, error = %e, "device authorization failed");
        e
    })?;

    let token_resp = exchange_device_token(cfg, &device_token).map_err(|e| {
        error!(provider = cfg.provider, error = %e, "token exchange failed");
        e
    })?;

    let tokens = into_oauth_tokens(token_resp, cfg);
    save_tokens(dir, cfg.provider, &tokens)?;
    println!("Authenticated successfully.");
    Ok(())
}

pub fn logout(cfg: &OAuthConfig, dir: &StateDir) -> Result<(), AgentError> {
    if delete_tokens(dir, cfg.provider)? {
        println!("Logged out of {}.", cfg.provider);
    } else {
        println!("Not currently logged in to {}.", cfg.provider);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use test_case::test_case;

    #[test]
    fn extract_account_id_from_jwt() {
        use base64::Engine;
        use base64::engine::general_purpose::URL_SAFE_NO_PAD;

        let header = URL_SAFE_NO_PAD.encode(b"{}");
        let payload = URL_SAFE_NO_PAD
            .encode(serde_json::json!({"chatgpt_account_id": "acct_123"}).to_string().as_bytes());
        let token = format!("{header}.{payload}.sig");
        assert_eq!(extract_account_id(&token).as_deref(), Some("acct_123"));

        assert_eq!(extract_account_id("not.a.jwt"), None);
        assert_eq!(extract_account_id("invalid"), None);
    }

    #[test]
    fn oauth_config_lookup_openai() {
        let cfg = oauth_config("openai").expect("openai has OAuth config");
        assert_eq!(cfg.provider, "openai");
        assert!(cfg.account_id_from_jwt);
        assert_eq!(cfg.env_fallback, Some("OPENAI_API_KEY"));
    }

    #[test]
    fn oauth_config_lookup_unknown_returns_none() {
        assert!(oauth_config("anthropic").is_none());
        assert!(oauth_config("copilot").is_none());
    }

    #[test_case(400, r#"{"error":"invalid_grant"}"#, true  ; "invalid_grant_body")]
    #[test_case(401, "unauthorized",            true  ; "401_status")]
    #[test_case(500, "server error",            false ; "5xx_transient")]
    #[test_case(429, "slow down",               false ; "429_transient")]
    fn refresh_failure_classifies_revocation(status: u16, body: &str, expected_auth: bool) {
        let err = refresh_failure(&OPENAI_OAUTH, status, body.into());
        assert_eq!(err.is_auth_error(), expected_auth);
    }

    #[test_case("short body", "short body" ; "under_limit_passthrough")]
    #[test_case("", "" ; "empty")]
    fn body_excerpt_short_input_passes_through(input: &str, expected: &str) {
        assert_eq!(body_excerpt(input), expected);
    }

    #[test]
    fn body_excerpt_truncates_long_input() {
        let long = "x".repeat(MAX_ERROR_BODY * 3);
        let out = body_excerpt(&long);
        assert!(out.ends_with("...[truncated]"));
        assert!(out.chars().count() < long.chars().count());
    }

    #[test_case(r#"{"error":"invalid_grant"}"#, true  ; "invalid_grant")]
    #[test_case(r#"{"error":"expired_token"}"#, false ; "other_oauth_error")]
    #[test_case("not json",                   false ; "non_json")]
    #[test_case(r#"{}"#,                       false ; "no_error_field")]
    fn is_invalid_grant_detects_revocation(body: &str, expected: bool) {
        assert_eq!(is_invalid_grant(body), expected);
    }

    #[test]
    fn interval_clamped_to_max() {
        assert_eq!(5u64.clamp(1, MAX_POLL_INTERVAL_SECS), 5);
        assert_eq!(1u64.clamp(1, MAX_POLL_INTERVAL_SECS), 1);
        assert_eq!(
            u64::MAX.clamp(1, MAX_POLL_INTERVAL_SECS),
            MAX_POLL_INTERVAL_SECS
        );
    }
}
