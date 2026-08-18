use crate::session::ClaudeCodeStatus;

/// Detect Claude Code status when content has NOT changed since the last check.
///
/// Content-change detection (see `App::tick_status`) reports Working when the
/// pane redraws. This function covers the case where two captures are identical
/// while Claude is still busy: it reports Working whenever the interrupt hint is
/// present, and otherwise distinguishes WaitingInput, Idle, and Unknown from the
/// static content.
pub fn detect_static_status(content: &str) -> ClaudeCodeStatus {
    if has_confirmation_prompt(content) {
        return ClaudeCodeStatus::WaitingInput;
    }
    if is_busy(content) {
        return ClaudeCodeStatus::Working;
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
    if has_confirmation_prompt(content) {
        return ClaudeCodeStatus::WaitingInput;
    }

    if has_input_field(content) {
        if is_busy(content) {
            return ClaudeCodeStatus::Working;
        }
        return ClaudeCodeStatus::Idle;
    }

    if is_busy(content) {
        return ClaudeCodeStatus::Working;
    }

    ClaudeCodeStatus::Unknown
}

/// How many trailing lines can hold a live prompt. Dialogs are pinned to the
/// bottom of the pane, so anything further up is transcript, not a prompt.
const PROMPT_TAIL_LINES: usize = 10;

/// True when the pane is showing a dialog that blocks on a keypress.
///
/// Claude Code renders these as a numbered menu with the selection marker on
/// one option ("❯ 1. Yes") - tool permission prompts, the folder-trust check,
/// plan approval. The marker has to sit on a numbered option near the bottom of
/// the pane and the input box has to be gone, which keeps ordinary transcript
/// text out: matching "[y/n]" anywhere in the capture used to report
/// WaitingInput for a finished session that merely mentioned those characters.
/// The legacy inline form is still recognised when it terminates one of the
/// last lines.
pub fn has_confirmation_prompt(content: &str) -> bool {
    // A live dialog replaces the input box, so whenever the box is on screen the
    // prompt-shaped text above it is transcript rather than a pending question.
    if has_input_field(content) {
        return false;
    }

    let lines: Vec<&str> = content.lines().collect();
    let tail_start = lines.len().saturating_sub(PROMPT_TAIL_LINES);

    let menu = lines.iter().enumerate().skip(tail_start).any(|(i, line)| {
        // The input box also starts with ❯, but it always has a border above it.
        is_numbered_choice(line) && !(i > 0 && lines[i - 1].contains('─'))
    });

    menu || lines
        .iter()
        .rev()
        .take(3)
        .any(|line| is_yes_no_prompt(line))
}

/// True for a menu option carrying the selection marker, e.g. "❯ 1. Yes".
fn is_numbered_choice(line: &str) -> bool {
    let clean = strip_ansi(line);
    let Some((_, rest)) = clean.split_once('❯') else {
        return false;
    };
    let mut chars = rest.trim_start().chars();
    matches!((chars.next(), chars.next()), (Some(digit), Some('.')) if digit.is_ascii_digit())
}

/// True for a line that ends in an inline yes/no prompt, e.g. "Overwrite? [y/n]".
fn is_yes_no_prompt(line: &str) -> bool {
    let clean = strip_ansi(line);
    let trimmed = clean.trim_end();
    trimmed.ends_with("[y/n]") || trimmed.ends_with("[Y/n]")
}

/// Drop ANSI escape sequences from a captured line. Captures keep them (see
/// `Tmux::capture_pane`) and they sit between the glyphs the structural checks
/// look at - a real menu line reads "\e[38;5;153m❯\e[39m \e[38;5;246m1. Yes".
fn strip_ansi(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            // Skip up to and including the sequence's final byte.
            for next in chars.by_ref() {
                if next.is_ascii_alphabetic() || next == '~' {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
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

/// Detect input field: prompt line (❯) directly below the input box's top rule.
fn has_input_field(content: &str) -> bool {
    let lines: Vec<&str> = content.lines().collect();

    lines
        .iter()
        .enumerate()
        .skip(1)
        .any(|(i, line)| line.contains('❯') && is_horizontal_rule(lines[i - 1]))
}

/// True for the input box's border: a line of nothing but horizontal rule
/// glyphs. Lines carrying box corners ("╰───────") are deliberately excluded -
/// a submitted prompt is echoed directly below the welcome box, and that echo
/// is transcript, not a live input field.
fn is_horizontal_rule(line: &str) -> bool {
    let trimmed = strip_ansi(line);
    let trimmed = trimmed.trim();
    !trimmed.is_empty() && trimmed.chars().all(|c| c == '─')
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

    /// Verbatim tail of a real permission dialog, escape sequences included.
    const PERMISSION_DIALOG: &str = concat!(
        " Bash command\n",
        "   curl -s https://example.com -o page.html\n",
        "\u{1b}[39m This command requires approval\n",
        " Do you want to proceed?\n",
        " \u{1b}[38;5;153m❯\u{1b}[39m \u{1b}[38;5;246m1. \u{1b}[38;5;153mYes\n",
        "\u{1b}[39m   \u{1b}[38;5;246m2. \u{1b}[39mYes, and don't ask again for: curl *\n",
        "   \u{1b}[38;5;246m3. \u{1b}[39mNo\n",
        " \u{1b}[38;5;246mEsc to cancel · Tab to amend · ctrl+e to explain",
    );

    #[test]
    fn test_permission_dialog_waits_for_input() {
        assert_eq!(
            detect_status(PERMISSION_DIALOG),
            ClaudeCodeStatus::WaitingInput
        );
        assert_eq!(
            detect_static_status(PERMISSION_DIALOG),
            ClaudeCodeStatus::WaitingInput
        );
    }

    #[test]
    fn test_trust_dialog_waits_for_input() {
        let content = "Quick safety check: Is this a project you created or one you trust?\n\u{1b}[1m❯ 1. Yes, I trust this folder\n   2. No, exit\nEnter to confirm · Esc to cancel";
        assert_eq!(detect_status(content), ClaudeCodeStatus::WaitingInput);
    }

    #[test]
    fn test_transcript_mentioning_a_prompt_is_not_waiting() {
        // A finished session whose transcript happens to contain prompt-shaped
        // text: the input box below it is what counts.
        let content = "● Kept it so a [y/n] prompt still surfaces on the next tick.\n● Wrote: ❯ 1. Yes\n─────\n❯ \n─────\n  ⏵⏵ auto mode on (shift+tab to cycle)";
        assert_eq!(detect_status(content), ClaudeCodeStatus::Idle);
        assert_eq!(detect_static_status(content), ClaudeCodeStatus::Idle);
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
