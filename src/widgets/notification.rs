use std::collections::BTreeMap;

use chrono::{DateTime, Local};

use crate::render::FormattedPart;
use crate::{config::ZellijState, widgets::widget::Widget};

#[derive(Clone, Debug, Default)]
pub struct Message {
    pub body: String,
    pub received_at: DateTime<Local>,
}

pub struct NotificationWidget {
    show_interval: i64,
    format_unread: Vec<FormattedPart>,
    format_no_notifications: Vec<FormattedPart>,
}

impl NotificationWidget {
    pub fn new(config: &BTreeMap<String, String>) -> Self {
        let format_unread = match config.get("notification_format_unread") {
            Some(f) => FormattedPart::multiple_from_format_string(f, config),
            None => FormattedPart::multiple_from_format_string("", config),
        };

        let format_no_notifications = match config.get("notification_format_no_notifications") {
            Some(f) => FormattedPart::multiple_from_format_string(f, config),
            None => FormattedPart::multiple_from_format_string("", config),
        };

        Self {
            show_interval: show_interval_seconds(config),
            format_unread,
            format_no_notifications,
        }
    }
}

pub fn show_interval_seconds(config: &BTreeMap<String, String>) -> i64 {
    config
        .get("notification_show_interval")
        .and_then(|interval| interval.parse().ok())
        .unwrap_or(5)
}

pub fn toast_min_width(config: &BTreeMap<String, String>) -> usize {
    config
        .get("notification_toast_min_width")
        .and_then(|width| width.parse().ok())
        .unwrap_or_default()
}

impl Widget for NotificationWidget {
    fn process(&self, _name: &str, state: &ZellijState) -> String {
        let message = match state.incoming_notification {
            Some(ref message) => message.clone(),
            None => Message::default(),
        };

        let no_new =
            message.received_at.timestamp() + self.show_interval < Local::now().timestamp();

        tracing::debug!("no_new: {}", no_new);

        let format = match no_new {
            true => self.format_no_notifications.clone(),
            false => self.format_unread.clone(),
        };

        let mut output = "".to_owned();

        for f in format.iter() {
            let mut content = f.content.clone();

            if content.contains("{message}") {
                content = content.replace("{message}", message.body.as_str());
            }

            output = format!("{}{}", output, f.format_string(&content));
        }

        output.to_owned()
    }

    fn process_click(&self, _name: &str, _state: &ZellijState, _pos: usize) {}
}
