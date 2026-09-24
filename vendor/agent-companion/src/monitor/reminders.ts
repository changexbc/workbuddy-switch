import type { ConnectionState } from '../types/snapshot.js';
import type { RailItem } from './session-model.js';

// A muted question stays muted only for this exact round and pending payload.
export const questionKey = (item: RailItem) => JSON.stringify([item.session.roundId, item.session.pending || []]);
export function automaticReminderItems(items: RailItem[], connection: ConnectionState, mutedQuestions: Map<string, string>) {
  for (const [id, key] of mutedQuestions) {
    const item=items.find(row=>row.id===id);
    if(!item||item.session.status!=='wait'||questionKey(item)!==key)mutedQuestions.delete(id);
  }
  // Completed sessions stay in the rail with a checkmark; only questions interrupt.
  return items.filter(item=>!item.offline&&connection==='connected'&&item.session.status==='wait'&&mutedQuestions.get(item.id)!==questionKey(item));
}
