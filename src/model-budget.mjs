import { DatabaseSync } from 'node:sqlite';

// Offline model diagnostics share the core's persistent hourly request budget.
// Keep this small table independent of agent schema initialization/migrations.
export function callBudget(filename, now, max) {
  const db = new DatabaseSync(filename);
  try {
    db.exec('PRAGMA busy_timeout=5000; CREATE TABLE IF NOT EXISTS calls(ts REAL); BEGIN IMMEDIATE');
    try {
      db.prepare('DELETE FROM calls WHERE ts<?').run(now - 3600);
      const allowed = db.prepare('SELECT count(*) AS n FROM calls').get().n < max;
      if (allowed) db.prepare('INSERT INTO calls VALUES(?)').run(now);
      db.exec('COMMIT');
      return allowed;
    } catch (error) { db.exec('ROLLBACK'); throw error; }
  } finally { db.close(); }
}
