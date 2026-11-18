//! The guard of destructive shell commands. Its rules are the `RULES` table of this module.
//!
//! [`judge`] decides one command text. It returns [`Verdict::Allow`] when no rule of [`RULES`] matches, and [`Verdict::Refuse`]
//! with the id of the first matching rule. Text that cannot be tokenized, or that holds a command the guard cannot name, is refused
//! with the id `unparseable` or `undecidable`: the guard is fail-closed.
//!
//! [`LineGuard`] follows the command line that the shell of a session holds. The input of a surface changes that line, and the
//! guard judges the line at each submission. Input that the guard cannot follow exactly (cursor movement, history recall, completion)
//! makes the line untracked, and a submission of an untracked line is refused until the line is discarded with Ctrl-C or Ctrl-U.
//!
//! [`judge_transaction`] writes one event per verdict to the event log (`sidecar.vt.guard.allow` and
//! `sidecar.vt.guard.refuse`), so every decision is in the log with its rule id.

use serde_json::json;

use crate::events::{Events, Level, Subject};

/// The event of a line that the guard allowed.
pub const ALLOW_EVENT: &str = "sidecar.vt.guard.allow";
/// The event of a line that the guard refused.
pub const REFUSE_EVENT: &str = "sidecar.vt.guard.refuse";

/// The rule id of text that the tokenizer cannot read, such as an unbalanced quote.
pub const UNPARSEABLE: &str = "unparseable";
/// The rule id of a command whose name or target the guard cannot resolve before it runs, such as a computed command name.
pub const UNDECIDABLE: &str = "undecidable";
/// The rule id of a line that the guard cannot follow because the input edited it in a way the guard does not model.
pub const UNTRACKED: &str = "untracked";

/// The most nested levels of command substitution, `sh -c` and `eval` that the guard judges.
const MAX_DEPTH: usize = 8;

/// The decision on one command text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Allow,
    Refuse { rule: &'static str, reason: String },
}

/// A destructive-command rule: its stable id, what it refuses, and the test that matches a simple command.
pub struct Rule {
    pub id: &'static str,
    pub reason: &'static str,
    matches: fn(&[Word]) -> bool,
}

/// The declared destructive-command rules. A simple command that matches any of them is refused with the id of the first match.
pub const RULES: &[Rule] = &[
    Rule {
        id: "rm.recursive-force.root-or-home",
        reason: "recursive forced deletion of the root or the home folder",
        matches: rm_root_or_home,
    },
    Rule {
        id: "rm.recursive-force.undecidable",
        reason: "recursive forced deletion of a path that the guard cannot resolve",
        matches: rm_undecidable,
    },
    Rule {
        id: "git.push.force-protected",
        reason: "forced push to a protected branch, or to a branch that the guard cannot name",
        matches: git_push_force_protected,
    },
    Rule {
        id: "git.discard.uncommitted",
        reason: "discard of uncommitted work in the working tree or the index",
        matches: git_discard_uncommitted,
    },
    Rule {
        id: "disk.format",
        reason: "creation of a file system, or erasure of a disk or a partition",
        matches: disk_format,
    },
    Rule {
        id: "disk.write-device",
        reason: "write to a block device, or to a device that the guard cannot resolve",
        matches: disk_write_device,
    },
];

/// A word of a command after its quotes are removed. `expands` is set when the word holds a `$` expansion or a command
/// substitution that is not inside single quotes; its text keeps the expansion as written.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Word {
    text: String,
    expands: bool,
}

/// Decides one command text.
pub fn judge(command: &str) -> Verdict {
    judge_at(command, 0)
}

fn refuse(rule: &'static str, reason: &str) -> Verdict {
    Verdict::Refuse {
        rule,
        reason: reason.to_string(),
    }
}

