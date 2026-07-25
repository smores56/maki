use crate::model::ModelEntry;

inventory::submit!(maki_config::providers::BuiltInProvider {
    slug: "llama-cpp",
    display_name: super::local::LLAMACPP_DISPLAY_NAME,
    protocol: maki_config::providers::Protocol::Openai,
    default_base_url: super::local::LLAMACPP_DEFAULT_HOST,
    default_api_key_env: "LLAMA_CPP_API_KEY",
    default_model: super::local::LLAMACPP_DEFAULT_MODEL,
    plans: None,
    login_url: None,
    needs_url: true,
});

pub(crate) const fn models() -> &'static [ModelEntry] {
    &[]
}
