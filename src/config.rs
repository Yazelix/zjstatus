use std::{collections::BTreeMap, str::FromStr, sync::Arc};

use itertools::Itertools;
use zellij_tile::prelude::*;

use crate::{
    border::{BorderConfig, BorderPosition, parse_border_config},
    render::{FormattedPart, RenderedParts, render_parts, widget_type},
    widgets::{command::CommandResult, notification, widget::Widget},
};
#[derive(Default, Debug, Clone)]
pub struct ZellijState {
    pub cols: usize,
    pub tab_width_limit: Option<usize>,
    pub command_results: BTreeMap<String, CommandResult>,
    pub pipe_results: BTreeMap<String, String>,
    pub mode: ModeInfo,
    pub panes: PaneManifest,
    pub tabs: Vec<TabInfo>,
    pub sessions: Vec<SessionInfo>,
    pub incoming_notification: Option<notification::Message>,
    pub cache_mask: u8,
}

pub fn apply_current_session_snapshot(
    state: &mut ZellijState,
    sessions: &[SessionInfo],
) -> bool {
    let Some(session) = sessions.iter().find(|session| session.is_current_session) else {
        return false;
    };
    let changed = state.tabs != session.tabs
        || state.panes != session.panes
        || state.mode.session_name.as_ref() != Some(&session.name);
    state.tabs.clone_from(&session.tabs);
    state.panes.clone_from(&session.panes);
    state.mode.session_name = Some(session.name.clone());
    state.cache_mask = UpdateEventMask::Tab as u8;
    changed
}

#[derive(Clone, Debug, Ord, Eq, PartialEq, PartialOrd, Copy)]
pub enum Part {
    Left,
    Center,
    Right,
}

impl FromStr for Part {
    fn from_str(part: &str) -> Result<Self> {
        match part {
            "l" => Ok(Part::Left),
            "c" => Ok(Part::Center),
            "r" => Ok(Part::Right),
            _ => anyhow::bail!("Invalid part: {}", part),
        }
    }

    type Err = anyhow::Error;
}

pub enum UpdateEventMask {
    Always = 0b10000000,
    Mode = 0b00000001,
    Tab = 0b00000011,
    Command = 0b00000100,
    Session = 0b00001000,
    None = 0b00000000,
}

const HOST_THEME_MODE: &str = "host_theme_mode";
const HOST_THEME_DARK_PREFIX: &str = "host_theme_dark_";
const HOST_THEME_LIGHT_PREFIX: &str = "host_theme_light_";

pub fn host_theme_configuration(
    config: &BTreeMap<String, String>,
    mode: HostTerminalThemeMode,
) -> BTreeMap<String, String> {
    let prefix = match mode {
        HostTerminalThemeMode::Dark => HOST_THEME_DARK_PREFIX,
        HostTerminalThemeMode::Light => HOST_THEME_LIGHT_PREFIX,
    };
    let mut themed = config.clone();
    for (key, value) in config {
        if let Some(key) = key.strip_prefix(prefix) {
            themed.insert(key.to_owned(), value.clone());
        }
    }
    themed.insert(
        HOST_THEME_MODE.to_owned(),
        match mode {
            HostTerminalThemeMode::Dark => "dark",
            HostTerminalThemeMode::Light => "light",
        }
        .to_owned(),
    );
    themed
}

pub fn configured_host_theme_mode(
    config: &BTreeMap<String, String>,
) -> Option<HostTerminalThemeMode> {
    match config.get(HOST_THEME_MODE).map(String::as_str) {
        Some("dark") => Some(HostTerminalThemeMode::Dark),
        Some("light") => Some(HostTerminalThemeMode::Light),
        _ => None,
    }
}

pub fn event_mask_from_widget_name(name: &str) -> u8 {
    match name {
        "command" => UpdateEventMask::Always as u8,
        "datetime" => UpdateEventMask::Always as u8,
        "mode" => UpdateEventMask::Mode as u8,
        "notifications" => UpdateEventMask::Always as u8,
        "session" => UpdateEventMask::Mode as u8,
        "swap_layout" => UpdateEventMask::Tab as u8,
        "tabs" => UpdateEventMask::Tab as u8,
        "pipe" => UpdateEventMask::Always as u8,
        _ => UpdateEventMask::None as u8,
    }
}

