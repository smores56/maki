-- DeepSeek provider manifest.

maki.provider.register({
  slug = "deepseek",
  display_name = "DeepSeek",
  family = "generic",
  supports_thinking = true,
  accepts_arbitrary_models = false,
  fallback_max_output = 384000,
  fallback_context_window = 1000000,
  qualities = {
    "Thinking mode toggle (on/off)",
    "open-weight models",
  },
  engine = maki.provider.openai_compat({
    base_url = "https://api.deepseek.com",
    api_key_env = "DEEPSEEK_API_KEY",
    max_tokens_field = "max_tokens",
    include_stream_usage = true,
    provider_name = "DeepSeek",
    thinking = "deepseek",
  }),
  auth = maki.auth.env_key({
    slug = "deepseek",
    env_var = "DEEPSEEK_API_KEY",
    login_url = "https://platform.deepseek.com/api_keys",
  }),
  models = {
    {
      id = "deepseek-v4-flash",
      tier = "medium",
      default = true,
      supports_vision = false,
      supports_thinking = true,
      pricing = {
        input = 0.14,
        output = 0.28,
        cache_write = 0.00,
        cache_read = 0.0028,
      },
      max_output_tokens = 384000,
      context_window = 1000000,
    },
    {
      id = "deepseek-v4-pro",
      tier = "strong",
      default = true,
      supports_vision = false,
      supports_thinking = true,
      pricing = {
        input = 0.435,
        output = 0.87,
        cache_write = 0.00,
        cache_read = 0.003625,
      },
      max_output_tokens = 384000,
      context_window = 1000000,
    },
  },
})
