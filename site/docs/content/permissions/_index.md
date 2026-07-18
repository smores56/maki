+++
title = "Permissions"
weight = 4
[extra]
group = "Reference"
+++

# Permissions

Maki uses a permission system to decide what each tool is allowed to do and when to ask you first.

Rules come from three layers, combined for resolution:

1. **Session rules**, set during the current session (in-memory only)
2. **Config rules**, loaded from TOML permission files
3. **Builtin rules**, the hardcoded defaults

Any matching deny blocks the tool. No exceptions.

## Check Flow

For every tool call, Maki resolves permission like this:

1. **Deny wins**: if any rule from any layer matches the tool and scope with a deny, the call is blocked immediately.
2. If **YOLO** is active and no deny matched, allowed.
3. **Plan file auto-allow**: file write tools targeting the plan file path are allowed automatically (only if no deny rule matched in step 1).
4. Fall back to `default` (per-tool, then global). Built-in default is `"prompt"`.

## Builtin Defaults

| Tool | Scope | Notes |
|------|-------|-------|
| `write` | Project directory | Files outside require permission |
| `edit` | Project directory | Files outside require permission |
| `multiedit` | Project directory | Files outside require permission |
| `task` | `*` (all) | Subagent spawning always allowed |

These tools require explicit permission:

- `bash` - Shell commands
- `websearch` - Web search queries
- `webfetch` - URL fetching

Container tools like `batch` and `code_execution` prompt for each inner tool individually.

## TOML Configuration

There are two permission files:

- **Global**: `~/.config/maki/permissions.toml`
- **Project**: `.maki/permissions.toml` (takes precedence over global)

```toml
default = "deny"

[bash]
allow = [
    "cargo *",
    "git *",
]
deny = [
    "rm -rf *",
    "sudo *",
]

[read]
default = "allow"

[mcp.deepwiki]
allow = ["search", "fetch"]

[mcp.github]
deny = ["admin_delete"]
```

Each tool gets its own section with `allow` and `deny` arrays. Values are glob-like scope patterns.

> **Note:** In MCP server sections (`[mcp.*]`), the boolean forms `allow = true` and `deny = true` are deprecated and ignored. Use `default = "allow"` or `default = "deny"` instead. For native tool sections (e.g. `[bash]`), `allow = true` still works.

### The `default` key

Controls what happens when no allow or deny rule matches. Can be `"prompt"` (built-in default), `"deny"`, or `"allow"`. Set it globally or per-tool:

```toml
default = "deny"

[bash]
default = "prompt"
allow = ["cargo *"]
```

Here everything is denied by default, except `bash` which still prompts, and `cargo *` commands which are allowed.

Note: `default = "allow"` only works in the global file. Projects cannot grant themselves full access.

## Scope Patterns

| Pattern | Matches |
|---------|--------|
| `*` | Any single value |
| `**` | Everything |
| `prefix*` | Values starting with prefix |
| `dir/**` | `dir` itself or anything under it |
| `exact` | Exact match only |

## MCP Tool Permissions

MCP tools use natural TOML nesting. Server names are table keys under `[mcp]`, tool names are array values:

```toml
[mcp.deepwiki]
allow = ["search", "fetch"]    # allow these tools

[mcp.github]
deny = ["admin_delete"]         # deny this tool

[mcp.lean-lsp]
default = "allow"               # allow all tools on this server
```

Tool names must match `^[a-zA-Z0-9_-]{1,64}$` (no dots, max 64 chars). Server names cannot contain dots.

## Permission Prompts

When a tool needs permission, Maki asks you. Here are the keys:

| Key | Action |
|-----|--------|
| `y` | Allow once |
| `s` | Allow for this session |
| `a` | Always allow (project, saved to `.maki/permissions.toml`) |
| `A` | Always allow (global, saved to `~/.config/maki/permissions.toml`) |
| `n` | Deny once |
| `d` | Deny always (project) |
| `D` | Deny always (global) |

### Scope Generalization

When you pick "always allow", the saved scope is generalized so it stays useful beyond just that one command:

- **bash**: `cargo test --all` becomes `cargo *`
- **write/edit/multiedit**: `/path/to/file.rs` becomes `/path/to/**`
- **MCP tools**: always `*` (per-tool, so allowing `deepwiki.search` won't cover `deepwiki.fetch`)
- **webfetch/websearch**: always `*`

For MCP tools, both allow and deny decisions generalize to `*` (the entire tool). This is because MCP tool inputs are opaque JSON with no meaningful scope pattern to differentiate. Denying a single MCP invocation denies the tool entirely until you revoke the rule.

## YOLO Mode

To skip all prompts, toggle YOLO with the `/yolo` command, or run with `--yolo`. Explicit deny rules still apply.

To start in YOLO mode every time:

```lua
-- ~/.config/maki/init.lua
maki.setup({
    always_yolo = true,
})
```

## Bash Command Parsing

Bash commands get parsed with tree-sitter to extract individual commands. Something like `cd /tmp && cargo test` is checked as two separate commands.

Some constructs are too complex to analyze statically, so they always trigger a prompt:

- Command substitution: `$(...)`, backticks
- Process substitution: `<(...)`, `>(...)`
- Subshells: `(...)`, `{ ... }`
- Arithmetic expansion: `$((...))`

## Session Persistence

When you save a session, its permission rules are saved too. Loading the session restores them.

## Plugin Permissions

Plugins (bundled or third-party) run Lua inside Maki and can touch the filesystem, network, shell, and keymap. Each plugin carries a permission set that gates these capabilities, separate from the tool-call rules above.

A plugin declares its permissions in a `plugin.toml` beside its `init.lua`:

```toml
[package]
name = "my-plugin"

[permissions]
fs_read = true
fs_write = false
net = false
run = false
env = false
keymap = true
```

All six default to `true` when omitted. A plugin that omits `[permissions]` entirely is granted everything.

| Permission | Gates |
|------------|-------|
| `fs_read` | `maki.fs.read` and read access |
| `fs_write` | `maki.fs.write` and write access |
| `net` | `maki.net.*` |
| `run` | `maki.fn.*` and shell spawning |
| `env` | `maki.env.*` |
| `keymap` | `maki.keymap.set` and `maki.keymap.del` |

A plugin denied `keymap` gets a runtime error when it tries to rebind a key. A plugin can only `del` its own bindings, never another plugin's.

### Trust tiers

- **Bundled plugins** (shipped with Maki) and your init files (`~/.config/maki/init.lua`, `.maki/init.lua`) always run fully trusted. They are your config, the same way your shell rc files are.
- **Third-party plugins** load from the `[permissions]` table in their `plugin.toml`. To run an untrusted plugin with no keymap access, pin it:

```toml
[permissions]
keymap = false
```

Because init files run trusted, a committed `.maki/init.lua` can rebind any key including `<C-c>` or `<Esc>`. That is the cost of making keymaps user-configurable: the init file is already trusted to run arbitrary Lua via `maki.fn` and `maki.fs`, so keymap access adds no new power to an init file. Audit project init files the same way you audit any project script before running Maki in that project.

To boot with the full default keymap and no Lua at all, use `--no-plugins`.
