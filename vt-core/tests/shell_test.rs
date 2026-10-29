// The shell that a terminal session opens: the setting value login is the login shell of the
// account, and the shell starts as a login shell.
#[path = "support/events_test.rs"]
mod events_test;

use std::ffi::CStr;
use std::fs::OpenOptions;
use std::os::fd::AsRawFd;
use std::sync::{Mutex, OnceLock};

use soksak_sidecar_vt_core::DaemonEvent;
use soksak_sidecar_vt_core::pty::{PtyService, resolve_directory, resolve_shell};
use tokio::time::{Duration, timeout};

// Runs one at a time with the other PTY checks of the same workspace.
fn native_pty_test_lock() -> std::fs::File {
    static LOCAL: OnceLock<Mutex<()>> = OnceLock::new();
    let _local = LOCAL.get_or_init(|| Mutex::new(())).lock().unwrap();
    let path = std::env::temp_dir().join("soksak-vt-core-pty-tests.lock");
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(path)
        .expect("open native PTY test lock");
    let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
    assert_eq!(result, 0, "lock native PTY tests");
    file
}

#[test]
fn login_resolves_to_the_account_login_shell() {
    let entry = unsafe { libc::getpwuid(libc::getuid()) };
    assert!(
        !entry.is_null(),
        "the test account has no user database entry"
    );
    let expected = unsafe { CStr::from_ptr((*entry).pw_shell) }
        .to_str()
        .unwrap()
        .to_string();
    assert_eq!(resolve_shell("login").unwrap(), expected);
}

#[test]
fn an_explicit_shell_must_be_an_executable_absolute_path() {
    assert_eq!(resolve_shell("/bin/sh").unwrap(), "/bin/sh");
    for invalid in ["", "sh", "bin/sh", "/nonexistent/shell", "/etc/hosts"] {
        let error = resolve_shell(invalid).unwrap_err();
        assert!(
            error.contains("terminal shell"),
            "{invalid:?} was not rejected with a terminal shell error: {error}"
        );
    }
}

#[tokio::test]
async fn a_session_shell_starts_as_a_login_shell() {
    let _lock = native_pty_test_lock();
    let service = PtyService::new(events_test::TestEvents::new().handle());
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let (session, _) = service
        .open_shell("", "/bin/sh", None, 80, 24, tx)
        .expect("open a login shell session");
    service
        .write(&session, b"printf 'ARGV0=%s\\n' \"$0\"\n")
        .expect("write to the shell");
    let mut output = String::new();
    let found = timeout(Duration::from_secs(10), async {
        while let Some(event) = rx.recv().await {
            if let DaemonEvent::Output { data, .. } = event {
                output.push_str(&String::from_utf8_lossy(&data));
                if output.contains("ARGV0=-sh\r\n") {
                    return true;
                }
            }
        }
        false
    })
    .await
    .unwrap_or(false);
    let _ = service.close(&session);
    assert!(found, "the shell did not report argv0 -sh: {output:?}");
}

/// Collects output until it satisfies the predicate. If it is not satisfied in time, returns false
/// with the output collected so far.
async fn collect_until(
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<DaemonEvent>,
    output: &mut String,
    predicate: impl Fn(&str) -> bool,
) -> bool {
    if predicate(output) {
        return true;
    }
    timeout(Duration::from_secs(10), async {
        while let Some(event) = rx.recv().await {
            if let DaemonEvent::Output { data, .. } = event {
                output.push_str(&String::from_utf8_lossy(&data));
                if predicate(output) {
                    return true;
                }
            }
        }
        false
    })
    .await
    .unwrap_or(false)
}

/// Checks whether the needles appear in the output in this order.
fn in_order(output: &str, needles: &[&str]) -> bool {
    let mut rest = output;
    for needle in needles {
        match rest.find(needle) {
            Some(index) => rest = &rest[index + needle.len()..],
            None => return false,
        }
    }
    true
}

