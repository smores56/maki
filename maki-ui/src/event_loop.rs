use std::sync::Arc;
use std::time::Duration;

use arc_swap::{ArcSwap, ArcSwapOption};
use color_eyre::Result;
use color_eyre::eyre::Context;

use crossterm::event::{
    Event, KeyEventKind, MouseButton, MouseEvent as CtMouseEvent, MouseEventKind,
};
use maki_agent::Envelope;
use maki_agent::command::CustomCommand;
use maki_agent::permissions::PermissionManager;
use maki_agent::{AgentConfig, CancelToken, McpCommand};
use maki_config::UiConfig;
use maki_lua::{EventHandle, HintReader, KeymapReader, LuaCommandReader, UiAction};
use maki_providers::Timeouts;
use maki_providers::provider::{Provider, fetch_all_models, from_model};
use maki_providers::{Message, Model};
use maki_storage::StateDir;
use tracing::warn;

use crate::AppSession;
use crate::agent::{AgentCommand, AgentHandles, ModelSlot, shared_queue::QueueItem};
use crate::app::shell::{ShellEvent, spawn_shell};
use crate::app::{App, Msg};
use crate::components::input::Submission;
use crate::components::usage_modal::UsageFetchState;
use crate::components::{Action, ExitRequest, Status};
use crate::doorbell::{Doorbell, NotifyingSlot, Ringer};
use crate::input::InputSource;

use crate::storage_writer::StorageWriter;
use crate::terminal;

pub struct EventLoopParams {
    pub model: Model,
    pub needs_login: bool,
    pub commands: Vec<CustomCommand>,
    pub session: AppSession,
    pub storage: StateDir,
    pub config: AgentConfig,
    pub ui_config: UiConfig,
    pub input_history_size: usize,
    pub permissions: Arc<PermissionManager>,
    pub timeouts: Timeouts,
    pub exit_on_done: bool,
    pub lua_command_reader: LuaCommandReader,
    pub keymap_reader: KeymapReader,
    pub hint_reader: HintReader,
    pub ui_action_rx: Option<flume::Receiver<UiAction>>,
    pub lua_event_handle: Option<EventHandle>,
}

pub(crate) struct EventLoop<'t> {
    terminal: &'t mut ratatui::DefaultTerminal,
    app: App,
    handles: AgentHandles,
    model_slot: Arc<ArcSwap<ModelSlot>>,
    config: AgentConfig,
    permissions: Arc<PermissionManager>,
    shell_tx: flume::Sender<ShellEvent>,
    shell_rx: flume::Receiver<ShellEvent>,
    warn_rx: flume::Receiver<String>,
    warn_tx: flume::Sender<String>,
    available_models: Arc<ArcSwapOption<Vec<String>>>,
    storage_writer: Arc<StorageWriter>,
    timeouts: Timeouts,
    ui_action_rx: Option<flume::Receiver<UiAction>>,
    input: InputSource,
    doorbell: Doorbell,
    agent_rx: Option<flume::Receiver<Envelope>>,
    _model_fetch_task: smol::Task<()>,
}

struct BackgroundModels {
    available: Arc<ArcSwapOption<Vec<String>>>,
    warn_rx: flume::Receiver<String>,
    warn_tx: flume::Sender<String>,
    task: smol::Task<()>,
}

enum Woke {
    Input(Event),
    Agent(Envelope),
    Shell(ShellEvent),
    Warn(String),
    UiAction(UiAction),
    Doorbell,
    Deadline,
    InputDead,
    AgentDead,
    UiActionDead,
}

fn merge_batch(
    available: &NotifyingSlot<Option<Arc<Vec<String>>>>,
    batch: maki_providers::provider::ModelBatch,
    warn_tx: &flume::Sender<String>,
) {
    for w in batch.warnings {
        let _ = warn_tx.try_send(w);
    }
    if batch.models.is_empty() {
        return;
    }
    let mut merged = available
        .slot()
        .load()
        .as_deref()
        .cloned()
        .unwrap_or_default();
    for spec in &batch.models {
        if !merged.contains(spec) {
            merged.push(spec.clone());
        }
    }
    available.store(Some(Arc::new(merged)));
}

