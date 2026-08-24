use std::mem;
use std::sync::Arc;

use crossterm::event::{KeyCode, KeyEvent};
use maki_agent::command::CustomCommand;
use maki_agent::{McpPromptInfo, McpSnapshotReader};
use maki_lua::{
    CompletionReply, EventHandle, LuaCommandInfo, LuaCommandReader, TriggerSnapshotReader,
};
use nucleo::pattern::{CaseMatching, Normalization};
use nucleo::{Config, Matcher, Nucleo, Utf32String};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph};

use crate::repaint::Dirty;
use crate::text_buffer::TextBuffer;
use crate::theme;

const TICK_TIMEOUT_MS: u64 = 10;
const PAD: usize = 1;
const GAP: usize = 2;
const MENTION_LOADING_ROW: &str = "…";
const MENTION_EMPTY_ROW: &str = "no matches";

pub struct BuiltinCommand {
    pub name: &'static str,
    pub description: &'static str,
    pub max_args: usize,
}

pub const BUILTIN_COMMANDS: &[BuiltinCommand] = &[
    BuiltinCommand {
        name: "/tasks",
        description: "Browse and search tasks",
        max_args: 0,
    },
    BuiltinCommand {
        name: "/compact",
        description: "Summarize and compact conversation history",
        max_args: 0,
    },
    BuiltinCommand {
        name: "/new",
        description: "Start a new session",
        max_args: 0,
    },
    BuiltinCommand {
        name: "/help",
        description: "Show keybindings",
        max_args: 0,
    },
    BuiltinCommand {
        name: "/usage",
        description: "Show token usage breakdown",
        max_args: 0,
    },
    BuiltinCommand {
        name: "/queue",
        description: "Remove items from queue",
        max_args: 0,
    },
    BuiltinCommand {
        name: "/model",
        description: "Switch model",
        max_args: 0,
    },
    BuiltinCommand {
        name: "/theme",
        description: "Switch color theme",
        max_args: 0,
    },
    BuiltinCommand {
        name: "/mcp",
        description: "Configure MCP servers",
        max_args: 0,
    },
    BuiltinCommand {
        name: "/login",
        description: "Authenticate with an LLM provider",
        max_args: 0,
    },
    BuiltinCommand {
        name: "/cd",
        description: "Change working directory",
        max_args: 1,
    },
    BuiltinCommand {
        name: "/btw",
        description: "Ask a quick question (no tools, no history pollution)",
        max_args: usize::MAX,
    },
    BuiltinCommand {
        name: "/yolo",
        description: "Toggle YOLO mode (skip all permission prompts)",
        max_args: 0,
    },
    BuiltinCommand {
        name: "/thinking",
        description: "Toggle extended thinking (off, adaptive, effort level, or budget)",
        max_args: 1,
    },
    BuiltinCommand {
        name: "/fast",
        description: "Toggle Anthropic fast mode (Opus only)",
        max_args: 0,
    },
    BuiltinCommand {
        name: "/workflow",
        description: "Toggle workflow mode (task callable inside code_execution)",
        max_args: 0,
    },
    BuiltinCommand {
        name: "/exit",
        description: "Exit the application",
        max_args: 0,
    },
    BuiltinCommand {
        name: "/reload",
        description: "Reload plugins and config",
        max_args: 0,
    },
];

pub struct ParsedCommand {
    pub name: String,
    pub args: String,
}

pub enum CommandAction {
    Consumed,
    Execute(ParsedCommand),
    Complete(String),
    CompleteRange {
        start: usize,
        end: usize,
        text: String,
    },
    Passthrough,
}

#[derive(Clone)]
enum CommandType {
    Builtin(&'static BuiltinCommand),
    Custom(usize),
    McpPrompt(usize),
    Lua(usize),
    Mention(usize),
}

struct MentionItem {
    label: String,
    insert: String,
    kind: String,
}

enum MentionState {
    Pending {
        generation: u64,
        rx: flume::Receiver<CompletionReply>,
    },
    Settled,
}

struct CommandItem {
    name: String,
    max_args: usize,
    command_type: CommandType,
}

struct Match {
    command_type: CommandType,
    indices: Vec<u32>,
}

pub struct CommandPalette {
    selected: usize,
    filtered: Vec<Match>,
    custom: Arc<[CustomCommand]>,
    mcp_reader: McpSnapshotReader,
    mcp_prompts: Vec<McpPromptInfo>,
    mcp_generation: u64,
    lua_reader: LuaCommandReader,
    lua_commands: Vec<LuaCommandInfo>,
    lua_generation: u64,
    trigger_reader: TriggerSnapshotReader,
    triggers: Vec<String>,
    trigger_generation: u64,
    mention_items: Vec<MentionItem>,
    mention_query: String,
    mention_range: Option<(usize, usize)>,
    mention_state: Option<MentionState>,
    mention_generation: u64,
    active_trigger: Option<char>,
    nucleo: Nucleo<CommandItem>,
    matcher: Matcher,
    current_arg_count: usize,
    event_handle: EventHandle,
}

impl CommandPalette {
    pub fn new(
        custom_commands: Arc<[CustomCommand]>,
        mcp_reader: McpSnapshotReader,
        lua_reader: LuaCommandReader,
        trigger_reader: TriggerSnapshotReader,
        event_handle: EventHandle,
    ) -> Self {
        let snap = mcp_reader.load();
        let mcp_generation = snap.generation;
        let prompts = snap.prompts.clone();

        let lua_snap = lua_reader.load();
        let lua_generation = lua_snap.generation;
        let lua_commands = lua_snap.commands.clone();

        let trigger_snap = trigger_reader.load();
        let trigger_generation = trigger_snap.generation;
        let triggers = trigger_snap.triggers.clone();

        let nucleo = Self::build_nucleo(&custom_commands, &prompts, &lua_commands, &[]);
        Self {
            selected: 0,
            filtered: Vec::new(),
            custom: custom_commands,
            mcp_reader,
            mcp_prompts: prompts,
            mcp_generation,
            lua_reader,
            lua_commands,
            lua_generation,
            trigger_reader,
            triggers,
            trigger_generation,
            mention_items: Vec::new(),
            mention_query: String::new(),
            mention_range: None,
            mention_state: None,
            mention_generation: 0,
            active_trigger: None,
            nucleo,
            matcher: Matcher::new(Config::DEFAULT),
            current_arg_count: 0,
            event_handle,
        }
    }

