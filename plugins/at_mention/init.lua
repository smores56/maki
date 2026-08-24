local TRIGGER = "@"
local ABS_ANCHOR = "/"
local HOME_ANCHOR = "~"
local CWD_ANCHOR = "./"
local PARENT_ANCHOR = "../"

local function anchored_root(query, cwd)
  if query:sub(1, 1) == ABS_ANCHOR then
    return ABS_ANCHOR, ABS_ANCHOR, query:sub(2)
  end
  if query:sub(1, 1) == HOME_ANCHOR then
    local home = maki.uv.os_homedir()
    if not home then
      return nil
    end
    return HOME_ANCHOR, home, query:sub(2):gsub("^/+", "")
  end
  local anchor = query:sub(1, 3) == PARENT_ANCHOR and PARENT_ANCHOR
    or (query:sub(1, 2) == CWD_ANCHOR and CWD_ANCHOR or "")
  local root = cwd
  local rest = query
  while rest == "." or rest == ".." or rest:sub(1, 2) == CWD_ANCHOR or rest:sub(1, 3) == PARENT_ANCHOR do
    if rest == ".." or rest:sub(1, 3) == PARENT_ANCHOR then
      root = maki.fs.normalize(root .. "/..")
      rest = rest == ".." and "" or rest:sub(4)
    else
      rest = rest == "." and "" or rest:sub(3)
    end
  end
  return anchor, root, rest
end

local function format_label(entry, anchor, cwd, home)
  local label
  if anchor == ABS_ANCHOR then
    label = entry.path
  elseif anchor == HOME_ANCHOR then
    local ok, rel = pcall(maki.fs.relpath, home, entry.path)
    label = ok and rel ~= "" and (HOME_ANCHOR .. "/" .. rel) or entry.path
  else
    local ok, rel = pcall(maki.fs.relpath, cwd, entry.path)
    if not ok then
      label = entry.path
    elseif rel == "" then
      label = "."
    elseif anchor == CWD_ANCHOR and rel:sub(1, 3) ~= PARENT_ANCHOR then
      label = CWD_ANCHOR .. rel
    else
      label = rel
    end
  end
  if entry.kind == "dir" then
    label = label .. "/"
  end
  return label
end

maki.api.register_completion({
  trigger = TRIGGER,
  provider = function(query, ctx)
    local anchor, root, rest = anchored_root(query, ctx.cwd)
    if not root then
      return {}
    end
    local files, err = maki.fs.fuzzy_files(rest, { cwd = root })
    if not files then
      maki.log.warn("at_mention: " .. err)
      return {}
    end
    local candidates = {}
    for i, entry in ipairs(files) do
      local label = format_label(entry, anchor, ctx.cwd, root)
      candidates[i] = { label = label, insert = TRIGGER .. label, kind = entry.kind }
    end
    return candidates
  end,
})