#[derive(Default, Debug)]
pub struct ModuleConfig {
    pub left_parts_config: String,
    pub left_parts: Vec<FormattedPart>,
    pub center_parts_config: String,
    pub center_parts: Vec<FormattedPart>,
    pub right_parts_config: String,
    pub right_parts: Vec<FormattedPart>,
    pub right_segments: Option<Vec<Vec<FormattedPart>>>,
    pub right_separator: Vec<FormattedPart>,
    pub format_space: FormattedPart,
    pub hide_frame_for_single_pane: bool,
    pub hide_frame_except_for_search: bool,
    pub hide_frame_except_for_fullscreen: bool,
    pub hide_frame_except_for_scroll: bool,
    pub border: BorderConfig,
    pub format_precedence: Vec<Part>,
    pub hide_on_overlength: bool,
}

impl ModuleConfig {
    pub fn new(config: &BTreeMap<String, String>) -> anyhow::Result<Self> {
        let format_space_config = match config.get("format_space") {
            Some(space_config) => space_config,
            None => "",
        };

        let hide_frame_for_single_pane = match config.get("hide_frame_for_single_pane") {
            Some(toggle) => toggle == "true",
            None => false,
        };
        let hide_frame_except_for_search = match config.get("hide_frame_except_for_search") {
            Some(toggle) => toggle == "true",
            None => false,
        };
        let hide_frame_except_for_fullscreen = match config.get("hide_frame_except_for_fullscreen")
        {
            Some(toggle) => toggle == "true",
            None => false,
        };
        let hide_frame_except_for_scroll = match config.get("hide_frame_except_for_scroll") {
            Some(toggle) => toggle == "true",
            None => false,
        };

        let left_parts_config = match config.get("format_left") {
            Some(conf) => conf,
            None => "",
        };

        let right_parts_config = match config.get("format_right") {
            Some(conf) => conf,
            None => "",
        };

        let center_parts_config = match config.get("format_center") {
            Some(conf) => conf,
            None => "",
        };

        let format_precedence = match config.get("format_precedence") {
            Some(conf) => {
                let prec = conf
                    .chars()
                    .map(|c| Part::from_str(&c.to_string()))
                    .collect();

                match prec {
                    Ok(prec) => prec,
                    Err(e) => {
                        anyhow::bail!("Invalid format_precedence: {}", e);
                    }
                }
            }
            None => vec![Part::Left, Part::Center, Part::Right],
        };

        let hide_on_overlength = match config.get("format_hide_on_overlength") {
            Some(opt) => opt == "true",
            None => false,
        };

        let border_config = parse_border_config(config).unwrap_or_default();

        Ok(Self {
            left_parts_config: left_parts_config.to_owned(),
            left_parts: parts_from_config(Some(&left_parts_config.to_owned()), config),
            center_parts_config: center_parts_config.to_owned(),
            center_parts: parts_from_config(Some(&center_parts_config.to_owned()), config),
            right_parts_config: right_parts_config.to_owned(),
            right_parts: if config.contains_key("format_right_separator") {
                vec![]
            } else {
                parts_from_config(Some(&right_parts_config.to_owned()), config)
            },
            right_segments: config.get("format_right_separator").map(|_| {
                right_parts_config
                    .split("{segment}")
                    .map(|s| FormattedPart::multiple_from_format_string(s, config))
                    .collect()
            }),
            right_separator: parts_from_config(config.get("format_right_separator"), config),
            format_space: FormattedPart::from_format_string(format_space_config, config),
            hide_frame_for_single_pane,
            hide_frame_except_for_search,
            hide_frame_except_for_fullscreen,
            hide_frame_except_for_scroll,
            border: border_config,
            format_precedence,
            hide_on_overlength,
        })
    }

