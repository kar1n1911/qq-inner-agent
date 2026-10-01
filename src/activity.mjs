import { activeAt } from './policy.mjs';

// A bounded Gaussian-shaped inactivity curve, not a normalized probability density.
export function activityProbability(now, schedule, rhythm) {
  if (!schedule.enabled || activeAt(now, schedule)) return rhythm.dayProbability;
  const parts = new Intl.DateTimeFormat('en-GB', { timeZone: schedule.timezone, hour: '2-digit', minute: '2-digit', second: '2-digit', hourCycle: 'h23' }).formatToParts(new Date(now * 1000));
  const value = type => Number(parts.find(p => p.type === type).value);
  const minute = value('hour') * 60 + value('minute') + value('second') / 60;
  const minutes = text => { const [h, m] = text.split(':').map(Number); return h * 60 + m; };
  const start = minutes(schedule.inactiveStart), end = minutes(schedule.activeStart);
  const duration = (end - start + 1440) % 1440;
  const x = ((minute - start + 1440) % 1440) / duration;
  const edge = Math.exp(-0.5 * (0.5 / rhythm.sigma) ** 2);
  const gaussian = Math.exp(-0.5 * ((x - 0.5) / rhythm.sigma) ** 2);
  const dip = Math.max(0, Math.min(1, (gaussian - edge) / (1 - edge)));
  return rhythm.edgeProbability - (rhythm.edgeProbability - rhythm.centerProbability) * dip;
}

export class ActivityRhythm {
  constructor(store, agent, random = Math.random) {
    this.db = store.db; this.agent = agent; this.random = random;
    this.signature = JSON.stringify([agent.schedule, agent.rhythm]);
    this.db.exec(`CREATE TABLE IF NOT EXISTS activity_rhythm (
      id INTEGER PRIMARY KEY CHECK(id=1), signature TEXT, started REAL, until REAL,
      active INTEGER, probability REAL, draw REAL)`);
  }
  snapshot(now) {
    const { schedule, rhythm } = this.agent;
    if (!rhythm.enabled) return { enabled: false, active: activeAt(now, schedule), until: null, started: null };
    let row = this.db.prepare('SELECT * FROM activity_rhythm WHERE id=1').get();
    if (!row || row.signature !== this.signature || now < row.started || now >= row.until) {
      const probability = activityProbability(now, schedule, rhythm), draw = this.random();
      const active = draw < probability;
      const min = active ? rhythm.activeMinSeconds : rhythm.restMinSeconds;
      const max = active ? rhythm.activeMaxSeconds : rhythm.restMaxSeconds;
      row = { started: now, until: now + min + Math.floor(this.random() * (max - min + 1)), active: Number(active), probability, draw };
      this.db.prepare(`INSERT OR REPLACE INTO activity_rhythm VALUES(1,?,?,?,?,?,?)`)
        .run(this.signature, row.started, row.until, row.active, row.probability, row.draw);
    }
    return { enabled: true, active: !!row.active, started: row.started, until: row.until,
      probability: row.probability, draw: row.draw, currentProbability: activityProbability(now, schedule, rhythm) };
  }
}
