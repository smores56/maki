use std::sync::{Arc, Mutex};

use flume::Sender;
use maki_storage::id::SessionRef;
use serde_json::Value;

use crate::model::Model;
use crate::provider::{BoxFuture, Provider};
use crate::{AgentError, Message, ProviderEvent, RequestOptions, StreamResponse, dialect};

use crate::providers::ResolvedAuth;
use crate::providers::oauth::ACCOUNT_ID_HEADER;
use crate::providers::openai_compat::{OpenAiCompatConfig, OpenAiCompatProvider};

static CONFIG: OpenAiCompatConfig = OpenAiCompatConfig {
    api_key_env: "OPENAI_API_KEY",
    base_url: "https://api.openai.com/v1",
    max_tokens_field: "max_completion_tokens",
    include_stream_usage: true,
    provider_name: "OpenAI",
};

// Codex models route to the ChatGPT Coding Plan backend, which requires a
// `chatgpt-account-id` header (derived from the OAuth token's JWT and written
// into shared auth by `OAuthAuthSource::resolve`). The router's base URL lives
// here, not in `OAuthConfig`, because endpoint selection is per-model routing,
// not an auth concern.
const CODING_PLAN_BASE_URL: &str = "https://chatgpt.com/backend-api/codex";

// Non-codex models OpenAI offers for subscription usage via the Coding Plan.
// Codex models are matched by their `-codex` substring in
// `coding_plan_context_window`, so they never need listing here.
pub(crate) const PLAN_MODELS: &[&str] = &[
    "gpt-5.6-luna",
    "gpt-5.6-terra",
    "gpt-5.6-sol",
    "gpt-5.5",
    "gpt-5.4",
    "gpt-5.4-mini",
    "gpt-5.2",
];

const CODEX_PLAN_CONTEXT_WINDOW: u32 = 272_000;
const GPT_5_6_PLAN_CONTEXT_WINDOW: u32 = 372_000;

fn is_codex_model(model_id: &str) -> bool {
    coding_plan_context_window(model_id).is_some()
}

// Codex models match by substring so future releases route without a registry
// edit; the named non-codex plans match exactly to avoid catching near-misses
// like `gpt-5.6-terra-preview`.
fn coding_plan_context_window(model_id: &str) -> Option<u32> {
    if model_id.contains("-codex") {
        return Some(CODEX_PLAN_CONTEXT_WINDOW);
    }
    if !PLAN_MODELS.contains(&model_id) {
        return None;
    }
    Some(if model_id.starts_with("gpt-5.6-") {
        GPT_5_6_PLAN_CONTEXT_WINDOW
    } else {
        CODEX_PLAN_CONTEXT_WINDOW
    })
}

pub struct OpenAi {
    compat: OpenAiCompatProvider,
    auth: Arc<Mutex<ResolvedAuth>>,
    system_prefix: Option<String>,
}

impl OpenAi {
    pub(crate) fn with_auth(
        auth: Arc<Mutex<ResolvedAuth>>,
        timeouts: crate::providers::Timeouts,
    ) -> Self {
        Self {
            compat: OpenAiCompatProvider::new(&CONFIG, timeouts),
            auth,
            system_prefix: None,
        }
    }

    pub(crate) fn with_system_prefix(mut self, prefix: Option<String>) -> Self {
        self.system_prefix = prefix;
        self
    }

    fn current_auth(&self) -> ResolvedAuth {
        self.auth.lock().unwrap().clone()
    }

    // OAuth is active when shared auth carries the account-id header that
    // `OAuthAuthSource::resolve` wrote from the JWT-derived account id. This
    // reads the resolved state already shared with the engine, no disk access.
    fn is_oauth(&self) -> bool {
        self.auth
            .lock()
            .unwrap()
            .headers
            .iter()
            .any(|(name, _)| name == ACCOUNT_ID_HEADER)
    }

    fn codex_auth(&self) -> ResolvedAuth {
        let mut auth = self.current_auth();
        auth.base_url = Some(
            if self.is_oauth() {
                CODING_PLAN_BASE_URL
            } else {
                CONFIG.base_url
            }
            .into(),
        );
        auth
    }
}

impl Provider for OpenAi {
    fn stream_message<'a>(
        &'a self,
        model: &'a Model,
        messages: &'a [Message],
        system: &'a str,
        tools: &'a Value,
        event_tx: &'a Sender<ProviderEvent>,
        opts: RequestOptions,
        _session_id: Option<&'a SessionRef>,
    ) -> BoxFuture<'a, Result<StreamResponse, AgentError>> {
        Box::pin(async move {
            let mut buf = String::new();
            let system = super::super::with_prefix(&self.system_prefix, system, &mut buf);

            if is_codex_model(&model.id) {
                let body = super::responses::build_body(model, messages, system, tools);
                let stream_timeout = self.compat.stream_timeout();
                let codex_auth = self.codex_auth();
                return super::responses::do_stream(
                    self.compat.client(),
                    model,
                    &body,
                    event_tx,
                    &codex_auth,
                    stream_timeout,
                )
                .await;
            }

            let mut body = self.compat.build_body(model, messages, system, tools);
            opts.thinking
                .apply_reasoning_effort(&mut body, &dialect::STANDARD, model);
            let auth = self.current_auth();
            self.compat
                .do_stream(model, &[], &body, event_tx, &auth)
                .await
        })
    }

    fn list_models(&self) -> BoxFuture<'_, Result<Vec<crate::model::ModelInfo>, AgentError>> {
        Box::pin(async {
            if self.is_oauth() {
                let models = super::models()
                    .iter()
                    .flat_map(|e| e.prefixes.iter())
                    .filter(|id| is_codex_model(id))
                    .map(|&s| crate::model::ModelInfo::id_only(s.to_string()))
                    .collect();
                return Ok(models);
            }
            let auth = self.current_auth();
            self.compat.do_list_models(&auth).await
        })
    }

    fn adjust_model(&self, model: &mut Model) {
        if self.is_oauth()
            && let Some(context_window) = coding_plan_context_window(&model.id)
        {
            model.context_window = model.context_window.min(context_window);
        }
    }
}

