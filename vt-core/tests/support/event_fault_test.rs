use super::*;

impl EventLog {
    /// Test-only: poisons the lock of the writer handle, as a panic in its holder does, and returns the count of joins that
    /// drop makes. The drop of the log must join the writer.
    #[cfg(feature = "fault-injection")]
    pub fn poison_writer_for_test(&self) -> Arc<AtomicUsize> {
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = self.writer.lock().unwrap();
            panic!("the writer lock is poisoned by the fault-injection feature");
        }));
        self.joins.clone()
    }

    /// Test-only: stores `stop` as the stop of the policy watch and poisons the lock that guards it, as a panic in the
    /// holder of the lock does. The drop of the log must still run the stop.
    #[cfg(feature = "fault-injection")]
    pub fn poison_policy_watch_for_test(&self, stop: impl FnOnce() + Send + 'static) {
        *self.policy_watch.lock().unwrap() = Some(Box::new(stop));
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = self.policy_watch.lock().unwrap();
            panic!("the policy watch lock is poisoned by the fault-injection feature");
        }));
    }
}
