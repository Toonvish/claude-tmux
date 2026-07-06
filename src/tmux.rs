use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Command;

use anyhow::{Context, Result};

use crate::detection::detect_status;
use crate::git::GitContext;
use crate::session::{ClaudeCodeStatus, Pane, Session};

/// Wrapper for tmux command execution
pub struct Tmux;

impl Tmux {
    /// List all tmux sessions with their metadata
    pub fn list_sessions() -> Result<Vec<Session>> {
        let output = Command::new("tmux")
            .args([
                "list-sessions",
                "-F",
                "#{session_name}\t#{session_created}\t#{session_attached}\t#{session_windows}",
            ])
            .output()
            .context("Failed to execute tmux list-sessions")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            // No sessions is not an error for us
            if stderr.contains("no server running") || stderr.contains("no sessions") {
                return Ok(Vec::new());
            }
            anyhow::bail!("tmux list-sessions failed: {}", stderr);
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let mut sessions = Vec::new();

        // One process-table snapshot shared across every pane. Used to find
        // Claude Code by walking each pane's process subtree, which is far
        // more reliable than tmux's `pane_current_command` (that reports
        // `node` for node-based installs, or a child command like `bash`
        // while Claude is running a tool).
        let proc_table = crate::process::ProcTable::snapshot();

        // All panes across all sessions, grouped by session name, in one
        // call. Do NOT query panes per-session with `list-panes -t <name>`:
        // that target is resolved relative to the invoking client, so a
        // numeric session name (e.g. "1") collides with a window index and
        // silently returns the *current* client's panes instead. `-a` avoids
        // target resolution entirely.
        let panes_by_session = Self::all_panes();

        for line in stdout.lines() {
            let parts: Vec<&str> = line.split('\t').collect();
            if parts.len() >= 4 {
                let name = parts[0].to_string();
                let created = parts[1].parse().unwrap_or(0);
                let attached = parts[2] == "1";
                let window_count = parts[3].parse().unwrap_or(1);

                // Get panes for this session
                let panes = panes_by_session.get(&name).cloned().unwrap_or_default();

                // Find every pane running claude. Prefer the process-subtree
                // scan; fall back to `pane_current_command` only when the
                // snapshot is unavailable (e.g. `ps` missing).
                let claude_panes: Vec<&Pane> = panes
                    .iter()
                    .filter(|p| {
                        if proc_table.is_empty() {
                            crate::process::is_claude_proc(&p.current_command, "")
                        } else {
                            proc_table.subtree_has_claude(p.pid)
                        }
                    })
                    .collect();

                // Emit one Session row per claude pane. Sessions with zero
                // claude panes still produce a single row with no claude info.
                let multi = claude_panes.len() > 1;

                if claude_panes.is_empty() {
                    let working_directory = panes
                        .first()
                        .map(|p| p.current_path.clone())
                        .unwrap_or_default();
                    let git_context = GitContext::detect(&working_directory);

                    sessions.push(Session {
                        name: name.clone(),
                        created,
                        attached,
                        working_directory,
                        window_count,
                        panes: panes.clone(),
                        claude_code_pane: None,
                        claude_code_status: ClaudeCodeStatus::Unknown,
                        window_label: None,
                        target_window_index: None,
                        git_context,
                    });
                } else {
                    for claude_pane in claude_panes {
                        let status = Self::capture_pane(&claude_pane.id, 15, true)
                            .map(|content| detect_status(&content))
                            .unwrap_or(ClaudeCodeStatus::Unknown);

                        let working_directory = claude_pane.current_path.clone();
                        let git_context = GitContext::detect(&working_directory);

                        let (window_label, target_window_index) = if multi {
                            (
                                Some(claude_pane.window_name.clone()),
                                Some(claude_pane.window_index.clone()),
                            )
                        } else {
                            (None, None)
                        };

                        sessions.push(Session {
                            name: name.clone(),
                            created,
                            attached,
                            working_directory,
                            window_count,
                            panes: panes.clone(),
                            claude_code_pane: Some(claude_pane.id.clone()),
                            claude_code_status: status,
                            window_label,
                            target_window_index,
                            git_context,
                        });
                    }
                }
            }
        }

        // Sort by attached status, then name, then window label so the rows
        // for a multi-claude session stay grouped in a stable order.
        sessions.sort_by(|a, b| {
            b.attached
                .cmp(&a.attached)
                .then_with(|| a.name.cmp(&b.name))
                .then_with(|| a.window_label.cmp(&b.window_label))
        });

        Ok(sessions)
    }

