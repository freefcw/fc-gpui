use gpui::{DialogError, DialogKind, DialogOptions};

enum ToolAttempt {
    Missing,
    Finished(usize),
}

pub fn show_dialog(options: &DialogOptions) -> Result<usize, DialogError> {
    dialog_result(try_zenity(options), try_kdialog(options))
}

fn dialog_result(zenity: ToolAttempt, kdialog: ToolAttempt) -> Result<usize, DialogError> {
    match zenity {
        ToolAttempt::Finished(index) => Ok(index),
        ToolAttempt::Missing => match kdialog {
            ToolAttempt::Finished(index) => Ok(index),
            ToolAttempt::Missing => Err(DialogError::unavailable(
                "no dialog program found; install zenity or kdialog",
            )),
        },
    }
}

/// Maps a kdialog exit code to a button index.
///
/// `0`, `1`, and `2` are the yes, no, and cancel buttons when that many
/// buttons exist. Any other code is the last button, which is the cancel
/// button, rather than the first button.
pub(crate) fn kdialog_exit_index(code: Option<i32>, button_count: usize) -> Option<usize> {
    if button_count == 0 {
        return None;
    }
    match code {
        Some(0) => Some(0),
        Some(1) if button_count >= 2 => Some(1),
        Some(2) if button_count >= 3 => Some(2),
        _ => Some(button_count - 1),
    }
}

fn try_zenity(options: &DialogOptions) -> ToolAttempt {
    let message = build_message(options);
    let button_count = options.buttons.len();

    if button_count <= 1 {
        let zenity_type = match options.kind {
            DialogKind::Error => "--error",
            DialogKind::Warning => "--warning",
            DialogKind::Info => "--info",
        };
        let ran = std::process::Command::new("zenity")
            .args([zenity_type, "--title", &options.title, "--text", &message])
            .output()
            .is_ok();
        return if ran {
            ToolAttempt::Finished(0)
        } else {
            ToolAttempt::Missing
        };
    }

    let mut cmd = std::process::Command::new("zenity");
    cmd.args(["--question", "--title", &options.title, "--text", &message]);

    cmd.args(["--ok-label", &options.buttons[0]]);
    cmd.args(["--cancel-label", &options.buttons[1]]);

    for button in options.buttons.iter().skip(2) {
        cmd.args(["--extra-button", button]);
    }

    let Ok(output) = cmd.output() else {
        return ToolAttempt::Missing;
    };

    if output.status.success() {
        return ToolAttempt::Finished(0);
    }

    if button_count > 2 {
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stdout_trimmed = stdout.trim();
        for (idx, button) in options.buttons.iter().enumerate().skip(2) {
            if stdout_trimmed == button.as_ref() {
                return ToolAttempt::Finished(idx);
            }
        }
    }

    ToolAttempt::Finished(1)
}

fn try_kdialog(options: &DialogOptions) -> ToolAttempt {
    let message = build_message(options);
    let button_count = options.buttons.len();

    if button_count <= 1 {
        let kdialog_type = match options.kind {
            DialogKind::Error => "--error",
            DialogKind::Warning => "--sorry",
            DialogKind::Info => "--msgbox",
        };
        let ran = std::process::Command::new("kdialog")
            .args([kdialog_type, &message, "--title", &options.title])
            .output()
            .is_ok();
        return if ran {
            ToolAttempt::Finished(0)
        } else {
            ToolAttempt::Missing
        };
    }

    let mut args = vec!["--warningyesno".to_string(), message];
    if button_count >= 3 {
        args[0] = "--warningyesnocancel".to_string();
    }

    args.push("--title".to_string());
    args.push(options.title.to_string());

    if button_count >= 1 {
        args.push("--yes-label".to_string());
        args.push(options.buttons[0].to_string());
    }
    if button_count >= 2 {
        args.push("--no-label".to_string());
        args.push(options.buttons[1].to_string());
    }
    if button_count >= 3 {
        args.push("--cancel-label".to_string());
        args.push(options.buttons[2].to_string());
    }

    let Ok(output) = std::process::Command::new("kdialog").args(&args).output() else {
        return ToolAttempt::Missing;
    };

    kdialog_exit_index(output.status.code(), button_count)
        .map(ToolAttempt::Finished)
        .unwrap_or(ToolAttempt::Missing)
}

fn build_message(options: &DialogOptions) -> String {
    match &options.detail {
        Some(detail) if !options.message.is_empty() => {
            format!("{}\n\n{}", options.message, detail)
        }
        Some(detail) => detail.to_string(),
        None => options.message.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::{ToolAttempt, dialog_result, kdialog_exit_index};
    use gpui::DialogError;

    #[test]
    fn missing_dialog_programs_are_not_the_first_button() {
        let error = dialog_result(ToolAttempt::Missing, ToolAttempt::Missing)
            .expect_err("missing tools must fail");
        assert!(error.message().contains("zenity"));
        let _ = DialogError::unavailable("unused");
    }

    #[test]
    fn zenity_result_wins_over_kdialog() {
        let index = dialog_result(ToolAttempt::Finished(1), ToolAttempt::Finished(0)).unwrap();
        assert_eq!(index, 1);
    }

    #[test]
    fn unknown_kdialog_exit_is_the_cancel_button() {
        assert_eq!(kdialog_exit_index(Some(0), 3), Some(0));
        assert_eq!(kdialog_exit_index(Some(1), 3), Some(1));
        assert_eq!(kdialog_exit_index(Some(2), 3), Some(2));
        assert_eq!(kdialog_exit_index(Some(9), 3), Some(2));
        assert_eq!(kdialog_exit_index(None, 2), Some(1));
        assert_eq!(kdialog_exit_index(Some(4), 0), None);
    }
}
