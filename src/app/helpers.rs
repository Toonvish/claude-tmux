//! Helper utilities for the app module
//!
//! Pure functions for path manipulation, name sanitization and status smoothing.

use std::path::PathBuf;
use std::time::Duration;

use crate::session::ClaudeCodeStatus;

/// How long a pane keeps reporting Working after its last redraw.
pub const WORKING_HOLD: Duration = Duration::from_secs(2);

/// Smooth out the status of panes that redraw intermittently.
///
/// While background subagents run, Claude Code sits at the prompt (no interrupt
/// hint) and refreshes their progress roughly once a second, so consecutive
/// 500 ms captures alternate between changed and unchanged and the raw status
/// flips between Working and Idle. Keep reporting Working until the pane has
/// been quiet for `WORKING_HOLD`. Working and WaitingInput pass through
/// untouched so confirmation prompts surface immediately.
pub fn hold_working(
    status: ClaudeCodeStatus,
    since_last_change: Option<Duration>,
) -> ClaudeCodeStatus {
    match status {
        ClaudeCodeStatus::Idle | ClaudeCodeStatus::Unknown
            if since_last_change.is_some_and(|elapsed| elapsed < WORKING_HOLD) =>
        {
            ClaudeCodeStatus::Working
        }
        other => other,
    }
}

/// Expand ~ to home directory in a path string
pub fn expand_path(path: &str) -> PathBuf {
    if let Some(stripped) = path.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(stripped);
        }
    } else if path == "~" {
        if let Some(home) = dirs::home_dir() {
            return home;
        }
    }
    PathBuf::from(path)
}

/// Sanitize a branch name for use as a session name
/// e.g., "feature/new-thing" -> "new-thing"
pub fn sanitize_for_session_name(branch: &str) -> String {
    branch
        .rsplit('/')
        .next()
        .unwrap_or(branch)
        .replace(['/', '\\', ' ', ':', '.'], "-")
}

/// Generate default worktree path from repo path and branch name
/// e.g., ~/repos/project + feature/foo -> ~/repos/project-foo
pub fn default_worktree_path(repo_path: &std::path::Path, branch: &str) -> PathBuf {
    let parent = repo_path.parent().unwrap_or(repo_path);
    let repo_name = repo_path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("repo");
    let branch_suffix = sanitize_for_session_name(branch);
    parent.join(format!("{}-{}", repo_name, branch_suffix))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hold_keeps_working_between_redraws() {
        let recent = Some(Duration::from_millis(500));
        assert_eq!(
            hold_working(ClaudeCodeStatus::Idle, recent),
            ClaudeCodeStatus::Working
        );
        assert_eq!(
            hold_working(ClaudeCodeStatus::Unknown, recent),
            ClaudeCodeStatus::Working
        );
    }

    #[test]
    fn hold_expires_when_pane_goes_quiet() {
        let stale = Some(WORKING_HOLD + Duration::from_millis(1));
        assert_eq!(
            hold_working(ClaudeCodeStatus::Idle, stale),
            ClaudeCodeStatus::Idle
        );
        assert_eq!(
            hold_working(ClaudeCodeStatus::Idle, None),
            ClaudeCodeStatus::Idle
        );
    }

    #[test]
    fn hold_never_masks_a_prompt() {
        assert_eq!(
            hold_working(
                ClaudeCodeStatus::WaitingInput,
                Some(Duration::from_millis(100))
            ),
            ClaudeCodeStatus::WaitingInput
        );
    }
}
