mod acp;
mod migrate;
mod subcmd;
mod tui;

use color_eyre::Result;
use color_eyre::eyre::{Context, Report};

use maki_agent::tools::ToolRegistry;
use maki_config::{PluginsConfig, RawConfig};
use maki_lua::PluginHost;
use maki_storage::StateDir;
use std::path::Path;
use std::sync::Arc;

use crate::cli::{AuthAction, Cli, Command, McpAction, MigrateAction};
use crate::update;

const RUNTIME_ADVISORY: &str = "Lua runtime failed to start";
const RUNTIME_HINT: &str = "This is a maki bug or corrupted install.";
const INIT_LUA_ADVISORY: &str = "Failed to load init.lua";
const INIT_LUA_HINT: &str = "Retry with `maki --no-plugins`.";

/// Spawn the Lua host thread. Maps a spawn failure to `StartError::Runtime`
/// so the entry point surfaces the "maki bug or corrupted install" advisory.
fn start_host(jit: bool) -> Result<PluginHost, StartError> {
    PluginHost::with_jit(Arc::clone(ToolRegistry::global_arc()), jit)
        .map_err(|e| StartError::Runtime(Report::from(e).wrap_err("initialize lua plugin host")))
}

/// Load user `init.lua` files (or skip under `--no-plugins`). Maps a failure
/// to `StartError::InitLua` so the entry point suggests `--no-plugins`.
fn load_init_or_err(
    host: &PluginHost,
    no_plugins: bool,
    cwd: &Path,
) -> Result<Option<RawConfig>, StartError> {
    host.load_init_files_or_skip(no_plugins, cwd)
        .map_err(|e| StartError::InitLua(Report::from(e).wrap_err("load init.lua files")))
}

/// `start_host` then `load_init_or_err`, so the three entry points share one
/// boot sequence instead of repeating the match/match/“if let Err on builtins”
/// triad inline. The per-caller config-build step stays out of the helper.
fn boot_lua_host(
    no_jit: bool,
    no_plugins: bool,
    cwd: &Path,
) -> Result<(PluginHost, Option<RawConfig>), StartError> {
    let host = start_host(!no_jit)?;
    let raw = load_init_or_err(&host, no_plugins, cwd)?;
    Ok((host, raw))
}

/// Load bundled plugins, mapping a failure to `StartError::Runtime` so it
/// surfaces the corrupted-install advisory rather than a bare error dump.
fn load_builtins_or_start_err(
    host: &mut PluginHost,
    plugins: &PluginsConfig,
) -> Result<(), StartError> {
    host.load_builtins(plugins)
        .map_err(|e| StartError::Runtime(Report::from(e).wrap_err("load builtin plugins")))
}

/// Which step of bringing the Lua host up failed, so each entry point can
/// print a focused recovery hint instead of a bare error dump.
#[derive(Debug, thiserror::Error)]
pub(super) enum StartError {
    /// `PluginHost::with_jit` or `load_builtins` failed: the runtime thread
    /// never came up or a bundled plugin is broken.
    Runtime(Report),
    /// `load_init_files_or_skip` failed: a user `init.lua` threw.
    InitLua(Report),
    /// Any other setup failure (bad config, model resolution, etc.) that
    /// doesn't carry a Lua recovery hint.
    Other(Report),
}

impl std::fmt::Display for StartError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.fmt_advisory())
    }
}

impl StartError {
    pub(super) fn fmt_advisory(&self) -> String {
        match self {
            StartError::Runtime(e) => {
                format!("{RUNTIME_ADVISORY}: {e:#}. {RUNTIME_HINT}")
            }
            StartError::InitLua(e) => {
                format!("{INIT_LUA_ADVISORY}: {e:#}. {INIT_LUA_HINT}")
            }
            StartError::Other(e) => format!("{e:#}"),
        }
    }
}

pub fn dispatch(cli: Cli) -> Result<()> {
    match cli.command {
        Some(Command::Auth { action }) => {
            let storage = StateDir::resolve().context("resolve data directory")?;
            match action {
                AuthAction::Login { provider } => {
                    subcmd::auth_login(provider.as_deref(), &storage)?
                }
                AuthAction::Logout { provider } => subcmd::auth_logout(&provider, &storage)?,
                AuthAction::Status => subcmd::auth_status(&storage)?,
            }
        }
        Some(Command::Index { path }) => {
            subcmd::index(&path, cli.no_plugins, cli.no_jit)?;
        }
        Some(Command::Models) => {
            subcmd::models();
        }
        Some(Command::Mcp { action }) => {
            let storage = StateDir::resolve().context("resolve data directory")?;
            match action {
                McpAction::Auth { server } => subcmd::mcp_auth(&server, &storage)?,
                McpAction::Logout { server } => subcmd::mcp_logout(&server, &storage)?,
            }
        }
        Some(Command::Update { yes, no_color }) => {
            update::update(yes, no_color).map_err(|e| color_eyre::eyre::eyre!("{e}"))?;
        }
        Some(Command::Rollback) => {
            update::rollback().map_err(|e| color_eyre::eyre::eyre!("{e}"))?;
        }
        Some(Command::Acp { model, yolo }) => {
            acp::run(model, yolo, cli.no_plugins, cli.no_jit)?;
        }
        Some(Command::Migrate { action }) => match action {
            MigrateAction::Xdg => migrate::xdg()?,
        },
        Some(Command::Prompt {
            variant,
            plan,
            tools,
            names,
        }) => {
            subcmd::prompt(&variant, plan, tools, names, cli.no_plugins, cli.no_jit)?;
        }
        None => {
            tui::run(cli)?;
        }
    }
    Ok(())
}
