/**
 * Bridge Types
 */

export type MessageType =
  | 'agent_response'
  | 'tool_start'
  | 'tool_result'
  | 'approval_request'
  | 'user_input'
  | 'approval_response'
  | 'command'
  | 'error'
  | 'session_start'
  | 'session_end'
  | 'turn_complete';  // Claude fires Stop after every turn, not session end

export interface BridgeMessage {
  type: MessageType;
  sessionId: string;
  timestamp: string;
  content: string;
  metadata?: Record<string, unknown>;
}

export interface Session {
  id: string;
  chatId: number;
  threadId?: number;
  hostname?: string;
  projectDir?: string;
  tmuxTarget?: string;  // Persisted tmux session target (e.g., "1:0.0")
  tmuxSocket?: string;  // Persisted tmux socket path (e.g., "/tmp/tmux-1000/default")
  startedAt: Date;
  lastActivity: Date;
  status: 'active' | 'ended' | 'aborted';
  metadata?: Record<string, unknown>;
}

export interface PendingApproval {
  id: string;
  sessionId: string;
  prompt: string;
  createdAt: Date;
  expiresAt: Date;
  status: 'pending' | 'approved' | 'rejected' | 'expired';
}

export interface SocketClientInfo {
  id: string;
  connectedAt: Date;
  sessionId?: string;
}
