/**
 * Message Chunking Utilities
 * Handles splitting long messages for Telegram's 4096 char limit
 */

const DEFAULT_MAX_LENGTH = 4000; // Leave room for part headers

interface ChunkOptions {
  maxLength?: number;
  preserveCodeBlocks?: boolean;
  addPartHeaders?: boolean;
}

/**
 * Find all code block positions in the text
 */
function findCodeBlocks(text: string): Array<{ start: number; end: number }> {
  const blocks: Array<{ start: number; end: number }> = [];
  let match;

  const regex = /```[\s\S]*?```/g;
  while ((match = regex.exec(text)) !== null) {
    blocks.push({
      start: match.index,
      end: match.index + match[0].length
    });
  }

  return blocks;
}

/**
 * Check if a position is inside a code block
 */
function isInsideCodeBlock(
  position: number,
  codeBlocks: Array<{ start: number; end: number }>
): boolean {
  return codeBlocks.some(block => position > block.start && position < block.end);
}

/**
 * Find the best split point near the target position
 */
function findBestSplitPoint(
  text: string,
  targetPosition: number,
  codeBlocks: Array<{ start: number; end: number }>
): number {
  // If we're inside a code block, try to split before it
  for (const block of codeBlocks) {
    if (targetPosition > block.start && targetPosition < block.end) {
      // Try to split before this code block if there's room
      if (block.start > 100) {
        return block.start;
      }
      // Otherwise, split after this code block
      return Math.min(block.end, text.length);
    }
  }

  // Look for natural break points (newlines, sentence ends)
  const searchStart = Math.max(0, targetPosition - 200);
  const searchEnd = Math.min(text.length, targetPosition + 50);
  const searchText = text.slice(searchStart, searchEnd);

  // Priority: double newline > single newline > period > space
  const breakPoints = [
    { pattern: /\n\n/g, offset: 2 },
    { pattern: /\n/g, offset: 1 },
    { pattern: /\. /g, offset: 2 },
    { pattern: / /g, offset: 1 }
  ];

  for (const { pattern, offset } of breakPoints) {
    let match;
    let bestMatch = -1;

    while ((match = pattern.exec(searchText)) !== null) {
      const absolutePos = searchStart + match.index + offset;
      if (absolutePos <= targetPosition && !isInsideCodeBlock(absolutePos, codeBlocks)) {
        bestMatch = absolutePos;
      }
    }

    if (bestMatch !== -1) {
      return bestMatch;
    }
  }

  // Fallback to target position
  return targetPosition;
}

/**
 * Chunk a message into parts that fit Telegram's limit
 */
export function chunkMessage(
  text: string,
  options: ChunkOptions = {}
): string[] {
  const {
    maxLength = DEFAULT_MAX_LENGTH,
    preserveCodeBlocks = true,
    addPartHeaders = true
  } = options;

  // If it fits, return as-is
  if (text.length <= maxLength) {
    return [text];
  }

  const chunks: string[] = [];
  const codeBlocks = preserveCodeBlocks ? findCodeBlocks(text) : [];
  let remaining = text;
  let chunkIndex = 0;

  while (remaining.length > 0) {
    chunkIndex++;

    if (remaining.length <= maxLength) {
      chunks.push(remaining);
      break;
    }

    // Find best split point
    const splitPoint = findBestSplitPoint(remaining, maxLength, codeBlocks);

    // Extract chunk
    let chunk = remaining.slice(0, splitPoint).trim();
    remaining = remaining.slice(splitPoint).trim();

    // Update code block positions for remaining text
    codeBlocks.forEach(block => {
      block.start -= splitPoint;
      block.end -= splitPoint;
    });
    // Remove blocks that are now fully before the split
    while (codeBlocks.length > 0 && codeBlocks[0].end < 0) {
      codeBlocks.shift();
    }

    chunks.push(chunk);
  }

  // Add part headers if needed
  if (addPartHeaders && chunks.length > 1) {
    return chunks.map((chunk, i) =>
      `📄 *Part ${i + 1}/${chunks.length}*\n\n${chunk}`
    );
  }

  return chunks;
}

/**
 * Estimate the number of chunks a message will need
 */
export function estimateChunks(text: string, maxLength: number = DEFAULT_MAX_LENGTH): number {
  return Math.ceil(text.length / maxLength);
}

/**
 * Check if a message needs chunking
 */
export function needsChunking(text: string, maxLength: number = DEFAULT_MAX_LENGTH): boolean {
  return text.length > maxLength;
}

export { DEFAULT_MAX_LENGTH };
