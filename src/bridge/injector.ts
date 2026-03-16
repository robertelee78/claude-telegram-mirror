/**
 * Input Injector
 * Injects user input from Telegram into Claude Code CLI
 */

import { spawnSync } from 'child_process';
import { EventEmitter } from 'events';
import logger from '../utils/logger.js';

/**
 * Injection method types
 */
type InjectionMethod = 'tmux' | 'none';

/**
 * Input Injector Configuration
 */
interface InjectorConfig {
  method: InjectionMethod;
  tmuxSession?: string;
  tmuxSocket?: string;  // Explicit socket path for tmux -S
}

/**
 * Validate a slash command against the character whitelist.
 * Only alphanumerics, underscores, hyphens, spaces, and forward slashes are allowed.
 * Exported for unit testing.
 */
export function isValidSlashCommand(command: string): boolean {
  if (!command || command.length === 0) return false;
  // Whitelist: letters, digits, underscore, hyphen, space, forward slash
  return /^[a-zA-Z0-9_\- \/]+$/.test(command);
}

/**
 * Input Injector Class
 * Handles sending user input from Telegram to Claude Code
 */
export class InputInjector extends EventEmitter {
  private method: InjectionMethod = 'none';
  private tmuxSession: string | null = null;
  private tmuxSocket: string | null = null;  // Socket path for explicit targeting

  constructor(config: Partial<InjectorConfig> = {}) {
    super();
    if (config.tmuxSession) {
      this.tmuxSession = config.tmuxSession;
    }
    if (config.method) {
      this.method = config.method;
    }
  }

  /**
   * Returns the socket args array for tmux commands.
   * Uses -S <path> when a socket path is configured, otherwise empty array.
   */
  private get socketArgs(): string[] {
    return this.tmuxSocket ? ['-S', this.tmuxSocket] : [];
  }

  /**
   * Detect available injection method
   */
  async detectMethod(): Promise<InjectionMethod> {
    // Check for tmux
    if (this.isTmuxAvailable()) {
      const session = this.detectTmuxSession();
      if (session) {
        this.tmuxSession = session;
        logger.info('Detected tmux session', { session });
        return 'tmux';
      }
    }

    logger.warn('No injection method available');
    return 'none';
  }

  /**
   * Initialize injector
   */
  async init(): Promise<boolean> {
    this.method = await this.detectMethod();

    if (this.method === 'none') {
      logger.warn('Input injection not available');
      return false;
    }

    logger.info('Input injector initialized', { method: this.method });
    return true;
  }

  /**
   * Inject input text into Claude Code
   */
  async inject(text: string): Promise<boolean> {
    switch (this.method) {
      case 'tmux':
        return this.injectViaTmux(text);

      default:
        logger.warn('No injection method configured');
        return false;
    }
  }

  /**
   * Validate that the tmux target pane exists
   * BUG-001 fix: Check before injection to fail fast with clear error
   * @returns { valid: true } if pane exists, { valid: false, reason: string } if not
   */
  validateTarget(): { valid: boolean; reason?: string } {
    if (!this.tmuxSession) {
      return { valid: false, reason: 'No tmux session configured' };
    }

    const result = spawnSync(
      'tmux',
      [...this.socketArgs, 'list-panes', '-t', this.tmuxSession],
      { stdio: 'pipe', encoding: 'utf8' }
    );

    if (result.status === 0) {
      return { valid: true };
    }

    return {
      valid: false,
      reason: `Pane "${this.tmuxSession}" not found. Claude may have moved to a different pane.`
    };
  }

  /**
   * Inject via tmux send-keys
   */
  private injectViaTmux(text: string): boolean {
    if (!this.tmuxSession) {
      logger.warn('No tmux session');
      return false;
    }

    // BUG-001 fix: Validate target exists before attempting injection
    const validation = this.validateTarget();
    if (!validation.valid) {
      logger.warn('Target validation failed', {
        session: this.tmuxSession,
        socket: this.tmuxSocket,
        reason: validation.reason
      });
      return false;
    }

    try {
      // Use spawnSync with argument arrays — no shell interpretation, no escaping needed.
      // The -l flag tells tmux to treat the string as literal key input.
      const sendResult = spawnSync(
        'tmux',
        [...this.socketArgs, 'send-keys', '-t', this.tmuxSession, '-l', text],
        { stdio: 'pipe', encoding: 'utf8' }
      );

      logger.debug('Running tmux send-keys', {
        session: this.tmuxSession,
        socket: this.tmuxSocket,
        textLength: text.length
      });

      if (sendResult.status !== 0) {
        throw new Error(sendResult.stderr || 'tmux send-keys failed');
      }

      // Send Enter key separately to submit
      const enterResult = spawnSync(
        'tmux',
        [...this.socketArgs, 'send-keys', '-t', this.tmuxSession, 'Enter'],
        { stdio: 'pipe', encoding: 'utf8' }
      );

      if (enterResult.status !== 0) {
        throw new Error(enterResult.stderr || 'tmux send-keys Enter failed');
      }

      logger.debug('Injected via tmux', { session: this.tmuxSession, socket: this.tmuxSocket });
      return true;
    } catch (error: unknown) {
      const execError = error as { stderr?: string; message?: string };
      logger.error('Failed to inject via tmux', {
        error,
        stderr: execError.stderr,
        message: execError.message,
        session: this.tmuxSession,
        socket: this.tmuxSocket,
        textLength: text.length
      });
      return false;
    }
  }