    /// Every item the palette matches, in display order. The one place that
    /// enumerates the sources: matching and name lookup both read it.
    fn items<'a>(
        custom_commands: &'a [CustomCommand],
        mcp_prompts: &'a [McpPromptInfo],
        lua_commands: &'a [LuaCommandInfo],
        mention_items: &'a [MentionItem],
    ) -> impl Iterator<Item = CommandItem> + 'a {
        let builtins = BUILTIN_COMMANDS.iter().map(|cmd| CommandItem {
            name: cmd.name.to_string(),
            max_args: cmd.max_args,
            command_type: CommandType::Builtin(cmd),
        });
        let custom = custom_commands
            .iter()
            .enumerate()
            .map(|(i, cmd)| CommandItem {
                name: cmd.display_name(),
                max_args: if cmd.has_args() { usize::MAX } else { 0 },
                command_type: CommandType::Custom(i),
            });
        let prompts = mcp_prompts.iter().enumerate().map(|(i, p)| CommandItem {
            name: format!("/{}", p.display_name),
            max_args: if p.arguments.is_empty() {
                0
            } else {
                usize::MAX
            },
            command_type: CommandType::McpPrompt(i),
        });
        let lua = lua_commands.iter().enumerate().map(|(i, cmd)| CommandItem {
            name: cmd.name.to_string(),
            max_args: cmd.max_args,
            command_type: CommandType::Lua(i),
        });
        let mentions = mention_items
            .iter()
            .enumerate()
            .map(|(i, item)| CommandItem {
                name: item.label.clone(),
                max_args: usize::MAX,
                command_type: CommandType::Mention(i),
            });
        builtins
            .chain(custom)
            .chain(prompts)
            .chain(lua)
            .chain(mentions)
    }

    fn build_nucleo(
        custom_commands: &[CustomCommand],
        mcp_prompts: &[McpPromptInfo],
        lua_commands: &[LuaCommandInfo],
        mention_items: &[MentionItem],
    ) -> Nucleo<CommandItem> {
        let nucleo = Nucleo::new(Config::DEFAULT, Arc::new(|| {}), None, 1);
        let injector = nucleo.injector();

        for item in Self::items(custom_commands, mcp_prompts, lua_commands, mention_items) {
            injector.push(item, |item, cols| {
                cols[0] = Utf32String::from(item.name.as_str());
            });
        }

        nucleo
    }

    pub fn handle_key(&mut self, key: KeyEvent, input: &str) -> CommandAction {
        if !self.is_active() {
            return CommandAction::Passthrough;
        }
        let mention_mode = self.mention_trigger().is_some();
        match key.code {
            KeyCode::Up => {
                self.move_up();
                CommandAction::Consumed
            }
            KeyCode::Down => {
                self.move_down();
                CommandAction::Consumed
            }
            KeyCode::Esc => {
                self.close();
                CommandAction::Consumed
            }
            KeyCode::Enter => {
                if mention_mode {
                    self.complete_mention(self.selected)
                } else {
                    match self.confirm(input) {
                        Some(cmd) => {
                            self.close();
                            CommandAction::Execute(cmd)
                        }
                        None => CommandAction::Consumed,
                    }
                }
            }
            KeyCode::Tab => {
                if mention_mode {
                    self.complete_mention(0)
                } else if let Some(item) = self.filtered.get(self.selected) {
                    let name = self.item_name(item);
                    let text = if self.item_has_args(item) {
                        format!("{name} ")
                    } else {
                        name
                    };
                    CommandAction::Complete(text)
                } else {
                    CommandAction::Consumed
                }
            }
            _ => CommandAction::Passthrough,
        }
    }

    pub fn is_active(&self) -> bool {
        match self.active_trigger {
            Some('/') => !self.filtered.is_empty(),
            Some(_) => true,
            None => false,
        }
    }

    pub(crate) fn mention_pending(&self) -> bool {
        matches!(self.mention_state, Some(MentionState::Pending { .. }))
    }

    #[cfg(test)]
    pub(crate) fn match_count(&self) -> usize {
        self.filtered.len()
    }

    #[cfg(test)]
    pub(crate) fn match_label(&self, index: usize) -> Option<String> {
        self.filtered.get(index).map(|m| self.item_name(m))
    }

    pub fn sync(&mut self, input: &str, cursor: usize, cwd: &str) {
        let mcp_snap = self.mcp_reader.load();
        let lua_snap = self.lua_reader.load();
        let trigger_snap = self.trigger_reader.load();
        if mcp_snap.generation != self.mcp_generation
            || lua_snap.generation != self.lua_generation
            || trigger_snap.generation != self.trigger_generation
        {
            self.mcp_generation = mcp_snap.generation;
            self.mcp_prompts = mcp_snap.prompts.clone();
            self.lua_generation = lua_snap.generation;
            self.lua_commands = lua_snap.commands.clone();
            self.trigger_generation = trigger_snap.generation;
            self.triggers = trigger_snap.triggers.clone();
            self.nucleo = Self::build_nucleo(
                &self.custom,
                &self.mcp_prompts,
                &self.lua_commands,
                &self.mention_items,
            );
        }

        if input.starts_with('/') {
            self.mention_state = None;
            self.active_trigger = Some('/');
            self.sync_slash(input);
            return;
        }

        let Some((trigger, start, end)) = trigger_at(input, cursor, &self.triggers) else {
            self.close();
            return;
        };
        if trigger == '/' {
            self.close();
            return;
        }
        self.start_mention(trigger, start, end, input, cwd);
    }

    fn sync_slash(&mut self, input: &str) {
        let stripped = &input[1..];
        let parts: Vec<&str> = stripped.split_whitespace().collect();
        let cmd_word = parts.first().copied().unwrap_or(stripped);
        let trailing_space = stripped.ends_with(char::is_whitespace);

        self.current_arg_count = if trailing_space {
            parts.len()
        } else {
            parts.len().saturating_sub(1)
        };

        self.nucleo.pattern.reparse(
            0,
            cmd_word,
            CaseMatching::Ignore,
            Normalization::Smart,
            false,
        );

        self.tick_nucleo();
    }

    fn start_mention(&mut self, trigger: char, start: usize, end: usize, input: &str, cwd: &str) {
        self.mention_range = Some((start, end));
        self.mention_items.clear();
        self.mention_query = input
            [TextBuffer::char_to_byte(input, start + 1)..TextBuffer::char_to_byte(input, end)]
            .to_string();
        self.filtered.clear();
        self.selected = 0;
        self.active_trigger = Some(trigger);

        if self.event_handle.is_disconnected() {
            self.mention_state = Some(MentionState::Settled);
        } else {
            self.mention_generation += 1;
            let rx = self.event_handle.resolve_completion(
                &trigger.to_string(),
                &self.mention_query,
                cwd,
            );
            self.mention_state = Some(MentionState::Pending {
                generation: self.mention_generation,
                rx,
            });
        }
    }

    fn apply_mention_reply(&mut self, reply: CompletionReply) {
        self.mention_items = reply
            .into_iter()
            .flat_map(|group| {
                group.items.into_iter().map(|item| MentionItem {
                    label: item.label,
                    insert: item.insert,
                    kind: item.kind,
                })
            })
            .collect();
        self.nucleo = Self::build_nucleo(
            &self.custom,
            &self.mcp_prompts,
            &self.lua_commands,
            &self.mention_items,
        );
        self.nucleo.pattern.reparse(
            0,
            &self.mention_query,
            CaseMatching::Ignore,
            Normalization::Smart,
            false,
        );
        self.tick_nucleo();
    }

    fn drain_mentions(&mut self) -> bool {
        let Some(MentionState::Pending { generation, rx }) = self.mention_state.take() else {
            return false;
        };
        if generation != self.mention_generation {
            return false;
        }
        let mut applied = false;
        loop {
            match rx.try_recv() {
                Ok(reply) => {
                    self.apply_mention_reply(reply);
                    applied = true;
                }
                Err(flume::TryRecvError::Empty) => break,
                Err(flume::TryRecvError::Disconnected) => break,
            }
        }
        if !applied {
            self.mention_state = Some(MentionState::Pending { generation, rx });
        }
        applied
    }

    pub fn tick(&mut self) -> Dirty {
        Dirty::from(self.drain_mentions())
    }

    fn tick_nucleo(&mut self) {
        loop {
            let status = self.nucleo.tick(TICK_TIMEOUT_MS);
            if status.changed {
                self.refresh_matches();
            }
            if !status.running {
                break;
            }
        }
    }

    fn refresh_matches(&mut self) {
        let snapshot = self.nucleo.snapshot();
        let pattern = snapshot.pattern();
        let has_pattern = !pattern.column_pattern(0).atoms.is_empty();
        let mention_mode = self.mention_trigger().is_some();

        self.filtered.clear();
        let count = snapshot.matched_item_count();
        for item in snapshot.matched_items(0..count) {
            let cmd_item = &item.data;
            let col = &item.matcher_columns[0];
            if matches!(cmd_item.command_type, CommandType::Mention(_)) != mention_mode {
                continue;
            }

            if self.current_arg_count > cmd_item.max_args {
                continue;
            }

            let indices = if has_pattern {
                let mut indices_buf = vec![];
                pattern.column_pattern(0).indices(
                    col.slice(..),
                    &mut self.matcher,
                    &mut indices_buf,
                );
                indices_buf
            } else {
                Vec::new()
            };

            self.filtered.push(Match {
                command_type: cmd_item.command_type.clone(),
                indices,
            });
        }

        self.selected = self.selected.min(self.filtered.len().saturating_sub(1));
    }

    pub fn close(&mut self) {
        self.filtered.clear();
        self.current_arg_count = 0;
        self.active_trigger = None;
        self.mention_state = None;
        self.mention_range = None;
    }

    pub fn move_up(&mut self) {
        if self.filtered.is_empty() {
            return;
        }
        self.selected = if self.selected == 0 {
            self.filtered.len() - 1
        } else {
            self.selected - 1
        };
    }

    pub fn move_down(&mut self) {
        if self.filtered.is_empty() {
            return;
        }
        self.selected = if self.selected == self.filtered.len() - 1 {
            0
        } else {
            self.selected + 1
        };
    }

    fn item_name(&self, m: &Match) -> String {
        match &m.command_type {
            CommandType::Builtin(cmd) => cmd.name.to_string(),
            CommandType::Custom(i) => self.custom[*i].display_name(),
            CommandType::McpPrompt(i) => format!("/{}", self.mcp_prompts[*i].display_name),
            CommandType::Lua(i) => self.lua_commands[*i].name.to_string(),
            CommandType::Mention(i) => self.mention_items[*i].label.clone(),
        }
    }

    fn item_has_args(&self, m: &Match) -> bool {
        match &m.command_type {
            CommandType::Builtin(cmd) => cmd.max_args > 0,
            CommandType::Custom(i) => self.custom[*i].has_args(),
            CommandType::McpPrompt(i) => !self.mcp_prompts[*i].arguments.is_empty(),
            CommandType::Lua(i) => self.lua_commands[*i].max_args > 0,
            CommandType::Mention(_) => false,
        }
    }

    fn item_description(&self, m: &Match) -> &str {
        match &m.command_type {
            CommandType::Builtin(cmd) => cmd.description,
            CommandType::Custom(i) => &self.custom[*i].description,
            CommandType::McpPrompt(i) => &self.mcp_prompts[*i].description,
            CommandType::Lua(i) => &self.lua_commands[*i].description,
            CommandType::Mention(_) => "",
        }
    }

    fn mention_item(&self, m: &Match) -> Option<&MentionItem> {
        match &m.command_type {
            CommandType::Mention(i) => self.mention_items.get(*i),
            _ => None,
        }
    }

    fn mention_trigger(&self) -> Option<char> {
        match self.active_trigger {
            Some(t) if t != '/' => Some(t),
            _ => None,
        }
    }

    fn complete_mention(&self, index: usize) -> CommandAction {
        let Some(item) = self.filtered.get(index).and_then(|m| self.mention_item(m)) else {
            return CommandAction::Consumed;
        };
        let Some((start, end)) = self.mention_range else {
            return CommandAction::Consumed;
        };
        let mut text = item.insert.clone();
        if !text.ends_with(char::is_whitespace) {
            text.push(' ');
        }
        CommandAction::CompleteRange { start, end, text }
    }

    pub fn confirm(&self, input: &str) -> Option<ParsedCommand> {
        let item = self.filtered.get(self.selected)?;
        let name = self.item_name(item);
        let args = input
            .strip_prefix('/')
            .and_then(|s| s.split_once(char::is_whitespace))
            .map(|(_, a)| a.trim())
            .unwrap_or("");
        Some(ParsedCommand {
            name,
            args: args.to_string(),
        })
    }

    /// Name lookup for `maki.api.run_command`, returning the registered
    /// spelling that [`crate::app::App`] dispatches on. Case-insensitive like
    /// typing, but never fuzzy: an alias names one command on purpose, and a
    /// typo should report itself instead of running the closest neighbor.
    pub fn resolve(&self, name: &str) -> Option<String> {
        Self::items(
            &self.custom,
            &self.mcp_prompts,
            &self.lua_commands,
            &self.mention_items,
        )
        .filter(|item| !matches!(item.command_type, CommandType::Mention(_)))
        .map(|item| item.name)
        .find(|n| n.eq_ignore_ascii_case(name))
    }

    pub fn find_custom_command(&self, display_name: &str) -> Option<&CustomCommand> {
        self.custom
            .iter()
            .find(|c| c.display_name() == display_name)
    }

    pub fn find_mcp_prompt(&self, slash_name: &str) -> Option<&McpPromptInfo> {
        let name = slash_name.strip_prefix('/')?;
        self.mcp_prompts.iter().find(|p| p.display_name == name)
    }

    pub fn find_lua_command(&self, name: &str) -> Option<&LuaCommandInfo> {
        self.lua_commands.iter().find(|c| c.name.as_ref() == name)
    }

    pub fn view(&self, frame: &mut Frame, input_area: Rect) -> Option<Rect> {
        if self.mention_trigger().is_some() {
            return self.view_mention(frame, input_area);
        }

        let filtered = &self.filtered;
        if filtered.is_empty() {
            return None;
        }

        let popup_height = (filtered.len() as u16).min(input_area.y);
        if popup_height == 0 {
            return None;
        }

        let max_name = filtered
            .iter()
            .map(|item| self.item_name(item).len())
            .max()
            .unwrap_or(0);
        let max_desc = filtered
            .iter()
            .map(|item| self.item_description(item).len())
            .max()
            .unwrap_or(0);
        let popup_width = (PAD + max_name + GAP + max_desc + PAD) as u16;

        let popup = Rect {
            x: input_area.x,
            y: input_area.y.saturating_sub(popup_height),
            width: popup_width.min(input_area.width),
            height: popup_height,
        };

        let t = theme::current();
        let lines: Vec<Line> = filtered
            .iter()
            .enumerate()
            .map(|(i, m)| {
                let name = self.item_name(m);
                let desc = self.item_description(m);
                let selected = i == self.selected;
                let name_pad = max_name - name.len() + GAP;

                if selected {
                    let s = t.item_selected;
                    let highlighted_name = self.build_highlighted_spans(&name, &m.indices, s);
                    let mut spans = vec![Span::styled(" ".repeat(PAD), s)];
                    spans.extend(highlighted_name);
                    spans.push(Span::styled(" ".repeat(name_pad), s));
                    spans.push(Span::styled(desc, s));
                    spans.push(Span::styled(" ".repeat(PAD), s));
                    Line::from(spans)
                } else {
                    let highlighted_name = self.build_highlighted_spans(&name, &m.indices, t.item);
                    let mut spans = vec![Span::raw(" ".repeat(PAD))];
                    spans.extend(highlighted_name);
                    spans.push(Span::raw(" ".repeat(name_pad)));
                    spans.push(Span::styled(desc, t.item_desc));
                    spans.push(Span::raw(" ".repeat(PAD)));
                    Line::from(spans)
                }
            })
            .collect();

        frame.render_widget(Clear, popup);
        frame.render_widget(
            Paragraph::new(lines).style(Style::new().bg(t.background)),
            popup,
        );

        Some(popup)
    }

    fn view_mention(&self, frame: &mut Frame, input_area: Rect) -> Option<Rect> {
        let rows: Vec<MentionRow<'_>> = if self.mention_pending() {
            vec![MentionRow::Status(MENTION_LOADING_ROW)]
        } else if self.filtered.is_empty() {
            vec![MentionRow::Status(MENTION_EMPTY_ROW)]
        } else {
            self.filtered
                .iter()
                .enumerate()
                .filter_map(|(i, m)| {
                    let item = self.mention_item(m)?;
                    Some(MentionRow::Item {
                        label: &item.label,
                        kind: &item.kind,
                        indices: &m.indices,
                        selected: i == self.selected,
                    })
                })
                .collect()
        };

        let (max_label, max_kind) = rows.iter().fold((0, 0), |(label, kind), row| match row {
            MentionRow::Status(text) => (label.max(text.len()), kind),
            MentionRow::Item {
                label: item_label,
                kind: item_kind,
                ..
            } => (label.max(item_label.len()), kind.max(item_kind.len())),
        });
        let popup_height = (rows.len() as u16).min(input_area.y);
        if popup_height == 0 {
            return None;
        }

        let popup = Rect {
            x: input_area.x,
            y: input_area.y.saturating_sub(popup_height),
            width: (PAD + max_label + GAP + max_kind + PAD).min(input_area.width as usize) as u16,
            height: popup_height,
        };

        let t = theme::current();
        let lines: Vec<Line> = rows
            .into_iter()
            .map(|row| match row {
                MentionRow::Status(text) => Line::from(vec![Span::styled(
                    format!("{}{}{}", " ".repeat(PAD), text, " ".repeat(PAD)),
                    t.item_desc,
                )]),
                MentionRow::Item {
                    label,
                    kind,
                    indices,
                    selected,
                } => {
                    let label_pad = max_label - label.len() + GAP;
                    if selected {
                        let s = t.item_selected;
                        let mut spans = vec![Span::styled(" ".repeat(PAD), s)];
                        spans.extend(self.build_highlighted_spans(label, indices, s));
                        spans.push(Span::styled(" ".repeat(label_pad), s));
                        spans.push(Span::styled(kind, s));
                        spans.push(Span::styled(" ".repeat(PAD), s));
                        Line::from(spans)
                    } else {
                        let mut spans = vec![Span::raw(" ".repeat(PAD))];
                        spans.extend(self.build_highlighted_spans(label, indices, t.item));
                        spans.push(Span::raw(" ".repeat(label_pad)));
                        spans.push(Span::styled(kind, t.item_desc));
                        spans.push(Span::raw(" ".repeat(PAD)));
                        Line::from(spans)
                    }
                }
            })
            .collect();

        frame.render_widget(Clear, popup);
        frame.render_widget(
            Paragraph::new(lines).style(Style::new().bg(t.background)),
            popup,
        );

        Some(popup)
    }

    fn build_highlighted_spans(&self, text: &str, indices: &[u32], base: Style) -> Vec<Span<'_>> {
        if indices.is_empty() {
            return vec![Span::styled(text.to_string(), base)];
        }

        let t = theme::current();
        let highlight = base
            .fg(t.accent.fg.unwrap_or_default())
            .add_modifier(Modifier::BOLD);

        let mut spans = Vec::new();
        let mut in_match = false;
        let mut run = String::new();

        for (i, ch) in text.chars().enumerate() {
            let is_match = indices.binary_search(&(i as u32)).is_ok();
            if is_match != in_match && !run.is_empty() {
                spans.push(Span::styled(
                    mem::take(&mut run),
                    if in_match { highlight } else { base },
                ));
            }
            in_match = is_match;
            run.push(ch);
        }

        if !run.is_empty() {
            spans.push(Span::styled(run, if in_match { highlight } else { base }));
        }

        spans
    }
}

