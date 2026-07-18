return function(U)
  local get_text = U.get_text
  local find_child = U.find_child
  local new_entry = U.new_entry
  local new_import_entry = U.new_import_entry
  local compact_ws = U.compact_ws
  local line_start = U.line_start
  local format_skeleton = U.format_skeleton
  local extract_fields_truncated = U.extract_fields_truncated
  local doc_comment_start_line = U.doc_comment_start_line
  local SECTION = U.SECTION
  local CHILD_BRIEF = U.CHILD_BRIEF

  local function is_doc_comment(node, source)
    return node:type() == "doc_comment" and get_text(node, source):sub(1, 3) == "///"
  end

  local ZIG_MODIFIER_KINDS = {
    pub = true,
    export = true,
    extern = true,
    inline = true,
    ["noinline"] = true,
    comptime = true,
    linksection = true,
    addrspace = true,
    align = true,
    packed = true,
    ["threadlocal"] = true,
    ["const"] = true,
    ["var"] = true,
  }

  local function is_zig_modifier(node)
    return ZIG_MODIFIER_KINDS[node:type()] and true or false
  end

  local function back_extend_doc(entry, node, source)
    local doc_start = doc_comment_start_line(node, source, is_doc_comment, is_zig_modifier)
    if doc_start and doc_start < entry.line_start then
      entry.line_start = doc_start
    end
  end

  local function get_first_identifier(node)
    for _, child in ipairs(node:children()) do
      if child:type() == "IDENTIFIER" then
        return child
      end
    end
    return nil
  end

  local function first_identifier_recursive(node)
    for _, child in ipairs(node:children()) do
      if child:type() == "IDENTIFIER" then
        return child
      end
    end
    for _, child in ipairs(node:children()) do
      local nested = first_identifier_recursive(child)
      if nested then
        return nested
      end
    end
    return nil
  end

  local function first_string_literal(node)
    for _, child in ipairs(node:children()) do
      local t = child:type()
      if t == "STRINGLITERALSINGLE" or t == "STRINGLITERAL" then
        return child
      end
    end
    for _, child in ipairs(node:children()) do
      local nested = first_string_literal(child)
      if nested then
        return nested
      end
    end
    return nil
  end

  local function import_paths_from_string(raw)
    local cleaned = raw:gsub('^"', ""):gsub('"$', "")
    local parts = {}
    for p in cleaned:gmatch("[^/]+") do
      parts[#parts + 1] = p
    end
    return parts
  end

  local function extract_import_from_suffix_expr(suffix_expr, source)
    local builtin = find_child(suffix_expr, "BUILTINIDENTIFIER")
    if not builtin or get_text(builtin, source) ~= "@import" then
      return nil
    end
    local args = find_child(suffix_expr, "FnCallArguments")
    if not args then
      return nil
    end
    local str_node = first_string_literal(args)
    if not str_node then
      return nil
    end
    return import_paths_from_string(get_text(str_node, source))
  end

  local function extract_import_paths(node, source)
    local err_union_expr = find_child(node, "ErrorUnionExpr")
    if not err_union_expr then
      return nil
    end
    local suffix_expr = find_child(err_union_expr, "SuffixExpr")
    if not suffix_expr then
      return nil
    end
    return extract_import_from_suffix_expr(suffix_expr, source)
  end

  local function extract_function(fn_proto, source)
    local name_node = get_first_identifier(fn_proto)
    if not name_node then
      return nil
    end
    local name = get_text(name_node, source)

    local params_node = find_child(fn_proto, "ParamDeclList")
    local params = params_node and compact_ws(get_text(params_node, source)) or "()"

    local ret = ""
    for _, child in ipairs(fn_proto:children()) do
      if child:type() == "ErrorUnionExpr" then
        local type_text = compact_ws(get_text(child, source))
        if type_text ~= "void" then
          ret = " " .. type_text
        end
        break
      end
    end

    return new_entry(SECTION.Function, fn_proto, name .. params .. ret)
  end

  local function extract_vardecl(node, source)
    local first_id = get_first_identifier(node)
    if not first_id then
      return nil
    end
    local name = get_text(first_id, source)

    local type_str = ""
    local saw_colon = false
    for _, child in ipairs(node:children()) do
      if child:type() == ":" then
        saw_colon = true
      elseif saw_colon and child:type() == "ErrorUnionExpr" then
        type_str = ": " .. compact_ws(get_text(child, source))
        break
      end
    end

    local is_const = false
    for _, child in ipairs(node:children()) do
      if child:type() == "const" then
        is_const = true
        break
      end
    end

    local prefix = is_const and "const " or "var "
    return new_entry(SECTION.Constant, node, prefix .. name .. type_str)
  end

  local function container_keyword(container_decl)
    local decl_type = find_child(container_decl, "ContainerDeclType")
    if not decl_type then
      return ""
    end
    for _, child in ipairs(decl_type:children()) do
      local t = child:type()
      if t == "struct" or t == "enum" or t == "union" or t == "opaque" then
        return t
      end
    end
    return ""
  end

  local function format_enum_variant_field(field, source)
    local id = first_identifier_recursive(field)
    return id and get_text(id, source) or "_"
  end

  local function format_typed_field(field, source)
    local fname_node = get_first_identifier(field)
    local fname = fname_node and get_text(fname_node, source) or "_"
    local ftype = ""
    local after_colon = false
    for _, child in ipairs(field:children()) do
      if after_colon and child:type() == "ErrorUnionExpr" then
        ftype = compact_ws(get_text(child, source))
        break
      end
      if child:type() == ":" then
        after_colon = true
      end
    end
    return ftype ~= "" and (fname .. ": " .. ftype) or fname
  end

  local function format_container_field(field, source)
    for _, child in ipairs(field:children()) do
      if child:type() == ":" then
        return format_typed_field(field, source)
      end
    end
    return format_enum_variant_field(field, source)
  end

  local function extract_container(container_decl, source, assigned_name)
    local keyword = container_keyword(container_decl)
    local name = assigned_name or ""
    local label = keyword .. (name ~= "" and (" " .. name) or "")
    local entry = new_entry(SECTION.Type, container_decl, label)

    local is_enum = keyword == "enum"
    local format_fn = is_enum and format_enum_variant_field or format_container_field
    entry.children = extract_fields_truncated(container_decl, source, "ContainerField", format_fn)
    if is_enum then
      entry.child_kind = CHILD_BRIEF
    end
    return entry
  end

  local function extract_error_set(error_set_decl, source, assigned_name)
    local name = assigned_name or ""
    local label = "error" .. (name ~= "" and (" " .. name) or "")
    local entry = new_entry(SECTION.Type, error_set_decl, label)
    entry.children = extract_fields_truncated(error_set_decl, source, "IDENTIFIER", function(f, src)
      return get_text(f, src)
    end)
    entry.child_kind = CHILD_BRIEF
    return entry
  end

  local function extract_vardecl_with_value(node, source)
    local first_id = get_first_identifier(node)
    local assigned_name = first_id and get_text(first_id, source) or nil

    local err_union_expr = find_child(node, "ErrorUnionExpr")
    if err_union_expr then
      local suffix_expr = find_child(err_union_expr, "SuffixExpr")
      if suffix_expr then
        local import_paths = extract_import_from_suffix_expr(suffix_expr, source)
        if import_paths and #import_paths > 0 then
          return { new_import_entry(node, { import_paths }) }
        end

        local container_decl = find_child(suffix_expr, "ContainerDecl")
        if container_decl then
          return { extract_container(container_decl, source, assigned_name) }
        end

        local error_set_decl = find_child(suffix_expr, "ErrorSetDecl")
        if error_set_decl then
          return { extract_error_set(error_set_decl, source, assigned_name) }
        end
      end
    end

    local e = extract_vardecl(node, source)
    return e and { e } or {}
  end

  local function extract_node_entries(node, source)
    local kind = node:type()

    if kind == "TestDecl" then
      return {}
    end

    if kind == "Decl" then
      local var_decl = find_child(node, "VarDecl")
      if var_decl then
        return extract_vardecl_with_value(var_decl, source)
      end

      if find_child(node, "usingnamespace") then
        local paths = extract_import_paths(node, source)
        return paths and #paths > 0 and { new_import_entry(node, { paths }) } or {}
      end

      local fn_proto = find_child(node, "FnProto")
      if fn_proto then
        local e = extract_function(fn_proto, source)
        return e and { e } or {}
      end
    end

    return {}
  end

  local function count_module_doc_lines(text)
    local n = 0
    for line in text:gmatch("([^\n]*)\n?") do
      if line:sub(1, 3) == "//!" then
        n = n + 1
      end
    end
    return n
  end

  local function detect_module_doc(root, source)
    local start_line
    local doc_lines = 0
    for _, child in ipairs(root:children()) do
      if child:type() == "container_doc_comment" then
        local text = get_text(child, source)
        if text:sub(1, 3) == "//!" then
          if not start_line then
            start_line = line_start(child)
          end
          doc_lines = doc_lines + count_module_doc_lines(text)
        end
      elseif not child:extra() then
        break
      end
    end
    if start_line then
      return { start_line, start_line + doc_lines - 1 }
    end
    return nil
  end

  return {
    import_separator = "/",

    extract = function(source, root)
      local entries = {}
      local test_lines = {}
      for _, child in ipairs(root:children()) do
        if child:type() == "TestDecl" then
          test_lines[#test_lines + 1] = line_start(child)
        else
          local child_entries = extract_node_entries(child, source)
          if child_entries[1] then
            back_extend_doc(child_entries[1], child, source)
          end
          for _, e in ipairs(child_entries) do
            entries[#entries + 1] = e
          end
        end
      end
      local module_doc = detect_module_doc(root, source)
      return format_skeleton(entries, test_lines, module_doc, "/")
    end,
  }
end
