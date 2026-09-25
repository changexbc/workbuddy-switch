/** A human label for work directories that are generated as session IDs. */
export function railProjectLabel(session: {source: string; project?: string}, provider: string): string {
  const project = session.project?.trim();
  if (!project || project === session.source) return provider;
  if (session.source === 'codeg' && /^[0-9a-f]{32,64}$/i.test(project)) return '聊天';
  if (session.source === 'workbuddy') {
    const task = /^(\d{4})-(\d{2})-(\d{2})-(\d{2})-(\d{2})-(\d{2})$/.exec(project);
    if (task) {
      const [, year, month, day, hour, minute, second] = task;
      const date = new Date(Date.UTC(+year, +month - 1, +day, +hour, +minute, +second));
      if (date.getUTCFullYear() === +year && date.getUTCMonth() + 1 === +month && date.getUTCDate() === +day
        && date.getUTCHours() === +hour && date.getUTCMinutes() === +minute && date.getUTCSeconds() === +second) {
        return `任务 · ${month}/${day} ${hour}:${minute}`;
      }
    }
  }
  return project;
}
