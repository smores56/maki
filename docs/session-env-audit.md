# Task 0.3: Env/cwd ambient-state audit

Audit of every `current_dir()` and `env::var` read in the maki workspace,
classified for the multi-session daemon refactor (phase 2.3).

Classification key:

- **A** session-scoped: read while running a session (tool execution, path
  resolution, provider key lookup, .env loading, template rendering). Needs
  threading off the ambient process state in phase 2.3.
- **B** process-scoped: logging, config discovery at startup, CLI entry. The
  cwd is the user's shell cwd at launch; leave it ambient.
- **C** test-only.

Per the task rules, a category-A hit was only FIXED if an explicit in-scope
value (ToolContext field, params struct field, `PermissionManager.cwd`,
etc.) ALREADY exists and is reachable without new plumbing. No new function
parameters or accessors were added.

## current_dir() hits

### 1. maki-agent/src/template.rs:7 — A, DEFERRED(phase 2.3)

`env_vars()` builds the `{cwd}` template var used to render instructions and
tool descriptions during a session. Free function; no ToolContext in scope.
Called from `headless.rs:105` (session setup) and `subcmd.rs:542` (CLI
entry). No explicit cwd value reachable here without new plumbing. Deferred.

### 2. maki-agent/src/tools/mod.rs:239 — A, DEFERRED(phase 2.3)

`resolve_path()` resolves a relative path against cwd for tool input
(glob, grep, read, write, edit, permissions scope normalization). Free
function; no ToolContext in scope. Deferred.

### 3. maki-agent/src/tools/mod.rs:249 — A, DEFERRED(phase 2.3)

`resolve_search_path()` returns cwd when no path is given (grep default
search root). Same free function, no context. Deferred.

### 4. maki-agent/src/tools/mod.rs:255 — B (process-scoped cache), DEFERRED note for 2.3

`static CWD: LazyLock<Option<PathBuf>>` caches the process cwd once and is
read by `relative_path()` (line 260) to shorten displayed paths in tool
output. This is process-scoped: set once per process, invariant across the
lifetime of a single-process run, so it is category B. BUT under the daemon
multiple sessions run in one process with different cwds, so this cache must
be replaced with a per-session value in 2.3. Marked DEFERRED(2.3) for that
replacement even though it is category B today. No fix possible now: the
cache has no in-scope session value and `relative_path` is a free function.

### 5. maki-agent/src/tools/mod.rs:453 — B

`cli_tool_ctx()` builds a minimal ToolContext for CLI one-shot tool
execution (`maki index`, etc.). Seeds `PermissionManager.cwd` with the
shell cwd at launch. This is CLI entry: the cwd is determined by the
user's shell when starting maki. Recategorized from the task's "TEST" label:
it is not inside `#[cfg(test)]` (that module starts at line 461), it is real
CLI-entry code. No other cwd source exists in this function. Leave.

### 6. maki-agent/src/tools/mod.rs:743 — C

Inside `#[cfg(test)]`. Leave.

### 7. maki-agent/src/tools/mod.rs:765 — C

Inside `#[cfg(test)]`. Leave.

### 8. maki-lua/src/api/uv.rs:11 — A, DEFERRED(phase 2.3)

`uv.cwd` mirrors the Neovim `vim.uv.cwd` API: signature takes `()` only
(see AGENTS.md in maki-lua/src/api: signatures must match Neovim for plugin
portability). No context parameter. Plugin execution is session-scoped, but
the ambient read cannot be replaced without changing the Neovim-mirrored
API or adding out-of-band session state. Deferred.

### 9. maki-lua/src/api/util/ctx.rs:319 — A, DEFERRED(phase 2.3)

`find_instructions` lua method resolves a directory relative to cwd then
searches for instruction files. The `this` handler object has no cwd field;
`resolve_abs_with_cwd` takes cwd as an arg but no session value is on
`this`. Deferred.

### 10. maki-lua/src/api/fs.rs:33 — A, DEFERRED(phase 2.3)

`make_absolute()` helper makes a path absolute by joining onto cwd. Free
function, no context. Mirrors `vim.fs` semantics. Deferred.

### 11. src/sdk_mode.rs:464 — B

Top-level SDK mode entry; cwd is the user's shell cwd when launching maki.
Leave.

### 12. src/cmd/subcmd.rs:452 — B

CLI subcommand dispatch entry. Leave.

### 13. src/cmd/subcmd.rs:496 — B

CLI subcommand dispatch entry. Leave.

### 14. src/cmd/subcmd.rs:539 — B

CLI subcommand dispatch entry (`maki` interactive startup path). Leave.

