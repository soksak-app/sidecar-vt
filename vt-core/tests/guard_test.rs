// The destructive-command guard: each declared rule refuses its command,
// ordinary commands are allowed, text that cannot be judged is refused, and each decision of an input is written to the event log
// with its rule id.
use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use soksak_sidecar_vt_core::events::{EventLog, Events, Options, POLICY_FILE, read_lines};
use soksak_sidecar_vt_core::guard::{
    ALLOW_EVENT, LineGuard, LineInput, REFUSE_EVENT, Verdict, judge, judge_transaction,
};

fn assert_refused(command: &str, rule: &str) {
    match judge(command) {
        Verdict::Refuse {
            rule: found,
            reason,
        } => {
            assert_eq!(found, rule, "{command:?} refused with reason {reason:?}");
            assert!(!reason.is_empty(), "{command:?} has a reason");
        }
        Verdict::Allow => panic!("{command:?} was allowed; expected refusal with rule {rule}"),
    }
}

fn assert_allowed(command: &str) {
    assert_eq!(
        judge(command),
        Verdict::Allow,
        "{command:?} is an ordinary command"
    );
}

#[test]
fn refuses_recursive_forced_delete_of_root() {
    assert_refused("rm -rf /", "rm.recursive-force.root-or-home");
    assert_refused("rm -fr /*", "rm.recursive-force.root-or-home");
}

#[test]
fn refuses_recursive_forced_delete_of_home() {
    assert_refused("rm -rf ~", "rm.recursive-force.root-or-home");
    assert_refused("rm -Rf ~/", "rm.recursive-force.root-or-home");
}

#[test]
fn refuses_recursive_forced_delete_of_home_variable() {
    assert_refused("rm -rf $HOME", "rm.recursive-force.root-or-home");
    assert_refused("rm -rf \"${HOME}\"", "rm.recursive-force.root-or-home");
    assert_refused(
        "rm --recursive --force $HOME/*",
        "rm.recursive-force.root-or-home",
    );
}

#[test]
fn refuses_recursive_forced_delete_of_unresolved_path() {
    assert_refused("rm -rf \"$TARGET\"", "rm.recursive-force.undecidable");
    assert_refused("rm -rf $(cat list.txt)", "rm.recursive-force.undecidable");
}

#[test]
fn refuses_forced_push_to_main() {
    assert_refused("git push --force origin main", "git.push.force-protected");
    assert_refused("git push -f origin HEAD:master", "git.push.force-protected");
    assert_refused("git push origin +main", "git.push.force-protected");
    assert_refused(
        "git push --force-with-lease origin refs/heads/main",
        "git.push.force-protected",
    );
}

#[test]
fn refuses_forced_push_without_a_named_branch() {
    assert_refused("git push --force", "git.push.force-protected");
    assert_refused("git push -f origin", "git.push.force-protected");
}

#[test]
fn refuses_discard_of_all_uncommitted_changes_by_checkout() {
    assert_refused("git checkout -- .", "git.discard.uncommitted");
    assert_refused("git -C ./repo checkout .", "git.discard.uncommitted");
}

#[test]
fn refuses_hard_reset() {
    assert_refused("git reset --hard HEAD~1", "git.discard.uncommitted");
}

#[test]
fn refuses_forced_clean() {
    assert_refused("git clean -fd", "git.discard.uncommitted");
    assert_refused("git clean --force -x", "git.discard.uncommitted");
}

#[test]
fn refuses_mkfs_on_a_device() {
    assert_refused("mkfs.ext4 /dev/disk2s1", "disk.format");
    assert_refused("sudo mkfs -t vfat /dev/sdb1", "disk.format");
}

#[test]
fn refuses_dd_output_to_a_block_device() {
    assert_refused("dd if=image.iso of=/dev/disk2 bs=1m", "disk.write-device");
    assert_refused("dd if=/dev/zero of=$DISK", "disk.write-device");
}

#[test]
fn refuses_redirection_to_a_block_device() {
    assert_refused("cat image.bin > /dev/sda", "disk.write-device");
    assert_refused("cat image.bin >/dev/nvme0n1", "disk.write-device");
}

#[test]
fn refuses_diskutil_erase() {
    assert_refused("diskutil eraseDisk JHFS+ Scratch /dev/disk2", "disk.format");
}

#[test]
fn refuses_destructive_command_inside_a_shell_script_argument() {
    assert_refused("sh -c 'rm -rf ~'", "rm.recursive-force.root-or-home");
    assert_refused("eval \"rm -rf /\"", "rm.recursive-force.root-or-home");
}

#[test]
fn refuses_destructive_command_inside_a_command_substitution() {
    assert_refused("echo $(rm -rf ~)", "rm.recursive-force.root-or-home");
    assert_refused("echo `git reset --hard`", "git.discard.uncommitted");
}

#[test]
fn refuses_command_whose_name_is_computed() {
    assert_refused("$TOOL -rf ~", "undecidable");
}

#[test]
fn refuses_a_destructive_command_after_a_separator() {
    assert_refused("ls -la && rm -rf ~", "rm.recursive-force.root-or-home");
    assert_refused("ls | xargs echo; git clean -f", "git.discard.uncommitted");
}

#[test]
fn allows_ordinary_listing_command() {
    assert_allowed("ls -la");
}

#[test]
fn allows_recursive_delete_of_a_relative_project_folder() {
    assert_allowed("rm -rf ./build");
    assert_allowed("rm -rf ~/project/build");
}

