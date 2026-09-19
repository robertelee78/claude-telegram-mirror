//! ADR-016: host observers for non-Claude agent CLIs.
//!
//! A **host observer** is a long-lived client of ctm's own Unix socket that speaks
//! `BridgeMessage` — exactly what `ctm hook` is for Claude Code, minus the
//! one-process-per-event shape. It translates a host's native event stream into the
//! daemon's host-neutral protocol and translates the daemon's replies (`HostInject`,
//! `QuestionResponse`, `ApprovalResponse`) back into host API calls.
//!
//! Why this shape (ADR-016 Decision): the daemon's approval routing, priority
//! queueing, topic buffering and the entire Telegram question UI are reused
//! unchanged. The daemon dispatches on host at exactly three points
//! (`daemon/host_dispatch.rs`); everything else stays host-blind.
//!
//! Observers run as tokio tasks inside the daemon process (no extra service units)
//! and connect to the daemon's socket like any external client, so `socket.rs`'s
//! `_client_id` injection and per-client write routing apply to them unmodified.
//!
//! Per-host implementations live in `opencode.rs` and `codex.rs`; they share
//! [`link::ObserverLink`] for the socket side and [`HostCaps`] for honest capability
//! reporting.

pub mod codex;
pub mod codex_daemon;
pub mod codex_hook_cmd;
pub mod codex_hooks;
pub mod codex_rpc;
pub mod detect;
pub mod link;
pub mod opencode;
pub mod opencode_pipe;
pub mod opencode_plugin;

use crate::types::HostKind;

/// How a host surfaces structured multiple-choice questions.
///
/// The Telegram layer asks this instead of assuming — the structural guard against a
/// future PR-E (ADR-014/015): a UI that *knows* a host cannot do something never ships
/// a button that silently does nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StructuredQuestions {
    /// Claude Code: questions render natively in the Ink TUI and are answered by
    /// scraping/driving that widget over tmux (`daemon/callback_handlers.rs`).
    ViaTuiScrape,
    /// Typed JSON on the host's event bus, answerable by API in every mode.
    Native,
    /// Typed JSON, but the host only emits questions in a specific mode — Codex's
    /// `item/tool/requestUserInput` exists only under `collaborationMode: plan`
    /// (verified: `default_mode_request_user_input` is an under-development flag).
    NativePlanModeOnly,
}

/// Runtime capabilities a host reports. All values were established by executed
/// spikes (ADR-016 §Reformulated hypothesis), not read from documentation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HostCaps {
    pub structured_questions: StructuredQuestions,
    /// Can a message be delivered into a turn that is already running (steer), as
    /// opposed to queued until idle? tmux cannot; both native hosts can.
    pub steer: bool,
    /// Does the approval reply vocabulary include a durable "always allow"?
    /// OpenCode: `always`; Codex: `acceptWithExecpolicyAmendment`. Claude: no.
    pub always_decision: bool,
    /// Can an image be injected as user input? Codex `UserInput::image` only.
    pub inject_images: bool,
    /// Does the host narrate a *remote* decision in its own TUI? Both native hosts
    /// dismiss the prompt but print nothing attributing the answer to Telegram
    /// (verified), so ctm compensates (OpenCode: `/tui/show-toast`).
    pub narrates_remote_answers: bool,
}

impl HostCaps {
    pub const fn for_host(kind: HostKind) -> Self {
        match kind {
            HostKind::ClaudeCode => Self {
                structured_questions: StructuredQuestions::ViaTuiScrape,
                steer: false,
                always_decision: false,
                inject_images: false,
                narrates_remote_answers: true, // keystrokes ARE the local answer
            },
            HostKind::OpenCode => Self {
                structured_questions: StructuredQuestions::Native,
                steer: true,
                always_decision: true,
                inject_images: false,
                narrates_remote_answers: false,
            },
            HostKind::Codex => Self {
                structured_questions: StructuredQuestions::NativePlanModeOnly,
                steer: true,
                always_decision: true,
                inject_images: true,
                narrates_remote_answers: false,
            },
        }
    }
}