fn startup_directory(name: &str, files: &[(&str, &str)]) -> std::path::PathBuf {
    let directory = std::env::temp_dir().join(format!("{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).unwrap();
    for (file, content) in files {
        std::fs::write(directory.join(file), content).unwrap();
    }
    directory
}

#[tokio::test]
async fn a_zsh_session_reads_the_user_startup_files_and_reports_prompt_and_command_marks() {
    let _lock = native_pty_test_lock();
    let home = startup_directory(
        "soksak-zsh-integration-test",
        &[
            (".zshenv", "export ZSHENV_READ=1\n"),
            (".zprofile", "ZPROFILE_READ=1\n"),
            (".zshrc", "ZSHRC_READ=1\nPS1='T> '\n"),
            (".zlogin", "ZLOGIN_READ=1\n"),
        ],
    );
    // This check sets ZDOTDIR in the session environment.
    let service =
        PtyService::new(events_test::TestEvents::new().handle()).with_environment(vec![(
            "ZDOTDIR".to_string(),
            Some(home.clone().into_os_string()),
        )]);
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let opened = service.open_shell("", "/bin/zsh", None, 80, 24, tx);
    let (session, _) = opened.expect("open a zsh session");
    let mut output = String::new();
    let prompted =
        collect_until(&mut rx, &mut output, |text| text.contains("\x1b]133;A\x07")).await;
    assert!(prompted, "zsh did not report a prompt start: {output:?}");
    output.clear();
    service
        .write(
            &session,
            b"print -r -- \"STATE=$ZDOTDIR|$ZSHENV_READ$ZPROFILE_READ$ZSHRC_READ$ZLOGIN_READ|${SOKSAK_ZSH_ZDOTDIR-unset}\"\n"
        )
        .unwrap();
    let state = format!("STATE={}|1111|unset\r\n", home.display());
    let expected = [
        "\x1b]133;C\x07",
        state.as_str(),
        "\x1b]133;D;0\x07",
        "\x1b]133;A\x07",
    ];
    let reported = collect_until(&mut rx, &mut output, |text| in_order(text, &expected)).await;
    assert!(
        reported,
        "zsh did not report {expected:?} in order: {output:?}"
    );
    output.clear();
    service.write(&session, b"false\n").unwrap();
    let failed = collect_until(&mut rx, &mut output, |text| {
        in_order(
            text,
            &["\x1b]133;C\x07", "\x1b]133;D;1\x07", "\x1b]133;A\x07"],
        )
    })
    .await;
    let _ = service.close(&session);
    let _ = std::fs::remove_dir_all(&home);
    assert!(
        failed,
        "zsh did not report the failed command status: {output:?}"
    );
}

#[tokio::test]
async fn a_bash_session_stays_a_login_shell_keeps_the_user_prompt_command_and_reports_marks() {
    let _lock = native_pty_test_lock();
    // Appends the user's PROMPT_COMMAND, as macOS recommends.
    let home = startup_directory(
        "soksak-bash-integration-test",
        &[(
            ".bash_profile",
            "export PROFILE_READ=1\nuser_hook() { USER_HOOK_RAN=1; }\nPROMPT_COMMAND=\"${PROMPT_COMMAND:+$PROMPT_COMMAND; }user_hook\"\nPS1='T> '\n",
        )],
    );
    let service =
        PtyService::new(events_test::TestEvents::new().handle()).with_environment(vec![(
            "HOME".to_string(),
            Some(home.clone().into_os_string()),
        )]);
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let opened = service.open_shell("", "/bin/bash", None, 80, 24, tx);
    let (session, _) = opened.expect("open a bash session");
    let mut output = String::new();
    let prompted = collect_until(&mut rx, &mut output, |text| {
        text.contains("\x1b]133;A;redraw=last\x07")
    })
    .await;
    assert!(prompted, "bash did not report a prompt start: {output:?}");
    // The user PROMPT_COMMAND entry at the first prompt is not a command.
    assert!(
        !output.contains("\x1b]133;C\x07"),
        "the first prompt reported a command start: {output:?}"
    );
    output.clear();
    service
        .write(
            &session,
            b"printf 'STATE=%s|%s|%s|%s\\n' \"$0\" \"$PROFILE_READ$USER_HOOK_RAN\" \"${SOKSAK_BASH_INTEGRATION-unset}\" \"$(env | grep -c '^PROMPT_COMMAND=')\"\n"
        )
        .unwrap();
    let expected = [
        "\x1b]133;C\x07",
        "STATE=-bash|11|unset|0\r\n",
        "\x1b]133;D;0\x07",
        "\x1b]133;A;redraw=last\x07",
    ];
    let reported = collect_until(&mut rx, &mut output, |text| in_order(text, &expected)).await;
    assert!(
        reported,
        "bash did not report {expected:?} in order: {output:?}"
    );
    output.clear();
    service.write(&session, b"false\n").unwrap();
    let failed = collect_until(&mut rx, &mut output, |text| {
        in_order(
            text,
            &[
                "\x1b]133;C\x07",
                "\x1b]133;D;1\x07",
                "\x1b]133;A;redraw=last\x07",
            ],
        )
    })
    .await;
    assert!(
        failed,
        "bash did not report the failed command status: {output:?}"
    );
    output.clear();
    // The trap does not change the last argument ($_) of the previous command.
    service.write(&session, b"true last-argument\n").unwrap();
    service
        .write(&session, b"printf 'LAST=%s\\n' \"$_\"\n")
        .unwrap();
    let kept = collect_until(&mut rx, &mut output, |text| {
        in_order(
            text,
            &["LAST=last-argument\r\n", "\x1b]133;A;redraw=last\x07"],
        )
    })
    .await;
    assert!(kept, "the trap changed $_: {output:?}");
    output.clear();
    // An empty input line is not a command.
    service.write(&session, b"\n").unwrap();
    let empty = collect_until(&mut rx, &mut output, |text| {
        text.contains("\x1b]133;A;redraw=last\x07")
    })
    .await;
    let _ = service.close(&session);
    let _ = std::fs::remove_dir_all(&home);
    assert!(
        empty,
        "bash did not show a prompt after an empty line: {output:?}"
    );
    assert!(
        !output.contains("\x1b]133;C\x07") && !output.contains("\x1b]133;D"),
        "an empty line reported a command: {output:?}"
    );
}

#[tokio::test]
async fn a_bash_debug_trap_from_the_startup_files_is_kept_and_no_marks_are_reported() {
    let _lock = native_pty_test_lock();
    let home = startup_directory(
        "soksak-bash-debug-trap-test",
        &[(".bash_profile", "trap 'USER_TRAP=1' DEBUG\nPS1='T> '\n")],
    );
    let service =
        PtyService::new(events_test::TestEvents::new().handle()).with_environment(vec![(
            "HOME".to_string(),
            Some(home.clone().into_os_string()),
        )]);
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let opened = service.open_shell("", "/bin/bash", None, 80, 24, tx);
    let (session, _) = opened.expect("open a bash session");
    service
        .write(&session, b"printf 'TRAP=%s\\n' \"$(trap -p DEBUG)\"\n")
        .unwrap();
    let mut output = String::new();
    let shown = collect_until(&mut rx, &mut output, |text| {
        in_order(text, &["TRAP=trap -- 'USER_TRAP=1' DEBUG\r\n", "T> "])
    })
    .await;
    let _ = service.close(&session);
    let _ = std::fs::remove_dir_all(&home);
    assert!(shown, "the user DEBUG trap was not kept: {output:?}");
    assert!(
        !output.contains("\x1b]133;"),
        "bash reported marks without a command start: {output:?}"
    );
}

#[tokio::test]
async fn a_resize_during_bash_startup_does_not_report_a_command_at_the_first_prompt() {
    let _lock = native_pty_test_lock();
    let home = startup_directory(
        "soksak-bash-startup-resize-test",
        &[(".bash_profile", "PS1='T> '\n")],
    );
    let service =
        PtyService::new(events_test::TestEvents::new().handle()).with_environment(vec![(
            "HOME".to_string(),
            Some(home.clone().into_os_string()),
        )]);
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let opened = service.open_shell("", "/bin/bash", None, 80, 24, tx);
    let (session, _) = opened.expect("open a bash session");
    // The app sends the surface size right after it opens the session. SIGWINCH arrives while the
    // shell reads its startup files.
    service.resize(&session, 79, 24).unwrap();
    service.resize(&session, 80, 24).unwrap();
    let mut output = String::new();
    let prompted = collect_until(&mut rx, &mut output, |text| {
        in_order(text, &["\x1b]133;A;redraw=last\x07", "T> "])
    })
    .await;
    let _ = service.close(&session);
    let _ = std::fs::remove_dir_all(&home);
    assert!(prompted, "bash did not show its first prompt: {output:?}");
    assert!(
        !output.contains("\x1b]133;C\x07"),
        "the first prompt reported a command start: {output:?}"
    );
}

#[test]
fn a_start_directory_must_be_an_existing_absolute_directory() {
    assert_eq!(resolve_directory("/tmp").unwrap(), "/tmp");
    for invalid in ["", "tmp", "/nonexistent/directory", "/etc/hosts"] {
        let error = resolve_directory(invalid).unwrap_err();
        assert!(
            error.contains("terminal directory"),
            "{invalid:?} was not rejected with a terminal directory error: {error}"
        );
    }
}

#[tokio::test]
async fn a_session_starts_in_the_requested_directory() {
    let _lock = native_pty_test_lock();
    let directory = startup_directory("soksak-start-directory-test", &[]);
    let directory = std::fs::canonicalize(&directory).unwrap();
    let service = PtyService::new(events_test::TestEvents::new().handle());
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let (session, _) = service
        .open_shell("", "/bin/sh", Some(directory.to_str().unwrap()), 80, 24, tx)
        .expect("open a shell session");
    service
        .write(&session, b"printf 'PWD=%s\\n' \"$(pwd -P)\"\n")
        .unwrap();
    let mut output = String::new();
    let expected = format!("PWD={}\r\n", directory.display());
    let found = collect_until(&mut rx, &mut output, |text| text.contains(&expected)).await;
    let _ = service.close(&session);
    let _ = std::fs::remove_dir_all(&directory);
    assert!(
        found,
        "the shell did not start in {}: {output:?}",
        directory.display()
    );
}

/// The directory URI that the shell reports: percent-encodes each byte that is not an unreserved
/// character or `/`.
fn directory_uri(path: &std::path::Path) -> String {
    let host = soksak_sidecar_vt_core::directory_uri::host_name().unwrap();
    let mut encoded = String::new();
    for byte in path.to_str().unwrap().bytes() {
        if byte.is_ascii_alphanumeric() || b"._~/-".contains(&byte) {
            encoded.push(byte as char);
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    format!("\x1b]7;file://{host}{encoded}\x07")
}

async fn reports_its_directory(shell: &str, variable: &str, files: &[(&str, &str)]) {
    let _lock = native_pty_test_lock();
    let home = startup_directory(&format!("soksak-{variable}-osc7-test"), files);
    let start = home.join("a b-한글%");
    std::fs::create_dir_all(&start).unwrap();
    let start = std::fs::canonicalize(&start).unwrap();
    let service =
        PtyService::new(events_test::TestEvents::new().handle()).with_environment(vec![(
            variable.to_string(),
            Some(home.clone().into_os_string()),
        )]);
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let opened = service.open_shell("", shell, Some(start.to_str().unwrap()), 80, 24, tx);
    let (session, _) = opened.expect("open a shell session");
    let expected = directory_uri(&start);
    let mut output = String::new();
    let first = collect_until(&mut rx, &mut output, |text| text.contains(&expected)).await;
    // After a directory change, the shell reports the new directory before the next prompt.
    service.write(&session, b"cd /\n").unwrap();
    let changed = collect_until(&mut rx, &mut output, |text| {
        in_order(
            text,
            &[expected.as_str(), &directory_uri(std::path::Path::new("/"))],
        )
    })
    .await;
    let _ = service.close(&session);
    let _ = std::fs::remove_dir_all(&home);
    assert!(first, "{shell} did not report {expected:?}: {output:?}");
    assert!(changed, "{shell} did not report / after cd: {output:?}");
}

#[tokio::test]
async fn zsh_reports_its_working_directory_before_each_prompt() {
    reports_its_directory("/bin/zsh", "ZDOTDIR", &[(".zshrc", "PS1='T> '\n")]).await;
}

#[tokio::test]
async fn bash_reports_its_working_directory_before_each_prompt() {
    reports_its_directory("/bin/bash", "HOME", &[(".bash_profile", "PS1='T> '\n")]).await;
}
