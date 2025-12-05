/**
 * Input Injector
 * Injects user input from Telegram into Claude Code CLI
 */

import { execSync } from 'child_process';
import { existsSync } from 'fs';
import { EventEmitter } from 'events';
import logger from '../utils/logger.js';

/**
 * Injection method types
 */
type InjectionMethod = 'tmux' | 'pty' | 'fifo' | 'none';

/**
 * Input Injector Configuration
 */
interface InjectorConfig {
  method: InjectionMethod;
  tmuxSession?: string;
  fifoPath?: string;
}

/**
 * Input Injector Class
 * Handles sending user input from Telegram to Claude Code
 */
export class InputInjector extends EventEmitter {
  private config: InjectorConfig;
  private method: InjectionMethod = 'none';
  private tmuxSession: string | null = null;

  constructor(config: Partial<InjectorConfig> = {}) {
    super();
    this.config = {
      method: config.method || 'tmux',
      tmuxSession: config.tmuxSession,
      fifoPath: config.fifoPath
    };
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

    // Check for PTY
    if (process.stdout.isTTY && process.stdin.isTTY) {
      return 'pty';
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

      case 'pty':
        return this.injectViaPty(text);

      case 'fifo':
        return this.injectViaFifo(text);

      default:
        logger.warn('No injection method configured');
        return false;
    }
  }

  /**
   * Inject via tmux send-keys
   */
  private injectViaTmux(text: string): boolean {
    if (!this.tmuxSession) {
      logger.warn('No tmux session');
      return false;
    }

    try {
      // Escape special characters for tmux
      const escapedText = this.escapeTmuxText(text);

      // Use -l (literal) flag to send text as-is, then send Enter separately
      // This is more reliable than quoting with Enter appended
      execSync(`tmux send-keys -t "${this.tmuxSession}" -l "${escapedText}"`, {
        stdio: 'ignore'
      });

      // Send Enter key separately to submit
      execSync(`tmux send-keys -t "${this.tmuxSession}" Enter`, {
        stdio: 'ignore'
      });

      logger.debug('Injected via tmux', { session: this.tmuxSession });
      return true;
    } catch (error) {
      logger.error('Failed to inject via tmux', { error });
      return false;
    }
  }

  /**
   * Inject via PTY (direct stdin)
   */
  private injectViaPty(text: string): boolean {
    try {
      process.stdin.push(text + '\n');
      return true;
    } catch (error) {
      logger.error('Failed to inject via PTY', { error });
      return false;
    }
  }

  /**
   * Inject via FIFO pipe
   */
  private injectViaFifo(text: string): boolean {
    if (!this.config.fifoPath || !existsSync(this.config.fifoPath)) {
      logger.warn('FIFO path not available');
      return false;
    }

    try {
      execSync(`echo "${text}" > "${this.config.fifoPath}"`, {
        stdio: 'ignore'
      });
      return true;
    } catch (error) {
      logger.error('Failed to inject via FIFO', { error });
      return false;
    }
  }

  /**
   * Check if tmux is available
   */
  private isTmuxAvailable(): boolean {
    try {
      execSync('which tmux', { stdio: 'ignore' });
      return true;
    } catch {
      return false;
    }
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

    try {
      // Get current session name
      const session = execSync('tmux display-message -p "#S"', {
        encoding: 'utf8'
      }).trim();
      return session;
    } catch {
      return null;
    }
  }

  /**
   * Find a tmux session running Claude Code
   */
  private findClaudeCodeSession(): string | null {
    try {
      // List all tmux sessions and panes
      const output = execSync(
        'tmux list-panes -a -F "#{session_name}:#{pane_current_command}" 2>/dev/null',
        { encoding: 'utf8' }
      );

      const lines = output.trim().split('\n');

      for (const line of lines) {
        const [session, command] = line.split(':');
        // Look for node/claude processes
        if (command && (command.includes('claude') || command.includes('node'))) {
          return session;
        }
      }

      // Fallback: look for any session with "claude" in the name
      const sessions = execSync('tmux list-sessions -F "#{session_name}" 2>/dev/null', {
        encoding: 'utf8'
      }).trim().split('\n');

      for (const session of sessions) {
        if (session.toLowerCase().includes('claude') || session.toLowerCase().includes('code')) {
          return session;
        }
      }

      return null;
    } catch {
      return null;
    }
  }

  /**
   * Escape text for tmux send-keys with -l flag
   * With -l (literal), tmux handles most characters, we only need to escape double quotes
   * since we wrap the text in double quotes for the shell command
   */
  private escapeTmuxText(text: string): string {
    // Only escape double quotes and backslashes for the shell
    // Single quotes, $, ` are all fine with -l flag
    return text
      .replace(/\\/g, '\\\\')
      .replace(/"/g, '\\"');
  }

  /**
   * Send special key
   */
  async sendKey(key: 'Enter' | 'Escape' | 'Tab' | 'Ctrl-C'): Promise<boolean> {
    if (this.method !== 'tmux' || !this.tmuxSession) {
      return false;
    }

    try {
      const keyMap: Record<string, string> = {
        'Enter': 'Enter',
        'Escape': 'Escape',
        'Tab': 'Tab',
        'Ctrl-C': 'C-c'
      };

      execSync(`tmux send-keys -t "${this.tmuxSession}" ${keyMap[key]}`, {
        stdio: 'ignore'
      });
      return true;
    } catch (error) {
      logger.error('Failed to send key', { key, error });
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
   * Set tmux session explicitly
   */
  setTmuxSession(session: string): void {
    this.tmuxSession = session;
    if (session) {
      this.method = 'tmux';
    }
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
