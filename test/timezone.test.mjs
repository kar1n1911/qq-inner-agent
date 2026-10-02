import test from 'node:test';
import assert from 'node:assert/strict';
import { defaults, merge, validate } from '../src/config.mjs';
import { activeAt, quiet } from '../src/policy.mjs';

// 2026-06-15T12:00:00Z —— +08:00 的本地时间是 20:00。
const TS = 1_781_524_800;

test('the config validator accepts a fixed UTC offset', () => {
  for (const timezone of ['+08:00', '-03:00', 'UTC', '+05:30', 'Europe/Stockholm']) {
    assert.doesNotThrow(
      () => validate(merge(defaults, { agent: {
        quietHours: { start: 23, end: 8, timezone },
        schedule: { enabled: true, activeStart: '08:00', inactiveStart: '23:00', timezone },
      } })),
      `${timezone} should be a valid time zone`,
    );
  }
});

test('a fixed offset shifts the local clock and ignores daylight saving', () => {
  const window = timezone => activeAt(TS, { enabled: true, activeStart: '20:00', inactiveStart: '21:00', timezone });
  assert.equal(window('+08:00'), true, '20:00 local falls in the window');
  assert.equal(window('UTC'), false, '12:00 local does not');
  assert.equal(window('-03:00'), false, '09:00 local does not');
  assert.equal(window('+09:00'), false, '21:00 local is already past the window');

  // 冬天的同一时段：+08:00 不随季节变化，Europe/Stockholm 会。
  assert.equal(activeAt(1_767_225_600, { enabled: true, activeStart: '08:00', inactiveStart: '09:00', timezone: '+08:00' }), true);
  assert.equal(activeAt(1_767_225_600, { enabled: true, activeStart: '08:00', inactiveStart: '09:00', timezone: 'Europe/Stockholm' }), false);
});

test('quiet hours accept a fixed offset too', () => {
  assert.equal(quiet(TS, { start: 23, end: 8, timezone: '+08:00' }), false);
  assert.equal(quiet(TS, { start: 20, end: 21, timezone: '+08:00' }), true);
  assert.equal(quiet(TS, { start: 12, end: 13, timezone: 'UTC' }), true);
});

test('the dashboard offers both steps of the time zone choice', async () => {
  const { readFileSync } = await import('node:fs');
  const html = readFileSync(new URL('../web/index.html', import.meta.url), 'utf8');
  assert.match(html, /<datalist id="timezone-options">/, 'the option list should exist');
  // 固定偏移排在区域名之前，对应"先选 UTC±X，再选区域"。
  // 注意要匹配 datalist 里的 option，输入框自身也带有 value="Europe/Stockholm"。
  const offsets = html.indexOf('<option value="+08:00">');
  const region = html.indexOf('<option value="Europe/Stockholm">');
  assert.ok(offsets !== -1 && region !== -1 && offsets < region, 'offsets should be listed first');
  // 两个时区输入都要挂上这个列表。
  assert.match(html, /id="quiet-timezone" list="timezone-options"/);
  assert.match(html, /data-config="agent\.schedule\.timezone" list="timezone-options"/);
});
