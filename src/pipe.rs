use chrono::{Duration, Local};

use crate::{
    config::ZellijState,
    widgets::{command::TIMESTAMP_FORMAT, notification},
};

/// Parses the line protocol and updates the state accordingly
///
/// The protocol is as follows:
///
/// zjstatus::command_name::args
///
/// It first starts with `zjstatus` as a prefix to indicate that the line is
/// used for the line protocol and zjstatus should parse it. It is followed
/// by the command name and then the arguments. The following commands are
/// available:
///
/// - `rerun` - Reruns the command with the given name (like in the config) as
///             argument. E.g. `zjstatus::rerun::command_1`
///
/// The function returns a boolean indicating whether the state has been
/// changed and the UI should be re-rendered.
#[tracing::instrument(skip(state))]
pub fn parse_protocol(state: &mut ZellijState, input: &str) -> bool {
    tracing::debug!("parsing protocol");
    let mut should_render = false;
    for line in input.split('\n') {
        should_render |= process_line(state, line);
    }
    should_render
}

#[tracing::instrument(skip_all)]
fn process_line(state: &mut ZellijState, line: &str) -> bool {
    let mut parts = line.splitn(4, "::");
    let (Some("zjstatus"), Some(command), Some(argument)) =
        (parts.next(), parts.next(), parts.next())
    else {
        return false;
    };

    tracing::debug!("command: {command}");

    match command {
        "rerun" => {
            rerun_command(state, argument);
            true
        }
        "notify" => {
            notify(state, argument);
            true
        }
        "pipe" => {
            let Some(content) = parts.next() else {
                return false;
            };
            pipe(state, argument, content);
            true
        }
        _ => false,
    }
}

fn pipe(state: &mut ZellijState, name: &str, content: &str) {
    tracing::debug!("saving pipe result {name} {content}");
    state
        .pipe_results
        .insert(name.to_owned(), content.to_owned());
}

fn notify(state: &mut ZellijState, message: &str) {
    state.incoming_notification = Some(notification::Message {
        body: message.to_string(),
        received_at: Local::now(),
    });
}

fn rerun_command(state: &mut ZellijState, command_name: &str) {
    let Some(command_result) = state.command_results.get_mut(command_name) else {
        return;
    };

    let ts = Local::now() - Duration::try_days(1).unwrap();

    command_result.context.insert(
        "timestamp".to_string(),
        ts.format(TIMESTAMP_FORMAT).to_string(),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pipe_protocol_processes_all_lines_and_preserves_separator_text() {
        let mut state = ZellijState::default();

        assert!(parse_protocol(
            &mut state,
            "zjstatus::pipe::first::ready\n\
             zjstatus::pipe::workspace::agent::plan"
        ));

        assert_eq!(
            state.pipe_results.get("first").map(String::as_str),
            Some("ready")
        );
        assert_eq!(
            state.pipe_results.get("workspace").map(String::as_str),
            Some("agent::plan")
        );
    }
}
