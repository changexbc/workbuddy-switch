// @ts-check
import internalPrompts from './codex-internal-prompts.json' with { type: 'json' };

/**
 * @param {unknown} prompt
 * @returns {boolean}
 */
// Narrow, observed templates only. Do not hide user tasks based on keywords,
// cwd, missing titles, or the mere presence of a background process.
export function isInternalCodexPrompt(prompt) {
  const normalized = String(prompt || '').trim().replace(/\s+/g, ' ');
  return internalPrompts.some(prefix => normalized.startsWith(prefix));
}
/**
 * @param {import('../types/snapshot.js').Session} session
 * @returns {boolean}
 */
export function visibleSession(session) {
  return session.source !== 'codex' || !isInternalCodexPrompt(session.title);
}
