// 调用真实 Engine/Store；只替换 I/O、时钟与随机。UUID 不进入比较。
import { readFileSync } from 'node:fs';
import assert from 'node:assert/strict';
import { Engine } from './src/engine.mjs';
import { Store } from './src/store.mjs';
import { defaults, merge } from './src/config.mjs';
const input = JSON.parse(readFileSync(0, 'utf8'));
const output = [];
for (const test of input) {
  let now = 43200, model = {}, sends = 0, engine;
  const trace = [], store = new Store(':memory:');
  const config = merge(defaults, test.config);
  const event = (step) => ({ post_type:'message', message_type:'group', self_id:99, user_id:step.sender ?? 20, group_id:step.group ?? 10, message_id:step.id ?? 'm1', time:now, sender:{nickname:'Human'}, message:step.text ?? '[CQ:at,qq=99]你好' });
  const transport = {
    selfId:'99', connected:true, online:true,
    call:async () => { throw Error('unsupported'); },
    send:async (chat,text,face) => {
      assert.equal(store.db.prepare("SELECT count(*) n FROM deliveries WHERE chat=? AND status='pending'").get(chat).n, 1);
      assert.equal(store.handled(chat).pause_done, 1);
      trace.push(['send',chat,text,face ?? null]); sends++;
      if (model.delivery) throw Object.assign(Error(model.delivery), {code:'mock_delivery',uncertain:model.delivery === 'uncertain'});
      return {message_id:`sent${sends}`};
    }
  };
  const provider = {json:async (system,payload) => {
    const stage = system.match(/TASK: (\w+)/)[1];
    trace.push(['model',stage,payload.addressedHint ?? null,payload.trigger ?? null,payload.history?.map(m=>m.id) ?? [],payload.lengthTarget ?? null]);
    if (!store.callBudget(now,test.budget ?? 1000)) throw Object.assign(Error('budget'),{code:'hourly_api_budget'});
    if (model.effectStage === stage) {
      const effect = model.effect; delete model.effectStage;
      if (effect === 'message') engine.ingest(event({id:'new',text:'新的问题'}));
      if (effect === 'learning') store.resetLearning('group:10',now);
      if (effect === 'orientation') engine.orientation.joined('group:10',now);
      if (effect === 'activity') now += 31;
      if (effect === 'expired') now += 1001;
      if (effect === 'offline') transport.online = false;
    }
    if (model.invalidStage === stage) return model.invalid ?? {};
    if (stage === 'FORM') return {allocation:model.allocation ?? 'open', candidates:model.empty ? [] : [{kind:'system2',text:'建议从土壤湿度判断浇水'}], ...(model.learning !== undefined ? {learning:model.learning}: {})};
    if (stage === 'EVALUATE') return {ratings:payload.candidates.map(c=>({id:c.id,motivation:model.score ?? 5,relevance:4,originality:4,for:['relevance','bad','coherence','balance'],against:['balance']}))};
    if (stage === 'FORECAST') return {shouldSend:!model.veto,outcomes:{reply:0.6,silence:0.4,negative:0},responseMode:model.veto?'wait':'answer',plan:'接住当前问题'};
    if (stage === 'ARTICULATE') return {text:model.reply ?? '可以先看看盆土是否已经干透。'};
    return {style:'谨慎接话',summary:'园艺讨论',topics:['园艺']};
  }};
  // P6c：真实 JS 的长度 payload 和 message_sent 日志也进入 golden。
  const draws = [...(test.expressionDraws ?? [])];
  Math.random = () => 0.9;
  engine = new Engine(config,store,provider,transport,{now:()=>now,random:()=>0,expressionRandom:()=>draws.shift() ?? 0.5,activityRandom:()=>0,log:(event,data)=>{trace.push(['log',event,data]);}});
  for (const step of test.steps) {
    if (step.op === 'model') model = {...step.value};
    if (step.op === 'ingest') engine.ingest(event(step));
    if (step.op === 'advance') now += step.seconds;
    if (step.op === 'run') {engine.tick(); await Promise.all([...engine.running]);}
    if (step.op === 'seed') {const id=store.delivery('group:10',step.proactive ?? true,now);store.finishDelivery(id,step.status ?? 'sent');}
    if (step.op === 'restore') engine.restore();
    if (step.op === 'ready') {const r=engine.orientation.ensure('group:10'); store.db.prepare("UPDATE group_orientation SET status='ready' WHERE chat=?").run(r.chat);}
    if (step.op === 'notice') engine.ingest({post_type:'notice',notice_type:'group_increase',self_id:99,user_id:99,group_id:10,time:now});
  }
  const rows = sql=>store.db.prepare(sql).all();
  output.push({label:test.label,trace,decisions:rows('SELECT chat,action,score,tags FROM decisions ORDER BY rowid').map(r=>({...r,tags:JSON.parse(r.tags)})),deliveries:rows('SELECT chat,proactive,status FROM deliveries ORDER BY rowid'),assessments:rows('SELECT chat,human_id,status FROM send_assessments ORDER BY rowid'),thoughts:rows('SELECT chat,text,kind,used,score,subject FROM thoughts ORDER BY rowid'),handled:rows('SELECT * FROM handled ORDER BY chat'),chats:[...engine.chats],calls:rows('SELECT count(*) n FROM calls')[0].n,lastError:engine.lastError});
  await engine.stop(); store.close();
}
process.stdout.write(JSON.stringify(output));
