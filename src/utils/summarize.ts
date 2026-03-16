/**
 * Tool Summarizer
 * Maps tool names and inputs to human-readable one-liners.
 */

/**
 * Show last 2 path components with .../ prefix.
 * E.g. `/opt/project/src/utils/config.ts` -> `.../utils/config.ts`
 */
export function shortPath(filePath: string): string {
  const parts = filePath.split('/').filter(Boolean);
  if (parts.length <= 2) return filePath;
  return `.../${parts.slice(-2).join('/')}`;
}

/**
 * Show the hostname (or first 40 chars) of a URL.
 */
function shortUrl(url: string): string {
  try {
    const parsed = new URL(url);
    return parsed.hostname;
  } catch {
    return url.slice(0, 40);
  }
}

/**
 * Truncate a string to a given length, appending "..." if truncated.
 */
function truncate(str: string, maxLen: number): string {
  if (str.length <= maxLen) return str;
  return str.slice(0, maxLen) + '...';
}

// Commands that are trivial prefixes in chained commands
const TRIVIAL_COMMANDS = new Set([
  'cd', 'export', 'echo', 'sleep', 'set', 'source', 'true', 'false', ':'
]);

// Wrapper commands that should be stripped to get the real command
const WRAPPER_COMMANDS = new Set([
  'sudo', 'nohup', 'timeout', 'env', 'nice'
]);

/**
 * For chained commands (&&, ;, ||), skip trivial prefixes
 * and return the first meaningful command.
 * Pipes are NOT split -- they are part of the same command.
 */
export function findMeaningfulCommand(command: string): string {
  // Split on &&, ;, || but not | (single pipe)
  // Use a regex that matches && or || or ; but not a lone |
  const segments = command.split(/\s*(?:&&|;|\|\|)\s*/);

  for (const segment of segments) {
    const trimmed = segment.trim();
    if (!trimmed) continue;
    const firstWord = trimmed.split(/\s+/)[0];
    if (!TRIVIAL_COMMANDS.has(firstWord)) {
      return trimmed;
    }
  }

  // If all segments are trivial, return the last one (or original)
  const lastSegment = segments[segments.length - 1]?.trim();
  return lastSegment || command.trim();
}

/**
 * Strip wrapper commands (sudo, nohup, etc.) and return the inner command.
 */
function stripWrappers(command: string): string {
  const parts = command.trim().split(/\s+/);
  let i = 0;
  while (i < parts.length && WRAPPER_COMMANDS.has(parts[i])) {
    i++;
    // For 'timeout' and 'env', skip one extra argument
    // timeout has a duration arg: `timeout 30 cmd`
    // env may have VAR=val pairs
    if (i < parts.length) {
      const prev = parts[i - 1];
      if (prev === 'timeout' && i < parts.length && /^[\d.]+[smhd]?$/.test(parts[i])) {
        i++;
      }
    }
  }
  if (i >= parts.length) return command.trim();
  return parts.slice(i).join(' ');
}

/**
 * Summarize a Bash command to a human-readable description.
 */
