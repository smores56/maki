-- Tree-sitter / Starlark AST primitives for the Bazel indexer.
--
-- This module is private to analyze.lua: extractors (build, module, bzl) must
-- not require it. Anything an extractor needs is exposed through analyze.lua's
-- record API instead, so the AST/tree-sitter shape stays a single-layer concern.
--
-- The arborium starlark grammar wraps most expressions in two extra layers
-- before the real node:
--   expression_statement > expression > primary_expression > <real node>
-- Calls, assignments, and bare literals all arrive this way. The unwrap helpers
-- here collapse those wrappers so callers see `call` / `assignment` / `string`
-- / `list` / ... directly.

return function(U)
  local get_text = U.get_text
  local compact_ws = U.compact_ws
  local line_start = U.line_start
  local line_end = U.line_end

  local STRING_PREFIX = {
    B = true,
    F = true,
    R = true,
    U = true,
    b = true,
    f = true,
    r = true,
    u = true,
  }

  local M = {}

  -- Descend one layer of the grammar's expression wrappings:
  --   expression_statement  -> first named child
  --   expression            -> primary_expression child
  --   primary_expression    -> first named child (the real node)
  -- Returns `node` unchanged if it is none of these.
  local function unwrap_once(node)
    local t = node:type()

    if t == "expression_statement" then
      return node:named_children()[1]
    end

    if t == "expression" then
      return node:named_children()[1]
    end

    if t == "primary_expression" then
      return node:named_children()[1]
    end

    return node
  end

  -- Fully unwrap expression/primary_expression layers, stopping at the first
  -- node that is neither.
  local function unwrap_all(node)
    while node ~= nil do
      local t = node:type()
      if t == "expression_statement" then
        node = node:named_children()[1]
      elseif t == "expression" or t == "primary_expression" then
        node = unwrap_once(node)
      else
        break
      end
    end

    return node
  end

  -- Strip expression_statement / expression / primary_expression wrappers.
  function M.unwrap(node)
    return unwrap_all(node)
  end

  -- Return `node` if it is `target_type`, otherwise unwrap the grammar's
  -- expression wrappers and return the first node of type `target_type`.
  function M.unwrap_to(node, target_type)
    if node:type() == target_type then
      return node
    end

    local cur = node
    while cur ~= nil do
      local t = cur:type()
      if t == target_type then
        return cur
      end
      if t ~= "expression_statement" and t ~= "expression" and t ~= "primary_expression" then
        break
      end
      cur = unwrap_once(cur)
    end

    return nil
  end

  function M.assignment_name(node, source)
    local left = node:field("left")[1]

    if not left then
      return nil
    end

    return M.compact_ws_node(left, source)
  end

  function M.assignment_value(node)
    return node:field("right")[1]
  end

  -- Strip whitespace and backslash line-continuation so multi-line attribute
  -- access (`pip.\\\n  parse`) collapses to its single-line form (`pip.parse`).
  -- Identifiers and dotted attribute paths cannot contain whitespace or
  -- backslashes, so this is safe.
  --
  -- Note: arborium starlark only parses line continuation when it sits AFTER
  -- the dot (`pip.\\\n  parse`). Continuation between the identifier and the
  -- dot is not parsed as an attribute call, so no fix here can recover it.
  function M.call_target(call, source)
    local fn_node = call:field("function")[1]

    if not fn_node then
      return nil
    end

    -- The callee is wrapped: `call.function` is a `primary_expression` (or the
    -- bare identifier). Unwrap to render only the callee's text, not the
    -- expression wrappers' (which are empty anyway).
    fn_node = unwrap_all(fn_node)

    return (get_text(fn_node, source):gsub("[%s\\]+", ""))
  end

  function M.call_args(call)
    local args_node = call:field("arguments")[1]

    if not args_node then
      return {}
    end

    local args = {}

    for _, child in ipairs(args_node:named_children()) do
      if child:type() ~= "comment" then
        args[#args + 1] = child
      end
    end

    return args
  end

  -- Render the value of a `keyword_argument` node for use as a positional
  -- entry. Positional arguments inside Bazel calls arrive as `expression`
  -- wrappers around the real value; unwrap them so value-record extraction
  -- (new_value_record) sees the concrete node kind.
  function M.call_arg(node)
    return unwrap_all(node)
  end

  -- Caller is responsible for verifying that `node` is a keyword_argument.
  function M.keyword_name(node, source)
    local name_node = node:field("name")[1]

    if not name_node then
      return nil
    end

    return get_text(name_node, source)
  end

  -- Caller is responsible for verifying that `node` is a keyword_argument.
  function M.keyword_value(node)
    return node:field("value")[1]
  end

  function M.dictionary_splat_keys(node, source)
    if node:type() ~= "dictionary_splat" then
      return {}
    end

    local dict = unwrap_all(node:named_children()[1])
    if not dict or dict:type() ~= "dictionary" then
      return {}
    end

    local keys = {}
    for _, child in ipairs(dict:named_children()) do
      if child:type() == "pair" then
        local key = child:field("key")[1]
        local key_text = key and M.raw_string_contents(unwrap_all(key), source)
        if key_text then
          keys[#keys + 1] = key_text
        end
      end
    end

    return keys
  end

  -- Source text of `node` with whitespace runs collapsed to single spaces.
  -- For label/name extraction and tightly-rendered call args; do not use for
  -- doc strings or binding values where indentation matters.
  function M.compact_ws_node(node, source)
    return compact_ws(get_text(node, source))
  end

  -- Return content bounds for a Starlark string literal, excluding prefixes
  -- and quote delimiters. Also reports whether the literal is triple-quoted.
  local function string_content_bounds(text)
    local quote_start = 1

    while quote_start <= #text and STRING_PREFIX[text:sub(quote_start, quote_start)] do
      quote_start = quote_start + 1
    end

    local quote = text:sub(quote_start, quote_start)

    if quote ~= '"' and quote ~= "'" then
      return nil
    end

    local triple_quote = quote:rep(3)
    local is_triple_quoted = text:sub(quote_start, quote_start + 2) == triple_quote

    if is_triple_quoted then
      if #text < quote_start + 5 or text:sub(-3) ~= triple_quote then
        return nil
      end

      return quote_start + 3, #text - 3, true
    end

    if #text < quote_start + 1 or text:sub(-1) ~= quote then
      return nil
    end

    return quote_start + 1, #text - 1, false
  end

  -- Strip quotes from a string literal node. Handles single, double, and
  -- triple-quoted strings. Returns nil for non-string nodes.
  -- Note: uses compact_ws_node (collapses whitespace), which is intentional
  -- for names, labels, and module paths. Do NOT use this for doc string bodies
  -- where whitespace matters.
  function M.raw_string_contents(node, source)
    node = unwrap_all(node)

    if node:type() ~= "string" then
      return nil
    end

    local text = M.compact_ws_node(node, source)
    local content_start, content_end = string_content_bounds(text)

    if not content_start then
      return nil
    end

    return text:sub(content_start, content_end)
  end

  -- Render a `list` node tightly for the index. Walks the list's elements
  -- directly so multi-line / trailing-comma forms render the same as their
  -- single-line equivalents, and recurses into nested lists.
  function M.compact_list(node, source)
    local items = {}

    for _, child in ipairs(node:named_children()) do
      local kind = child:type()
      if kind ~= "comment" then
        if kind == "list" then
          items[#items + 1] = M.compact_list(child, source)
        else
          items[#items + 1] = M.compact_ws_node(child, source)
        end
      end
    end

    return "[" .. table.concat(items, ", ") .. "]"
  end

  local function compact_default_parameter(node, source)
    local name = node:field("name")[1]
    local value = node:field("value")[1]

    if not name or not value then
      return nil
    end

    local type_node = node:field("type")[1]
    local type_text = type_node and ": " .. M.compact_ws_node(type_node, source) or ""
    local value_text = M.compact_ws_node(unwrap_all(value), source)

    return M.compact_ws_node(name, source) .. type_text .. "=" .. value_text
  end

  local function compact_parameter(node, source)
    if node:type() == "parameter" then
      node = node:named_children()[1] or node
    end

    local kind = node:type()

    if kind == "default_parameter" or kind == "typed_default_parameter" then
      local rendered = compact_default_parameter(node, source)
      if rendered then
        return rendered
      end
    end

    return M.compact_ws_node(node, source)
  end

  -- Render a function's `parameters` node tightly for the index.
  function M.compact_params(node, source)
    local params = {}

    for _, child in ipairs(node:named_children()) do
      if child:type() ~= "comment" then
        params[#params + 1] = compact_parameter(child, source)
      end
    end

    return "(" .. table.concat(params, ", ") .. ")"
  end

  function M.node_lines(node)
    return line_start(node), line_end(node)
  end

  -- A module doc string is a top-level triple-quoted string. In arborium the
  -- layers are expression_statement > expression > primary_expression > string.
  function M.is_doc_string(node, source)
    local str = M.unwrap_to(node, "string")

    if not str then
      return false
    end

    local text = get_text(str, source)
    local _, _, is_triple_quoted = string_content_bounds(text)

    return is_triple_quoted == true
  end

  -- Pull a leading module doc string out of the top-level nodes. Returns:
  --   doc   {start, end} line range of the doc string, or nil
  --   rest  remaining nodes
  function M.split_preamble(root, source)
    local nodes = root:named_children()
    local doc = nil
    local index = 1

    while index <= #nodes and nodes[index]:type() == "comment" do
      index = index + 1
    end

    if index <= #nodes and M.is_doc_string(nodes[index], source) then
      local s, e = M.node_lines(nodes[index])
      doc = { s, e }
      index = index + 1
    end

    local rest = {}
    for i = index, #nodes do
      rest[#rest + 1] = nodes[i]
    end

    return doc, rest
  end

  -- Walk a load_statement's named children. Returns the module string contents
  -- (or nil) and a list of imported-name records (alias|nil, name) describing
  -- positional and aliased imports in source order. The first string child is
  -- the module path; aliased_load nodes contribute (alias, name) pairs; plain
  -- string children are positional imports (alias = nil).
  function M.load_pieces(load_stmt, source)
    local module = nil
    local names = {}

    for _, child in ipairs(load_stmt:named_children()) do
      local kind = child:type()

      if kind == "string" then
        if not module then
          module = M.raw_string_contents(child, source)
        else
          names[#names + 1] = { alias = nil, name = M.raw_string_contents(child, source) }
        end
      elseif kind == "aliased_load" then
        local alias_node = child:field("alias")[1]
        local name_node = child:field("name")[1]
        if alias_node and name_node then
          names[#names + 1] = {
            alias = get_text(alias_node, source),
            name = M.raw_string_contents(name_node, source),
          }
        end
      end
    end

    return module, names
  end

  M.get_text = get_text

  return M
end