#[cfg(test)]
mod tests {
    use test_case::test_case;

    use super::*;

    #[test_case("gpt-5.6-luna")]
    #[test_case("gpt-5.6-terra")]
    #[test_case("gpt-5.6-sol")]
    fn gpt_5_6_models_use_coding_plan(model_id: &str) {
        assert!(is_codex_model(model_id));
    }

    #[test_case("gpt-5.6-luna", Some(372_000))]
    #[test_case("gpt-5.6-terra", Some(372_000))]
    #[test_case("gpt-5.6-sol", Some(372_000))]
    #[test_case("gpt-5.5", Some(272_000))]
    #[test_case("gpt-5.3-codex", Some(272_000))]
    #[test_case("gpt-5.7-codex", Some(272_000) ; "unlisted codex model still routes")]
    #[test_case("gpt-5.6-terra-preview", None ; "non-codex near-match is rejected")]
    #[test_case("gpt-5.4-nano", None)]
    fn coding_plan_context_window_resolves_plan_models(model_id: &str, expected: Option<u32>) {
        assert_eq!(coding_plan_context_window(model_id), expected);
    }

    fn make_openai(auth: ResolvedAuth) -> OpenAi {
        OpenAi::with_auth(
            Arc::new(Mutex::new(auth)),
            crate::providers::Timeouts::default(),
        )
    }

    #[test]
    fn is_oauth_false_for_api_key_only_auth() {
        let provider = make_openai(ResolvedAuth::bearer("sk-test"));
        assert!(!provider.is_oauth());
    }

    #[test]
    fn is_oauth_true_when_account_id_header_present() {
        let mut headers = vec![("authorization".into(), "Bearer tok".into())];
        headers.push((ACCOUNT_ID_HEADER.into(), "acct_123".into()));
        let provider = make_openai(ResolvedAuth {
            base_url: None,
            headers,
        });
        assert!(provider.is_oauth());
    }

    #[test]
    fn codex_auth_uses_coding_plan_base_url_when_oauth() {
        let mut headers = vec![("authorization".into(), "Bearer tok".into())];
        headers.push((ACCOUNT_ID_HEADER.into(), "acct_123".into()));
        let provider = make_openai(ResolvedAuth {
            base_url: None,
            headers,
        });
        let auth = provider.codex_auth();
        assert_eq!(auth.base_url.as_deref(), Some(CODING_PLAN_BASE_URL));
        assert!(
            auth.headers
                .iter()
                .any(|(name, value)| name == ACCOUNT_ID_HEADER && value == "acct_123")
        );
    }

    #[test]
    fn codex_auth_uses_standard_base_url_when_api_key() {
        let provider = make_openai(ResolvedAuth::bearer("sk-test"));
        let auth = provider.codex_auth();
        assert_eq!(auth.base_url.as_deref(), Some(CONFIG.base_url));
    }

    #[test]
    fn adjust_model_caps_context_window_for_codex_when_oauth() {
        let mut headers = vec![("authorization".into(), "Bearer tok".into())];
        headers.push((ACCOUNT_ID_HEADER.into(), "acct_123".into()));
        let provider = make_openai(ResolvedAuth {
            base_url: None,
            headers,
        });
        let mut model = Model::from_spec("openai/gpt-5.7-codex").unwrap();
        model.context_window = 1_000_000;
        provider.adjust_model(&mut model);
        assert_eq!(model.context_window, CODEX_PLAN_CONTEXT_WINDOW);
    }

    #[test]
    fn adjust_model_does_not_cap_when_api_key() {
        let provider = make_openai(ResolvedAuth::bearer("sk-test"));
        let mut model = Model::from_spec("openai/gpt-5.7-codex").unwrap();
        model.context_window = 1_000_000;
        provider.adjust_model(&mut model);
        assert_eq!(model.context_window, 1_000_000);
    }

    #[test]
    fn list_models_returns_codex_only_when_oauth() {
        let mut headers = vec![("authorization".into(), "Bearer tok".into())];
        headers.push((ACCOUNT_ID_HEADER.into(), "acct_123".into()));
        let provider = make_openai(ResolvedAuth {
            base_url: None,
            headers,
        });
        smol::block_on(async {
            let models = provider.list_models().await.unwrap();
            assert!(models.iter().all(|m| is_codex_model(&m.id)));
            assert!(!models.is_empty());
        });
    }
}