enum MentionRow<'a> {
    Status(&'static str),
    Item {
        label: &'a str,
        kind: &'a str,
        indices: &'a [u32],
        selected: bool,
    },
}

/// The cursor token is the contiguous non-whitespace run containing
/// `cursor` (a char index; the position right after the last char counts
/// as inside the trailing token). When the token's first char is a
/// registered trigger, returns (trigger char, token start, token end) in
/// char indices. A token mid-word (e.g. `user@example.com`) never
/// triggers.
fn trigger_at(input: &str, cursor: usize, triggers: &[String]) -> Option<(char, usize, usize)> {
    let chars: Vec<char> = input.chars().collect();
    let len = chars.len();
    let cursor = cursor.min(len);
    if cursor < len && chars[cursor].is_whitespace() {
        return None;
    }
    let mut start = cursor;
    while start > 0 && !chars[start - 1].is_whitespace() {
        start -= 1;
    }
    let mut end = cursor;
    while end < len && !chars[end].is_whitespace() {
        end += 1;
    }
    if start == end {
        return None;
    }
    let trigger = chars[start];
    let registered = trigger == '/' || triggers.iter().any(|t| t.starts_with(trigger));
    registered.then_some((trigger, start, end))
}

#[cfg(test)]
mod tests {
    use super::*;
    use maki_agent::{McpPromptArg, McpSnapshot};
    use test_case::test_case;

