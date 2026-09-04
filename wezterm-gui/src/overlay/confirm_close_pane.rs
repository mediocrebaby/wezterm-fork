#[cfg(any(test, not(target_os = "linux")))]
use rfd::MessageDialogResult;
#[cfg(not(target_os = "linux"))]
use rfd::{MessageButtons, MessageDialog, MessageLevel};
#[cfg(target_os = "linux")]
use std::process::Command;
#[cfg(target_os = "linux")]
use window::WindowOps;

const CLOSE_PANE_TITLE: &str = "Close this terminal pane?";
const CLOSE_PANE_DESCRIPTION: &str =
    "A process is still running in this pane. Closing the pane will terminate the process.";
const CLOSE_TAB_TITLE: &str = "Close this terminal tab?";
const CLOSE_TAB_DESCRIPTION: &str =
    "One or more processes are still running in this tab. Closing the tab will terminate them.";
const CLOSE_WINDOW_TITLE: &str = "Close this terminal?";
const CLOSE_WINDOW_DESCRIPTION: &str =
    "One or more processes are still running in this terminal. Closing the terminal will terminate them.";
const QUIT_TITLE: &str = "Quit WezTerm?";
const QUIT_DESCRIPTION: &str =
    "One or more terminal processes may still be running. Quitting WezTerm will terminate them.";
const CLOSE_BUTTON: &str = "Close Terminal";
const QUIT_BUTTON: &str = "Quit WezTerm";
const CANCEL_BUTTON: &str = "Cancel";

#[cfg(target_os = "linux")]
const ZENITY_PROGRAM: &str = "zenity";
#[cfg(target_os = "linux")]
const ZENITY_NO_MARKUP_ARG: &str = "--no-markup";
#[cfg(target_os = "linux")]
const ZENITY_QUESTION_ARG: &str = "--question";
#[cfg(target_os = "linux")]
const ZENITY_TITLE_ARG: &str = "--title";
#[cfg(target_os = "linux")]
const ZENITY_TEXT_ARG: &str = "--text";
#[cfg(target_os = "linux")]
const ZENITY_OK_LABEL_ARG: &str = "--ok-label";
#[cfg(target_os = "linux")]
const ZENITY_CANCEL_LABEL_ARG: &str = "--cancel-label";
#[cfg(target_os = "linux")]
const ZENITY_CANCEL_EXIT_CODE: i32 = 1;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CloseConfirmation {
    Pane,
    Tab,
    Window,
    Application,
}

impl CloseConfirmation {
    fn title(self) -> &'static str {
        match self {
            Self::Pane => CLOSE_PANE_TITLE,
            Self::Tab => CLOSE_TAB_TITLE,
            Self::Window => CLOSE_WINDOW_TITLE,
            Self::Application => QUIT_TITLE,
        }
    }

    fn description(self) -> &'static str {
        match self {
            Self::Pane => CLOSE_PANE_DESCRIPTION,
            Self::Tab => CLOSE_TAB_DESCRIPTION,
            Self::Window => CLOSE_WINDOW_DESCRIPTION,
            Self::Application => QUIT_DESCRIPTION,
        }
    }

    fn accept_button(self) -> &'static str {
        match self {
            Self::Application => QUIT_BUTTON,
            Self::Pane | Self::Tab | Self::Window => CLOSE_BUTTON,
        }
    }
}

pub fn confirm_close(window: &::window::Window, scope: CloseConfirmation) -> bool {
    #[cfg(target_os = "linux")]
    {
        return confirm_close_linux(window, scope);
    }

    #[cfg(not(target_os = "linux"))]
    let result = MessageDialog::new()
        .set_parent(window)
        .set_level(MessageLevel::Warning)
        .set_title(scope.title())
        .set_description(scope.description())
        .set_buttons(MessageButtons::OkCancelCustom(
            scope.accept_button().to_string(),
            CANCEL_BUTTON.to_string(),
        ))
        .show();

    #[cfg(not(target_os = "linux"))]
    return result_confirms_close(scope, result);
}

#[cfg(target_os = "linux")]
fn confirm_close_linux(window: &::window::Window, scope: CloseConfirmation) -> bool {
    let mut child = match Command::new(ZENITY_PROGRAM)
        .arg(ZENITY_NO_MARKUP_ARG)
        .arg(ZENITY_QUESTION_ARG)
        .args([ZENITY_TITLE_ARG, scope.title()])
        .args([ZENITY_TEXT_ARG, scope.description()])
        .args([ZENITY_OK_LABEL_ARG, scope.accept_button()])
        .args([ZENITY_CANCEL_LABEL_ARG, CANCEL_BUTTON])
        .spawn()
    {
        Ok(child) => child,
        Err(err) => {
            log::error!(
                "Unable to start {ZENITY_PROGRAM} for {:?} close confirmation: {err:#}",
                scope
            );
            return false;
        }
    };

    window.center_child_process_window(child.id());

    match child.wait() {
        Ok(status) if status.success() => true,
        Ok(status) if status.code() == Some(ZENITY_CANCEL_EXIT_CODE) => false,
        Ok(status) => {
            log::error!(
                "{ZENITY_PROGRAM} for {:?} close confirmation exited unexpectedly with {status}",
                scope
            );
            false
        }
        Err(err) => {
            log::error!(
                "Waiting for {ZENITY_PROGRAM} process {} during {:?} close confirmation failed: {err:#}",
                child.id(),
                scope
            );
            false
        }
    }
}

#[cfg(any(test, not(target_os = "linux")))]
fn result_confirms_close(scope: CloseConfirmation, result: MessageDialogResult) -> bool {
    matches!(result, MessageDialogResult::Ok)
        || matches!(result, MessageDialogResult::Custom(button) if button == scope.accept_button())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn close_scopes_have_specific_copy() {
        assert_eq!(CloseConfirmation::Pane.title(), CLOSE_PANE_TITLE);
        assert_eq!(CloseConfirmation::Tab.description(), CLOSE_TAB_DESCRIPTION);
        assert_eq!(CloseConfirmation::Window.title(), CLOSE_WINDOW_TITLE);
        assert_eq!(CloseConfirmation::Application.accept_button(), QUIT_BUTTON);
    }

    #[test]
    fn only_the_accept_button_confirms_close() {
        assert!(result_confirms_close(
            CloseConfirmation::Window,
            MessageDialogResult::Custom(CLOSE_BUTTON.to_string())
        ));
        assert!(result_confirms_close(
            CloseConfirmation::Application,
            MessageDialogResult::Ok
        ));
        assert!(!result_confirms_close(
            CloseConfirmation::Window,
            MessageDialogResult::Custom(CANCEL_BUTTON.to_string())
        ));
        assert!(!result_confirms_close(
            CloseConfirmation::Window,
            MessageDialogResult::Cancel
        ));
    }
}