fn spawn_model_fetch(
    model_slot: &Arc<ArcSwap<ModelSlot>>,
    timeouts: Timeouts,
    bell: Ringer,
) -> BackgroundModels {
    let available: Arc<ArcSwapOption<Vec<String>>> = Arc::new(ArcSwapOption::empty());
    let bg = NotifyingSlot::new(Arc::clone(&available), bell.clone());
    let (warn_tx, warn_rx) = flume::unbounded::<String>();
    let warn_tx_bg = warn_tx.clone();
    let model_slot = NotifyingSlot::new(Arc::clone(model_slot), bell);
    let task = smol::spawn(async move {
        let warn_tx = warn_tx_bg;
        let done = Box::new(move || {
            let spec = model_slot.slot().load().model.spec();
            let mut resolved = match Model::from_spec(&spec) {
                Ok(m) => m,
                Err(e) => {
                    warn!(spec = %spec, error = %e, "failed to resolve model after discovery");
                    return;
                }
            };
            let provider = match from_model(&mut resolved, timeouts) {
                Ok(p) => p,
                Err(e) => {
                    warn!(spec = %spec, error = %e, "failed to create provider after discovery");
                    return;
                }
            };
            model_slot.store(Arc::new(ModelSlot {
                model: resolved,
                provider: Arc::from(provider),
            }));
        });
        fetch_all_models(|batch| merge_batch(&bg, batch, &warn_tx), Some(done)).await;
    });
    BackgroundModels {
        available,
        warn_rx,
        warn_tx,
        task,
    }
}

fn restore_session(app: &mut App, handles: &AgentHandles) {
    app.permissions
        .load_session_rules(crate::app::session_state::stored_to_rules(
            &app.state.session.meta.session_rules,
        ));
    *handles
        .tool_outputs
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = app.state.session.tool_outputs.clone();
    app.restore_display();
    for w in app.state.warnings.drain(..) {
        app.status_bar.flash(w);
    }
}

impl<'t> EventLoop<'t> {
    pub(crate) fn new(
        terminal: &'t mut ratatui::DefaultTerminal,
        params: EventLoopParams,
    ) -> Result<Self> {
        let EventLoopParams {
            mut model,
            needs_login,
            commands,
            session,
            storage,
            config,
            ui_config,
            input_history_size,
            permissions,
            timeouts,
            exit_on_done,
            lua_command_reader,
            keymap_reader,
            hint_reader,
            ui_action_rx,
            lua_event_handle,
        } = params;

        std::thread::spawn(crate::highlight::warmup);
        // Producers that publish while is_animating() stays true are drained by
        // tick()/view() and the 16ms frame deadline, so need no doorbell:
        // image decodes, the /btw stream, session-list loading, the file-picker
        // walker, live tool buffers, Lua float windows, and the restore flag.
        // If any ever leaves is_animating(), it must gain a doorbell.
        let doorbell = Doorbell::new();
        crate::update::spawn_check(doorbell.ringer());

        let storage_writer = Arc::new(StorageWriter::new(storage.clone()));
        let (shell_tx, shell_rx) = flume::unbounded::<ShellEvent>();

        let resumed = !session.messages.is_empty();
        let initial_history = session.messages.clone();
        let cwd = std::env::current_dir().unwrap_or_else(|_| ".".into());

        let provider: Arc<dyn Provider> = if needs_login {
            Arc::from(maki_providers::provider::from_model_fallback(
                &mut model, timeouts,
            ))
        } else {
            Arc::from(from_model(&mut model, timeouts).context("create provider")?)
        };
        let model_slot = Arc::new(ArcSwap::from_pointee(ModelSlot {
            model: model.clone(),
            provider,
        }));
        let bg = spawn_model_fetch(&model_slot, timeouts, doorbell.ringer());
        let handles = AgentHandles::spawn(
            &model_slot,
            initial_history,
            config.clone(),
            ui_config.tool_output_lines,
            &permissions,
            cwd,
            Some(session.id.clone()),
            timeouts,
            lua_event_handle.clone(),
            doorbell.ringer(),
        );

        let custom_commands: Arc<[CustomCommand]> = Arc::from(commands);
        let mut app = App::new(
            &model,
            session,
            storage,
            bg.available.clone(),
            handles.mcp_reader(),
            handles.mcp_config_errors.clone(),
            lua_command_reader,
            keymap_reader,
            hint_reader,
            Arc::clone(&storage_writer),
            ui_config,
            input_history_size,
            Arc::clone(&permissions),
            custom_commands,
            doorbell.ringer(),
        );
        app.exit_on_done = exit_on_done;
        app.lua_event_handle = lua_event_handle;

        if needs_login {
            app.login_picker.open(app.storage.clone());
        }

        handles.apply_to_app(&mut app);

        if !handles.mcp_config_errors.is_empty() {
            app.flash(format!("MCP config error: {}", handles.mcp_config_errors));
        }

        if resumed {
            restore_session(&mut app, &handles);
        }

        let agent_rx = Some(handles.agent_rx.clone());
        Ok(Self {
            terminal,
            app,
            handles,
            model_slot,
            config,
            permissions,
            shell_tx,
            shell_rx,
            warn_rx: bg.warn_rx,
            warn_tx: bg.warn_tx,
            available_models: bg.available,
            storage_writer,
            timeouts,
            ui_action_rx,
            input: InputSource::spawn(),
            doorbell,
            agent_rx,
            _model_fetch_task: bg.task,
        })
    }

