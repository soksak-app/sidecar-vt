// The shell integration files that the sessions of one service install at the same time.

use soksak_sidecar_vt_core::shell_integration::install_in;
use std::sync::{Arc, Barrier};

// Sessions that start at the same time in one process each install the same files, and each
// install succeeds.
#[test]
fn sessions_that_start_together_each_install_the_shell_integration() {
    const SESSIONS: usize = 8;
    for round in 0..50 {
        let temporary = tempfile::tempdir().unwrap();
        let barrier = Arc::new(Barrier::new(SESSIONS));
        let threads: Vec<_> = (0..SESSIONS)
            .map(|_| {
                let barrier = barrier.clone();
                let temporary = temporary.path().to_path_buf();
                std::thread::spawn(move || {
                    barrier.wait();
                    install_in(&temporary)
                })
            })
            .collect();
        for thread in threads {
            let root = thread.join().unwrap().unwrap_or_else(|error| {
                panic!("round {round}: an install of a session failed: {error}")
            });
            assert!(root.join("zsh/.zshenv").is_file(), "round {round}");
            assert!(
                root.join("bash-integration.bash").is_file(),
                "round {round}"
            );
        }
    }
}
