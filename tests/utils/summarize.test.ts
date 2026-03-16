/**
 * Tool Summarizer Tests
 */

import { describe, it, expect } from 'vitest';
import {
  summarizeToolAction,
  summarizeToolResult,
  findMeaningfulCommand,
  shortPath
} from '../../src/utils/summarize.js';

describe('shortPath', () => {
  it('should return last 2 components with .../ prefix for long paths', () => {
    expect(shortPath('/opt/project/src/utils/config.ts')).toBe('.../utils/config.ts');
  });

  it('should return full path for short paths', () => {
    expect(shortPath('/src/file.ts')).toBe('/src/file.ts');
  });

  it('should handle root-level file', () => {
    expect(shortPath('/file.ts')).toBe('/file.ts');
  });

  it('should handle 3-component paths', () => {
    expect(shortPath('/opt/project/file.ts')).toBe('.../project/file.ts');
  });
});

describe('findMeaningfulCommand', () => {
  it('should skip cd prefix in chained commands', () => {
    expect(findMeaningfulCommand('cd /tmp && cargo test')).toBe('cargo test');
  });

  it('should skip export prefix', () => {
    expect(findMeaningfulCommand('export FOO=bar && npm run build')).toBe('npm run build');
  });

  it('should skip multiple trivial prefixes', () => {
    expect(findMeaningfulCommand('cd /tmp && export PATH=/usr && cargo build')).toBe('cargo build');
  });

  it('should return the command as-is if no trivial prefix', () => {
    expect(findMeaningfulCommand('cargo test --release')).toBe('cargo test --release');
  });

  it('should split on semicolons', () => {
    expect(findMeaningfulCommand('echo hi; cargo build')).toBe('cargo build');
  });

  it('should split on ||', () => {
    expect(findMeaningfulCommand('true || cargo test')).toBe('cargo test');
  });

  it('should NOT split on single pipe', () => {
    expect(findMeaningfulCommand('cargo test | grep FAIL')).toBe('cargo test | grep FAIL');
  });

  it('should handle all-trivial commands by returning the last one', () => {
    expect(findMeaningfulCommand('cd /tmp && echo done')).toBe('echo done');
  });
});

describe('summarizeToolAction - Bash cargo commands', () => {
  it('should summarize cargo build', () => {
    expect(summarizeToolAction('Bash', { command: 'cargo build' })).toBe('Building project');
  });

  it('should summarize cargo build --release', () => {
    expect(summarizeToolAction('Bash', { command: 'cargo build --release' })).toBe('Building project (release)');
  });

  it('should summarize cargo test', () => {
    expect(summarizeToolAction('Bash', { command: 'cargo test' })).toBe('Running tests');
  });

  it('should summarize cargo clippy', () => {
    expect(summarizeToolAction('Bash', { command: 'cargo clippy' })).toBe('Running linter');
  });

  it('should summarize cargo fmt', () => {
    expect(summarizeToolAction('Bash', { command: 'cargo fmt' })).toBe('Formatting code');
  });

  it('should summarize cargo run', () => {
    expect(summarizeToolAction('Bash', { command: 'cargo run' })).toBe('Running project');
  });

  it('should summarize cargo doc', () => {
    expect(summarizeToolAction('Bash', { command: 'cargo doc' })).toBe('Generating docs');
  });

  it('should summarize cargo bench', () => {
    expect(summarizeToolAction('Bash', { command: 'cargo bench' })).toBe('Running benchmarks');
  });

  it('should summarize cargo check', () => {
    expect(summarizeToolAction('Bash', { command: 'cargo check' })).toBe('Type checking');
  });

  it('should summarize cargo publish', () => {
    expect(summarizeToolAction('Bash', { command: 'cargo publish' })).toBe('Publishing crate');
  });
});