    pub(crate) fn run(mut self, initial_prompt: Option<String>) -> Result<(Option<String>, i32)> {
        if let Some(prompt) = initial_prompt {
            let sub = Submission {
                text: prompt,
                images: Vec::new(),
            };
            let actions = self.app.handle_submit(sub);
            self.dispatch(actions);
        }
        loop {
            self.tick();
            self.drain_channels();
            self.terminal.draw(|f| self.app.view(f))?;

            if self.app.exit_request != ExitRequest::None {
                return Ok(self.shutdown());
            }
            match self.wait_for_event() {
                Woke::Input(raw) => {
                    if let Some(msg) = self.translate_input(raw) {
                        let actions = self.app.update(msg);
                        self.dispatch(actions);
                    }
                }
                Woke::Agent(envelope) => self.handle_agent_envelope(envelope),
                Woke::Shell(ev) => self.app.handle_shell_event(ev),
                Woke::Warn(w) => self.app.flash(w),
                Woke::UiAction(a) => self.handle_ui_action(a),
                Woke::Doorbell | Woke::Deadline => {}
                Woke::AgentDead => self.mark_agent_dead(),
                Woke::UiActionDead => self.ui_action_rx = None,
                Woke::InputDead => {
                    return Err(color_eyre::eyre::eyre!("input reader thread exited"));
                }
            }
        }
    }

    fn tick(&mut self) {
        self.app.tick_edge_scroll();
        self.app.tick_error_expiry();
        self.app.poll_image_paste();
        self.app.btw_modal.poll();
        self.app.status_bar.poll_branch_update();
        self.app.mcp_picker.refresh();
        self.app.float_mgr.tick();
    }

    fn handle_agent_envelope(&mut self, envelope: Envelope) {
        let actions = self.app.update(Msg::Agent(Box::new(envelope)));
        self.dispatch(actions);
    }

    fn handle_ui_action(&mut self, action: UiAction) {
        match action {
            UiAction::Flash(msg) => {
                self.app.flash(msg);
            }
            UiAction::OpenEditor { path, reply_tx } => {
                let _pause = self.input.pause();
                let code = match crate::terminal::open_in_editor(&path, self.terminal) {
                    Ok(code) => code,
                    Err(e) => {
                        self.app.flash(e);
                        -1
                    }
                };
                let _ = reply_tx.send(code);
            }
            UiAction::OpenWin {
                buf,
                config,
                focus,
                event_tx,
                cmd_rx,
            } => {
                self.app
                    .float_mgr
                    .open(buf, config, focus, event_tx, cmd_rx);
                if focus {
                    self.app
                        .transition_plan(crate::app::mode::PlanTrigger::InteractivePrompt);
                }
            }
        }
    }

