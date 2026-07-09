use std::path::PathBuf;

use crate::git::GitContext;

/// Status of a Claude Code instance in a pane
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ClaudeCodeStatus {
    /// Waiting at prompt, ready for input
    Idle,
    /// Actively processing a request
    Working,
    /// Awaiting user confirmation/input (y/n prompt, etc.)
    WaitingInput,
    /// Cannot determine status
    #[default]
    Unknown,
}

impl ClaudeCodeStatus {
    /// Returns the display symbol for this status
    pub fn symbol(&self) -> &'static str {
        match self {
            ClaudeCodeStatus::Idle => "○",
            ClaudeCodeStatus::Working => "●",
            ClaudeCodeStatus::WaitingInput => "◐",
            ClaudeCodeStatus::Unknown => "?",
        }
    }

    /// Returns the display label for this status
    pub fn label(&self) -> &'static str {
        match self {
            ClaudeCodeStatus::Idle => "idle",
            ClaudeCodeStatus::Working => "working",
            ClaudeCodeStatus::WaitingInput => "input",
            ClaudeCodeStatus::Unknown => "unknown",
        }
    }
}

/// Format a context-token count compactly: "532", "1.5k", "68k", "1.2M".
fn format_token_count(n: u64) -> String {
    if n < 1_000 {
        format!("{}", n)
    } else if n < 10_000 {
        format!("{:.1}k", n as f64 / 1_000.0)
    } else if n < 1_000_000 {
        format!("{}k", n / 1_000)
    } else {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    }
}

/// A tmux pane within a session
#[derive(Debug, Clone)]
pub struct Pane {
    /// Pane ID (e.g., "%0")
    pub id: String,
    /// PID of the pane's root process (the shell), used to walk the pane's
    /// process subtree when detecting Claude Code.
    pub pid: i32,
    /// Current command running in the pane
    pub current_command: String,
    /// Current working directory
    pub current_path: PathBuf,
    /// Window index this pane belongs to (e.g., "0", "1")
    pub window_index: String,
    /// Window name this pane belongs to
    pub window_name: String,
}

/// A tmux session that may contain a Claude Code instance
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct Session {
    /// Session name
    pub name: String,
    /// Unix timestamp when session was created
    pub created: i64,
    /// Whether a client is attached to this session
    pub attached: bool,
    /// Working directory (from the Claude Code pane, or first pane)
    pub working_directory: PathBuf,
    /// Number of windows in this session
    pub window_count: usize,
    /// All panes in this session
    pub panes: Vec<Pane>,
    /// Pane ID containing Claude Code, if any
    pub claude_code_pane: Option<String>,
    /// Status of Claude Code in this session
    pub claude_code_status: ClaudeCodeStatus,
    /// Window label to show next to the session name, used when a session
    /// has multiple claude panes and is shown as multiple rows.
    pub window_label: Option<String>,
    /// Window index to target when switching, if this row represents a
    /// specific claude pane within a multi-pane session.
    pub target_window_index: Option<String>,
    /// Git context, if the working directory is a git repository
    pub git_context: Option<GitContext>,
    /// Token usage read from the Claude Code transcript, if available
    pub token_usage: Option<crate::usage::TokenUsage>,
}

impl Session {
    /// Returns the name to display in the session list. Includes a
    /// `:window` suffix when this row represents a specific claude pane
    /// within a session that has multiple claude instances.
    pub fn display_name(&self) -> String {
        match &self.window_label {
            Some(label) => format!("{}:{}", self.name, label),
            None => self.name.clone(),
        }
    }

    /// Returns the tmux switch target.
    ///
    /// Prefers the claude pane id (e.g. `%42`) when known: tmux resolves a
    /// pane-id target through the full session/window/pane hierarchy, so a
    /// single `switch-client -t %42` lands the client on the exact pane.
    /// Falls back to `name:window_index`, then to the bare session name.
    pub fn switch_target(&self) -> String {
        if let Some(pane_id) = &self.claude_code_pane {
            return pane_id.clone();
        }
        match &self.target_window_index {
            Some(idx) => format!("{}:{}", self.name, idx),
            None => self.name.clone(),
        }
    }

    /// Compact human-readable context-token count for the session row
    /// (e.g. "532", "1.5k", "68k", "1.2M"). Empty string when unknown.
    pub fn token_display(&self) -> String {
        match &self.token_usage {
            Some(usage) => format_token_count(usage.context_tokens),
            None => String::new(),
        }
    }

    /// Returns a shortened version of the working directory for display
    pub fn display_path(&self) -> String {
        let path = &self.working_directory;

        // Try to replace home directory with ~
        if let Some(home) = dirs::home_dir() {
            if let Ok(stripped) = path.strip_prefix(&home) {
                return format!("~/{}", stripped.display());
            }
        }

        path.display().to_string()
    }

    /// Returns a human-readable duration since session creation
    pub fn duration(&self) -> String {
        use std::time::{SystemTime, UNIX_EPOCH};

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);

        let elapsed_secs = (now - self.created).max(0) as u64;

        let days = elapsed_secs / 86400;
        let hours = (elapsed_secs % 86400) / 3600;
        let minutes = (elapsed_secs % 3600) / 60;

        if days > 0 {
            format!("{}d {}h", days, hours)
        } else if hours > 0 {
            format!("{}h {}m", hours, minutes)
        } else {
            format!("{}m", minutes.max(1))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_token_counts() {
        assert_eq!(format_token_count(532), "532");
        assert_eq!(format_token_count(1_500), "1.5k");
        assert_eq!(format_token_count(68_930), "68k");
        assert_eq!(format_token_count(1_200_000), "1.2M");
    }
}
