/**
 * Hooks Module
 * Exports all hook-related functionality
 */

export { HookHandler, main as processHook } from './handler.js';
export { installHooks, uninstallHooks, checkHookStatus, printHookStatus } from './installer.js';
export type {
  HookEventType,
  HookEvent,
  AnyHookEvent,
  HookHandlerConfig,
  HookResult,
  StopHookEvent,
  SubagentStopHookEvent,
  PreToolUseHookEvent,
  PostToolUseHookEvent,
  NotificationHookEvent,
  UserPromptSubmitHookEvent
} from './types.js';
