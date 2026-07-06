use std::collections::HashMap;
use std::process::Command;

/// A single process entry from the system process table.
#[derive(Debug, Clone)]
struct Proc {
    comm: String,
    args: String,
}

/// A snapshot of the system process table, used to detect Claude Code
/// instances by walking a tmux pane's process subtree.
///
/// Relying on tmux's `pane_current_command` alone is unreliable: a
/// node-based Claude install reports `node` (not `claude`), and a pane
/// whose Claude is currently running a tool reports the child command
/// (`bash`, `git`, ...) as the foreground process. In both cases the
/// Claude process still exists somewhere in the pane's subtree.
#[derive(Debug, Default)]
pub struct ProcTable {
    procs: HashMap<i32, Proc>,
    children: HashMap<i32, Vec<i32>>,
}

impl ProcTable {
    /// Capture the current process table via `ps`.
    ///
    /// Returns an empty table if `ps` is unavailable or fails; callers
    /// should fall back to `pane_current_command` in that case.
    pub fn snapshot() -> Self {
        let output = Command::new("ps")
            .args(["-eo", "pid=,ppid=,comm=,args="])
            .output();

        let stdout = match output {
            Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).into_owned(),
            _ => return Self::default(),
        };

        Self::parse(&stdout)
    }

    /// Parse `ps -eo pid=,ppid=,comm=,args=` output into a process table.
    fn parse(output: &str) -> Self {
        let mut procs = HashMap::new();
        let mut children: HashMap<i32, Vec<i32>> = HashMap::new();

        for line in output.lines() {
            // pid ppid comm args...
            let mut it = line.split_whitespace();
            let (Some(pid), Some(ppid), Some(comm)) = (it.next(), it.next(), it.next()) else {
                continue;
            };
            let (Ok(pid), Ok(ppid)) = (pid.parse::<i32>(), ppid.parse::<i32>()) else {
                continue;
            };
            let args = it.collect::<Vec<_>>().join(" ");

            children.entry(ppid).or_default().push(pid);
            procs.insert(
                pid,
                Proc {
                    comm: comm.to_string(),
                    args,
                },
            );
        }

        Self { procs, children }
    }

    /// Returns true if the process subtree rooted at `pane_pid` contains a
    /// Claude Code process (including `pane_pid` itself).
    ///
    /// Returns false when the table is empty (e.g. `ps` failed), so callers
    /// can distinguish "no claude" from "could not check" via `is_empty`.
    pub fn subtree_has_claude(&self, pane_pid: i32) -> bool {
        // Breadth-first walk of descendants. Depths are tiny (shell ->
        // claude -> maybe a tool), so this is cheap.
        let mut stack = vec![pane_pid];
        let mut seen = std::collections::HashSet::new();

        while let Some(pid) = stack.pop() {
            if !seen.insert(pid) {
                continue; // guard against cycles in a malformed table
            }
            if let Some(proc) = self.procs.get(&pid) {
                if is_claude_proc(&proc.comm, &proc.args) {
                    return true;
                }
            }
            if let Some(kids) = self.children.get(&pid) {
                stack.extend(kids.iter().copied());
            }
        }

        false
    }

    /// Whether the table holds no entries (snapshot failed or `ps` missing).
    pub fn is_empty(&self) -> bool {
        self.procs.is_empty()
    }
}

/// Classify a single process as Claude Code or not, from its `comm`
/// (process name) and full `args` (command line).
///
/// Matches two install shapes:
///   - Native binary: the process name is exactly `claude`.
///   - Node/Bun/Deno launcher: the command line points at the
///     `@anthropic-ai/claude-code` package (e.g. `node .../cli.js`).
///
/// The launcher case matches on the package path in `args` regardless of
/// `comm`, because a JS runtime often renames its main thread (observed as
/// `MainThread`, not `node`), so `comm` is not a reliable signal.
///
/// Deliberately does NOT match on a loose `contains("claude")`, which would
/// catch this very tool (`claude-tmux`), editors with a Claude file open, or
/// shells sitting in a `.claude` directory. The `claude-code` package path
/// is specific enough to avoid those false positives.
pub fn is_claude_proc(comm: &str, args: &str) -> bool {
    if comm == "claude" {
        return true;
    }

    args.contains("anthropic-ai/claude-code") || args.contains("claude-code/cli.js")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_binary_matches() {
        assert!(is_claude_proc("claude", "claude"));
        assert!(is_claude_proc("claude", "/home/u/.local/bin/claude"));
    }

    #[test]
    fn node_launcher_matches() {
        assert!(is_claude_proc(
            "node",
            "node /usr/lib/node_modules/@anthropic-ai/claude-code/cli.js"
        ));
        assert!(is_claude_proc(
            "node",
            "node /home/u/.claude/local/node_modules/@anthropic-ai/claude-code/cli.js"
        ));
        assert!(is_claude_proc("bun", "bun /opt/claude-code/cli.js"));
        // A JS runtime that renamed its main thread still matches on argv.
        assert!(is_claude_proc(
            "MainThread",
            "node /usr/lib/node_modules/@anthropic-ai/claude-code/cli.js"
        ));
    }

    #[test]
    fn non_claude_does_not_match() {
        // This tool itself.
        assert!(!is_claude_proc("claude-tmux", "claude-tmux"));
        // An unrelated node process.
        assert!(!is_claude_proc(
            "node",
            "node feeder/pixel-agents-feeder.cjs --server wss://host/feed"
        ));
        // A shell, an editor with a claude file open.
        assert!(!is_claude_proc("zsh", "-zsh"));
        assert!(!is_claude_proc("nvim", "nvim ~/.claude/CLAUDE.md"));
        // A node process merely sitting in a .claude dir (no package marker).
        assert!(!is_claude_proc("node", "node build.js"));
    }

    #[test]
    fn subtree_detects_claude_under_shell() {
        // shell (100) -> node claude (200) -> bash tool (300)
        let table = ProcTable::parse(
            "100 1 zsh -zsh\n\
             200 100 node node /usr/lib/node_modules/@anthropic-ai/claude-code/cli.js\n\
             300 200 bash bash -c ls\n",
        );
        assert!(table.subtree_has_claude(100));
        assert!(table.subtree_has_claude(200));
        // A pane rooted at an unrelated pid sees no claude.
        assert!(!table.subtree_has_claude(999));
    }

    #[test]
    fn subtree_finds_claude_when_foreground_is_child() {
        // Native claude (200) running a git subprocess (300) in the foreground.
        let table = ProcTable::parse(
            "100 1 zsh -zsh\n\
             200 100 claude claude\n\
             300 200 git git status\n",
        );
        // Even though the pane's foreground command would be `git`, the
        // subtree from the shell pid still finds claude.
        assert!(table.subtree_has_claude(100));
    }

    #[test]
    fn empty_table_reports_empty() {
        let table = ProcTable::default();
        assert!(table.is_empty());
        assert!(!table.subtree_has_claude(100));
    }

    #[test]
    fn handles_cycle_without_hanging() {
        // Malformed table with a parent cycle.
        let table = ProcTable::parse("100 200 a a\n200 100 b b\n");
        assert!(!table.subtree_has_claude(100));
    }
}
