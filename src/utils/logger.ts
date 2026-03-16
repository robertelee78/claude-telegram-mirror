/**
 * Logger Configuration
 */

import winston from 'winston';

const { combine, timestamp, printf, colorize } = winston.format;

/**
 * Scrub bot tokens from text to prevent credential leakage in logs.
 * Matches the pattern bot{id}:{secret}/ in Telegram API URLs.
 */
export function scrubBotToken(text: string): string {
  return text.replace(/bot\d+:[A-Za-z0-9_-]+\//g, 'bot<REDACTED>');
}

const scrubTokenFormat = winston.format((info) => {
  if (typeof info.message === 'string') {
    info.message = scrubBotToken(info.message);
  }
  // Scrub string values in metadata
  for (const key of Object.keys(info)) {
    if (key !== 'level' && key !== 'message' && typeof info[key] === 'string') {
      info[key] = scrubBotToken(info[key]);
    }
  }
  return info;
})();

const logFormat = printf(({ level, message, timestamp, ...meta }) => {
  const metaStr = Object.keys(meta).length ? ` ${JSON.stringify(meta)}` : '';
  return `${timestamp} [${level}]: ${message}${metaStr}`;
});

export const logger = winston.createLogger({
  level: process.env.LOG_LEVEL || 'info',
  format: combine(
    scrubTokenFormat,
    timestamp({ format: 'YYYY-MM-DD HH:mm:ss' }),
    logFormat
  ),
  transports: [
    new winston.transports.Console({
      stderrLevels: ['error', 'warn', 'info', 'http', 'verbose', 'debug', 'silly'],
      format: combine(
        colorize(),
        timestamp({ format: 'YYYY-MM-DD HH:mm:ss' }),
        logFormat
      )
    })
  ]
});

export default logger;