    pub fn handle_mouse_action(
        &mut self,
        mut state: ZellijState,
        mouse: Mouse,
        widget_map: BTreeMap<String, Arc<dyn Widget>>,
    ) {
        let col = match mouse {
            Mouse::LeftClick(_, col) | Mouse::RightClick(_, col) => col,
            _ => return,
        };
        let [left, center, right] = self.render_sections(&mut state, &widget_map);
        let center_offset = console::measure_text_width(&left.output)
            + console::measure_text_width(&self.get_spacer_left(
                &left.output,
                &center.output,
                state.cols,
            ));
        let right_offset = if center.output.is_empty() {
            console::measure_text_width(&left.output)
                + console::measure_text_width(&self.get_spacer(
                    &left.output,
                    &right.output,
                    state.cols,
                ))
        } else {
            center_offset
                + console::measure_text_width(&center.output)
                + console::measure_text_width(&self.get_spacer_right(
                    &right.output,
                    &center.output,
                    state.cols,
                ))
        };
        for (section, offset) in [(&left, 0), (&center, center_offset), (&right, right_offset)] {
            if let Some(local_col) = col.checked_sub(offset) {
                for (name, range) in &section.hits {
                    if range.contains(&local_col) {
                        if let Some(widget) = widget_map.get(widget_type(name)) {
                            widget.process_click(name, &state, local_col - range.start);
                        }
                        return;
                    }
                }
            }
        }
    }

    fn render_sections(
        &mut self,
        state: &mut ZellijState,
        widgets: &BTreeMap<String, Arc<dyn Widget>>,
    ) -> [RenderedParts; 3] {
        state.tab_width_limit = None;
        let mut left = render_parts(&mut self.left_parts, widgets, state);
        let mut center = render_parts(&mut self.center_parts, widgets, state);
        let mut right = RenderedParts::default();
        if let Some(segments) = &mut self.right_segments {
            // Hidden widgets still receive updates and keep their command refresh cadence.
            let segments: Vec<_> = segments
                .iter_mut()
                .map(|segment| render_parts(segment, widgets, state))
                .collect();
            let separator = render_parts(&mut self.right_separator, widgets, state);
            let left_width = console::measure_text_width(&left.output);
            if left_width > state.cols {
                let tabs_width: usize = left
                    .hits
                    .iter()
                    .filter(|(name, _)| name == "tabs")
                    .map(|(_, range)| range.len())
                    .sum();
                state.tab_width_limit = Some(
                    state
                        .cols
                        .saturating_sub(left_width.saturating_sub(tabs_width)),
                );
                left = render_parts(&mut self.left_parts, widgets, state);
            } else {
                let available = state
                    .cols
                    .saturating_sub(left_width + console::measure_text_width(&center.output));
                let separator_width = console::measure_text_width(&separator.output);
                let mut width = 0;
                let mut visible = Vec::new();
                for part in segments.into_iter().rev() {
                    if console::strip_ansi_codes(&part.output).trim().is_empty() {
                        continue;
                    }
                    let part_width = console::measure_text_width(&part.output);
                    let gap = if visible.is_empty() {
                        0
                    } else {
                        separator_width
                    };
                    if width + gap + part_width > available {
                        break;
                    }
                    width += gap + part_width;
                    visible.push(part);
                }
                for part in visible.into_iter().rev() {
                    if !right.output.is_empty() {
                        right.append(separator.clone());
                    }
                    right.append(part);
                }
            }
        } else {
            right = render_parts(&mut self.right_parts, widgets, state);
        }
        if self.hide_on_overlength {
            let (l, c, r) =
                self.trim_output(&left.output, &center.output, &right.output, state.cols);
            for (section, output) in [(&mut left, l), (&mut center, c), (&mut right, r)] {
                if output.is_empty() {
                    *section = RenderedParts::default();
                }
            }
        }
        [left, center, right]
    }