describe('summarizeToolAction - Bash git commands', () => {
  it('should summarize git commit', () => {
    expect(summarizeToolAction('Bash', { command: 'git commit -m "fix"' })).toBe('Committing changes');
  });

  it('should summarize git push', () => {
    expect(summarizeToolAction('Bash', { command: 'git push origin main' })).toBe('Pushing to remote');
  });

  it('should summarize git pull', () => {
    expect(summarizeToolAction('Bash', { command: 'git pull' })).toBe('Pulling from remote');
  });

  it('should summarize git checkout', () => {
    expect(summarizeToolAction('Bash', { command: 'git checkout feature-branch' })).toBe('Switching branch');
  });

  it('should summarize git switch', () => {
    expect(summarizeToolAction('Bash', { command: 'git switch main' })).toBe('Switching branch');
  });

  it('should summarize git status', () => {
    expect(summarizeToolAction('Bash', { command: 'git status' })).toBe('Checking status');
  });

  it('should summarize git diff', () => {
    expect(summarizeToolAction('Bash', { command: 'git diff HEAD~1' })).toBe('Viewing diff');
  });

  it('should summarize git log', () => {
    expect(summarizeToolAction('Bash', { command: 'git log --oneline' })).toBe('Viewing history');
  });

  it('should summarize git clone', () => {
    expect(summarizeToolAction('Bash', { command: 'git clone https://github.com/foo/bar' })).toBe('Cloning repository');
  });

  it('should summarize git stash', () => {
    expect(summarizeToolAction('Bash', { command: 'git stash' })).toBe('Stashing changes');
  });

  it('should summarize git fetch', () => {
    expect(summarizeToolAction('Bash', { command: 'git fetch origin' })).toBe('Fetching remote');
  });
});

describe('summarizeToolAction - Bash npm/yarn/pnpm/bun commands', () => {
  it('should summarize npm install', () => {
    expect(summarizeToolAction('Bash', { command: 'npm install' })).toBe('Installing dependencies');
  });

  it('should summarize npm ci', () => {
    expect(summarizeToolAction('Bash', { command: 'npm ci' })).toBe('Installing dependencies');
  });

  it('should summarize npm run build', () => {
    expect(summarizeToolAction('Bash', { command: 'npm run build' })).toBe('Building project');
  });

  it('should summarize npm test', () => {
    expect(summarizeToolAction('Bash', { command: 'npm test' })).toBe('Running tests');
  });

  it('should summarize npm run test', () => {
    expect(summarizeToolAction('Bash', { command: 'npm run test' })).toBe('Running tests');
  });

  it('should summarize npm run lint', () => {
    expect(summarizeToolAction('Bash', { command: 'npm run lint' })).toBe('Running linter');
  });

  it('should summarize npm publish', () => {
    expect(summarizeToolAction('Bash', { command: 'npm publish' })).toBe('Publishing package');
  });

  it('should summarize npx with package name', () => {
    expect(summarizeToolAction('Bash', { command: 'npx vitest --run' })).toBe('Running npx: vitest');
  });

  it('should summarize yarn install', () => {
    expect(summarizeToolAction('Bash', { command: 'yarn install' })).toBe('Installing dependencies');
  });

  it('should summarize yarn (bare)', () => {
    expect(summarizeToolAction('Bash', { command: 'yarn' })).toBe('Installing dependencies');
  });

  it('should summarize pnpm install', () => {
    expect(summarizeToolAction('Bash', { command: 'pnpm install' })).toBe('Installing dependencies');
  });

  it('should summarize bun test', () => {
    expect(summarizeToolAction('Bash', { command: 'bun test' })).toBe('Running tests');
  });
});

