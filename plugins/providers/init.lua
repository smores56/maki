local PROVIDERS = { "deepseek" }

for _, name in ipairs(PROVIDERS) do
  local ok, err = maki.api.provider_scope(name, function()
    require(name)
  end)
  if not ok then
    maki.log.error("provider " .. name .. " failed to load: " .. tostring(err))
  end
end