    /// List every pane across all sessions and windows, grouped by session
    /// name.
    ///
    /// Uses `list-panes -a` (all panes) rather than a per-session
    /// `list-panes -t <name>`: the latter resolves its target relative to
    /// the invoking client, so a numeric session name collides with a window
    /// index and returns the current client's panes instead of the named
    /// session's. Grouping the single `-a` listing by `#{session_name}`
    /// sidesteps target resolution entirely.
    fn all_panes() -> HashMap<String, Vec<Pane>> {
        let output = Command::new("tmux")
            .args([
                "list-panes",
                "-a",
                "-F",
                "#{session_name}\t#{pane_id}\t#{pane_current_command}\t#{pane_current_path}\t#{window_index}\t#{window_name}\t#{pane_pid}",
            ])
            .output();

        let stdout = match output {
            Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).into_owned(),
            _ => return HashMap::new(),
        };

        let mut panes_by_session: HashMap<String, Vec<Pane>> = HashMap::new();

        for line in stdout.lines() {
            let parts: Vec<&str> = line.split('\t').collect();
            if parts.len() >= 7 {
                panes_by_session
                    .entry(parts[0].to_string())
                    .or_default()
                    .push(Pane {
                        id: parts[1].to_string(),
                        current_command: parts[2].to_string(),
                        current_path: PathBuf::from(parts[3]),
                        window_index: parts[4].to_string(),
                        window_name: parts[5].to_string(),
                        pid: parts[6].parse().unwrap_or(0),
                    });
            }
        }

        panes_by_session
    }

    /// Capture the last N lines of a pane's content
    ///
    /// If `strip_empty` is true, empty lines are filtered out before taking the last N.
    /// This is useful for status detection. For preview display, use `strip_empty: false`
    /// to preserve the visual layout.
    ///
    /// ANSI escape sequences are always included - the UI handles rendering them.
    pub fn capture_pane(pane_id: &str, lines: usize, strip_empty: bool) -> Result<String> {
        let output = Command::new("tmux")
            .args([
                "capture-pane",
                "-t",
                pane_id,
                "-p", // Print to stdout
                "-J", // Join wrapped lines
                "-e", // Include escape sequences
            ])
            .output()
            .context("Failed to capture pane")?;

        if !output.status.success() {
            anyhow::bail!("Failed to capture pane {}", pane_id);
        }

        let content = String::from_utf8_lossy(&output.stdout);

        if strip_empty {
            // Filter out empty lines, then get last N (for status detection)
            let non_empty: Vec<&str> = content.lines().filter(|l| !l.trim().is_empty()).collect();
            let start = non_empty.len().saturating_sub(lines);
            let last_lines = &non_empty[start..];
            Ok(last_lines.join("\n"))
        } else {
            // Preserve internal empty lines but trim trailing ones (for preview display)
            let all_lines: Vec<&str> = content.lines().collect();

            // Find last non-empty line
            let last_non_empty = all_lines
                .iter()
                .rposition(|l| !l.trim().is_empty())
                .map(|i| i + 1)
                .unwrap_or(0);

            let trimmed = &all_lines[..last_non_empty];
            let start = trimmed.len().saturating_sub(lines);
            let last_lines = &trimmed[start..];
            Ok(last_lines.join("\n"))
        }
    }

    /// Switch the current client to the specified session
    pub fn switch_to_session(session: &str) -> Result<()> {
        let status = Command::new("tmux")
            .args(["switch-client", "-t", session])
            .status()
            .context("Failed to switch session")?;

        if !status.success() {
            anyhow::bail!("Failed to switch to session {}", session);
        }

        Ok(())
    }

    /// Create a new tmux session
    pub fn new_session(name: &str, path: &std::path::Path, start_claude: bool) -> Result<()> {
        let path_str = path.to_string_lossy();

        let status = Command::new("tmux")
            .args(["new-session", "-d", "-s", name, "-c", &path_str])
            .status()
            .context("Failed to create new session")?;

        if !status.success() {
            anyhow::bail!("Failed to create session {}", name);
        }

        if start_claude {
            // Send claude command to the new session
            let _ = Command::new("tmux")
                .args(["send-keys", "-t", name, "claude", "Enter"])
                .status();
        }

        Ok(())
    }

    /// Kill a tmux session
    pub fn kill_session(session: &str) -> Result<()> {
        let status = Command::new("tmux")
            .args(["kill-session", "-t", session])
            .status()
            .context("Failed to kill session")?;

        if !status.success() {
            anyhow::bail!("Failed to kill session {}", session);
        }

        Ok(())
    }

    /// Rename a tmux session
    pub fn rename_session(old_name: &str, new_name: &str) -> Result<()> {
        let status = Command::new("tmux")
            .args(["rename-session", "-t", old_name, new_name])
            .status()
            .context("Failed to rename session")?;

        if !status.success() {
            anyhow::bail!("Failed to rename session {} to {}", old_name, new_name);
        }

        Ok(())
    }

    /// Get the name of the currently attached session
    pub fn current_session() -> Result<Option<String>> {
        let output = Command::new("tmux")
            .args(["display-message", "-p", "#{session_name}"])
            .output()
            .context("Failed to get current session")?;

        if !output.status.success() {
            return Ok(None);
        }

        let name = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if name.is_empty() {
            Ok(None)
        } else {
            Ok(Some(name))
        }
    }
}
