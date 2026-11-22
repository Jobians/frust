//! Plain data types shared by the chat components. No framework dependency.

/// Who authored a transcript message.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ChatRole {
    /// The human.
    User,
    /// The assistant's reply text.
    Assistant,
    /// The assistant's reasoning, shown dimmer than a reply.
    Thought,
}

/// What a tool call does, for its icon and label.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ToolKind {
    /// Reads a file or resource.
    Read,
    /// Edits a file.
    Edit,
    /// Deletes a file or resource.
    Delete,
    /// Moves or renames.
    Move,
    /// Searches.
    Search,
    /// Runs a command.
    Execute,
    /// Reasoning step.
    Think,
    /// Fetches from the network.
    Fetch,
    /// Anything else.
    Other,
}

/// Progress of a tool call.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ToolStatus {
    /// Not started.
    Pending,
    /// Running.
    InProgress,
    /// Finished successfully.
    Completed,
    /// Finished with an error.
    Failed,
}

impl ToolStatus {
    /// Whether the call has reached a final state.
    pub fn is_finished(self) -> bool {
        matches!(self, Self::Completed | Self::Failed)
    }
}

/// Progress of one plan entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum PlanEntryStatus {
    /// Not started.
    Pending,
    /// Being worked on.
    InProgress,
    /// Done.
    Completed,
}

/// One line of a plan.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlanEntry {
    /// The entry's text.
    pub text: String,
    /// Its progress.
    pub status: PlanEntryStatus,
}

/// What choosing an approval option does.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ApprovalOptionKind {
    /// Allow this once.
    AllowOnce,
    /// Allow now and for later requests.
    AllowAlways,
    /// Reject this once.
    RejectOnce,
    /// Reject now and for later requests.
    RejectAlways,
}

impl ApprovalOptionKind {
    /// Whether the option grants the request.
    pub fn is_allow(self) -> bool {
        matches!(self, Self::AllowOnce | Self::AllowAlways)
    }
}

/// One choice offered by an approval prompt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApprovalOption {
    /// Stable id handed back to the app when chosen.
    pub id: String,
    /// Button label.
    pub label: String,
    /// What choosing it means.
    pub kind: ApprovalOptionKind,
}

/// Where an approval prompt stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ApprovalState {
    /// Waiting for the user.
    Pending,
    /// The user allowed it.
    Allowed,
    /// The user denied it.
    Denied,
    /// The request timed out or was withdrawn.
    Expired,
    /// Another client answered it.
    AnsweredElsewhere,
}

impl ApprovalState {
    /// Whether the prompt still waits for an answer.
    pub fn is_pending(self) -> bool {
        self == Self::Pending
    }
}

/// The input to a diff view: a file's before and after text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffInput {
    /// Path shown in the header.
    pub path: String,
    /// Text before the change; `None` for a new file.
    pub old_text: Option<String>,
    /// Text after the change.
    pub new_text: String,
}

impl DiffInput {
    /// Whether the change creates the file.
    pub fn is_new_file(&self) -> bool {
        self.old_text.is_none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allow_kinds() {
        assert!(ApprovalOptionKind::AllowOnce.is_allow());
        assert!(ApprovalOptionKind::AllowAlways.is_allow());
        assert!(!ApprovalOptionKind::RejectOnce.is_allow());
        assert!(!ApprovalOptionKind::RejectAlways.is_allow());
    }

    #[test]
    fn tool_status_finished() {
        assert!(!ToolStatus::Pending.is_finished());
        assert!(!ToolStatus::InProgress.is_finished());
        assert!(ToolStatus::Completed.is_finished());
        assert!(ToolStatus::Failed.is_finished());
    }

    #[test]
    fn approval_pending_only_when_pending() {
        assert!(ApprovalState::Pending.is_pending());
        assert!(!ApprovalState::AnsweredElsewhere.is_pending());
    }

    #[test]
    fn diff_input_new_file() {
        let mut d = DiffInput {
            path: "a.rs".into(),
            old_text: None,
            new_text: "x".into(),
        };
        assert!(d.is_new_file());
        d.old_text = Some(String::new());
        assert!(!d.is_new_file());
    }
}
