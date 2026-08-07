local V4 = "deepseek-v4"

maki.api.register_provider({
  slug = "deepseek",
  display_name = "DeepSeek",
  family = "generic",
  features = "Thinking mode toggle (on/off), open-weight models",

  codec = "openai",
  base_url = "https://api.deepseek.com",
  auth = { kind = "api_key", env = "DEEPSEEK_API_KEY", login_url = "https://platform.deepseek.com/api_keys" },

  supports_thinking = true,
  accepts_arbitrary_models = false,
  context_window = 1000000,
  max_output_tokens = 384000,

  -- DeepSeek accepts only "max"; adaptive keeps the model's own default depth
  -- by sending no effort at all.
  effort = { supported = { "max" } },

  models = {
    {
      prefixes = { "deepseek-v4-flash" },
      tier = "medium",
      default = true,
      vision = false,
      max_output_tokens = 384000,
      context_window = 1000000,
      pricing = { input = 0.14, output = 0.28, cache_write = 0.0, cache_read = 0.0028 },
    },
    {
      prefixes = { "deepseek-v4-pro" },
      tier = "strong",
      default = true,
      vision = false,
      max_output_tokens = 384000,
      context_window = 1000000,
      pricing = { input = 0.435, output = 0.87, cache_write = 0.0, cache_read = 0.003625 },
    },
  },

  on_request = function(body, ctx)
    if not ctx.thinking.enabled then
      body:set("thinking", { type = "disabled" })
      return
    end
    body:set("thinking", { type = "enabled" })
    -- reasoning_effort is applied by the codec from the declared dialect.

    -- V4 in thinking mode wants `reasoning_content` on every assistant turn
    -- (missing = 400); R1 refuses it as input. Gate on the V4 substring, the
    -- same trick Vercel's AI SDK uses, and back-fill turns that have none.
    -- The API only checks the field exists.
    -- https://api-docs.deepseek.com/guides/thinking_mode
    if not ctx.model.id:find(V4, 1, true) then
      return
    end
    for i = 1, ctx.messages:len() do
      local m = ctx.messages:get(i)
      if m:get("role") == "assistant" and not m:has("reasoning_content") then
        m:set("reasoning_content", "")
      end
    end
  end,

  -- DeepSeek reports cache hits in `prompt_cache_hit_tokens` instead of
  -- `prompt_tokens_details.cached_tokens`. Keys are TokenUsage's serde names.
  on_usage = function(raw)
    local details = raw.prompt_tokens_details
    local cached = math.max((details and details.cached_tokens) or 0, raw.prompt_cache_hit_tokens or 0)
    return {
      input_tokens = math.max(0, (raw.prompt_tokens or 0) - cached),
      output_tokens = raw.completion_tokens or 0,
      cache_read_input_tokens = cached,
      cache_creation_input_tokens = 0,
    }
  end,

  usage = function(ctx)
    local res, err = ctx:request("/user/balance")
    if not res then
      return nil, err
    end
    local parsed, decode_err = maki.json.decode(res.body)
    if not parsed then
      return nil, decode_err
    end
    local symbols = { USD = "$", CNY = "¥" }
    local limits = maki.json.array({}) -- empty Lua table encodes as {}, not []
    for _, b in ipairs(parsed.balance_infos or {}) do
      local s = symbols[b.currency] or ""
      limits[#limits + 1] = {
        label = "Balance",
        detail = string.format(
          "total: %s%s, topped-up: %s%s, granted: %s%s",
          s,
          b.total_balance,
          s,
          b.topped_up_balance,
          s,
          b.granted_balance
        ),
      }
    end
    return { limits = limits }
  end,
})