    fn empty_snapshot() -> McpSnapshotReader {
        McpSnapshotReader::empty()
    }

    fn synced(input: &str) -> CommandPalette {
        let mut p = CommandPalette::new(
            Arc::from([]),
            empty_snapshot(),
            LuaCommandReader::empty(),
            TriggerSnapshotReader::empty(),
            EventHandle::disconnected_for_test(),
        );
        p.sync(input, input.chars().count(), "/tmp");
        p
    }

    fn synced_with_custom(input: &str, custom: Arc<[CustomCommand]>) -> CommandPalette {
        let mut p = CommandPalette::new(
            custom,
            empty_snapshot(),
            LuaCommandReader::empty(),
            TriggerSnapshotReader::empty(),
            EventHandle::disconnected_for_test(),
        );
        p.sync(input, input.chars().count(), "/tmp");
        p
    }

    fn sample_custom() -> Arc<[CustomCommand]> {
        Arc::from([
            CustomCommand {
                name: "review".into(),
                description: "Code review".into(),
                content: "Review $ARGUMENTS".into(),
                scope: maki_agent::command::CommandScope::Project,
                accepts_args: true,
            },
            CustomCommand {
                name: "fix".into(),
                description: "Quick fix".into(),
                content: "Fix the code".into(),
                scope: maki_agent::command::CommandScope::User,
                accepts_args: false,
            },
        ])
    }