### 15. src/cmd/tui.rs:23 — B

TUI command entry. Leave.

### 16. src/cmd/tui.rs:67 — B

TUI command entry. Leave.

### 17. src/cmd/acp.rs:18 — B

ACP server command entry. Leave.

### 18. src/print.rs:165 — B

One-shot print/output entry. Leave.

## env::var / std::env::var hits (non-test)

### maki-agent/src/agent/compaction.rs:213 — A (session behavior), DEFERRED(phase 2.3)

`MAKI_DISABLE_AUTOCOMPACT` gates autocompaction during a session. Read in
the compaction path with no ToolContext or session config in scope.
Deferred.

### maki-lua/src/api/fn.rs:353 — A, DEFERRED(phase 2.3)

`env::var_os("PATH")` for executable lookup in plugin execution. Neovim
`vim.fn` mirror, no context parameter. Deferred.

### maki-lua/src/api/uv.rs:26 — A, DEFERRED(phase 2.3)

`uv.os_getenv` mirrors Neovim `vim.uv.os_getenv`: signature takes only the
env var name. No context parameter; per-session env is not reachable without
changing the mirrored API or adding out-of-band state. Deferred.

### maki-providers/src/providers/openai/auth.rs:291 — A-DEFERRED(2.3)

`OPENAI_API_KEY` read at provider construction. No ToolContext in scope at
the construction call site. Deferred to 2.3 when session env is threaded
into provider construction.

### maki-providers/src/providers/anthropic/bedrock.rs (14 hits, lines 56, 60, 64, 67, 71, 72, 74, 82, 93, 94, 161, 167, 492, 535) — A-DEFERRED(2.3)

AWS_*, HOME, ANTHROPIC_BEDROCK_BASE_URL, ANTHROPIC_MODEL reads at provider
construction / request build time. No ToolContext in scope. Deferred to
2.3.

### maki-providers/src/providers/opencode.rs:49,64 — A-DEFERRED(2.3)

`std::env::var` for opencode provider env/key discovery. Read at provider
construction. No context in scope. Deferred.

### maki-providers/src/providers/local.rs:58 — A-DEFERRED(2.3)

`std::env::var(cfg.host_env)` for local provider host lookup at
construction. No context in scope. Deferred.

### maki-providers/src/providers/copilot/auth.rs:22,100 — A-DEFERRED(2.3)

`env::var(key)` and `env::var_os("XDG_CONFIG_HOME")` for Copilot token /
config discovery at auth construction. No ToolContext in scope. Deferred.

### maki-providers/src/providers/mod.rs:182 — A-DEFERRED(2.3)

`std::env::var(env_var)` for provider key lookup at provider construction.
No ToolContext in scope. Deferred.

### Additional env::var hits found by re-running the grep (not in the original task list)

### src/cmd/subcmd.rs:131, 382, 419 — B

`env::var(b.default_api_key_env)` / `env::var(env_var)` in CLI provider
selection prompts. Config discovery at CLI entry. Leave.

## Summary

| Category | Count | Fixed | Deferred |
| --- | --- | --- | --- |
| A (session-scoped) | 16 | 0 | 16 |
| A-DEFERRED (provider key lookup, no context) | 19 | 0 | 19 |
| B (process-scoped) | 13 | 0 | 0 (1 noted for 2.3 cache replacement) |
| C (test-only) | 2 | 0 | 0 |

FIXED: 0. Every category-A hit lacks an existing in-scope explicit value.
The only candidate plumbing target (`PermissionManager.cwd`, line 161 of
permissions.rs) is a private field with no public accessor; reaching it
would require adding an accessor, which the task rules forbid (no new
plumbing). Per the conservative rule, all A hits are DEFERRED(phase 2.3)
rather than risk a behavior change or add plumbing now.

The phase-2.3 daemon work will need to:
1. Add a per-session cwd to ToolContext (or expose
   `PermissionManager.cwd`) and replace `resolve_path`,
   `resolve_search_path`, `relative_path`, and the `static CWD` cache.
2. Thread session env into provider construction (openai, bedrock,
   opencode, local, copilot, providers/mod) so key/base-url/model env is
   read from the session, not the process.
3. Decide how the Neovim-mirrored lua APIs (`uv.cwd`, `uv.os_getenv`,
   `fn` PATH lookup) source per-session cwd/env without breaking plugin
   portability, likely via an out-of-band session-scoped override.
4. Replace the `MAKI_DISABLE_AUTOCOMPACT` ambient read with a session
   config field.

Behavior-preserving guarantee: no call-site behavior changed, since zero
edits were made. For any future fix, the explicit value must equal today's
ambient one in the single-process case.