function summarizeBashCommand(rawCommand: string): string {
  // First find the meaningful command in chains
  const meaningful = findMeaningfulCommand(rawCommand);
  // Then strip wrappers
  const command = stripWrappers(meaningful);
  const parts = command.split(/\s+/);
  const first = parts[0];

  // cargo commands
  if (first === 'cargo') {
    const sub = parts[1];
    if (sub === 'build') {
      const isRelease = parts.includes('--release');
      return isRelease ? 'Building project (release)' : 'Building project';
    }
    if (sub === 'test') return 'Running tests';
    if (sub === 'clippy') return 'Running linter';
    if (sub === 'fmt') return 'Formatting code';
    if (sub === 'run') return 'Running project';
    if (sub === 'add') return 'Adding dependency';
    if (sub === 'install') return 'Installing tool';
    if (sub === 'clean') return 'Cleaning build';
    if (sub === 'doc') return 'Generating docs';
    if (sub === 'publish') return 'Publishing crate';
    if (sub === 'bench') return 'Running benchmarks';
    if (sub === 'check') return 'Type checking';
    return `Running \`cargo ${sub || ''}\``;
  }

  // git commands
  if (first === 'git') {
    const sub = parts[1];
    if (sub === 'clone') return 'Cloning repository';
    if (sub === 'commit') return 'Committing changes';
    if (sub === 'push') return 'Pushing to remote';
    if (sub === 'pull') return 'Pulling from remote';
    if (sub === 'checkout' || sub === 'switch') return 'Switching branch';
    if (sub === 'merge') return 'Merging branches';
    if (sub === 'rebase') return 'Rebasing';
    if (sub === 'stash') return 'Stashing changes';
    if (sub === 'diff') return 'Viewing diff';
    if (sub === 'log') return 'Viewing history';
    if (sub === 'status') return 'Checking status';
    if (sub === 'branch') return 'Managing branches';
    if (sub === 'tag') return 'Managing tags';
    if (sub === 'fetch') return 'Fetching remote';
    if (sub === 'reset') return 'Resetting changes';
    return `Running \`git ${sub || ''}\``;
  }

  // npm commands
  if (first === 'npm') {
    const sub = parts[1];
    if (sub === 'install' || sub === 'ci' || sub === 'i') return 'Installing dependencies';
    if (sub === 'run') {
      const script = parts[2];
      if (script === 'build') return 'Building project';
      if (script === 'test') return 'Running tests';
      if (script === 'lint') return 'Running linter';
      return `Running npm script: ${script || 'unknown'}`;
    }
    if (sub === 'test' || sub === 't') return 'Running tests';
    if (sub === 'publish') return 'Publishing package';
    return `Running \`npm ${sub || ''}\``;
  }

  // npx
  if (first === 'npx') {
    const pkg = parts[1] || 'unknown';
    return `Running npx: ${pkg}`;
  }

  // yarn
  if (first === 'yarn') {
    const sub = parts[1];
    if (!sub || sub === 'install') return 'Installing dependencies';
    if (sub === 'build') return 'Building project';
    if (sub === 'test') return 'Running tests';
    if (sub === 'lint') return 'Running linter';
    if (sub === 'add') return 'Installing dependencies';
    if (sub === 'publish') return 'Publishing package';
    return `Running \`yarn ${sub}\``;
  }

  // pnpm
  if (first === 'pnpm') {
    const sub = parts[1];
    if (sub === 'install' || sub === 'i') return 'Installing dependencies';
    if (sub === 'run') {
      const script = parts[2];
      if (script === 'build') return 'Building project';
      if (script === 'test') return 'Running tests';
      if (script === 'lint') return 'Running linter';
      return `Running pnpm script: ${script || 'unknown'}`;
    }
    if (sub === 'test') return 'Running tests';
    if (sub === 'add') return 'Installing dependencies';
    if (sub === 'publish') return 'Publishing package';
    return `Running \`pnpm ${sub || ''}\``;
  }

  // bun
  if (first === 'bun') {
    const sub = parts[1];
    if (sub === 'install' || sub === 'i') return 'Installing dependencies';
    if (sub === 'run') {
      const script = parts[2];
      if (script === 'build') return 'Building project';
      if (script === 'test') return 'Running tests';
      if (script === 'lint') return 'Running linter';
      return `Running bun script: ${script || 'unknown'}`;
    }
    if (sub === 'test') return 'Running tests';
    if (sub === 'add') return 'Installing dependencies';
    if (sub === 'publish') return 'Publishing package';
    return `Running \`bun ${sub || ''}\``;
  }

  // Python
  if (first === 'pip' || first === 'pip3') {
    if (parts[1] === 'install') return 'Installing Python dependencies';
    return `Running \`${first} ${parts[1] || ''}\``;
  }
  if (first === 'pytest' || (first === 'python' && parts[1] === '-m' && parts[2] === 'pytest')) {
    return 'Running Python tests';
  }
  if (first === 'python' || first === 'python3') {
    if (parts[1] === '-m' && parts[2] === 'pytest') return 'Running Python tests';
    return `Running \`${first}\``;
  }

  // Docker
  if (first === 'docker') {
    if (parts[1] === 'build') return 'Building Docker image';
    if (parts[1] === 'run') return 'Running container';
    return `Running \`docker ${parts[1] || ''}\``;
  }
  if (first === 'docker-compose' || first === 'docker compose') {
    if (parts[1] === 'up') return 'Starting containers';
    return `Running \`docker-compose ${parts[1] || ''}\``;
  }

  // make
  if (first === 'make') {
    const target = parts[1];
    return target ? `Running make: ${target}` : 'Building with make';
  }

  // TypeScript / JS tools
  if (first === 'tsc') return 'Type checking';
  if (first === 'vitest') return 'Running tests';
  if (first === 'eslint') return 'Running linter';

  // Network tools
  if (first === 'curl') return 'Fetching URL';
  if (first === 'wget') return 'Downloading file';
  if (first === 'ssh') return 'Connecting via SSH';
  if (first === 'scp') return 'Copying files via SSH';

  // File tools
  if (first === 'tar') return 'Archiving/extracting';
  if (first === 'chmod') return 'Changing permissions';
  if (first === 'chown') return 'Changing ownership';
  if (first === 'mkdir') return 'Creating directory';
  if (first === 'rm') return 'Removing files';
  if (first === 'cp') return 'Copying files';
  if (first === 'mv') return 'Moving files';

  // Search
  if (first === 'grep' || first === 'rg') return 'Searching files';
  if (first === 'find') return 'Finding files';

  // Infrastructure
  if (first === 'kubectl') return 'Managing Kubernetes';
  if (first === 'terraform') return 'Managing infrastructure';

  // Go
  if (first === 'go') {
    const sub = parts[1];
    if (sub === 'build') return 'Building Go project';
    if (sub === 'test') return 'Running Go tests';
    if (sub === 'run') return 'Running Go project';
    return `Running \`go ${sub || ''}\``;
  }

  // Rust
  if (first === 'rustc') return 'Compiling Rust';

  // Fallback
  return `Running \`${first}\``;
}