    #[test]
    fn slash_shows_builtins_plus_extras() {
        let builtin_count = synced("/").filtered.len();
        assert!(builtin_count > 0);

        let with_custom = synced_with_custom("/", sample_custom());
        assert_eq!(with_custom.filtered.len(), builtin_count + 2);

        let with_prompts = synced_with_prompts("/");
        assert_eq!(with_prompts.filtered.len(), builtin_count + 2);
    }

    #[test]
    fn close_deactivates() {
        let mut p = synced("/");
        p.close();
        assert!(!p.is_active());
    }

    #[test_case("/mp", true ; "compact_substring")]
    #[test_case("/ew", true ; "lowercase_substring")]
    #[test_case("/EW", true ; "uppercase_substring")]
    #[test_case("/zzz", false ; "no_match")]
    fn filter_by_substring(input: &str, expect_active: bool) {
        let p = synced(input);
        assert_eq!(p.is_active(), expect_active);
    }

    #[test]
    fn filter_custom_by_substring() {
        let p = synced_with_custom("/review", sample_custom());
        assert!(p.is_active());
        assert_eq!(p.filtered.len(), 1);
        assert!(matches!(p.filtered[0].command_type, CommandType::Custom(0)));
    }

    #[test]
    fn navigation_wraps() {
        let mut p = synced("/");
        p.move_up();
        assert_eq!(p.selected, p.filtered.len() - 1);
        p.move_down();
        assert_eq!(p.selected, 0);
    }

    #[test]
    fn confirm_when_inactive_returns_none() {
        let p = CommandPalette::new(
            Arc::from([]),
            empty_snapshot(),
            LuaCommandReader::empty(),
            TriggerSnapshotReader::empty(),
            EventHandle::disconnected_for_test(),
        );
        assert!(p.confirm("").is_none());
    }

    #[test]
    fn sync_clamps_selected() {
        let mut p = synced("/");
        p.selected = 100;
        p.sync("/", 1, "/tmp");
        assert_eq!(p.selected, p.filtered.len() - 1);
    }

    #[test]
    fn sync_filters_on_first_word_only() {
        let p = synced("/cd ~/foo");
        assert!(p.is_active());
        assert_eq!(p.filtered.len(), 1);
        let name = p.item_name(&p.filtered[0]);
        assert_eq!(name, "/cd");
    }

    #[test_case("/compact ", false ; "zero_arg_cmd_with_space")]
    #[test_case("/tasks ", false   ; "zero_arg_tasks_with_space")]
    #[test_case("/cd ", true        ; "one_arg_cmd_with_space")]
    #[test_case("/cd ~/foo", true   ; "one_arg_cmd_mid_arg")]
    #[test_case("/cd  ~/foo", true  ; "one_arg_cmd_double_space")]
    #[test_case("/cd ~/foo ", false ; "one_arg_cmd_second_space")]
    #[test_case("/btw hello world", true ; "btw_stays_active_with_many_args")]
    fn sync_respects_nargs(input: &str, expect_active: bool) {
        let p = synced(input);
        assert_eq!(p.is_active(), expect_active);
    }

    #[test]
    fn custom_command_with_args_stays_active() {
        let p = synced_with_custom("/project:review some args", sample_custom());
        assert!(p.is_active());
    }

    #[test]
    fn custom_command_without_args_hides_on_space() {
        let p = synced_with_custom("/user:fix ", sample_custom());
        assert!(!p.is_active());
    }

    #[test_case("/cd", "/cd", ""              ; "no_args")]
    #[test_case("/cd ~/foo", "/cd", "~/foo"   ; "with_args")]
    #[test_case("/CD ~/foo", "/cd", "~/foo"   ; "case_insensitive")]
    #[test_case("/compact", "/compact", ""    ; "other_command")]
    #[test_case("/cmp", "/compact", ""    ; "fuzzy-match-1")]
    #[test_case("/pct", "/compact", ""    ; "fuzzy-match-2")]
    #[test_case("/btw hello world", "/btw", "hello world" ; "btw_multi_word")]
    fn confirm_parses_args(input: &str, expected_name: &str, expected_args: &str) {
        let mut p = CommandPalette::new(
            Arc::from([]),
            empty_snapshot(),
            LuaCommandReader::empty(),
            TriggerSnapshotReader::empty(),
            EventHandle::disconnected_for_test(),
        );
        p.sync(input, input.chars().count(), "/tmp");
        let cmd = p.confirm(input).unwrap();
        assert_eq!(cmd.name, expected_name);
        assert_eq!(cmd.args, expected_args);
    }

    #[test]
    fn confirm_custom_command() {
        let custom = sample_custom();
        let mut p = CommandPalette::new(
            custom,
            empty_snapshot(),
            LuaCommandReader::empty(),
            TriggerSnapshotReader::empty(),
            EventHandle::disconnected_for_test(),
        );
        p.sync("/project:review", 15, "/tmp");
        assert!(p.is_active());
        let cmd = p.confirm("/project:review some-file.rs").unwrap();
        assert_eq!(cmd.name, "/project:review");
        assert_eq!(cmd.args, "some-file.rs");
    }

    #[test]
    fn find_custom_command_lookup() {
        let custom = sample_custom();
        let p = CommandPalette::new(
            custom,
            empty_snapshot(),
            LuaCommandReader::empty(),
            TriggerSnapshotReader::empty(),
            EventHandle::disconnected_for_test(),
        );
        let found = p.find_custom_command("/project:review");
        assert!(found.is_some());
        assert_eq!(found.unwrap().content, "Review $ARGUMENTS");
        assert!(p.find_custom_command("/nonexistent").is_none());
    }