fn judge_at(command: &str, depth: usize) -> Verdict {
    if depth > MAX_DEPTH {
        return refuse(UNPARSEABLE, "the command nests too deeply to judge");
    }
    let mut substitutions = Vec::new();
    let commands = match tokenize(command, &mut substitutions) {
        Ok(commands) => commands,
        Err(reason) => return refuse(UNPARSEABLE, reason),
    };
    if commands.is_empty() {
        // Blank text and comment-only text run nothing in the shell.
        let trimmed = command.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            return Verdict::Allow;
        }
        return refuse(UNPARSEABLE, "the text holds separators and no command");
    }
    for text in &substitutions {
        let verdict = judge_at(text, depth + 1);
        if verdict != Verdict::Allow {
            return verdict;
        }
    }
    for words in &commands {
        let verdict = judge_words(words, depth);
        if verdict != Verdict::Allow {
            return verdict;
        }
    }
    Verdict::Allow
}

fn judge_words(words: &[Word], depth: usize) -> Verdict {
    let words = skip_prefixes(words);
    let Some(first) = words.first() else {
        return Verdict::Allow;
    };
    if first.expands {
        return refuse(UNDECIDABLE, "the command name is computed at run time");
    }
    if let Some(script) = nested_script(words) {
        return match script {
            Ok(text) => judge_at(&text, depth + 1),
            Err(reason) => refuse(UNDECIDABLE, reason),
        };
    }
    for rule in RULES {
        if (rule.matches)(words) {
            return refuse(rule.id, rule.reason);
        }
    }
    Verdict::Allow
}

/// The command words that a shell runs with a script of its own: `sh -c TEXT` and `eval TEXT...`. The script is judged as
/// command text. A shell with a `-c` option and no script is refused, because its script is not known.
fn nested_script(words: &[Word]) -> Option<Result<String, &'static str>> {
    let name = base_name(&words[0].text);
    if matches!(name, "sh" | "bash" | "zsh" | "dash" | "ksh") {
        let index = words.iter().skip(1).position(|word| {
            word.text.starts_with('-')
                && !word.text.starts_with("--")
                && word.text.len() > 1
                && word.text[1..].chars().all(|c| c.is_ascii_alphabetic())
                && word.text.contains('c')
        })? + 1;
        return Some(match words.get(index + 1) {
            Some(script) => Ok(script.text.clone()),
            None => Err("the shell option -c has no script that the guard can read"),
        });
    }
    if name == "eval" && words.len() > 1 {
        let text = words[1..]
            .iter()
            .map(|word| word.text.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        return Some(Ok(text));
    }
    None
}

/// The words of a simple command after the wrappers and the assignments before its command name (`sudo`, `env`, `VAR=value`).
/// The options of a wrapper are skipped; `VALUE_OPTIONS` take their value as the next word.
fn skip_prefixes(words: &[Word]) -> &[Word] {
    const WRAPPERS: &[&str] = &[
        "sudo", "env", "command", "exec", "nohup", "time", "builtin", "nice", "!", "{", "}",
    ];
    const VALUE_OPTIONS: &[&str] = &["-u", "-g", "-C", "-h", "-n", "-p", "-U", "-r", "-t", "-D"];
    let mut index = 0;
    let mut wrapped = false;
    while index < words.len() {
        let word = &words[index];
        let text = word.text.as_str();
        if !word.expands && is_assignment(text) {
            index += 1;
        } else if WRAPPERS.contains(&text) {
            wrapped = true;
            index += 1;
        } else if wrapped && text.starts_with('-') && text.len() > 1 {
            index += if VALUE_OPTIONS.contains(&text) { 2 } else { 1 };
        } else {
            break;
        }
    }
    &words[index.min(words.len())..]
}

