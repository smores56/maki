use std::env;
use std::sync::Arc;

use color_eyre::Result;
use color_eyre::eyre::Context;

use maki_config::{load_env_files, load_permissions};
use maki_storage::StateDir;

use crate::cmd::{boot_lua_host, load_builtins_or_start_err};
use crate::setup;

pub fn run(model_arg: Option<String>, yolo: bool, no_plugins: bool, no_jit: bool) -> Result<()> {
    let storage = StateDir::resolve().context("resolve data directory")?;
    maki_providers::model_registry::load_from_storage(&storage);

    let cwd = env::current_dir().unwrap_or_else(|_| ".".into());
    load_env_files(&cwd);

    let (mut plugin_host, raw_config) = boot_lua_host(no_jit, no_plugins, &cwd)?;

    let mut config = raw_config
        .unwrap_or_default()
        .into_config(false)
        .context("invalid config")?;
    config.permissions = load_permissions(&cwd);

    if yolo || config.always_yolo {
        config.permissions.yolo = true;
    }
    config.validate()?;

    load_builtins_or_start_err(&mut plugin_host, &config.plugins)?;

    let timeouts = maki_providers::Timeouts {
        connect: config.provider.connect_timeout,
        low_speed: config.provider.low_speed_timeout,
        stream: config.provider.stream_timeout,
    };

    let model = setup::resolve_model(model_arg.as_deref(), &config.provider, &storage)?;

    setup::init_logging(&config.storage);
    setup::install_panic_log_hook();
    setup::warn_ignored_provider_fields();

    let (mcp_handle, _mcp_config_errors) = smol::block_on(maki_agent::mcp::start_connected(&cwd));

    let prompt_slots = plugin_host.event_handle().collect_prompt_slots();

    maki_acp::run(maki_acp::AcpParams {
        model,
        config: config.agent,
        permissions_config: config.permissions,
        timeouts,
        initial_wd: cwd,
        mcp_handle,
        prompt_slots: Arc::new(prompt_slots),
        yolo,
    })
}