    fn mark_agent_dead(&mut self) {
        self.agent_rx = None;
        if self.app.status == Status::Streaming {
            self.app.status = Status::error("agent stopped unexpectedly".into());
        }
    }

    fn drain_channels(&mut self) {
        while let Ok(event) = self.shell_rx.try_recv() {
            self.app.handle_shell_event(event);
        }

        while let Some(rx) = &self.agent_rx {
            match rx.try_recv() {
                Ok(envelope) => self.handle_agent_envelope(envelope),
                Err(flume::TryRecvError::Disconnected) => {
                    self.mark_agent_dead();
                    break;
                }
                Err(_) => break,
            }
        }

        while let Ok(warning) = self.warn_rx.try_recv() {
            self.app.flash(warning);
        }

        let slot_model = self.model_slot.load();
        if slot_model.model.context_window != self.app.state.model.context_window {
            self.app.update_model(&slot_model.model);
        }

        let actions: Option<Vec<UiAction>> =
            self.ui_action_rx.as_ref().map(|rx| rx.try_iter().collect());
        if let Some(actions) = actions {
            for action in actions {
                self.handle_ui_action(action);
            }
        }
    }

    fn wait_for_event(&self) -> Woke {
        let deadline = self.app.next_deadline();
        let mut sel = flume::Selector::new()
            .recv(&self.input.rx, |r| {
                r.map(Woke::Input).unwrap_or(Woke::InputDead)
            })
            .recv(self.doorbell.receiver(), |_| Woke::Doorbell)
            .recv(&self.shell_rx, |r| {
                r.map(Woke::Shell).unwrap_or(Woke::Deadline)
            })
            .recv(&self.warn_rx, |r| {
                r.map(Woke::Warn).unwrap_or(Woke::Deadline)
            });
        if let Some(rx) = &self.agent_rx {
            sel = sel.recv(rx, |r| r.map(Woke::Agent).unwrap_or(Woke::AgentDead));
        }
        if let Some(rx) = &self.ui_action_rx {
            sel = sel.recv(rx, |r| r.map(Woke::UiAction).unwrap_or(Woke::UiActionDead));
        }
        match deadline {
            Some(d) => sel.wait_deadline(d).unwrap_or(Woke::Deadline),
            None => sel.wait(),
        }
    }

    fn translate_input(&mut self, raw: Event) -> Option<Msg> {
        match raw {
            Event::Key(key) if key.kind == KeyEventKind::Press => Some(Msg::Key(key)),
            Event::Key(_) => None,
            Event::Paste(text) => Some(Msg::Paste(text)),
            Event::Mouse(mouse) => self.translate_mouse(mouse),
            _ => None,
        }
    }