fn is_assignment(text: &str) -> bool {
    let Some((name, _)) = text.split_once('=') else {
        return false;
    };
    !name.is_empty()
        && !name.starts_with(|c: char| c.is_ascii_digit())
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn base_name(text: &str) -> &str {
    text.rsplit('/').next().unwrap_or(text)
}

fn is_command(words: &[Word], names: &[&str]) -> bool {
    words.first().is_some_and(|word| names.contains(&base_name(&word.text)))
}

/// Splits command text into simple commands, each a list of words. Quotes, escapes, comments, `$` expansions and command
/// substitutions are read; the text of each substitution is appended to `substitutions` for judging. Errors name the defect.
fn tokenize(text: &str, substitutions: &mut Vec<String>) -> Result<Vec<Vec<Word>>, &'static str> {
    let chars: Vec<char> = text.chars().collect();
    let mut commands: Vec<Vec<Word>> = Vec::new();
    let mut current: Vec<Word> = Vec::new();
    let mut word: Option<Word> = None;
    let mut index = 0;
    while index < chars.len() {
        let c = chars[index];
        match c {
            ' ' | '\t' => {
                end_word(&mut word, &mut current);
                index += 1;
            }
            '\n' | '\r' | ';' | '&' | '|' | '(' | ')' => {
                end_word(&mut word, &mut current);
                end_command(&mut current, &mut commands);
                index += 1;
            }
            '#' if word.is_none() => {
                while index < chars.len() && chars[index] != '\n' {
                    index += 1;
                }
            }
            '\\' => {
                let next = chars
                    .get(index + 1)
                    .copied()
                    .ok_or("the text ends with a backslash")?;
                index += 2;
                if next != '\n' {
                    word_mut(&mut word).text.push(next);
                }
            }
            '\'' => {
                let end = (index + 1..chars.len())
                    .find(|&j| chars[j] == '\'')
                    .ok_or("unbalanced single quote")?;
                word_mut(&mut word)
                    .text
                    .extend(&chars[index + 1..end]);
                index = end + 1;
            }
            '"' => {
                index += 1;
                let target = word_mut(&mut word);
                loop {
                    let Some(&d) = chars.get(index) else {
                        return Err("unbalanced double quote");
                    };
                    match d {
                        '"' => {
                            index += 1;
                            break;
                        }
                        '\\' => match chars.get(index + 1) {
                            Some(&e) if matches!(e, '$' | '`' | '"' | '\\' | '\n') => {
                                if e != '\n' {
                                    target.text.push(e);
                                }
                                index += 2;
                            }
                            _ => {
                                target.text.push('\\');
                                index += 1;
                            }
                        },
                        '$' | '`' => {
                            index = expansion(&chars, index, &mut target.text, substitutions)?;
                            target.expands = true;
                        }
                        _ => {
                            target.text.push(d);
                            index += 1;
                        }
                    }
                }
            }
            '$' | '`' => {
                let target = word_mut(&mut word);
                index = expansion(&chars, index, &mut target.text, substitutions)?;
                target.expands = true;
            }
            _ => {
                word_mut(&mut word).text.push(c);
                index += 1;
            }
        }
    }
    end_word(&mut word, &mut current);
    end_command(&mut current, &mut commands);
    Ok(commands)
}

/// Reads the expansion that starts at `chars[index]`, a `$` or a backtick, and appends it as written to `text`. A command
/// substitution is also appended to `substitutions`. Returns the index after the expansion.
fn expansion(
    chars: &[char],
    index: usize,
    text: &mut String,
    substitutions: &mut Vec<String>,
) -> Result<usize, &'static str> {
    if chars[index] == '`' {
        let end = (index + 1..chars.len())
            .find(|&j| chars[j] == '`')
            .ok_or("unbalanced backtick")?;
        substitutions.push(chars[index + 1..end].iter().collect());
        text.extend(&chars[index..=end]);
        return Ok(end + 1);
    }
    match chars.get(index + 1).copied() {
        Some('(') => {
            let mut depth: i32 = 0;
            let mut end = index + 1;
            loop {
                let Some(&d) = chars.get(end) else {
                    return Err("unbalanced parenthesis in a command substitution");
                };
                match d {
                    '(' => depth += 1,
                    ')' => depth -= 1,
                    _ => {}
                }
                if depth == 0 {
                    break;
                }
                end += 1;
            }
            substitutions.push(chars[index + 2..end].iter().collect());
            text.extend(&chars[index..=end]);
            Ok(end + 1)
        }
        Some('{') => {
            let end = (index + 2..chars.len())
                .find(|&j| chars[j] == '}')
                .ok_or("unbalanced brace in a parameter expansion")?;
            text.extend(&chars[index..=end]);
            Ok(end + 1)
        }
        Some(c) if c.is_alphanumeric() || c == '_' => {
            let mut end = index + 1;
            while end < chars.len() && (chars[end].is_alphanumeric() || chars[end] == '_') {
                end += 1;
            }
            text.extend(&chars[index..end]);
            Ok(end)
        }
        Some(c) if "?#@*!$-".contains(c) => {
            text.extend(&chars[index..index + 2]);
            Ok(index + 2)
        }
        _ => {
            text.push('$');
            Ok(index + 1)
        }
    }
}

