return function(U)
  local get_text = U.get_text
  local find_child = U.find_child
  local compact_ws = U.compact_ws
  local format_range = U.format_range
  local line_start = U.line_start
  local line_end = U.line_end
  local new_entry = U.new_entry
  local ranged = U.ranged
  local SECTION = U.SECTION

  local SIG_KINDS = {
    function_signature = true,
    getter_signature = true,
    setter_signature = true,
    constructor_signature = true,
    constant_constructor_signature = true,
    factory_constructor_signature = true,
    redirecting_factory_constructor_signature = true,
    operator_signature = true,
  }

  local CONSTRUCTOR_KINDS = {
    constructor_signature = true,
    constant_constructor_signature = true,
    factory_constructor_signature = true,
    redirecting_factory_constructor_signature = true,
  }

  local RETURN_TYPE_KINDS = {
    type_identifier = true,
    void_type = true,
    function_type = true,
    record_type = true,
    named_type = true,
  }

  local function type_params(node, source)
    local tp_node = find_child(node, "type_parameters")
    return tp_node and get_text(tp_node, source) or ""
  end

  local function qualified_name(sig_node, source)
    local parts = {}
    for _, child in ipairs(sig_node:children()) do
      if child:type() == "identifier" then
        parts[#parts + 1] = get_text(child, source)
      end
    end
    if #parts == 0 then
      return nil
    end
    return table.concat(parts, ".")
  end

  local function return_type_text(sig_node, source, name_index)
    local parts = {}
    for i = 1, name_index - 1 do
      local child = sig_node:child(i - 1)
      if not child or not child:named() then
        break
      end
      parts[#parts + 1] = get_text(child, source)
    end
    if #parts == 0 then
      return nil
    end
    return table.concat(parts, "")
  end

  local function name_index_of(sig_node)
    for i, child in ipairs(sig_node:children()) do
      if child:type() == "identifier" then
        return i
      end
    end
    return nil
  end

  local function signature_text(sig_node, source)
    local kind = sig_node:type()
    if kind == "operator_signature" then
      return get_text(sig_node, source)
    end

    local name = qualified_name(sig_node, source)
    if not name then
      return nil
    end

    if kind == "getter_signature" then
      local idx = name_index_of(sig_node)
      local ret = idx and return_type_text(sig_node, source, idx)
      local ret_s = ret and (" " .. ret) or ""
      return compact_ws("get " .. name .. ret_s)
    end

    local params_node = find_child(sig_node, "formal_parameter_list")
    local params = params_node and get_text(params_node, source) or ""

    if kind == "setter_signature" then
      return compact_ws("set " .. name .. params)
    end

    if CONSTRUCTOR_KINDS[kind] then
      return compact_ws(name .. params)
    end

    local tp = type_params(sig_node, source)
    local idx = name_index_of(sig_node)
    local ret = idx and return_type_text(sig_node, source, idx)
    local ret_s = ret and (" " .. ret) or ""
    return compact_ws(name .. tp .. params .. ret_s)
  end

  local function inner_signature(node)
    for _, child in ipairs(node:children()) do
      local ckind = child:type()
      if SIG_KINDS[ckind] then
        return child
      end
    end
    return nil
  end

  local FIELD_LIST_KINDS = {
    initialized_identifier_list = "initialized_identifier",
    identifier_list = "identifier",
    static_final_declaration_list = "static_final_declaration",
  }

  local function field_name(id_node, source)
    if id_node:type() == "identifier" then
      return get_text(id_node, source)
    end
    return get_text(find_child(id_node, "identifier"), source)
  end

  local function add_field(out, id_node, source, type_node, range_node)
    local name = field_name(id_node, source)
    if not name then
      return
    end
    local text = type_node and (name .. " " .. get_text(type_node, source)) or name
    local lr = format_range(line_start(range_node), line_end(range_node))
    out[#out + 1] = ranged(text, lr)
  end

  local function type_node_of(node)
    for _, child in ipairs(node:children()) do
      if RETURN_TYPE_KINDS[child:type()] then
        return child
      end
    end
    return nil
  end

  local function extract_field_like(node, source, out)
    local type_node = type_node_of(node)
    for _, child in ipairs(node:children()) do
      local ckind = child:type()
      local list_kind = FIELD_LIST_KINDS[ckind]
      if list_kind then
        for _, id in ipairs(child:children()) do
          if id:type() == list_kind then
            add_field(out, id, source, type_node, id)
          end
        end
      elseif ckind == "initialized_identifier" or ckind == "static_final_declaration" or ckind == "identifier" then
        add_field(out, child, source, type_node, child)
      end
    end
  end

  local function extract_member(member, source)
    local kind = member:type()
    if kind == "method_signature" or kind == "declaration" then
      local sig = inner_signature(member)
      if sig then
        local text = signature_text(sig, source)
        if text then
          local lr = format_range(line_start(member), line_end(member))
          return { ranged(text, lr) }
        end
      end
      if kind == "declaration" then
        local fields = {}
        extract_field_like(member, source, fields)
        return fields
      end
    end
    return {}
  end

  local function extract_body_members(body_node, source)
    local members = {}
    for _, child in ipairs(body_node:children()) do
      for _, m in ipairs(extract_member(child, source)) do
        members[#members + 1] = m
      end
    end
    return members
  end

  local function class_name(node, source)
    for _, child in ipairs(node:children()) do
      if child:type() == "identifier" then
        return get_text(child, source)
      end
    end
    return nil
  end

  local function extract_classlike(node, source, prefix)
    local name = class_name(node, source)
    if not name then
      return nil
    end
    local tp = type_params(node, source)
    local body_node = find_child(node, "class_body")
    local entry = new_entry(SECTION.Class, node, prefix .. " " .. name .. tp)
    if body_node then
      entry.children = extract_body_members(body_node, source)
    end
    return entry
  end

  local function extract_signature_entry(node, source, section)
    local sig = inner_signature(node) or node
    if not SIG_KINDS[sig:type()] then
      return nil
    end
    local text = signature_text(sig, source)
    if not text then
      return nil
    end
    return new_entry(section, node, text)
  end

  return {
    import_separator = ".",
    is_doc_comment = function(node, source)
      return node:type() == "comment" and get_text(node, source):sub(1, 3) == "///"
    end,

    extract_nodes = function(node, source, _attrs)
      local kind = node:type()

      if kind == "class_definition" then
        local e = extract_classlike(node, source, "class")
        return e and { e } or {}
      elseif kind == "mixin_declaration" then
        local e = extract_classlike(node, source, "mixin")
        return e and { e } or {}
      elseif kind == "extension_type_declaration" then
        local e = extract_classlike(node, source, "extension type")
        return e and { e } or {}
      elseif kind == "extension_declaration" then
        local body_node = find_child(node, "class_body") or node:field("body")[1]
        if body_node then
          local name = class_name(node, source) or "_"
          local entry = new_entry(SECTION.Type, node, "extension " .. name)
          entry.children = extract_body_members(body_node, source)
          return { entry }
        end
        return {}
      elseif kind == "enum_declaration" then
        local name = class_name(node, source)
        if not name then
          return {}
        end
        local tp = type_params(node, source)
        return { new_entry(SECTION.Type, node, "enum " .. name .. tp) }
      elseif SIG_KINDS[kind] then
        local e = extract_signature_entry(node, source, SECTION.Function)
        return e and { e } or {}
      end

      return {}
    end,
  }
end
