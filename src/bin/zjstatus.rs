use zellij_tile::prelude::*;

use std::{collections::BTreeMap, path::PathBuf, sync::Arc};

use zjstatus::{
    config::{self, ModuleConfig, UpdateEventMask, ZellijState},
    frames, pipe,
    widgets::{
        command::{CommandWidget, store_command_result},
        datetime::DateTimeWidget,
        mode::ModeWidget,
        notification::NotificationWidget,
        pipe::PipeWidget,
        session::SessionWidget,
        swap_layout::SwapLayoutWidget,
        tabs::TabsWidget,
        widget::Widget,
    },
};

// Matches the old incidental Zellij session scan cadence.
const REFRESH_INTERVAL_SECONDS: f64 = 1.0;
const VIEW_REQUEST_PIPE: &str = "zjstatus.view_request.v1";
const VIEW_FRAME_PIPE: &str = "zjstatus.view_frame.v1";
const CONTROLLER_READY_PIPE: &str = "zjstatus.controller_ready.v1";
const MAX_BAR_WIDTH: usize = 10_000;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Role {
    #[default]
    Standalone,
    Controller,
    View,
}

impl Role {
    fn from_config(configuration: &BTreeMap<String, String>) -> Self {
        match configuration.get("role").map(String::as_str) {
            Some("controller") => Self::Controller,
            Some("view") => Self::View,
            _ => Self::Standalone,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ViewRequest {
    Width(usize),
    LeftClick(isize, usize),
    RightClick(isize, usize),
}

fn parse_view_request(raw: &str) -> Option<ViewRequest> {
    let mut fields = raw.split(':');
    let kind = fields.next()?;
    let request = match kind {
        "width" => {
            let width = fields.next()?.parse().ok()?;
            (width > 0 && width <= MAX_BAR_WIDTH).then_some(ViewRequest::Width(width))?
        }
        "left" | "right" => {
            let line = fields.next()?.parse().ok()?;
            let col = fields.next()?.parse().ok()?;
            if kind == "left" {
                ViewRequest::LeftClick(line, col)
            } else {
                ViewRequest::RightClick(line, col)
            }
        }
        _ => return None,
    };
    fields.next().is_none().then_some(request)
}

#[derive(Default)]
struct State {
    pending_events: Vec<Event>,
    got_permissions: bool,
    state: ZellijState,
    userspace_configuration: BTreeMap<String, String>,
    module_config: config::ModuleConfig,
    widget_map: BTreeMap<String, Arc<dyn Widget>>,
    focus_cwd_commands: Vec<String>,
    err: Option<anyhow::Error>,
    role: Role,
    view_width: Option<usize>,
    view_frame: String,
    views: BTreeMap<u32, usize>,
}

#[cfg(not(test))]
register_plugin!(State);

#[cfg(feature = "tracing")]
fn init_tracing() {
    use std::fs::File;
    use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

    let file = File::create("/host/.zjstatus.log");
    let file = match file {
        Ok(file) => file,
        Err(error) => panic!("Error: {:?}", error),
    };
    let debug_log = tracing_subscriber::fmt::layer().with_writer(Arc::new(file));

    tracing_subscriber::registry().with(debug_log).init();

    tracing::info!("tracing initialized");
}

impl ZellijPlugin for State {
    fn load(&mut self, configuration: BTreeMap<String, String>) {
        #[cfg(feature = "tracing")]
        init_tracing();

        self.role = Role::from_config(&configuration);
        if self.role == Role::View {
            request_permission(&[
                PermissionType::ReadApplicationState,
                PermissionType::ChangeApplicationState,
                PermissionType::RunCommands,
                PermissionType::MessageAndLaunchOtherPlugins,
            ]);
            subscribe(&[EventType::Mouse, EventType::PermissionRequestResult]);
            self.got_permissions = false;
            self.view_width = None;
            self.view_frame.clear();
            return;
        }

        let mut permissions = vec![
            PermissionType::ReadApplicationState,
            PermissionType::ChangeApplicationState,
            PermissionType::RunCommands,
        ];
        if self.role == Role::Controller {
            permissions.push(PermissionType::MessageAndLaunchOtherPlugins);
        }
        request_permission(&permissions);
        subscribe(&[
            EventType::Mouse,
            EventType::ModeUpdate,
            EventType::PaneUpdate,
            EventType::PermissionRequestResult,
            EventType::Timer,
            EventType::TabUpdate,
            EventType::SessionUpdate,
            EventType::RunCommandResult,
            EventType::CwdChanged,
            EventType::HostTerminalThemeChanged,
        ]);
        set_timeout(REFRESH_INTERVAL_SECONDS);

        let active_configuration = config::configured_host_theme_mode(&configuration)
            .map(|mode| config::host_theme_configuration(&configuration, mode))
            .unwrap_or_else(|| configuration.clone());
        self.module_config = match ModuleConfig::new(&active_configuration) {
            Ok(mc) => mc,
            Err(e) => {
                self.err = Some(e);
                return;
            }
        };
        self.widget_map = register_widgets(&active_configuration);
        self.focus_cwd_commands =
            zjstatus::widgets::command::focus_cwd_command_names(&active_configuration);
        self.userspace_configuration = configuration;
        self.pending_events = Vec::new();
        self.got_permissions = false;
        self.views.clear();
        self.state = ZellijState {
            cols: 0,
            tab_width_limit: None,
            command_results: BTreeMap::new(),
            pipe_results: BTreeMap::new(),
            mode: ModeInfo::default(),
            panes: PaneManifest::default(),
            tabs: Vec::new(),
            sessions: Vec::new(),
            cache_mask: 0,
            incoming_notification: None,
            focused_pane_id: None,
            focused_pane_cwd: None,
        };
    }

    fn pipe(&mut self, pipe_message: PipeMessage) -> bool {
        if self.role == Role::View {
            if pipe_message.name == VIEW_FRAME_PIPE {
                if let Some(frame) = pipe_message.payload {
                    self.view_frame = frame;
                    return true;
                }
            } else if pipe_message.name == CONTROLLER_READY_PIPE && self.got_permissions {
                self.request_frame();
            }
            return false;
        }
        if self.role == Role::Controller && pipe_message.name == VIEW_REQUEST_PIPE {
            return self.handle_view_request(&pipe_message);
        }

        let mut should_render = false;

        match pipe_message.source {
            PipeSource::Cli(_) => {
                if let Some(input) = pipe_message.payload {
                    should_render = pipe::parse_protocol(&mut self.state, &input);
                }
            }
            PipeSource::Plugin(_) => {
                if let Some(input) = pipe_message.payload {
                    should_render = pipe::parse_protocol(&mut self.state, &input);
                }
            }
            PipeSource::Keybind => {
                if let Some(input) = pipe_message.payload {
                    should_render = pipe::parse_protocol(&mut self.state, &input);
                }
            }
        }

        if self.role == Role::Controller && should_render {
            self.publish_views();
            false
        } else {
            should_render
        }
    }

    #[tracing::instrument(skip_all, fields(event_type))]
    fn update(&mut self, event: Event) -> bool {
        if self.role == Role::View {
            return self.update_view(event);
        }

        let mut should_render = false;
        if let Event::PermissionRequestResult(PermissionStatus::Granted) = event {
            self.got_permissions = true;

            for event in std::mem::take(&mut self.pending_events) {
                tracing::debug!("processing cached event");
                should_render |= self.handle_event(event);
            }
        }

        if !self.got_permissions {
            tracing::debug!("caching event");
            self.pending_events.push(event);

            return false;
        }

        should_render |= self.handle_event(event);
        if self.role == Role::Controller {
            if self.got_permissions && should_render {
                self.publish_views();
            }
            false
        } else {
            should_render
        }
    }

    #[tracing::instrument(skip_all)]
    fn render(&mut self, _rows: usize, cols: usize) {
        if self.role == Role::Controller {
            return;
        }
        if self.role == Role::View {
            if self.view_width != Some(cols) {
                self.view_width = Some(cols);
                self.view_frame.clear();
                if self.got_permissions {
                    self.request_frame();
                }
            }
            print!(
                "{}",
                if self.view_frame.is_empty() {
                    " … "
                } else {
                    &self.view_frame
                }
            );
            return;
        }
        if !self.got_permissions {
            return;
        }

        if let Some(err) = &self.err {
            println!("Error: {:?}", err);

            return;
        }

        self.state.cols = cols;

        tracing::debug!("{:?}", self.state.mode.session_name);

        let output = self
            .module_config
            .render_bar(self.state.clone(), self.widget_map.clone());

        print!("{}", output);
    }
}

impl State {
    fn update_focused_pane(&mut self) {
        let active_tab = self.state.tabs.iter().find(|t| t.active);

        let new_id = active_tab
            .and_then(|tab| self.state.panes.panes.get(&tab.position))
            .and_then(|panes| panes.iter().find(|p| p.is_focused && !p.is_plugin))
            .map(|p| PaneId::Terminal(p.id));

        if new_id == self.state.focused_pane_id {
            return;
        }

        self.state.focused_pane_id = new_id;

        let new_cwd = match new_id {
            Some(pane_id) => match get_pane_cwd(pane_id) {
                Ok(cwd) => Some(cwd),
                Err(e) => {
                    tracing::debug!("could not get pane cwd: {e}");
                    None
                }
            },
            None => None,
        };

        self.set_focused_pane_cwd(new_cwd);
    }

    fn set_focused_pane_cwd(&mut self, new_cwd: Option<PathBuf>) -> bool {
        if new_cwd == self.state.focused_pane_cwd {
            return false;
        }

        self.state.focused_pane_cwd = new_cwd;

        if self.focus_cwd_commands.is_empty() {
            return false;
        }

        self.invalidate_focus_cwd_commands();
        true
    }

    fn invalidate_focus_cwd_commands(&mut self) {
        for name in &self.focus_cwd_commands {
            pipe::invalidate_command_result(&mut self.state, name);
        }
    }

    fn request_frame(&self) {
        if let Some(width) = self.view_width {
            pipe_message_to_plugin(
                MessageToPlugin::new(VIEW_REQUEST_PIPE).with_payload(format!("width:{width}")),
            );
        }
    }

    fn update_view(&mut self, event: Event) -> bool {
        match event {
            Event::PermissionRequestResult(status) => {
                self.got_permissions = status == PermissionStatus::Granted;
                if self.got_permissions {
                    self.request_frame();
                }
                true
            }
            Event::Mouse(Mouse::LeftClick(line, col)) if self.got_permissions => {
                pipe_message_to_plugin(
                    MessageToPlugin::new(VIEW_REQUEST_PIPE)
                        .with_payload(format!("left:{line}:{col}")),
                );
                false
            }
            Event::Mouse(Mouse::RightClick(line, col)) if self.got_permissions => {
                pipe_message_to_plugin(
                    MessageToPlugin::new(VIEW_REQUEST_PIPE)
                        .with_payload(format!("right:{line}:{col}")),
                );
                false
            }
            _ => false,
        }
    }

    fn render_frame(&mut self, cols: usize) -> String {
        if let Some(err) = &self.err {
            return format!("Error: {err:?}");
        }
        self.state.cols = cols;
        self.module_config
            .render_bar(self.state.clone(), self.widget_map.clone())
    }

    fn publish_view(&mut self, plugin_id: u32, cols: usize) {
        let frame = self.render_frame(cols);
        pipe_message_to_plugin(
            MessageToPlugin::new(VIEW_FRAME_PIPE)
                .with_destination_plugin_id(plugin_id)
                .with_payload(frame),
        );
    }

    fn publish_views(&mut self) {
        let views: Vec<_> = self.views.iter().map(|(&id, &cols)| (id, cols)).collect();
        let mut frames = BTreeMap::new();
        for (plugin_id, cols) in views {
            let frame = frames
                .entry(cols)
                .or_insert_with(|| self.render_frame(cols))
                .clone();
            pipe_message_to_plugin(
                MessageToPlugin::new(VIEW_FRAME_PIPE)
                    .with_destination_plugin_id(plugin_id)
                    .with_payload(frame),
            );
        }
    }

    fn handle_view_request(&mut self, message: &PipeMessage) -> bool {
        let PipeSource::Plugin(plugin_id) = &message.source else {
            return false;
        };
        let plugin_id = *plugin_id;
        let Some(request) = message.payload.as_deref().and_then(parse_view_request) else {
            return false;
        };
        match request {
            ViewRequest::Width(cols) => {
                self.views.insert(plugin_id, cols);
                if self.got_permissions {
                    self.publish_view(plugin_id, cols);
                }
            }
            ViewRequest::LeftClick(line, col) | ViewRequest::RightClick(line, col) => {
                let Some(&cols) = self.views.get(&plugin_id) else {
                    return false;
                };
                self.state.cols = cols;
                let mouse = if matches!(request, ViewRequest::LeftClick(_, _)) {
                    Mouse::LeftClick(line, col)
                } else {
                    Mouse::RightClick(line, col)
                };
                self.module_config.handle_mouse_action(
                    self.state.clone(),
                    mouse,
                    self.widget_map.clone(),
                );
            }
        }
        false
    }

    fn handle_event(&mut self, event: Event) -> bool {
        let mut should_render = false;
        match event {
            Event::Mouse(mouse_info) => {
                tracing::Span::current().record("event_type", "Event::Mouse");
                tracing::debug!(mouse = ?mouse_info);

                self.module_config.handle_mouse_action(
                    self.state.clone(),
                    mouse_info,
                    self.widget_map.clone(),
                );
            }
            Event::ModeUpdate(mode_info) => {
                tracing::Span::current().record("event_type", "Event::ModeUpdate");
                tracing::debug!(mode = ?mode_info.mode);
                tracing::debug!(mode = ?mode_info.session_name);

                should_render = self.state.mode != mode_info;
                self.state.mode = mode_info;
                self.state.cache_mask = UpdateEventMask::Mode as u8;
            }
            Event::PaneUpdate(pane_info) => {
                tracing::Span::current().record("event_type", "Event::PaneUpdate");
                tracing::debug!(pane_count = ?pane_info.panes.len());

                if self.role == Role::Controller {
                    let live: std::collections::HashSet<_> = pane_info
                        .panes
                        .values()
                        .flatten()
                        .filter(|pane| pane.is_plugin)
                        .map(|pane| pane.id)
                        .collect();
                    self.views.retain(|plugin_id, _| live.contains(plugin_id));
                }

                frames::hide_frames_conditionally(
                    &frames::FrameConfig::new(
                        self.module_config.hide_frame_for_single_pane,
                        self.module_config.hide_frame_except_for_search,
                        self.module_config.hide_frame_except_for_fullscreen,
                        self.module_config.hide_frame_except_for_scroll,
                        &self.module_config.pane_frame_style,
                    ),
                    &self.state.tabs,
                    &pane_info,
                    &self.state.mode,
                    get_plugin_ids(),
                    false,
                );

                should_render = self.state.panes != pane_info;
                self.state.panes = pane_info;
                self.state.cache_mask = UpdateEventMask::Tab as u8;

                self.update_focused_pane();
            }
            Event::CwdChanged(pane_id, cwd, _clients) => {
                tracing::Span::current().record("event_type", "Event::CwdChanged");
                tracing::debug!(pane_id = ?pane_id, cwd = ?cwd);

                if Some(pane_id) == self.state.focused_pane_id
                    && self.set_focused_pane_cwd(Some(cwd))
                {
                    self.state.cache_mask = UpdateEventMask::Command as u8;
                    should_render = true;
                }
            }
            Event::PermissionRequestResult(result) => {
                tracing::Span::current().record("event_type", "Event::PermissionRequestResult");
                tracing::debug!(result = ?result);
                set_selectable(false);
                if result == PermissionStatus::Granted && self.role == Role::Controller {
                    pipe_message_to_plugin(MessageToPlugin::new(CONTROLLER_READY_PIPE));
                }
            }
            Event::RunCommandResult(exit_code, stdout, stderr, context) => {
                tracing::Span::current().record("event_type", "Event::RunCommandResult");
                tracing::debug!(
                    exit_code = ?exit_code,
                    stdout = ?String::from_utf8(stdout.clone()),
                    stderr = ?String::from_utf8(stderr.clone()),
                    context = ?context
                );

                if let Some(name) = context.get("name")
                    && self.focus_cwd_commands.iter().any(|n| n == name)
                    && context.get("cwd").map(PathBuf::from) != self.state.focused_pane_cwd
                {
                    tracing::debug!("discarding stale command result for {name}");
                    return false;
                }

                should_render =
                    store_command_result(&mut self.state, exit_code, stdout, stderr, context);
            }
            Event::SessionUpdate(session_info, _) => {
                tracing::Span::current().record("event_type", "Event::SessionUpdate");

                let current_session = session_info.iter().find(|s| s.is_current_session);

                if let Some(current_session) = current_session {
                    frames::hide_frames_conditionally(
                        &frames::FrameConfig::new(
                            self.module_config.hide_frame_for_single_pane,
                            self.module_config.hide_frame_except_for_search,
                            self.module_config.hide_frame_except_for_fullscreen,
                            self.module_config.hide_frame_except_for_scroll,
                            &self.module_config.pane_frame_style,
                        ),
                        &current_session.tabs,
                        &current_session.panes,
                        &self.state.mode,
                        get_plugin_ids(),
                        false,
                    );
                }

                should_render =
                    config::apply_current_session_snapshot(&mut self.state, &session_info);
                self.state.sessions = session_info;
            }
            Event::TabUpdate(tab_info) => {
                tracing::Span::current().record("event_type", "Event::TabUpdate");
                tracing::debug!(tab_count = ?tab_info.len());

                should_render = self.state.tabs != tab_info;
                self.state.tabs = tab_info;
                self.state.cache_mask = UpdateEventMask::Tab as u8;
            }
            Event::Timer(_) => {
                tracing::Span::current().record("event_type", "Event::Timer");
                set_timeout(REFRESH_INTERVAL_SECONDS);
                self.state.cache_mask = 0;

                should_render = true;
            }
            Event::HostTerminalThemeChanged(mode) => {
                tracing::Span::current().record("event_type", "Event::HostTerminalThemeChanged");
                should_render = self.apply_host_theme(mode);
            }
            _ => (),
        };
        should_render
    }

    fn apply_host_theme(&mut self, mode: HostTerminalThemeMode) -> bool {
        if config::configured_host_theme_mode(&self.userspace_configuration).is_none() {
            return false;
        }
        let configuration = config::host_theme_configuration(&self.userspace_configuration, mode);
        match ModuleConfig::new(&configuration) {
            Ok(module_config) => {
                self.module_config = module_config;
                self.widget_map = register_widgets(&configuration);
                self.focus_cwd_commands =
                    zjstatus::widgets::command::focus_cwd_command_names(&configuration);
                self.err = None;
            }
            Err(error) => self.err = Some(error),
        }
        true
    }
}

fn register_widgets(configuration: &BTreeMap<String, String>) -> BTreeMap<String, Arc<dyn Widget>> {
    let mut widget_map = BTreeMap::<String, Arc<dyn Widget>>::new();

    widget_map.insert(
        "command".to_owned(),
        Arc::new(CommandWidget::new(configuration)),
    );
    widget_map.insert(
        "datetime".to_owned(),
        Arc::new(DateTimeWidget::new(configuration)),
    );
    widget_map.insert("pipe".to_owned(), Arc::new(PipeWidget::new(configuration)));
    widget_map.insert(
        "swap_layout".to_owned(),
        Arc::new(SwapLayoutWidget::new(configuration)),
    );
    widget_map.insert("mode".to_owned(), Arc::new(ModeWidget::new(configuration)));
    widget_map.insert(
        "session".to_owned(),
        Arc::new(SessionWidget::new(configuration)),
    );
    widget_map.insert("tabs".to_owned(), Arc::new(TabsWidget::new(configuration)));
    widget_map.insert(
        "notifications".to_owned(),
        Arc::new(NotificationWidget::new(configuration)),
    );

    tracing::debug!("registered widgets: {:?}", widget_map.keys());

    widget_map
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Local;
    use zjstatus::widgets::command::CommandResult;

    #[test]
    fn controller_view_protocol_is_bounded_and_exact() {
        assert_eq!(
            parse_view_request("width:220"),
            Some(ViewRequest::Width(220))
        );
        assert_eq!(
            parse_view_request("left:0:42"),
            Some(ViewRequest::LeftClick(0, 42))
        );
        assert_eq!(
            parse_view_request("right:0:7"),
            Some(ViewRequest::RightClick(0, 7))
        );
        for invalid in [
            "",
            "width:0",
            "width:10001",
            "width:80:extra",
            "left:1",
            "hover:0:1",
        ] {
            assert_eq!(parse_view_request(invalid), None, "{invalid:?}");
        }
    }

    #[test]
    fn role_defaults_to_standalone() {
        assert_eq!(Role::from_config(&BTreeMap::new()), Role::Standalone);
        assert_eq!(
            Role::from_config(&BTreeMap::from([("role".into(), "view".into())])),
            Role::View
        );
        assert_eq!(
            Role::from_config(&BTreeMap::from([("role".into(), "controller".into())])),
            Role::Controller
        );
    }

    #[test]
    fn set_focused_pane_cwd_only_invalidates_on_change() {
        let mut state = State {
            focus_cwd_commands: vec!["command_branch".to_owned()],
            ..State::default()
        };
        state.state.focused_pane_cwd = Some(PathBuf::from("/tmp"));
        state.state.command_results.insert(
            "command_branch".to_owned(),
            CommandResult {
                context: BTreeMap::from([(
                    "timestamp".to_owned(),
                    Local::now()
                        .format(zjstatus::widgets::command::TIMESTAMP_FORMAT)
                        .to_string(),
                )]),
                ..CommandResult::default()
            },
        );

        let original_timestamp =
            state.state.command_results["command_branch"].context["timestamp"].clone();

        assert!(!state.set_focused_pane_cwd(Some(PathBuf::from("/tmp"))));
        assert_eq!(
            state.state.command_results["command_branch"].context["timestamp"],
            original_timestamp
        );

        assert!(state.set_focused_pane_cwd(Some(PathBuf::from("/var"))));
        assert_eq!(state.state.focused_pane_cwd, Some(PathBuf::from("/var")));
        assert_ne!(
            state.state.command_results["command_branch"].context["timestamp"],
            original_timestamp
        );
    }
}