describe('summarizeToolAction - Bash other commands', () => {
  it('should summarize pip install', () => {
    expect(summarizeToolAction('Bash', { command: 'pip install requests' })).toBe('Installing Python dependencies');
  });

  it('should summarize pytest', () => {
    expect(summarizeToolAction('Bash', { command: 'pytest tests/' })).toBe('Running Python tests');
  });

  it('should summarize python -m pytest', () => {
    expect(summarizeToolAction('Bash', { command: 'python -m pytest' })).toBe('Running Python tests');
  });

  it('should summarize docker build', () => {
    expect(summarizeToolAction('Bash', { command: 'docker build -t app .' })).toBe('Building Docker image');
  });

  it('should summarize docker run', () => {
    expect(summarizeToolAction('Bash', { command: 'docker run -d app' })).toBe('Running container');
  });

  it('should summarize make with target', () => {
    expect(summarizeToolAction('Bash', { command: 'make build' })).toBe('Running make: build');
  });

  it('should summarize make without target', () => {
    expect(summarizeToolAction('Bash', { command: 'make' })).toBe('Building with make');
  });

  it('should summarize tsc', () => {
    expect(summarizeToolAction('Bash', { command: 'tsc --noEmit' })).toBe('Type checking');
  });

  it('should summarize vitest', () => {
    expect(summarizeToolAction('Bash', { command: 'vitest --run' })).toBe('Running tests');
  });

  it('should summarize eslint', () => {
    expect(summarizeToolAction('Bash', { command: 'eslint src/' })).toBe('Running linter');
  });

  it('should summarize curl', () => {
    expect(summarizeToolAction('Bash', { command: 'curl https://example.com' })).toBe('Fetching URL');
  });

  it('should summarize kubectl', () => {
    expect(summarizeToolAction('Bash', { command: 'kubectl get pods' })).toBe('Managing Kubernetes');
  });

  it('should summarize terraform', () => {
    expect(summarizeToolAction('Bash', { command: 'terraform apply' })).toBe('Managing infrastructure');
  });

  it('should summarize go build', () => {
    expect(summarizeToolAction('Bash', { command: 'go build ./...' })).toBe('Building Go project');
  });

  it('should summarize go test', () => {
    expect(summarizeToolAction('Bash', { command: 'go test ./...' })).toBe('Running Go tests');
  });

  it('should summarize mkdir', () => {
    expect(summarizeToolAction('Bash', { command: 'mkdir -p /tmp/test' })).toBe('Creating directory');
  });

  it('should summarize rm', () => {
    expect(summarizeToolAction('Bash', { command: 'rm -rf dist/' })).toBe('Removing files');
  });

  it('should fallback to showing the command name', () => {
    expect(summarizeToolAction('Bash', { command: 'some-custom-tool --flag' })).toBe('Running `some-custom-tool`');
  });
});

describe('summarizeToolAction - wrapper stripping', () => {
  it('should strip sudo', () => {
    expect(summarizeToolAction('Bash', { command: 'sudo npm install' })).toBe('Installing dependencies');
  });

  it('should strip nohup', () => {
    expect(summarizeToolAction('Bash', { command: 'nohup cargo build' })).toBe('Building project');
  });

  it('should strip env', () => {
    expect(summarizeToolAction('Bash', { command: 'env cargo test' })).toBe('Running tests');
  });

  it('should strip chained trivial + wrapper', () => {
    expect(summarizeToolAction('Bash', { command: 'cd /project && sudo npm install' })).toBe('Installing dependencies');
  });
});

