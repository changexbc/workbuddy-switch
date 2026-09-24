// Migrated from hy4-pixcel-office: incremental JSONL, partial lines and rotation.
import fs from 'node:fs/promises';
import path from 'node:path';
import { StringDecoder } from 'node:string_decoder';
export async function walk(dir, match = n => n.endsWith('.jsonl'), depth = 0) {
  const entries = await fs.readdir(dir, { withFileTypes: true });
  const result = [];
  for (const e of entries) {
    const file = path.join(dir, e.name);
    if (e.isDirectory() && depth < 6 && !['node_modules', '.git'].includes(e.name)) result.push(...await walk(file, match, depth + 1));
    else if (e.isFile() && match(e.name)) result.push(file);
  }
  return result;
}
export class Tailer {
  constructor({ backfillBytes = 512 * 1024, onReset = () => {} } = {}) {
    this.backfillBytes = backfillBytes; this.onReset = onReset; this.state = new Map();
  }
  async pump(file, onRecord) {
    const stat = await fs.stat(file);
    let s = this.state.get(file);
    if (!s || s.inode !== stat.ino || stat.size < s.offset) {
      if (s) this.onReset(file);
      const offset = s ? 0 : Math.max(0, stat.size - this.backfillBytes);
      s = { inode: stat.ino, offset, remainder: '', skip: offset > 0, decoder: new StringDecoder('utf8') };
      this.state.set(file, s);
    }
    const handle = await fs.open(file, 'r'); let count = 0;
    try {
      while (s.offset < stat.size) {
        const buffer = Buffer.alloc(Math.min(256 * 1024, stat.size - s.offset));
        const { bytesRead } = await handle.read(buffer, 0, buffer.length, s.offset);
        if (!bytesRead) break;
        s.offset += bytesRead;
        const lines = (s.remainder + s.decoder.write(buffer.subarray(0, bytesRead))).split('\n');
        s.remainder = lines.pop();
        if (s.remainder.length > 2 * 1024 * 1024) { s.remainder = ''; s.skip = true; }
        for (const line of lines) {
          if (s.skip) { s.skip = false; continue; }
          let record; try { record = JSON.parse(line); } catch { continue; }
          onRecord(record); count++;
        }
      }
    } finally { await handle.close(); }
    return count;
  }
}