    fn sample_prompts() -> McpSnapshotReader {
        McpSnapshotReader::from_snapshot(McpSnapshot {
            infos: vec![],
            prompts: vec![
                McpPromptInfo {
                    display_name: "myserver:code-review".into(),
                    qualified_name: "myserver/code-review".into(),
                    description: "Review code changes".into(),
                    arguments: vec![McpPromptArg {
                        name: "diff".into(),
                        description: "The diff".into(),
                        required: true,
                    }],
                },
                McpPromptInfo {
                    display_name: "myserver:summarize".into(),
                    qualified_name: "myserver/summarize".into(),
                    description: "Summarize text".into(),
                    arguments: vec![],
                },
            ],
            pids: vec![],
            generation: 0,
        })
    }

    fn synced_with_prompts(input: &str) -> CommandPalette {
        let mut p = CommandPalette::new(
            Arc::from([]),
            sample_prompts(),
            LuaCommandReader::empty(),
            TriggerSnapshotReader::empty(),
            EventHandle::disconnected_for_test(),
        );
        p.sync(input, input.chars().count(), "/tmp");
        p
    }

    #[test]
    fn filter_mcp_prompt_by_substring() {
        let p = synced_with_prompts("/code");
        assert!(p.is_active());
        assert_eq!(p.filtered.len(), 1);
        assert!(matches!(
            p.filtered[0].command_type,
            CommandType::McpPrompt(0)
        ));
    }

    #[test]
    fn mcp_prompt_with_args_stays_active() {
        let p = synced_with_prompts("/myserver:code-review some diff");
        assert!(p.is_active());
    }

    #[test]
    fn mcp_prompt_without_args_hides_on_space() {
        let p = synced_with_prompts("/myserver:summarize ");
        assert!(
            !p.filtered
                .iter()
                .any(|f| matches!(f.command_type, CommandType::McpPrompt(1)))
        );
    }

    #[test]
    fn find_mcp_prompt_lookup() {
        let p = synced_with_prompts("/");
        let found = p.find_mcp_prompt("/myserver:code-review");
        assert!(found.is_some());
        assert_eq!(found.unwrap().qualified_name, "myserver/code-review");
        assert!(p.find_mcp_prompt("/nonexistent").is_none());
    }

    #[test]
    fn confirm_mcp_prompt_parses_args() {
        let input = "/myserver:code-review my-diff-content";
        let mut p = synced_with_prompts(input);
        p.selected = p
            .filtered
            .iter()
            .position(|f| matches!(f.command_type, CommandType::McpPrompt(0)))
            .unwrap();
        let cmd = p.confirm(input).unwrap();
        assert_eq!(cmd.name, "/myserver:code-review");
        assert_eq!(cmd.args, "my-diff-content");
    }

    #[test]
    fn mcp_update_clears_old_prompts() {
        let reader = sample_prompts();
        let mut p = CommandPalette::new(
            Arc::from([]),
            reader,
            LuaCommandReader::empty(),
            TriggerSnapshotReader::empty(),
            EventHandle::disconnected_for_test(),
        );

        p.sync("/", 1, "/tmp");
        let initial_count = p
            .filtered
            .iter()
            .filter(|f| matches!(f.command_type, CommandType::McpPrompt(_)))
            .count();
        assert_eq!(initial_count, 2, "Should have 2 MCP prompts initially");

        let updated_reader = McpSnapshotReader::from_snapshot(McpSnapshot {
            infos: vec![],
            prompts: vec![McpPromptInfo {
                display_name: "myserver:new-prompt".into(),
                qualified_name: "myserver/new-prompt".into(),
                description: "A new prompt".into(),
                arguments: vec![],
            }],
            pids: vec![],
            generation: 1,
        });

        p.mcp_reader = updated_reader;
        p.sync("/", 1, "/tmp");

        let updated_count = p
            .filtered
            .iter()
            .filter(|f| matches!(f.command_type, CommandType::McpPrompt(_)))
            .count();
        assert_eq!(
            updated_count, 1,
            "Should have only 1 MCP prompt after update"
        );

        assert!(!p.filtered.is_empty(), "Should have filtered results");
        let prompt = &p
            .filtered
            .iter()
            .find(|f| matches!(f.command_type, CommandType::McpPrompt(_)))
            .expect("Should have at least one MCP prompt");
        match &prompt.command_type {
            CommandType::McpPrompt(i) => {
                assert_eq!(p.mcp_prompts[*i].display_name, "myserver:new-prompt");
            }
            _ => panic!("Should have MCP prompt"),
        }
    }

    #[test_case("/cmp", "/compact" ; "compact_fuzzy")]
    #[test_case("/new", "/new" ; "new_exact")]
    #[test_case("/tsk", "/tasks" ; "tasks_fuzzy")]
    fn nucleo_highlights_matching_indices(input: &str, expected_cmd: &str) {
        let p = synced(input);
        assert!(p.is_active(), "Input '{}' should activate palette", input);
        // Find the expected match
        let matched = p
            .filtered
            .iter()
            .find(|m| p.item_name(m) == expected_cmd)
            .unwrap_or_else(|| panic!("Should find {} for input {}", expected_cmd, input));
        // Should have some highlight indices
        assert!(
            !matched.indices.is_empty(),
            "Match should have highlight indices"
        );
    }

    fn sample_lua_commands() -> LuaCommandReader {
        LuaCommandReader::from_commands(vec![
            LuaCommandInfo {
                name: Arc::from("/memory"),
                description: Arc::from("View memory files"),
                plugin: Arc::from("memory"),
                max_args: 0,
            },
            LuaCommandInfo {
                name: Arc::from("/deploy"),
                description: Arc::from("Deploy the project"),
                plugin: Arc::from("deploy_plugin"),
                max_args: 0,
            },
        ])
    }

    fn synced_with_nargs(input: &str, max_args: usize) -> CommandPalette {
        let reader = LuaCommandReader::from_commands(vec![LuaCommandInfo {
            name: Arc::from("/rename"),
            description: Arc::from("Rename the current session"),
            plugin: Arc::from("sessions"),
            max_args,
        }]);
        let mut p = CommandPalette::new(
            Arc::from([]),
            empty_snapshot(),
            reader,
            TriggerSnapshotReader::empty(),
            EventHandle::disconnected_for_test(),
        );
        p.sync(input, input.chars().count(), "/tmp");
        p
    }

    #[test_case("/rename", usize::MAX, true           ; "nargs_plus_no_args")]
    #[test_case("/rename ", usize::MAX, true          ; "nargs_plus_trailing_space")]
    #[test_case("/rename my title", usize::MAX, true  ; "nargs_plus_multi_word")]
    #[test_case("/rename title", 1, true              ; "nargs_one_single_word")]
    #[test_case("/rename my title", 1, false          ; "nargs_one_too_many")]
    #[test_case("/rename", 0, true                    ; "nargs_zero_no_args")]
    #[test_case("/rename title", 0, false             ; "nargs_zero_with_arg")]
    fn lua_command_respects_nargs(input: &str, max_args: usize, expect_active: bool) {
        assert_eq!(
            synced_with_nargs(input, max_args).is_active(),
            expect_active
        );
    }