#[test]
fn allows_non_destructive_git_commands() {
    assert_allowed("git push origin feature/x");
    assert_allowed("git push --force origin feature/x");
    assert_allowed("git reset HEAD~1");
    assert_allowed("git clean -nd");
    assert_allowed("git checkout main");
}

#[test]
fn allows_dd_to_a_regular_file() {
    assert_allowed("dd if=/dev/zero of=out.bin bs=1m count=1");
}

#[test]
fn allows_blank_input() {
    assert_allowed("");
    assert_allowed("   ");
}

#[test]
fn refuses_unbalanced_single_quote_as_unparseable() {
    assert_refused("echo 'abc", "unparseable");
}

#[test]
fn refuses_unbalanced_double_quote_as_unparseable() {
    assert_refused("echo \"abc", "unparseable");
}

#[test]
fn refuses_text_ending_with_a_backslash_as_unparseable() {
    assert_refused("ls \\", "unparseable");
}

#[test]
fn refuses_separators_without_a_command_as_unparseable() {
    assert_refused(";", "unparseable");
    assert_refused("&& ||", "unparseable");
}

#[test]
fn refuses_unclosed_command_substitution_as_unparseable() {
    assert_refused("echo $(ls", "unparseable");
}

#[test]
fn refuses_shell_option_c_without_a_script_as_undecidable() {
    assert_refused("bash -c", "undecidable");
}

#[test]
fn judges_a_line_that_the_input_submits() {
    let mut line = LineGuard::default();
    let verdicts = line.observe("s1", &[LineInput::Text("ls -la"), LineInput::Enter]);
    assert_eq!(verdicts, vec![Verdict::Allow]);
}

#[test]
fn refuses_a_submitted_destructive_line_and_keeps_the_transaction_unwritten() {
    let mut line = LineGuard::default();
    let verdicts = line.observe("s1", &[LineInput::Text("rm -rf ~"), LineInput::Enter]);
    assert!(matches!(
        verdicts.last(),
        Some(Verdict::Refuse {
            rule: "rm.recursive-force.root-or-home",
            ..
        })
    ));
    // The refused transaction is not written, so the shell line stays empty: an Enter on it runs nothing and is allowed,
    // and an ordinary line is allowed.
    assert_eq!(
        line.observe("s1", &[LineInput::Enter]),
        vec![Verdict::Allow]
    );
    assert_eq!(
        line.observe("s1", &[LineInput::Text("ls"), LineInput::Enter]),
        vec![Verdict::Allow]
    );
}

#[test]
fn refuses_a_line_recalled_by_history_as_untracked() {
    let mut line = LineGuard::default();
    let verdicts = line.observe("s1", &[LineInput::Other, LineInput::Enter]);
    assert!(matches!(
        verdicts.last(),
        Some(Verdict::Refuse {
            rule: "untracked",
            ..
        })
    ));
}

#[test]
fn judges_each_line_of_a_pasted_text_in_order() {
    let mut line = LineGuard::default();
    let verdicts = line.observe("s1", &[LineInput::Text("ls\nrm -rf ~\nls\n")]);
    assert_eq!(
        verdicts,
        vec![
            Verdict::Allow,
            Verdict::Refuse {
                rule: "rm.recursive-force.root-or-home",
                reason: "recursive forced deletion of the root or the home folder".to_string(),
            },
        ]
    );
}

#[test]
fn every_decision_of_an_input_is_written_to_the_event_log_with_its_rule() {
    let directory = tempfile::tempdir().expect("create the folder of the events");
    let all_classes = ["lifecycle", "input", "call", "io", "frame"];
    let classes: serde_json::Map<String, serde_json::Value> = all_classes
        .iter()
        .map(|class| (class.to_string(), json!("trace")))
        .collect();
    let policy = directory.path().join(POLICY_FILE);
    std::fs::write(
        &policy,
        json!({"schema": 1, "classes": classes}).to_string(),
    )
    .expect("write the policy file");
    let mut options = Options::service(directory.path().join("logs"), "test".to_string());
    options.policy_file = Some(policy);
    options.batch_interval = Duration::from_millis(20);
    let log: Events = Arc::new(EventLog::start(options).expect("start the writer"));

    let mut line = LineGuard::default();
    let cids = vec!["cid-1".to_string()];
    let allowed = judge_transaction(
        &log,
        &mut line,
        "s1",
        &cids,
        &[LineInput::Text("ls"), LineInput::Enter],
    );
    assert_eq!(allowed, None);
    let refused = judge_transaction(
        &log,
        &mut line,
        "s1",
        &cids,
        &[
            LineInput::Discard,
            LineInput::Text("rm -rf ~"),
            LineInput::Enter,
        ],
    );
    assert_eq!(
        refused.map(|(rule, _)| rule),
        Some("rm.recursive-force.root-or-home")
    );

    log.flush(Duration::from_secs(10))
        .expect("flush the writer");
    let lines = read_lines(log.path()).expect("read the file of the events");
    let decisions: Vec<(&str, &str)> = lines
        .iter()
        .filter_map(|event| {
            let name = event["event"].as_str()?;
            if name != ALLOW_EVENT && name != REFUSE_EVENT {
                return None;
            }
            Some((name, event["fields"]["rule"].as_str().unwrap_or("")))
        })
        .collect();
    assert_eq!(
        decisions,
        vec![
            (ALLOW_EVENT, ""),
            (REFUSE_EVENT, "rm.recursive-force.root-or-home")
        ]
    );
}