describe('summarizeToolAction - File tools', () => {
  it('should summarize Read with path', () => {
    expect(summarizeToolAction('Read', { file_path: '/opt/project/src/utils/config.ts' }))
      .toBe('Reading .../utils/config.ts');
  });

  it('should summarize Write with path', () => {
    expect(summarizeToolAction('Write', { file_path: '/opt/project/src/index.ts' }))
      .toBe('Writing .../src/index.ts');
  });

  it('should summarize Edit with path', () => {
    expect(summarizeToolAction('Edit', { file_path: '/home/user/project/main.rs' }))
      .toBe('Editing .../project/main.rs');
  });

  it('should summarize MultiEdit with path', () => {
    expect(summarizeToolAction('MultiEdit', { file_path: '/opt/app/src/lib.ts' }))
      .toBe('Editing .../src/lib.ts');
  });

  it('should summarize Grep with pattern', () => {
    expect(summarizeToolAction('Grep', { pattern: 'handleToolStart' }))
      .toBe("Searching for 'handleToolStart'");
  });

  it('should truncate long Grep patterns', () => {
    const longPattern = 'a'.repeat(50);
    const result = summarizeToolAction('Grep', { pattern: longPattern });
    expect(result).toBe(`Searching for '${longPattern.slice(0, 30)}...'`);
  });

  it('should summarize Glob with pattern', () => {
    expect(summarizeToolAction('Glob', { pattern: '**/*.ts' }))
      .toBe('Finding files: **/*.ts');
  });

  it('should summarize Task', () => {
    expect(summarizeToolAction('Task', {})).toBe('Running task');
  });

  it('should summarize WebSearch with query', () => {
    expect(summarizeToolAction('WebSearch', { query: 'vitest configuration guide' }))
      .toBe('Searching: vitest configuration guide');
  });

  it('should truncate long WebSearch queries', () => {
    const longQuery = 'a'.repeat(60);
    const result = summarizeToolAction('WebSearch', { query: longQuery });
    expect(result).toBe(`Searching: ${longQuery.slice(0, 40)}...`);
  });

  it('should summarize WebFetch with URL', () => {
    expect(summarizeToolAction('WebFetch', { url: 'https://docs.example.com/api/v2/guide' }))
      .toBe('Fetching docs.example.com');
  });

  it('should summarize TodoWrite', () => {
    expect(summarizeToolAction('TodoWrite', {})).toBe('Updating tasks');
  });

  it('should summarize TodoRead', () => {
    expect(summarizeToolAction('TodoRead', {})).toBe('Reading tasks');
  });

  it('should summarize AskUserQuestion', () => {
    expect(summarizeToolAction('AskUserQuestion', {})).toBe('Asking user a question');
  });

  it('should summarize NotebookEdit', () => {
    expect(summarizeToolAction('NotebookEdit', {})).toBe('Editing notebook');
  });

  it('should fallback for unknown tools', () => {
    expect(summarizeToolAction('SomeNewTool', {})).toBe('Using SomeNewTool');
  });
});

describe('summarizeToolResult', () => {
  it('should detect Rust compiler errors', () => {
    const output = 'error[E0308]: mismatched types\n --> src/main.rs:5:10';
    const result = summarizeToolResult('Bash', output);
    expect(result).toMatch(/^Failed:/);
    expect(result).toContain('error[E0308]');
  });

  it('should detect generic Error: pattern', () => {
    const output = 'Some output\nError: file not found\nMore output';
    expect(summarizeToolResult('Bash', output)).toMatch(/^Failed:/);
  });

  it('should detect FAILED pattern', () => {
    const output = 'test result: FAILED. 2 passed; 1 failed';
    expect(summarizeToolResult('Bash', output)).toBe('Tests failed');
  });

  it('should detect panic', () => {
    const output = "thread 'main' panicked at 'index out of bounds'";
    expect(summarizeToolResult('Bash', output)).toMatch(/^Panicked:/);
  });

  it('should detect npm ERR!', () => {
    const output = 'npm ERR! code ERESOLVE\nnpm ERR! Could not resolve dependency';
    expect(summarizeToolResult('Bash', output)).toBe('npm error');
  });

  it('should show line count for normal output', () => {
    const output = 'line1\nline2\nline3';
    expect(summarizeToolResult('Bash', output)).toBe('Completed (3 lines of output)');
  });

  it('should handle empty output', () => {
    expect(summarizeToolResult('Bash', '')).toBe('Completed (no output)');
  });

  it('should truncate long error lines', () => {
    const longError = 'Error: ' + 'x'.repeat(200);
    const result = summarizeToolResult('Bash', longError);
    // "Failed: " (8) + truncated to 60 + "..." (3) = 71
    expect(result.length).toBeLessThanOrEqual(71);
  });
});
