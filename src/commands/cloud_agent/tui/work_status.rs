//! Where a thread's work stands, as the platform computes it (OSC 7501
//! program-status terms). The TUI only reads it: which bucket a thread sits in
//! and which glyph it carries. It never derives a state of its own.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum WorkState {
    Idle,
    Working,
    Done,
    Blocked,
    Error,
}

impl WorkState {
    /// Unknown values read as idle: a state added on the platform before
    /// this build knows it should not shout.
    pub fn from_label(label: &str) -> Self {
        match label {
            "WORKING" => Self::Working,
            "DONE" => Self::Done,
            "BLOCKED" => Self::Blocked,
            "ERROR" => Self::Error,
            _ => Self::Idle,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkStatus {
    pub state: WorkState,
    /// What a BLOCKED thread waits on: PERMISSION, QUESTION or AUTH.
    pub kind: Option<String>,
    /// One line: the question, the failure, or the closing prose. Untrusted
    /// agent text, already cut to one line by the platform.
    pub message: Option<String>,
    /// DONE or ERROR, and nobody has looked since.
    pub unseen: bool,
    /// RFC 3339; orders threads of the same rank.
    pub at: Option<String>,
}

/// Where a thread ranks in the sidebar's one threads list, most urgent first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Section {
    /// Blocked on a person, or failed and not yet looked at.
    NeedsYou,
    Working,
    /// Finished and not yet looked at.
    Done,
    /// Everything else: threads with nothing to report, newest first.
    Threads,
}

impl Section {
    pub fn label(self) -> &'static str {
        match self {
            Section::NeedsYou => "needs you",
            Section::Working => "working",
            Section::Done => "done",
            Section::Threads => "threads",
        }
    }

    /// `None` for a thread nothing reported on: a plain shell, or a session
    /// from before work status.
    pub fn of(status: Option<&WorkStatus>) -> Section {
        match status {
            Some(s) if s.state == WorkState::Blocked => Section::NeedsYou,
            Some(s) if s.state == WorkState::Error && s.unseen => Section::NeedsYou,
            Some(s) if s.state == WorkState::Working => Section::Working,
            Some(s) if s.state == WorkState::Done && s.unseen => Section::Done,
            _ => Section::Threads,
        }
    }
}

impl WorkStatus {
    /// The detail pane's one-line headline: `blocked · question`,
    /// `error · unseen`.
    pub fn headline(&self) -> String {
        let state = match self.state {
            WorkState::Idle => "idle",
            WorkState::Working => "working",
            WorkState::Done => "done",
            WorkState::Blocked => "blocked",
            WorkState::Error => "error",
        };
        match (&self.kind, self.unseen) {
            (Some(kind), _) if self.state == WorkState::Blocked => {
                format!("{state} · {}", kind.to_lowercase())
            }
            (_, true) => format!("{state} · unseen"),
            _ => state.to_string(),
        }
    }
}

/// Builds a [`WorkStatus`] from any query's generated type: each query module
/// generates its own copy of the same fields and enums.
macro_rules! work_status_from {
    ($ws:expr) => {{
        let ws = $ws;
        $crate::commands::cloud_agent::tui::work_status::WorkStatus {
            state: $crate::commands::cloud_agent::tui::work_status::WorkState::from_label(
                &format!("{:?}", ws.state),
            ),
            kind: ws.kind.map(|kind| format!("{kind:?}")),
            message: ws.message,
            unseen: ws.unseen,
            at: ws.at.map(|at| at.to_rfc3339()),
        }
    }};
}
pub(crate) use work_status_from;

#[cfg(test)]
mod tests {
    use super::*;

    fn status(state: WorkState, unseen: bool) -> WorkStatus {
        WorkStatus {
            state,
            kind: None,
            message: None,
            unseen,
            at: None,
        }
    }

    #[test]
    fn sections_follow_the_platform_state() {
        let of = |state, unseen| Section::of(Some(&status(state, unseen)));
        assert_eq!(of(WorkState::Blocked, false), Section::NeedsYou);
        assert_eq!(of(WorkState::Error, true), Section::NeedsYou);
        assert_eq!(of(WorkState::Error, false), Section::Threads);
        assert_eq!(of(WorkState::Working, false), Section::Working);
        assert_eq!(of(WorkState::Done, true), Section::Done);
        assert_eq!(of(WorkState::Done, false), Section::Threads);
        assert_eq!(of(WorkState::Idle, false), Section::Threads);
        assert_eq!(Section::of(None), Section::Threads);
    }

    #[test]
    fn unknown_states_read_idle() {
        assert_eq!(WorkState::from_label("Other(\"PAUSED\")"), WorkState::Idle);
        assert_eq!(WorkState::from_label("BLOCKED"), WorkState::Blocked);
    }
}