    pub fn render_bar(
        &mut self,
        mut state: ZellijState,
        widget_map: BTreeMap<String, Arc<dyn Widget>>,
    ) -> String {
        if self.left_parts.is_empty()
            && self.center_parts.is_empty()
            && self.right_parts.is_empty()
            && self.right_segments.is_none()
        {
            return "No configuration found. See https://github.com/dj95/zjstatus/wiki/3-%E2%80%90-Configuration for more info".to_string();
        }
        let [left, center, right] = self.render_sections(&mut state, &widget_map);
        let (output_left, output_center, output_right) = (left.output, center.output, right.output);

        if self.border.enabled {
            let mut border_top = "".to_owned();
            if self.border.enabled && self.border.position == BorderPosition::Top {
                border_top = format!("{}\n", self.border.draw(state.cols));
            }

            let mut border_bottom = "".to_owned();
            if self.border.enabled && self.border.position == BorderPosition::Bottom {
                border_bottom = format!("\n{}", self.border.draw(state.cols));
            }

            if !output_center.is_empty() {
                return format!(
                    "{}{}{}{}{}{}{}",
                    border_top,
                    output_left,
                    self.get_spacer_left(&output_left, &output_center, state.cols),
                    output_center,
                    self.get_spacer_right(&output_right, &output_center, state.cols),
                    output_right,
                    border_bottom,
                );
            }

            return format!(
                "{}{}{}{}{}",
                border_top,
                output_left,
                self.get_spacer(&output_left, &output_right, state.cols),
                output_right,
                border_bottom,
            );
        }

        if !output_center.is_empty() {
            return format!(
                "{}{}{}{}{}",
                output_left,
                self.get_spacer_left(&output_left, &output_center, state.cols),
                output_center,
                self.get_spacer_right(&output_right, &output_center, state.cols),
                output_right,
            );
        }

        format!(
            "{}{}{}",
            output_left,
            self.get_spacer(&output_left, &output_right, state.cols),
            output_right,
        )
    }

    fn trim_output(
        &self,
        output_left: &str,
        output_center: &str,
        output_right: &str,
        cols: usize,
    ) -> (String, String, String) {
        let center_pos = (cols as f32 / 2.0).floor() as usize;

        let mut output = BTreeMap::from([
            (Part::Left, output_left.to_owned()),
            (Part::Center, output_center.to_owned()),
            (Part::Right, output_right.to_owned()),
        ]);

        let combinations = [
            (self.format_precedence[2], self.format_precedence[1]),
            (self.format_precedence[1], self.format_precedence[0]),
            (self.format_precedence[2], self.format_precedence[0]),
        ];

        for win in combinations.iter() {
            let (a, b) = win;

            let part_a = output.get(a).unwrap();
            let part_b = output.get(b).unwrap();

            let a_count = console::measure_text_width(part_a);
            let b_count = console::measure_text_width(part_b);

            let overlap = match (a, b) {
                (Part::Left, Part::Right) => a_count + b_count > cols,
                (Part::Right, Part::Left) => a_count + b_count > cols,
                (Part::Left, Part::Center) => a_count > center_pos.saturating_sub(b_count / 2),
                (Part::Center, Part::Left) => b_count > center_pos.saturating_sub(a_count / 2),
                (Part::Right, Part::Center) => a_count > center_pos.saturating_sub(b_count / 2),
                (Part::Center, Part::Right) => b_count > center_pos.saturating_sub(a_count / 2),
                _ => false,
            };

            if overlap {
                output.insert(*a, "".to_owned());
            }
        }

        output.values().cloned().collect_tuple().unwrap()
    }

    #[tracing::instrument(skip_all)]
    fn get_spacer_left(&self, output_left: &str, output_center: &str, cols: usize) -> String {
        let text_count = console::measure_text_width(output_left)
            + (console::measure_text_width(output_center) as f32 / 2.0).floor() as usize;

        let center_pos = (cols as f32 / 2.0).floor() as usize;

        // verify we are able to count the difference, since zellij sometimes drops a col
        // count of 0 on tab creation
        let space_count = center_pos.saturating_sub(text_count);

        tracing::debug!("space_count: {:?}", space_count);
        self.format_space.format_string(&" ".repeat(space_count))
    }

    #[tracing::instrument(skip_all)]
    fn get_spacer_right(&self, output_right: &str, output_center: &str, cols: usize) -> String {
        let text_count = console::measure_text_width(output_right)
            + (console::measure_text_width(output_center) as f32 / 2.0).ceil() as usize;

        let center_pos = (cols as f32 / 2.0).ceil() as usize;

        // verify we are able to count the difference, since zellij sometimes drops a col
        // count of 0 on tab creation
        let space_count = center_pos.saturating_sub(text_count);

        tracing::debug!("space_count: {:?}", space_count);
        self.format_space.format_string(&" ".repeat(space_count))
    }

