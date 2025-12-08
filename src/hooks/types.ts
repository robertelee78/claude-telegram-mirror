/**
 * Claude Code Hook Types
 * Type definitions for hook events
 */

/**
 * Hook event types that Claude Code sends
 */
export type HookEventType =
  | 'Stop'
  | 'SubagentStop'
  | 'PreToolUse'
  | 'PostToolUse'
  | 'Notification'
  | 'UserPromptSubmit';

/**
 * Base hook event structure
 * Note: Claude Code sends 'hook_event_name' not 'type' in the JSON payload
 */
export interface HookEvent {
  hook_event_name: HookEventType;
  session_id: string;
  timestamp?: string;
  hook_id?: string;
  transcript_path?: string;
  cwd?: string;
  permission_mode?: string;
}

/**
 * Stop hook - Called when the main agent stops
 */
export interface StopHookEvent extends HookEvent {
  hook_event_name: 'Stop';
  stop_hook_active: boolean;
  transcript_summary?: string;
}

/**
 * SubagentStop hook - Called when a subagent (Task tool) stops
 */
export interface SubagentStopHookEvent extends HookEvent {
  hook_event_name: 'SubagentStop';
  subagent_id: string;
  result?: string;
}

/**
 * PreToolUse hook - Called before a tool is executed
 */
export interface PreToolUseHookEvent extends HookEvent {
  hook_event_name: 'PreToolUse';
  tool_name: string;
  tool_input: Record<string, unknown>;
  tool_use_id?: string;  // Unique identifier for this tool call
}

/**
 * PostToolUse hook - Called after a tool is executed
 */
export interface PostToolUseHookEvent extends HookEvent {
  hook_event_name: 'PostToolUse';
  tool_name: string;
  tool_input: Record<string, unknown>;
  tool_output?: string;
  tool_error?: string;
}

/**
 * Notification hook - Called for status notifications
 */
export interface NotificationHookEvent extends HookEvent {
  hook_event_name: 'Notification';
  message: string;
  level?: 'info' | 'warning' | 'error';
  notification_type?: string;
}

/**
 * UserPromptSubmit hook - Called when user submits a prompt
 */
export interface UserPromptSubmitHookEvent extends HookEvent {
  hook_event_name: 'UserPromptSubmit';
  prompt: string;
}

/**
 * Union type for all hook events
 */
export type AnyHookEvent =
  | StopHookEvent
  | SubagentStopHookEvent
  | PreToolUseHookEvent
  | PostToolUseHookEvent
  | NotificationHookEvent
  | UserPromptSubmitHookEvent;

/**
 * Hook handler configuration
 */
export interface HookHandlerConfig {
  socketPath: string;
  sessionId?: string;
  verbose?: boolean;
}

/**
 * Hook result for PreToolUse (can block/modify)
 */
export interface HookResult {
  decision?: 'approve' | 'reject' | 'block';
  reason?: string;
  modified_input?: Record<string, unknown>;
}

export default AnyHookEvent;