/// Map a host's native tool name onto ctm's internal (Claude-shaped) vocabulary so
/// `summarize.rs` and `formatting.rs` never learn about hosts (ADR-016 Decision).
///
/// Unknown names pass through unchanged — the summarizer already has a generic
/// fallback, and MCP tools arrive prefixed (`server_tool`, `mcp__server__tool`) as an
/// open set that must not be dropped on the floor.
///
/// Verified vocabularies: OpenCode `GET /experimental/tool/ids` (lowercase); Codex
/// `ThreadItem` variants (camelCase) and hook-path names (`apply_patch`, `exec_command`).
pub fn normalize_tool_name(kind: HostKind, native: &str) -> String {
    let mapped = match kind {
        HostKind::ClaudeCode => native,
        HostKind::OpenCode => match native {
            "bash" => "Bash",
            "edit" => "Edit",
            "write" => "Write",
            "read" => "Read",
            "grep" => "Grep",
            "glob" => "Glob",
            "task" => "Task",
            "todowrite" => "TodoWrite",
            "webfetch" => "WebFetch",
            "websearch" => "WebSearch",
            "apply_patch" => "Edit",
            "question" => "AskUserQuestion",
            "skill" => "Skill",
            other => other,
        },
        HostKind::Codex => match native {
            // app-server `ThreadItem` variants
            "commandExecution" => "Bash",
            "fileChange" => "Edit",
            "mcpToolCall" => "MCP",
            "dynamicToolCall" => "Tool",
            "collabAgentToolCall" | "subAgentActivity" => "Task",
            "plan" => "TodoWrite",
            "webSearch" => "WebSearch",
            "imageView" => "Read",
            // hook-path names
            "apply_patch" => "Edit",
            "exec_command" | "shell" => "Bash",
            other => other,
        },
    };
    mapped.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caps_are_host_specific_and_honest() {
        let c = HostCaps::for_host(HostKind::ClaudeCode);
        assert_eq!(c.structured_questions, StructuredQuestions::ViaTuiScrape);
        assert!(!c.steer && !c.always_decision && !c.inject_images);

        let o = HostCaps::for_host(HostKind::OpenCode);
        assert_eq!(o.structured_questions, StructuredQuestions::Native);
        assert!(o.steer && o.always_decision && !o.inject_images);
        assert!(!o.narrates_remote_answers, "verified attribution gap");

        let x = HostCaps::for_host(HostKind::Codex);
        assert_eq!(
            x.structured_questions,
            StructuredQuestions::NativePlanModeOnly,
            "requestUserInput only exists in plan mode (spike-verified)"
        );
        assert!(x.inject_images, "UserInput::image variant exists on Codex");
    }

    #[test]
    fn tool_names_normalize_to_claude_vocabulary() {
        assert_eq!(normalize_tool_name(HostKind::OpenCode, "bash"), "Bash");
        assert_eq!(
            normalize_tool_name(HostKind::OpenCode, "question"),
            "AskUserQuestion"
        );
        assert_eq!(
            normalize_tool_name(HostKind::Codex, "commandExecution"),
            "Bash"
        );
        assert_eq!(normalize_tool_name(HostKind::Codex, "apply_patch"), "Edit");
        assert_eq!(normalize_tool_name(HostKind::Codex, "exec_command"), "Bash");
    }

    #[test]
    fn unknown_and_mcp_tool_names_pass_through() {
        assert_eq!(
            normalize_tool_name(HostKind::OpenCode, "ruvnet-brain_search"),
            "ruvnet-brain_search"
        );
        assert_eq!(
            normalize_tool_name(HostKind::Codex, "mcp__srv__tool"),
            "mcp__srv__tool"
        );
        assert_eq!(normalize_tool_name(HostKind::ClaudeCode, "Bash"), "Bash");
    }
}