    fn get_spacer(&self, output_left: &str, output_right: &str, cols: usize) -> String {
        let text_count =
            console::measure_text_width(output_left) + console::measure_text_width(output_right);

        // verify we are able to count the difference, since zellij sometimes drops a col
        // count of 0 on tab creation
        let space_count = cols.saturating_sub(text_count);

        self.format_space.format_string(&" ".repeat(space_count))
    }
}

fn parts_from_config(
    format: Option<&String>,
    config: &BTreeMap<String, String>,
) -> Vec<FormattedPart> {
    match format {
        Some(format) => match format.is_empty() {
            true => vec![],
            false => format
                .split("#[")
                .map(|s| FormattedPart::from_format_string(s, config))
                .collect(),
        },
        None => vec![],
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use anstyle::{Effects, RgbColor};

    #[test]
    fn current_session_snapshot_primes_visible_tab_state() {
        let tabs = vec![TabInfo {
            name: "ready".into(),
            ..Default::default()
        }];
        let sessions = vec![SessionInfo {
            name: "ready-session".into(),
            tabs: tabs.clone(),
            is_current_session: true,
            ..Default::default()
        }];
        let mut state = ZellijState::default();

        assert!(apply_current_session_snapshot(&mut state, &sessions));
        assert_eq!(state.tabs, tabs);
        assert_eq!(state.mode.session_name.as_deref(), Some("ready-session"));
        assert_eq!(state.cache_mask, UpdateEventMask::Tab as u8);
        assert!(!apply_current_session_snapshot(&mut state, &sessions));
    }

    #[test]
    fn hidden_segments_restore_updates_received_while_narrow() {
        use crate::widgets::session::SessionWidget;
        let config = BTreeMap::from([
            ("format_left".into(), "tabs".into()),
            ("format_right".into(), "first{segment}{session}".into()),
            ("format_right_separator".into(), " | ".into()),
            ("format_precedence".into(), "lrc".into()),
            ("format_hide_on_overlength".into(), "true".into()),
        ]);
        let widgets: BTreeMap<String, Arc<dyn Widget>> = BTreeMap::from([(
            "session".into(),
            Arc::new(SessionWidget::new(&config)) as Arc<dyn Widget>,
        )]);
        for narrow in [3, 6] {
            let mut renderer = ModuleConfig::new(&config).unwrap();
            let mut state = ZellijState {
                cols: 30,
                ..Default::default()
            };
            state.mode.session_name = Some("old".into());
            assert!(
                renderer
                    .render_bar(state.clone(), widgets.clone())
                    .contains("old")
            );
            state.cols = narrow;
            state.mode.session_name = Some("new".into());
            state.cache_mask = UpdateEventMask::Mode as u8;
            renderer.render_bar(state.clone(), widgets.clone());
            state.cols = 30;
            state.cache_mask = 0;
            let output = renderer.render_bar(state, widgets.clone());
            assert!(
                console::strip_ansi_codes(&output).contains("first | new"),
                "width {narrow}: {output:?}"
            );
        }
    }

    #[test]
    fn right_segments_keep_fitting_suffix_anchored_and_clickable() {
        use std::cell::RefCell;
        struct Commands(RefCell<Vec<String>>);
        impl Widget for Commands {
            fn process(&self, name: &str, state: &ZellijState) -> String {
                state.pipe_results.get(name).cloned().unwrap_or_default()
            }
            fn process_click(&self, name: &str, _: &ZellijState, _: usize) {
                self.0.borrow_mut().push(name.to_owned());
            }
        }
        let config = BTreeMap::from([
            ("format_left".into(), "tabs".into()),
            ("format_right".into(), "{command_editor}{segment}{command_empty}{segment}#[fg=red]{command_cpu}{segment}{command_version}".into()),
            ("format_right_separator".into(), " • ".into()),
            ("format_hide_on_overlength".into(), "true".into()),
            ("format_precedence".into(), "lrc".into()),
        ]);
        let commands = Arc::new(Commands(RefCell::new(Vec::new())));
        let widgets: BTreeMap<String, Arc<dyn Widget>> =
            BTreeMap::from([("command".into(), commands.clone() as Arc<dyn Widget>)]);
        let mut renderer = ModuleConfig::new(&config).unwrap();
        let mut state = ZellijState {
            pipe_results: BTreeMap::from([
                ("command_editor".into(), "界e\u{301}".into()),
                ("command_cpu".into(), "cpu".into()),
                ("command_version".into(), "ver".into()),
            ]),
            ..Default::default()
        };
        for (cols, expected) in [
            (30, "界e\u{301} • cpu • ver"),
            (18, "cpu • ver"),
            (12, "ver"),
            (6, ""),
            (30, "界e\u{301} • cpu • ver"),
        ] {
            state.cols = cols;
            let output = renderer.render_bar(state.clone(), widgets.clone());
            let plain = console::strip_ansi_codes(&output);
            assert_eq!(console::measure_text_width(&output), cols);
            assert!(plain.starts_with("tabs"));
            assert_eq!(plain[4..].trim(), expected, "width {cols}");
            assert!(expected.is_empty() || plain.ends_with(expected));
            commands.0.borrow_mut().clear();
            for col in 0..cols {
                renderer.handle_mouse_action(
                    state.clone(),
                    Mouse::LeftClick(0, col),
                    widgets.clone(),
                );
            }
            let clicks = commands.0.borrow();
            assert_eq!(
                clicks
                    .iter()
                    .filter(|n| n.as_str() == "command_editor")
                    .count(),
                if expected.contains('界') { 3 } else { 0 }
            );
            assert_eq!(
                clicks
                    .iter()
                    .filter(|n| n.as_str() == "command_cpu")
                    .count(),
                if expected.contains("cpu") { 3 } else { 0 }
            );
            assert_eq!(
                clicks
                    .iter()
                    .filter(|n| n.as_str() == "command_version")
                    .count(),
                if expected.contains("ver") { 3 } else { 0 }
            );
            assert!(!clicks.iter().any(|n| n == "command_empty"));
        }
    }

    #[test]
    fn test_formatted_part_from_string() {
        let input = "#[fg=#ff0000,bg=#00ff00,bold,italic]foo";

        let part = FormattedPart::from_format_string(input, &BTreeMap::new());

        assert_eq!(
            part,
            FormattedPart {
                fg: Some(RgbColor(255, 0, 0).into()),
                bg: Some(RgbColor(0, 255, 0).into()),
                effects: Effects::BOLD | Effects::ITALIC,
                content: "foo".to_owned(),
                ..Default::default()
            },
        )
    }

    #[test]
    fn host_theme_overlays_only_the_selected_palette() {
        let config = BTreeMap::from([
            ("format_right".to_owned(), "base".to_owned()),
            ("host_theme_mode".to_owned(), "dark".to_owned()),
            (
                "host_theme_dark_format_right".to_owned(),
                "dark bar".to_owned(),
            ),
            (
                "host_theme_light_format_right".to_owned(),
                "light bar".to_owned(),
            ),
            (
                "host_theme_light_tab_normal".to_owned(),
                "light tab".to_owned(),
            ),
        ]);

        let dark = host_theme_configuration(&config, HostTerminalThemeMode::Dark);
        assert_eq!(
            dark.get("format_right").map(String::as_str),
            Some("dark bar")
        );
        assert_eq!(dark.get("tab_normal"), None);
        assert_eq!(
            configured_host_theme_mode(&dark),
            Some(HostTerminalThemeMode::Dark)
        );

        let light = host_theme_configuration(&config, HostTerminalThemeMode::Light);
        assert_eq!(
            light.get("format_right").map(String::as_str),
            Some("light bar")
        );
        assert_eq!(
            light.get("tab_normal").map(String::as_str),
            Some("light tab")
        );
        assert_eq!(
            configured_host_theme_mode(&light),
            Some(HostTerminalThemeMode::Light)
        );
    }
}
