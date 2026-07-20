use crate::providers::ResolvedAuth;

pub use crate::providers::oauth::{OPENAI_OAUTH, is_oauth};

pub(crate) const CODING_PLAN_BASE_URL: &str = "https://chatgpt.com/backend-api/codex";

/// Subscription-tier auth for `*-codex` models: Bearer token plus the
/// `chatgpt-account-id` header OpenAI requires on the Coding Plan router. The
/// JWT account id was already extracted into the stored tokens by `oauth::login`.
pub(crate) fn build_coding_plan_resolved(tokens: &maki_storage::auth::OAuthTokens) -> ResolvedAuth {
    let mut headers = vec![(
        "authorization".into(),
        format!("Bearer {}", tokens.access),
    )];
    if let Some(account_id) = &tokens.account_id {
        headers.push(("chatgpt-account-id".into(), account_id.clone()));
    }
    ResolvedAuth {
        base_url: Some(CODING_PLAN_BASE_URL.into()),
        headers,
    }
}
