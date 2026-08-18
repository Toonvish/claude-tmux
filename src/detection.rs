use crate::session::ClaudeCodeStatus;

/// Detect Claude Code status when content has NOT changed since the last check.
///
/// Content-change detection (see `App::tick_status`) reports Working when the
/// pane redraws. This function covers the case where two captures are identical
/// while Claude is still busy: it reports Working whenever the interrupt hint is
/// present, and otherwise distinguishes WaitingInput, Idle, and Unknown from the
/// static content.
pub fn detect_static_status(content: &str) -> ClaudeCodeStatus {
    if is_busy(content) {
        return ClaudeCodeStatus::Working;
    }
    if content.contains("[y/n]") || content.contains("[Y/n]") {
        return ClaudeCodeStatus::WaitingInput;
    }
    if has_input_field(content) {
        return ClaudeCodeStatus::Idle;
    }
    ClaudeCodeStatus::Unknown
}

/// Detect Claude Code status from pane content.
///
/// Used as a fallback when no previous capture is available for comparison.
/// Prefer content-change detection (see `App::tick_status`) for reliable
/// Working vs Idle discrimination.
pub fn detect_status(content: &str) -> ClaudeCodeStatus {
    if has_input_field(content) {
        if is_busy(content) {
            return ClaudeCodeStatus::Working;
        }
        return ClaudeCodeStatus::Idle;
    }

    if is_busy(content) {
        return ClaudeCodeStatus::Working;
    }

    if content.contains("[y/n]") || content.contains("[Y/n]") {
        return ClaudeCodeStatus::WaitingInput;
    }

    ClaudeCodeStatus::Unknown
}

/// True when the pane shows any sign that Claude is busy: either its own
/// interrupt hint, or background subagents it is waiting on.
fn is_busy(content: &str) -> bool {
    is_working(content) || has_background_agents(content)
}

/// True when Claude Code is waiting on background subagents ("Waiting for 2
/// background agents to finish"). In this state the prompt still accepts input,
/// so the interrupt hint is absent even though the session is busy.
fn has_background_agents(content: &str) -> bool {
    content.contains("background agent")
}

/// True when the pane shows Claude Code's interrupt hint, i.e. Claude is
/// actively working on a task. Covers both "esc to interrupt" and
/// "ctrl+c to interrupt" wording. The hint is pinned to the bottom of the pane
/// while working and disappears when the task finishes or input is required.
fn is_working(content: &str) -> bool {
    content.contains("to interrupt")
}

/// Detect input field: prompt line (❯) with border directly above it.
fn has_input_field(content: &str) -> bool {
    let lines: Vec<&str> = content.lines().collect();

    for (i, line) in lines.iter().enumerate() {
        if line.contains('❯') {
            // Check if line above is a border
            if i > 0 && lines[i - 1].contains('─') {
                return true;
            }
        }
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_working() {
        // Border directly above prompt
        let content = "* (ctrl+c to interrupt)\n─────\n❯ hello";
        assert_eq!(detect_status(content), ClaudeCodeStatus::Working);
    }

    #[test]
    fn test_idle() {
        // Border directly above prompt
        let content = "● Done\n─────\n❯ hello";
        assert_eq!(detect_status(content), ClaudeCodeStatus::Idle);
    }

    #[test]
    fn test_no_border_above_prompt() {
        // Border exists but not directly above prompt - should be unknown
        let content = "─────\nsome text\n❯ hello";
        assert_eq!(detect_status(content), ClaudeCodeStatus::Unknown);
    }

    #[test]
    fn test_waiting_input() {
        let content = "Delete files? [y/n]";
        assert_eq!(detect_status(content), ClaudeCodeStatus::WaitingInput);
    }

    #[test]
    fn test_unknown() {
        let content = "random stuff";
        assert_eq!(detect_status(content), ClaudeCodeStatus::Unknown);
    }

    #[test]
    fn test_static_working_with_input_box() {
        // Interrupt hint present while the input box is on screen: Working wins
        // over Idle even though content is unchanged between ticks.
        let content = "✻ Thinking… (esc to interrupt)\n─────\n❯ ";
        assert_eq!(detect_static_status(content), ClaudeCodeStatus::Working);
    }

    #[test]
    fn test_static_idle() {
        // Input box, no interrupt hint → Idle.
        let content = "● Done\n─────\n❯ hello";
        assert_eq!(detect_static_status(content), ClaudeCodeStatus::Idle);
    }

    #[test]
    fn test_background_agents_are_working() {
        // Main loop is at the prompt while subagents run: no interrupt hint,
        // but the session is busy.
        let content = "✻ Waiting for 2 background agents to finish\n─────\n❯ \n─────\n  ● main\n  ◯ Explore  Reading foo.rs";
        assert_eq!(detect_status(content), ClaudeCodeStatus::Working);
        assert_eq!(detect_static_status(content), ClaudeCodeStatus::Working);
    }

    #[test]
    fn test_static_waiting_input() {
        let content = "Delete files? [y/n]";
        assert_eq!(detect_static_status(content), ClaudeCodeStatus::WaitingInput);
    }
}
