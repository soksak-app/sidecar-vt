//! The process list of a terminal session: the foreground program and the background jobs (core
//! docs/spec/process-list.md). This module holds the forms of the list and the rule that names the
//! entries from the members of the terminal; the platform reads the members and the processes.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// The process of an entry (core docs/spec/process-list.md#process). A read that the system
/// refused leaves `command`, `arguments`, `cwd` and `started` empty and names the read in `error`.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Process {
    pub group: i32,
    pub pid: i32,
    pub command: Option<String>,
    pub arguments: Option<Vec<String>>,
    pub cwd: Option<String>,
    /// The start time in milliseconds since the Unix epoch.
    pub started: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// The state of a background job (core docs/spec/process-list.md#background-jobs).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum JobState {
    Running,
    Stopped,
}

/// A background job: the process of its entry and its state.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Job {
    #[serde(flatten)]
    pub process: Process,
    pub state: JobState,
}

/// The process list of a session. `watched` holds the members of the background jobs that have
/// not ended, which the sidecar watches for their exits and signals; it is not part of the list.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Processes {
    pub foreground: Option<Process>,
    pub background: Vec<Job>,
    #[serde(skip)]
    pub watched: Vec<i32>,
}

/// The state of a member of the terminal (core docs/spec/process-list.md#members).
#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum MemberState {
    Running,
    Stopped,
    /// A zombie or a process that is handling its exit.
    Ended,
}

/// A process whose controlling terminal is the terminal of the session, with its group and state.
#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
pub struct Member {
    pub pid: i32,
    pub group: i32,
    pub state: MemberState,
}

/// An entry that the rule names: the group, the pid of the process of the entry and, for a job,
/// its state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Entry {
    pub group: i32,
    pub pid: i32,
    pub state: JobState,
}

/// The entries of the list (core docs/spec/process-list.md): the foreground entry of the group
/// `foreground`, or None when that group has no member that has not ended, and one background entry
/// for each other group of the members, except the group of the session leader `leader`, in
/// ascending order of the group. A session leader is the leader of its group, so the id of that
/// group is the pid of the leader. The process of an entry is the leader of the group when it has
/// not ended, otherwise the member with the lowest pid that has not ended.
pub fn entries(leader: i32, foreground: i32, members: &[Member]) -> (Option<Entry>, Vec<Entry>) {
    let mut groups: BTreeMap<i32, Vec<&Member>> = BTreeMap::new();
    for member in members
        .iter()
        .filter(|member| member.state != MemberState::Ended)
    {
        groups.entry(member.group).or_default().push(member);
    }
    let entry = |group: i32, live: &[&Member]| {
        let pid = live
            .iter()
            .find(|member| member.pid == group)
            .or_else(|| live.iter().min_by_key(|member| member.pid))
            .map(|member| member.pid)?;
        let state = if live
            .iter()
            .all(|member| member.state == MemberState::Stopped)
        {
            JobState::Stopped
        } else {
            JobState::Running
        };
        Some(Entry { group, pid, state })
    };
    let foreground_entry = groups
        .get(&foreground)
        .and_then(|live| entry(foreground, live));
    let background = groups
        .iter()
        .filter(|(group, _)| **group != foreground && **group != leader)
        .filter_map(|(group, live)| entry(*group, live))
        .collect();
    (foreground_entry, background)
}

/// The members of the background jobs that have not ended, which the sidecar watches.
pub fn job_members(leader: i32, foreground: i32, members: &[Member]) -> Vec<i32> {
    members
        .iter()
        .filter(|member| {
            member.state != MemberState::Ended
                && member.group != foreground
                && member.group != leader
        })
        .map(|member| member.pid)
        .collect()
}