fn word_mut(word: &mut Option<Word>) -> &mut Word {
    word.get_or_insert_with(Word::default)
}

fn end_word(word: &mut Option<Word>, current: &mut Vec<Word>) {
    if let Some(finished) = word.take() {
        current.push(finished);
    }
}

fn end_command(current: &mut Vec<Word>, commands: &mut Vec<Vec<Word>>) {
    if !current.is_empty() {
        commands.push(std::mem::take(current));
    }
}

/// The value of a word with the home folder written as `$HOME` or `${HOME}` replaced by `~`, or `None` when the word holds an
/// expansion that the guard does not resolve.
fn resolved_path(word: &Word) -> Option<String> {
    if !word.expands {
        return Some(word.text.clone());
    }
    let text = word.text.as_str();
    let rest = ["$HOME", "${HOME}"].iter().find_map(|home| {
        let rest = text.strip_prefix(*home)?;
        (rest.is_empty() || rest.starts_with('/')).then_some(rest)
    })?;
    let path = format!("~{rest}");
    if path.contains(['$', '`']) {
        None
    } else {
        Some(path)
    }
}

/// Whether `path` names the root folder or the home folder, with any trailing slash, `/.` or `/*`.
fn is_root_or_home(path: &str) -> bool {
    let mut rest = path;
    loop {
        if let Some(stripped) = rest.strip_suffix("/*").or_else(|| rest.strip_suffix("/.")) {
            rest = stripped;
        } else if let Some(stripped) = rest.strip_suffix('/') {
            rest = stripped;
        } else {
            break;
        }
    }
    (rest.is_empty() && path.starts_with('/')) || rest == "~"
}

/// The flags and the operands of `rm`: whether it is recursive, whether it is forced, and its operands.
fn rm_arguments(words: &[Word]) -> (bool, bool, Vec<&Word>) {
    let mut recursive = false;
    let mut force = false;
    let mut operands = Vec::new();
    let mut options_open = true;
    for word in &words[1..] {
        let text = word.text.as_str();
        if options_open && text == "--" {
            options_open = false;
        } else if options_open && text.starts_with("--") {
            match text {
                "--recursive" => recursive = true,
                "--force" => force = true,
                _ => {}
            }
        } else if options_open && text.starts_with('-') && text.len() > 1 {
            for flag in text[1..].chars() {
                match flag {
                    'r' | 'R' => recursive = true,
                    'f' => force = true,
                    _ => {}
                }
            }
        } else {
            operands.push(word);
        }
    }
    (recursive, force, operands)
}

fn rm_root_or_home(words: &[Word]) -> bool {
    if !is_command(words, &["rm"]) {
        return false;
    }
    let (recursive, force, operands) = rm_arguments(words);
    recursive
        && force
        && operands
            .iter()
            .any(|operand| resolved_path(operand).is_some_and(|path| is_root_or_home(&path)))
}

fn rm_undecidable(words: &[Word]) -> bool {
    if !is_command(words, &["rm"]) {
        return false;
    }
    let (recursive, force, operands) = rm_arguments(words);
    recursive && force && operands.iter().any(|operand| resolved_path(operand).is_none())
}

/// The subcommand of `git` and the index of its word: global options that take a value are skipped with it.
fn git_subcommand(words: &[Word]) -> Option<(usize, &str)> {
    if !is_command(words, &["git"]) {
        return None;
    }
    let mut index = 1;
    while index < words.len() {
        let text = words[index].text.as_str();
        if matches!(text, "-C" | "-c" | "--git-dir" | "--work-tree") {
            index += 2;
        } else if text.starts_with('-') {
            index += 1;
        } else {
            return Some((index, text));
        }
    }
    None
}

