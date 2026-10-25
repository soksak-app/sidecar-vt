use std::sync::Mutex;

/// The identifiers of the sessions whose next wait fails.
static FAILING_WAITS: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// Makes the next wait of the child of the session `session_id` fail.
pub fn fail_wait_of(session_id: &str) {
    FAILING_WAITS.lock().unwrap().push(session_id.to_string());
}

/// Whether the next wait of the session fails; the failure is taken once.
pub(crate) fn take_wait_failure(session_id: &str) -> bool {
    let mut failing = FAILING_WAITS.lock().unwrap();
    match failing.iter().position(|id| id == session_id) {
        Some(index) => {
            failing.remove(index);
            true
        }
        None => false,
    }
}