/**
 * Maps tool name + input to a human-readable one-liner.
 */
export function summarizeToolAction(tool: string, input: Record<string, unknown>): string {
  if (tool === 'Bash') {
    const command = input.command as string | undefined;
    if (command) {
      return summarizeBashCommand(command);
    }
    return 'Running command';
  }

  if (tool === 'Read') {
    const fp = input.file_path as string | undefined;
    return fp ? `Reading ${shortPath(fp)}` : 'Reading file';
  }

  if (tool === 'Write') {
    const fp = input.file_path as string | undefined;
    return fp ? `Writing ${shortPath(fp)}` : 'Writing file';
  }

  if (tool === 'Edit' || tool === 'MultiEdit') {
    const fp = input.file_path as string | undefined;
    return fp ? `Editing ${shortPath(fp)}` : 'Editing file';
  }

  if (tool === 'Grep') {
    const pattern = input.pattern as string | undefined;
    return pattern ? `Searching for '${truncate(pattern, 30)}'` : 'Searching files';
  }

  if (tool === 'Glob') {
    const pattern = input.pattern as string | undefined;
    return pattern ? `Finding files: ${pattern}` : 'Finding files';
  }

  if (tool === 'Task') return 'Running task';

  if (tool === 'WebSearch') {
    const query = input.query as string | undefined;
    return query ? `Searching: ${truncate(query, 40)}` : 'Searching the web';
  }

  if (tool === 'WebFetch') {
    const url = input.url as string | undefined;
    return url ? `Fetching ${shortUrl(url)}` : 'Fetching URL';
  }

  if (tool === 'TodoWrite') return 'Updating tasks';
  if (tool === 'TodoRead') return 'Reading tasks';
  if (tool === 'AskUserQuestion') return 'Asking user a question';
  if (tool === 'NotebookEdit') return 'Editing notebook';

  return `Using ${tool}`;
}

/**
 * Detect error patterns in tool output and return a summary.
 */
export function summarizeToolResult(_tool: string, output: string): string {
  if (!output) return 'Completed (no output)';

  const lines = output.split('\n');

  // Check for error patterns
  for (const line of lines) {
    // Rust compiler error
    if (line.includes('error[E')) {
      return `Failed: ${truncate(line.trim(), 60)}`;
    }
  }

  for (const line of lines) {
    // Generic Error:
    if (/\bError:/i.test(line)) {
      return `Failed: ${truncate(line.trim(), 60)}`;
    }
  }

  // FAILED pattern (typically test output)
  if (output.includes('FAILED')) {
    return 'Tests failed';
  }

  // Panic
  for (const line of lines) {
    if (line.includes('panic!') || line.includes('panicked at')) {
      return `Panicked: ${truncate(line.trim(), 60)}`;
    }
  }

  // npm error
  if (output.includes('npm ERR!')) {
    return 'npm error';
  }

  // Default
  const lineCount = lines.length;
  return `Completed (${lineCount} lines of output)`;
}