fn git_push_force_protected(words: &[Word]) -> bool {
    let Some((index, "push")) = git_subcommand(words) else {
        return false;
    };
    let arguments = &words[index + 1..];
    let mut force = false;
    let mut all_branches = false;
    let mut refspecs: Vec<&str> = Vec::new();
    for word in arguments {
        let text = word.text.as_str();
        let short_force = text.starts_with('-') && !text.starts_with("--") && text[1..].contains('f');
        if text == "--force" || text.starts_with("--force-") || short_force {
            force = true;
        } else if text == "--mirror" {
            // A mirror push deletes the remote branches that the local repository does not have.
            force = true;
            all_branches = true;
        } else if text == "--all" {
            all_branches = true;
        } else if !text.starts_with('-') {
            refspecs.push(text);
        }
    }
    let plus_refspec = refspecs.iter().any(|refspec| refspec.starts_with('+'));
    if !(force || plus_refspec) {
        return false;
    }
    // A forced push of every branch includes the protected branches.
    if all_branches || refspecs.len() < 2 {
        return true;
    }
    refspecs[1..].iter().any(|refspec| {
        let spec = refspec.strip_prefix('+').unwrap_or(*refspec);
        let destination = spec.rsplit(':').next().unwrap_or(spec);
        let branch = destination.strip_prefix("refs/heads/").unwrap_or(destination);
        matches!(branch, "main" | "master" | "HEAD" | "")
    })
}

/// The working-tree pathspecs that select every file: discarding them discards all uncommitted changes.
const ALL_PATHSPECS: &[&str] = &[".", "./", "*", ":/", ":/*"];

fn git_discard_uncommitted(words: &[Word]) -> bool {
    let Some((index, subcommand)) = git_subcommand(words) else {
        return false;
    };
    let arguments = &words[index + 1..];
    let positional_all = arguments
        .iter()
        .any(|word| !word.text.starts_with('-') && ALL_PATHSPECS.contains(&word.text.as_str()));
    match subcommand {
        "reset" => arguments.iter().any(|word| word.text == "--hard"),
        "checkout" => positional_all,
        "restore" => {
            positional_all
                && !arguments
                    .iter()
                    .any(|word| matches!(word.text.as_str(), "--staged" | "-S"))
        }
        "clean" => {
            let forced = arguments.iter().any(|word| {
                word.text == "--force"
                    || (word.text.starts_with('-')
                        && !word.text.starts_with("--")
                        && word.text[1..].contains('f'))
            });
            let dry_run = arguments.iter().any(|word| {
                word.text == "--dry-run"
                    || (word.text.starts_with('-')
                        && !word.text.starts_with("--")
                        && word.text[1..].contains('n'))
            });
            forced && !dry_run
        }
        _ => false,
    }
}

fn disk_format(words: &[Word]) -> bool {
    let Some(first) = words.first() else {
        return false;
    };
    let name = base_name(&first.text);
    if name.starts_with("mkfs") || name == "wipefs" {
        return true;
    }
    name == "diskutil"
        && words.get(1).is_some_and(|word| {
            matches!(
                word.text.as_str(),
                "eraseDisk" | "eraseVolume" | "partitionDisk" | "secureErase" | "zeroDisk" | "randomDisk"
            )
        })
}

/// The block device name prefixes under `/dev/` (macOS, Linux and virtual disks).
const BLOCK_DEVICE_PREFIXES: &[&str] = &[
    "disk", "rdisk", "sd", "hd", "nvme", "xvd", "vd", "mmcblk", "loop",
];

fn is_block_device(path: &str) -> bool {
    path.strip_prefix("/dev/")
        .is_some_and(|name| BLOCK_DEVICE_PREFIXES.iter().any(|prefix| name.starts_with(prefix)))
}

/// Whether a redirection of the simple command (`>`, `>>`) writes to a block device.
fn redirects_to_block_device(words: &[Word]) -> bool {
    words.iter().enumerate().any(|(index, word)| {
        if let Some((_, target)) = word.text.rsplit_once('>') {
            if target.is_empty() {
                return words
                    .get(index + 1)
                    .is_some_and(|next| is_block_device(&next.text));
            }
            return is_block_device(target);
        }
        index > 0
            && words[index - 1].text.ends_with('>')
            && is_block_device(&word.text)
    })
}

/// A `dd` output that is a block device or an expansion the guard cannot resolve, and a redirection to a block device.
fn disk_write_device(words: &[Word]) -> bool {
    let dd_output = is_command(words, &["dd"])
        && words.iter().skip(1).any(|word| {
            word.text
                .strip_prefix("of=")
                .is_some_and(|target| word.expands || is_block_device(target))
        });
    dd_output || redirects_to_block_device(words)
}