    #[test]
    fn confirm_lua_command_keeps_multi_word_args() {
        let input = "/rename my new title";
        let cmd = synced_with_nargs(input, usize::MAX).confirm(input).unwrap();
        assert_eq!(cmd.name, "/rename");
        assert_eq!(cmd.args, "my new title");
    }

    fn synced_with_lua(input: &str) -> CommandPalette {
        let mut p = CommandPalette::new(
            Arc::from([]),
            empty_snapshot(),
            sample_lua_commands(),
            TriggerSnapshotReader::empty(),
            EventHandle::disconnected_for_test(),
        );
        p.sync(input, input.chars().count(), "/tmp");
        p
    }

    #[test]
    fn lua_commands_appear_in_unfiltered_list() {
        let p = synced_with_lua("/");
        let lua_count = p
            .filtered
            .iter()
            .filter(|f| matches!(f.command_type, CommandType::Lua(_)))
            .count();
        assert_eq!(lua_count, 2);
    }

    #[test]
    fn lua_command_filtered_by_substring() {
        let p = synced_with_lua("/mem");
        assert!(p.is_active());
        let found = p
            .filtered
            .iter()
            .any(|f| matches!(f.command_type, CommandType::Lua(_)) && p.item_name(f) == "/memory");
        assert!(found);
    }

    #[test]
    fn find_lua_command_returns_matching_entry() {
        let p = synced_with_lua("/");
        let found = p.find_lua_command("/memory");
        assert!(found.is_some());
        assert_eq!(found.unwrap().plugin.as_ref(), "memory");
        assert!(p.find_lua_command("/nonexistent").is_none());
    }

    #[test]
    fn confirm_lua_command_parses_args() {
        let mut p = CommandPalette::new(
            Arc::from([]),
            empty_snapshot(),
            sample_lua_commands(),
            TriggerSnapshotReader::empty(),
            EventHandle::disconnected_for_test(),
        );
        p.sync("/memory", 7, "/tmp");
        let cmd = p.confirm("/memory some-arg").unwrap();
        assert_eq!(cmd.name, "/memory");
        assert_eq!(cmd.args, "some-arg");
    }

    #[test]
    fn lua_commands_update_on_generation_change() {
        let (writer, reader) = maki_lua::test_support::lua_command_writer_pair();
        writer.publish(vec![LuaCommandInfo {
            name: Arc::from("/old"),
            description: Arc::from("old command"),
            plugin: Arc::from("p"),
            max_args: 0,
        }]);
        let mut p = CommandPalette::new(
            Arc::from([]),
            empty_snapshot(),
            reader,
            TriggerSnapshotReader::empty(),
            EventHandle::disconnected_for_test(),
        );
        p.sync("/", 1, "/tmp");
        let initial_lua = p
            .filtered
            .iter()
            .filter(|f| matches!(f.command_type, CommandType::Lua(_)))
            .count();
        assert_eq!(initial_lua, 1);

        writer.publish(vec![
            LuaCommandInfo {
                name: Arc::from("/new1"),
                description: Arc::from("new"),
                plugin: Arc::from("p"),
                max_args: 0,
            },
            LuaCommandInfo {
                name: Arc::from("/new2"),
                description: Arc::from("new2"),
                plugin: Arc::from("p"),
                max_args: 0,
            },
        ]);
        p.sync("/", 1, "/tmp");
        let updated_lua = p
            .filtered
            .iter()
            .filter(|f| matches!(f.command_type, CommandType::Lua(_)))
            .count();
        assert_eq!(updated_lua, 2);
        assert!(p.find_lua_command("/old").is_none());
        assert!(p.find_lua_command("/new1").is_some());
    }

    fn trigger_list(triggers: &[&str]) -> Vec<String> {
        triggers.iter().map(|t| t.to_string()).collect()
    }

    fn palette_with_triggers(triggers: &[&str]) -> CommandPalette {
        CommandPalette::new(
            Arc::from([]),
            empty_snapshot(),
            LuaCommandReader::empty(),
            TriggerSnapshotReader::empty(),
            EventHandle::disconnected_for_test(),
        )
        .with_triggers(triggers)
    }

