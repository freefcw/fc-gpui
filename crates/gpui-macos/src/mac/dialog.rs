use crate::{DialogError, DialogKind, DialogOptions};
use futures::channel::oneshot;
use objc2::MainThreadMarker;
use objc2_app_kit::{NSAlert, NSAlertFirstButtonReturn, NSAlertStyle};
use objc2_foundation::NSString;

pub fn show_dialog(options: DialogOptions) -> oneshot::Receiver<Result<usize, DialogError>> {
    let (tx, rx) = oneshot::channel();
    unsafe {
        let alert = NSAlert::new(MainThreadMarker::new_unchecked());
        let style = match options.kind {
            DialogKind::Info => NSAlertStyle::Informational,
            DialogKind::Warning => NSAlertStyle::Warning,
            DialogKind::Error => NSAlertStyle::Critical,
        };
        alert.setAlertStyle(style);

        let title = NSString::from_str(options.title.as_ref());
        alert.setMessageText(&title);

        let informative = if let Some(detail) = &options.detail {
            format!("{}\n\n{}", options.message.as_ref(), detail.as_ref())
        } else {
            options.message.as_ref().to_string()
        };
        let message = NSString::from_str(&informative);
        alert.setInformativeText(&message);

        for button_label in &options.buttons {
            let label = NSString::from_str(button_label.as_ref());
            let _ = alert.addButtonWithTitle(&label);
        }
        if options.buttons.is_empty() {
            let ok_label = NSString::from_str("OK");
            let _ = alert.addButtonWithTitle(&ok_label);
        }

        let response = alert.runModal();
        let index = (response - NSAlertFirstButtonReturn) as usize;
        tx.send(Ok(index)).ok();
    }
    rx
}