    fn translate_mouse(&mut self, mouse: CtMouseEvent) -> Option<Msg> {
        match mouse.kind {
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                let (scroll, extra) = aggregate_scroll(
                    &self.input.rx,
                    mouse.column,
                    mouse.row,
                    scroll_delta(mouse.kind, self.app.ui_config.mouse_scroll_lines),
                    self.app.ui_config.mouse_scroll_lines,
                );
                if let Some(extra) = extra {
                    let actions = self.app.update(scroll);
                    self.dispatch(actions);
                    self.translate_input(extra)
                } else {
                    Some(scroll)
                }
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                let (drag, extra) = coalesce_drag(&self.input.rx, mouse);
                let actions = self.app.update(Msg::Mouse(drag));
                self.dispatch(actions);
                extra.and_then(|ev| self.translate_input(ev))
            }
            _ => Some(Msg::Mouse(mouse)),
        }
    }

    fn dispatch(&mut self, actions: Vec<Action>) {
        for action in actions {
            self.handle_action(action);
        }
    }

    fn respawn_agent(&mut self, history: Vec<Message>) {
        let lua_handle = self.app.lua_event_handle.clone();
        self.handles.respawn(
            history,
            &self.model_slot,
            self.config.clone(),
            self.app.ui_config.tool_output_lines,
            &self.permissions,
            &mut self.app,
            lua_handle,
        );
        self.agent_rx = Some(self.handles.agent_rx.clone());
    }

    fn handle_action(&mut self, action: Action) {
        match action {
            Action::SendMessage(input) => {
                let mut input = *input;
                input.preamble = self.app.shell.drain_results();
                let run_id = self.app.run_id;
                self.handles.queue.push(QueueItem::Message {
                    text: input.message.clone(),
                    image_count: input.images.len(),
                    input,
                    run_id,
                    displayed: true,
                });
            }
            Action::CancelAgent { run_id } => {
                let _ = self
                    .handles
                    .cmd_tx
                    .try_send(AgentCommand::Cancel { run_id });
            }
            Action::CancelSubagent { tool_use_id } => {
                let _ = self
                    .handles
                    .cmd_tx
                    .try_send(AgentCommand::CancelSubagent { tool_use_id });
            }
            Action::NewSession => {
                self.respawn_agent(Vec::new());
            }
            Action::LoadSession(loaded) => {
                let loaded = *loaded;
                if loaded.model_spec != self.model_slot.load().model.spec()
                    && let Ok(mut new_model) = Model::from_spec(&loaded.model_spec)
                    && let Ok(new_provider) = from_model(&mut new_model, self.timeouts)
                {
                    self.app.usage_slot.store(None);
                    self.model_slot.store(Arc::new(ModelSlot {
                        model: new_model,
                        provider: Arc::from(new_provider),
                    }));
                }
                self.respawn_agent(loaded.messages);
                *self
                    .handles
                    .tool_outputs
                    .lock()
                    .unwrap_or_else(|e| e.into_inner()) = loaded.tool_outputs;
            }
            Action::ChangeModel(spec) => self.change_model(spec),
            Action::RefreshProvider { slug } => self.refresh_provider(slug),
            Action::AssignTier(spec, tier) => {
                maki_providers::model_registry::set_and_persist(spec, tier, &self.app.storage);
            }
            Action::UnassignTier(spec, tier) => {
                maki_providers::model_registry::unset_and_persist(&spec, tier, &self.app.storage);
            }
            Action::Compact => {
                self.handles.queue.push(QueueItem::Compact {
                    run_id: self.app.run_id,
                });
            }
            Action::ToggleMcp(server_name, enabled) => {
                self.handles.send_mcp(McpCommand::Toggle {
                    server: server_name,
                    enabled,
                });
            }
            Action::ShellCommand {
                id,
                command,
                visible,
            } => {
                let (trigger, cancel) = CancelToken::new();
                self.app.shell.add_trigger(trigger);
                spawn_shell(
                    command,
                    id,
                    visible,
                    self.shell_tx.clone(),
                    cancel,
                    self.config.clone(),
                );
            }
            Action::OpenEditor(path) => {
                let _pause = self.input.pause();
                if let Err(e) = terminal::open_in_editor(&path, self.terminal) {
                    self.app.flash(e);
                }
            }
            Action::EditInputInEditor => {
                let _pause = self.input.pause();
                let current_text = self.app.input_box.buffer.value();
                match terminal::edit_temp_content(&current_text, self.terminal) {
                    Ok(edited) => self.app.input_box.set_input(edited),
                    Err(e) => self.app.flash(e),
                }
            }
            Action::Btw(question) => {
                let slot = self.model_slot.load();
                self.app
                    .start_btw(question, Arc::clone(&slot.provider), slot.model.clone());
            }
            Action::Suspend => {
                let _pause = self.input.pause();
                terminal::suspend(self.terminal);
            }
            Action::RefreshModels => self.refresh_models(),
            Action::RefreshUsage => self.refresh_usage(),
            Action::Quit => {}
        }
    }

    fn change_model(&mut self, spec: String) {
        match Model::from_spec(&spec) {
            Ok(mut new_model) => match from_model(&mut new_model, self.timeouts) {
                Ok(new_provider) => {
                    self.app.update_model(&new_model);
                    self.app.record_recent_model(&spec);
                    self.app.usage_slot.store(None);
                    self.model_slot.store(Arc::new(ModelSlot {
                        model: new_model,
                        provider: Arc::from(new_provider),
                    }));
                }
                Err(e) => self.app.flash(format!("Failed to create provider: {e}")),
            },
            Err(e) => self.app.flash(format!("Invalid model: {e}")),
        }
    }

    fn refresh_models(&self) {
        let available =
            NotifyingSlot::new(Arc::clone(&self.available_models), self.app.ringer.clone());
        let warn_tx = self.warn_tx.clone();
        available.store(None);
        smol::spawn(async move {
            fetch_all_models(|batch| merge_batch(&available, batch, &warn_tx), None).await;
        })
        .detach();
    }

    fn refresh_usage(&self) {
        let provider = Arc::clone(&self.model_slot.load().provider);
        let slot = NotifyingSlot::new(Arc::clone(&self.app.usage_slot), self.app.ringer.clone());
        slot.store(Some(Arc::new(UsageFetchState::Loading)));
        smol::spawn(async move {
            let state = match provider.fetch_usage().await {
                Ok(Some(usage)) => UsageFetchState::Ready(usage),
                Ok(None) => UsageFetchState::Unsupported,
                Err(e) => UsageFetchState::Error(e.user_message()),
            };
            slot.store(Some(Arc::new(state)));
        })
        .detach();
    }

    fn refresh_provider(&mut self, slug: String) {
        let current = self.model_slot.load();
        let current_model = &current.model;

        if current_model.provider.to_string() == slug {
            let mut m = current_model.clone();
            if let Ok(provider) = maki_providers::provider::from_model(&mut m, self.timeouts) {
                self.app.usage_slot.store(None);
                self.model_slot.store(Arc::new(ModelSlot {
                    model: m,
                    provider: Arc::from(provider),
                }));
            }
        } else if let Some(builtin) = maki_config::providers::builtin_provider(&slug) {
            let spec = builtin.default_model.to_string();
            self.change_model(spec);
        }
    }

    fn shutdown(mut self) -> (Option<String>, i32) {
        let exit_code = self.app.exit_request.code();
        let session_id = self
            .app
            .has_content()
            .then(|| self.app.state.session.id.clone());
        maki_agent::mcp::kill_process_groups(&self.handles.mcp_reader().load().pids);
        self.app.cmd_tx = None;
        self.app.answer_tx = None;
        drop(self.app);
        self.handles.shutdown(Duration::from_secs(3));
        match Arc::try_unwrap(self.storage_writer) {
            Ok(writer) => writer.shutdown(Duration::from_secs(3)),
            Err(_) => {
                warn!("storage writer has outstanding references, skipping graceful shutdown")
            }
        }
        (session_id, exit_code)
    }
}