    impl CommandPalette {
        fn with_triggers(mut self, triggers: &[&str]) -> Self {
            self.triggers = trigger_list(triggers);
            self
        }
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, crossterm::event::KeyModifiers::NONE)
    }

    #[test_case("@foo", 0, Some(('@', 0, 4))      ; "cursor_at_trigger")]
    #[test_case("@foo", 2, Some(('@', 0, 4))      ; "cursor_mid_token")]
    #[test_case("@foo", 4, Some(('@', 0, 4))      ; "cursor_at_token_end")]
    #[test_case("@", 1, Some(('@', 0, 1))         ; "trigger_as_last_char")]
    #[test_case("a @foo", 4, Some(('@', 2, 6))    ; "mid_line_after_space")]
    #[test_case("  @foo", 2, Some(('@', 2, 6))    ; "leading_space_token")]
    #[test_case("user@example.com", 4, None       ; "email_at_mid_word")]
    #[test_case("a@b", 1, None                     ; "trigger_mid_token")]
    #[test_case("$foo", 0, None                    ; "unregistered_trigger")]
    #[test_case("", 0, None                        ; "empty_input")]
    #[test_case("foo ", 4, None                    ; "cursor_on_space")]
    fn trigger_detection(input: &str, cursor: usize, expected: Option<(char, usize, usize)>) {
        assert_eq!(trigger_at(input, cursor, &trigger_list(&["@"])), expected);
    }

    #[test]
    fn slash_trigger_is_registered_for_detection() {
        let registered = trigger_list(&["@"]);
        assert_eq!(trigger_at("/new", 1, &registered), Some(('/', 0, 4)));
        assert_eq!(trigger_at("a /new", 2, &registered), Some(('/', 2, 6)));
    }

    #[test]
    fn slash_trigger_mid_input_stays_closed() {
        let mut p = palette_with_triggers(&["@"]);
        p.sync("a /new", 2, "/tmp");
        assert!(!p.is_active(), "slash commands only open at input start");
    }

    #[test]
    fn mention_sync_enters_pending_and_fires_request() {
        let (handle, probe) = maki_lua::test_support::probed_event_handle();
        let mut p = CommandPalette::new(
            Arc::from([]),
            empty_snapshot(),
            LuaCommandReader::empty(),
            TriggerSnapshotReader::empty(),
            handle,
        )
        .with_triggers(&["@"]);
        p.sync("@src/fo", 7, "/tmp");
        assert!(p.is_active());
        assert!(p.mention_pending());
        assert_eq!(p.mention_range, Some((0, 7)));
        assert!(probe.try_recv().is_some(), "resolve request fired");
    }

    #[test]
    fn mention_with_disconnected_handle_settles_empty() {
        let mut p = palette_with_triggers(&["@"]);
        p.sync("@src/fo", 7, "/tmp");
        assert!(p.is_active());
        assert!(!p.mention_pending());
        assert_eq!(p.match_count(), 0);
    }

    #[test]
    fn mention_enter_consumed_while_pending() {
        let mut p = palette_with_triggers(&["@"]);
        p.active_trigger = Some('@');
        p.mention_range = Some((0, 8));
        p.mention_state = Some(MentionState::Pending {
            generation: 1,
            rx: flume::bounded(1).1,
        });
        assert!(matches!(
            p.handle_key(key(KeyCode::Enter), "@src/fo"),
            CommandAction::Consumed
        ));
    }

    #[test]
    fn mention_enter_without_match_is_consumed() {
        let mut p = palette_with_triggers(&["@"]);
        p.active_trigger = Some('@');
        p.mention_range = Some((0, 8));
        p.mention_state = Some(MentionState::Settled);
        assert!(matches!(
            p.handle_key(key(KeyCode::Enter), "@src/fo"),
            CommandAction::Consumed
        ));
        assert!(p.is_active(), "popup stays open on consumed Enter");
    }

    #[test]
    fn mention_enter_completes_selected_range() {
        let mut p = palette_with_triggers(&["@"]);
        p.active_trigger = Some('@');
        p.mention_range = Some((2, 8));
        p.mention_state = Some(MentionState::Settled);
        p.mention_items = vec![MentionItem {
            label: "src/foo.rs".into(),
            insert: "@src/foo.rs".into(),
            kind: "file".into(),
        }];
        p.filtered = vec![Match {
            command_type: CommandType::Mention(0),
            indices: vec![],
        }];
        let action = p.handle_key(key(KeyCode::Enter), "hi @src/fo");
        assert!(matches!(
            action,
            CommandAction::CompleteRange { start: 2, end: 8, text } if text == "@src/foo.rs "
        ));
    }

    #[test]
    fn mention_enter_keeps_insert_space_when_present() {
        let mut p = palette_with_triggers(&["@"]);
        p.active_trigger = Some('@');
        p.mention_range = Some((0, 1));
        p.mention_state = Some(MentionState::Settled);
        p.mention_items = vec![MentionItem {
            label: "src/".into(),
            insert: "@src/ ".into(),
            kind: "dir".into(),
        }];
        p.filtered = vec![Match {
            command_type: CommandType::Mention(0),
            indices: vec![],
        }];
        let action = p.handle_key(key(KeyCode::Enter), "@");
        assert!(matches!(
            action,
            CommandAction::CompleteRange { start: 0, end: 1, text } if text == "@src/ "
        ));
    }

    #[test]
    fn mention_tab_completes_top_match() {
        let mut p = palette_with_triggers(&["@"]);
        p.active_trigger = Some('@');
        p.mention_range = Some((0, 6));
        p.mention_state = Some(MentionState::Settled);
        p.mention_items = vec![
            MentionItem {
                label: "top".into(),
                insert: "@top".into(),
                kind: "file".into(),
            },
            MentionItem {
                label: "bottom".into(),
                insert: "@bottom".into(),
                kind: "file".into(),
            },
        ];
        p.filtered = vec![
            Match {
                command_type: CommandType::Mention(0),
                indices: vec![],
            },
            Match {
                command_type: CommandType::Mention(1),
                indices: vec![],
            },
        ];
        p.selected = 1;
        let action = p.handle_key(key(KeyCode::Tab), "@topx");
        assert!(matches!(
            action,
            CommandAction::CompleteRange { start: 0, end: 6, text } if text == "@top "
        ));
    }

    #[test]
    fn mention_esc_closes() {
        let mut p = palette_with_triggers(&["@"]);
        p.sync("@src", 4, "/tmp");
        assert!(p.is_active());
        p.handle_key(key(KeyCode::Esc), "@src");
        assert!(!p.is_active());
    }

    #[test]
    fn stale_mention_reply_is_dropped() {
        let mut p = palette_with_triggers(&["@"]);
        let (tx, rx) = flume::bounded(1);
        p.active_trigger = Some('@');
        p.mention_range = Some((0, 5));
        p.mention_generation = 2;
        p.mention_state = Some(MentionState::Pending { generation: 1, rx });
        tx.send(vec![]).unwrap();
        let _ = p.tick();
        assert!(!p.mention_pending(), "stale reply never settles the popup");
        assert_eq!(p.match_count(), 0);
        assert!(p.mention_items.is_empty());
    }

    #[test]
    fn fresh_mention_reply_populates_corpus() {
        let mut p = palette_with_triggers(&["@"]);
        let (tx, rx) = flume::bounded(1);
        p.active_trigger = Some('@');
        p.mention_range = Some((0, 8));
        p.mention_query = "src/fo".into();
        p.mention_generation = 1;
        p.mention_state = Some(MentionState::Pending { generation: 1, rx });
        tx.send(vec![maki_lua::CompletionGroup {
            provider: "picker".into(),
            items: vec![maki_lua::CompletionItem {
                label: "src/foo.rs".into(),
                insert: "@src/foo.rs".into(),
                kind: "file".into(),
            }],
        }])
        .unwrap();
        assert_eq!(p.tick(), Dirty::YES, "reply owes a frame");
        assert!(!p.mention_pending());
        assert_eq!(p.match_count(), 1);
        assert_eq!(p.match_label(0).as_deref(), Some("src/foo.rs"));
        let action = p.handle_key(key(KeyCode::Enter), "@src/fo");
        assert!(matches!(
            action,
            CommandAction::CompleteRange { start: 0, end: 8, text } if text == "@src/foo.rs "
        ));
    }

    #[test]
    fn mention_mode_filters_out_commands() {
        let mut p = palette_with_triggers(&["@"]);
        p.active_trigger = Some('@');
        p.mention_range = Some((0, 4));
        p.mention_query = "new".into();
        p.mention_items = vec![MentionItem {
            label: "new-file.txt".into(),
            insert: "@new-file.txt".into(),
            kind: "file".into(),
        }];
        p.nucleo = CommandPalette::build_nucleo(
            &p.custom,
            &p.mcp_prompts,
            &p.lua_commands,
            &p.mention_items,
        );
        p.nucleo
            .pattern
            .reparse(0, "new", CaseMatching::Ignore, Normalization::Smart, false);
        p.tick_nucleo();
        assert_eq!(p.match_count(), 1);
        assert!(matches!(
            p.filtered[0].command_type,
            CommandType::Mention(0)
        ));
    }

    #[test]
    fn slash_mode_ignores_mention_items() {
        let mut p = palette_with_triggers(&["@"]);
        p.mention_items = vec![MentionItem {
            label: "compact".into(),
            insert: "@compact".into(),
            kind: "file".into(),
        }];
        p.sync("/cmp", 4, "/tmp");
        assert!(p.is_active());
        assert_eq!(p.filtered.len(), 1);
        assert!(matches!(
            p.filtered[0].command_type,
            CommandType::Builtin(_)
        ));
    }

    #[test]
    fn trigger_at_after_mention_range_moves_out() {
        let mut p = palette_with_triggers(&["@"]);
        p.sync("@src", 4, "/tmp");
        assert!(p.is_active());
        p.sync("@src ", 5, "/tmp");
        assert!(!p.is_active(), "space after token closes the popup");
    }
}
