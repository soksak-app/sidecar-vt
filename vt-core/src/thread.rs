//! The entrance that starts a thread of the service (core docs/spec/logging.md#census): a panic of the thread is recorded as
//! an error record that names the thread and carries the text of the panic, in the place of ending the thread without a trace.

use crate::events::Events;
use std::thread::JoinHandle;

/// The text of a panic payload.
fn panic_text(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(text) = payload.downcast_ref::<&str>() {
        (*text).to_string()
    } else if let Some(text) = payload.downcast_ref::<String>() {
        text.clone()
    } else {
        "the payload of the panic is not text".to_string()
    }
}

/// Starts the thread `name` with `work`. A panic of `work` is the error record `thread <name>` and the thread ends.
pub fn start<F>(log: &Events, name: &str, work: F) -> Result<JoinHandle<()>, String>
where
    F: FnOnce() + Send + 'static,
{
    let log = log.clone();
    let label = name.to_string();
    std::thread::Builder::new()
        .name(name.to_string())
        .spawn(move || {
            if let Err(payload) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(work)) {
                log.record_error(
                    &format!("thread {label}"),
                    format!("panicked: {}", panic_text(payload.as_ref())),
                );
            }
        })
        .map_err(|error| format!("start the thread {name}: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_panic_of_a_thread_is_an_error_record_that_names_the_thread() {
        let events = crate::events::test_support::TestEvents::new();
        let handle = start(&events.handle(), "probe", || panic!("injected panic")).unwrap();
        handle.join().expect("the entrance catches the panic");
        let lines = events.lines();
        let record = lines
            .iter()
            .find(|line| line["event"] == "sidecar.vt.record.error")
            .expect("the panic is recorded");
        assert_eq!(record["fields"]["where"], "thread probe");
        assert_eq!(record["fields"]["text"], "panicked: injected panic");
    }
}