fn scroll_delta(kind: MouseEventKind, lines: u32) -> i32 {
    if kind == MouseEventKind::ScrollUp {
        lines as i32
    } else {
        -(lines as i32)
    }
}

fn aggregate_scroll(
    rx: &flume::Receiver<Event>,
    column: u16,
    row: u16,
    mut delta: i32,
    scroll_lines: u32,
) -> (Msg, Option<Event>) {
    while let Ok(next) = rx.try_recv() {
        if let Event::Mouse(m) = next {
            match m.kind {
                MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                    delta += scroll_delta(m.kind, scroll_lines);
                }
                _ => return (Msg::Scroll { column, row, delta }, Some(Event::Mouse(m))),
            }
        } else {
            return (Msg::Scroll { column, row, delta }, Some(next));
        }
    }
    (Msg::Scroll { column, row, delta }, None)
}

fn coalesce_drag(
    rx: &flume::Receiver<Event>,
    mut latest: CtMouseEvent,
) -> (CtMouseEvent, Option<Event>) {
    while let Ok(next) = rx.try_recv() {
        if let Event::Mouse(m) = next {
            if matches!(m.kind, MouseEventKind::Drag(MouseButton::Left)) {
                latest = m;
            } else {
                return (latest, Some(Event::Mouse(m)));
            }
        } else {
            return (latest, Some(next));
        }
    }
    (latest, None)
}