/// The lines of a surface as the input changes them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineInput<'a> {
    /// Text inserted at the cursor: typed characters, a paste, or an input method insertion.
    Text(&'a str),
    /// A key that deletes the character before the cursor.
    Backspace,
    /// Ctrl-C or Ctrl-U, which the shell uses to discard the line.
    Discard,
    /// A key that submits the line.
    Enter,
    /// Any other key. It may move the cursor, recall a line or complete one, so the guard cannot follow the line after it.
    Other,
}

/// The command line that the shell of a session holds, as the guard follows it.
#[derive(Debug)]
pub struct LineGuard {
    session: Option<String>,
    line: String,
    tracked: bool,
}

impl Default for LineGuard {
    fn default() -> Self {
        Self {
            session: None,
            line: String::new(),
            tracked: true,
        }
    }
}

impl LineGuard {
    /// Applies the inputs of one transaction of `session` to the line and returns the verdict of each line that the
    /// transaction submits, in order. The list ends at the first refusal. A transaction with a refusal writes nothing, so
    /// the line keeps its state from before the transaction, and the allowed lines before the refusal are not written either.
    pub fn observe(&mut self, session: &str, inputs: &[LineInput<'_>]) -> Vec<Verdict> {
        if self.session.as_deref() != Some(session) {
            self.session = Some(session.to_string());
            self.line.clear();
            self.tracked = true;
        }
        let mut line = self.line.clone();
        let mut tracked = self.tracked;
        let mut verdicts = Vec::new();
        for input in inputs {
            match *input {
                LineInput::Text(text) => {
                    for c in text.chars() {
                        if c == '\n' || c == '\r' {
                            if !submit(&mut line, &mut tracked, &mut verdicts) {
                                return verdicts;
                            }
                        } else if tracked {
                            line.push(c);
                        }
                    }
                }
                LineInput::Enter => {
                    if !submit(&mut line, &mut tracked, &mut verdicts) {
                        return verdicts;
                    }
                }
                LineInput::Backspace => {
                    if tracked {
                        line.pop();
                    }
                }
                LineInput::Discard => {
                    line.clear();
                    tracked = true;
                }
                LineInput::Other => tracked = false,
            }
        }
        self.line = line;
        self.tracked = tracked;
        verdicts
    }
}

/// Judges the line that a submission sends. Returns whether the line was allowed; an allowed line is cleared.
fn submit(line: &mut String, tracked: &mut bool, verdicts: &mut Vec<Verdict>) -> bool {
    let verdict = if *tracked {
        judge(line)
    } else {
        refuse(
            UNTRACKED,
            "the command line was edited in a way the guard does not follow; discard it with Ctrl-C",
        )
    };
    let allowed = verdict == Verdict::Allow;
    verdicts.push(verdict);
    if allowed {
        line.clear();
        *tracked = true;
    }
    allowed
}

/// Judges the inputs of one transaction of `session`, writes one event per verdict to `log` (the allow event at `info`, the
/// refuse event at `error`), and returns the rule id and the reason of the refusal, when the transaction has one.
/// The writer counts an event that the policy or a full queue does not store, so the refusal is in force either way.
pub fn judge_transaction(
    log: &Events,
    line: &mut LineGuard,
    session: &str,
    cids: &[String],
    inputs: &[LineInput<'_>],
) -> Option<(&'static str, String)> {
    let mut refusal = None;
    for verdict in line.observe(session, inputs) {
        match verdict {
            Verdict::Allow => {
                log.emit_for(
                    Level::Info,
                    ALLOW_EVENT,
                    Subject::session(session, cids),
                    &json!({"decision": "allow", "rule": null, "reason": null}),
                );
            }
            Verdict::Refuse { rule, reason } => {
                log.emit_for(
                    Level::Error,
                    REFUSE_EVENT,
                    Subject::session(session, cids),
                    &json!({"decision": "refuse", "rule": rule, "reason": reason}),
                );
                refusal = Some((rule, reason));
            }
        }
    }
    refusal
}