  /**
   * Check if tmux is available
   */
  private isTmuxAvailable(): boolean {
    const result = spawnSync('tmux', ['-V'], { stdio: 'ignore' });
    return result.status === 0;
  }

  /**
   * Detect current tmux session
   */
  private detectTmuxSession(): string | null {
    // Check if we're inside tmux
    if (!process.env.TMUX) {
      // Look for Claude Code sessions
      return this.findClaudeCodeSession();
    }

    // Get current session name
    const result = spawnSync('tmux', ['display-message', '-p', '#S'], {
      encoding: 'utf8'
    });

    if (result.status === 0 && result.stdout) {
      return result.stdout.trim();
    }

    return null;
  }

  /**
   * Find a tmux session running Claude Code
   */
  private findClaudeCodeSession(): string | null {
    try {
      // List all tmux sessions and panes
      const panesResult = spawnSync(
        'tmux',
        ['list-panes', '-a', '-F', '#{session_name}:#{pane_current_command}'],
        { encoding: 'utf8' }
      );

      if (panesResult.status === 0 && panesResult.stdout) {
        const lines = panesResult.stdout.trim().split('\n');

        for (const line of lines) {
          const [session, command] = line.split(':');
          // Look for node/claude processes
          if (command && (command.includes('claude') || command.includes('node'))) {
            return session;
          }
        }
      }

      // Fallback: look for any session with "claude" in the name
      const sessionsResult = spawnSync(
        'tmux',
        ['list-sessions', '-F', '#{session_name}'],
        { encoding: 'utf8' }
      );

      if (sessionsResult.status === 0 && sessionsResult.stdout) {
        const sessions = sessionsResult.stdout.trim().split('\n');
        for (const session of sessions) {
          if (session.toLowerCase().includes('claude') || session.toLowerCase().includes('code')) {
            return session;
          }
        }
      }

      return null;
    } catch {
      return null;
    }
  }

  /**
   * Escape text for tmux send-keys.
   * @deprecated No longer needed with spawnSync argument arrays — tmux receives raw text
   * without shell interpretation. Kept for any external callers.
   */
  escapeTmuxText(text: string): string {
    return text
      .replace(/\\/g, '\\\\')
      .replace(/"/g, '\\"');
  }

  /**
   * Send special key
   * BUG-004 fix: Include socket flag for correct tmux server targeting
   */
  async sendKey(key: 'Enter' | 'Escape' | 'Tab' | 'Ctrl-C' | 'Ctrl-U'): Promise<boolean> {
    if (this.method !== 'tmux' || !this.tmuxSession) {
      return false;
    }

    try {
      const keyMap: Record<string, string> = {
        'Enter': 'Enter',
        'Escape': 'Escape',
        'Tab': 'Tab',
        'Ctrl-C': 'C-c',
        'Ctrl-U': 'C-u'
      };

      // BUG-004 fix: Include socket args to target correct tmux server
      const result = spawnSync(
        'tmux',
        [...this.socketArgs, 'send-keys', '-t', this.tmuxSession, keyMap[key]],
        { stdio: 'ignore' }
      );

      return result.status === 0;
    } catch (error) {
      logger.error('Failed to send key', { key, error });
      return false;
    }
  }

  /**
   * Send slash command (like /clear)
   * Validates against the character whitelist before sending.
   * Sends command text with -l (literal) flag, then Enter key separately.
   */
  async sendSlashCommand(command: string): Promise<boolean> {
    if (this.method !== 'tmux' || !this.tmuxSession) {
      return false;
    }

    // Whitelist validation — reject commands containing shell-special characters
    if (!isValidSlashCommand(command)) {
      logger.warn('Slash command rejected: unsafe characters', { command });
      return false;
    }

    try {
      // Send command text using -l (literal) flag — no shell interpretation
      const sendResult = spawnSync(
        'tmux',
        [...this.socketArgs, 'send-keys', '-t', this.tmuxSession, '-l', command],
        { stdio: 'ignore' }
      );

      if (sendResult.status !== 0) {
        throw new Error('tmux send-keys failed for slash command');
      }

      // Send Enter separately
      const enterResult = spawnSync(
        'tmux',
        [...this.socketArgs, 'send-keys', '-t', this.tmuxSession, 'Enter'],
        { stdio: 'ignore' }
      );

      return enterResult.status === 0;
    } catch (error) {
      logger.error('Failed to send slash command', { command, error });
      return false;
    }
  }

  /**
   * Get current injection method
   */
  getMethod(): InjectionMethod {
    return this.method;
  }

  /**
   * Get tmux session name
   */
  getTmuxSession(): string | null {
    return this.tmuxSession;
  }

  /**
   * Set tmux session explicitly (with optional socket path)
   */
  setTmuxSession(session: string, socket?: string): void {
    this.tmuxSession = session;
    this.tmuxSocket = socket || null;
    if (session) {
      this.method = 'tmux';
    }
  }

  /**
   * Get tmux socket path
   */
  getTmuxSocket(): string | null {
    return this.tmuxSocket;
  }
}

/**
 * Create and initialize an input injector
 */
export async function createInjector(config?: Partial<InjectorConfig>): Promise<InputInjector | null> {
  const injector = new InputInjector(config);
  const success = await injector.init();
  return success ? injector : null;
}

export default InputInjector;
