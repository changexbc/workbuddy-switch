import type { ConnectionState } from '../types/snapshot.js';
import type { RailItem } from './session-model.js';
import { permissionReminderKey, prolongedPermissionCheck } from './permission-check.js';

// A muted question stays muted only for this exact round and pending payload.
export const questionKey = (item: RailItem) => JSON.stringify([item.session.roundId, item.session.pending || []]);
export function automaticReminderItems(items: RailItem[], connection: ConnectionState, mutedQuestions: Map<string, string>, now = Date.now()) {
  for (const [id, key] of mutedQuestions) {
    const item=items.find(row=>row.id===id);
    const current = item && (item.session.status==='wait' ? questionKey(item)
      : prolongedPermissionCheck(item.session, now) ? permissionReminderKey(item.session) : null);
    if(current!==key)mutedQuestions.delete(id);
  }
  // A failed round asks for attention the same way a completed one does.
  // `aborted` stays out on purpose: the user stopped that round themselves, so
  // a card for it would be noise rather than news.
  return items.filter(item=>!item.offline&&connection==='connected'&&(
    item.session.status==='done'||item.session.status==='error'||(item.session.status==='wait'&&mutedQuestions.get(item.id)!==questionKey(item))
    ||(prolongedPermissionCheck(item.session, now)&&mutedQuestions.get(item.id)!==permissionReminderKey(item.session))
  ));
}
