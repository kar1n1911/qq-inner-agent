// 真实生产 JS oracle；仅使用内存 SQLite，任何输入都不接触 data/。
import fs from 'node:fs';
import {Store} from '../../../src/store.mjs';
import {defaults} from '../../../src/config.mjs';
import {memorySubjects,parseMemoryUpdates} from '../../../src/memory.mjs';
import {tokens,rankMemories} from '../../../src/memory-ranking.mjs';
import {parseExpressions,personalityContext,decorationChoices,decorate} from '../../../src/expression.mjs';
const input=JSON.parse(fs.readFileSync(0,'utf8')),s=new Store(':memory:');
const settings={...defaults.agent.memory,...input.settings},es={...defaults.agent.expression,...input.expressions};
const clean=r=>{const v={...r}; if(v.chat&&v.layer)delete v.id;return v;};
const out=[];
for(const a of input.actions) {
 try {
  let value=null;const now=a.now??1000, chat=a.chat??'group:10',sender=a.sender??'20';
  switch(a.op){
   case 'subjects':value=memorySubjects(chat,sender);break;
   case 'parse':value=parseMemoryUpdates(a.value,a.history,chat,sender,settings);break;
   case 'parseExpressions':value=parseExpressions(a.value,a.history,chat,sender);break;
   case 'tokens':value=tokens(a.text).map(t=>Array.from({length:t.length},(_,i)=>t.charCodeAt(i)));break;
   case 'rank':value=rankMemories(a.rows,a.query,now,settings,{requireMatch:a.requireMatch??false});break;
   case 'message':s.message(a.message);break;
   case 'capture':s.memory.capture(a.message,now,settings);break;
   case 'apply':s.memory.apply(chat,a.updates,now,settings);break;
   case 'configure':s.memory.configure(now,{...settings,...a.settings});break;
   case 'short':value=s.memory.short(chat,sender,now,settings,a.excluded).map(clean);break;
   case 'context':value=s.memory.context(chat,sender,now,settings,a.query??'').map(v=>({...v,long_term:v.long_term.map(clean),traits:v.traits.map(clean)}));break;
   case 'scoped':value=s.retrieveScoped(chat,sender,a.query,now,settings,a.options).map(clean);break;
   case 'learn':value=s.learn(chat,a.update,now,a.lastId??'',defaults.agent.learning,a.epoch??0,a.layered?{updates:a.layered,settings,expressions:a.expressionUpdates,expressionSettings:es}:null);break;
   case 'state':value=s.learningState(chat);value={...value,sources:JSON.parse(value.sources),rawSourcesType:typeof value.sources};break;
   case 'reset':s.resetLearning(chat,now,a.subject);break;
   case 'expressionApply':s.expressions.apply(chat,a.updates,now,es);break;
   case 'expressionContext':value=s.expressions.context(chat,sender,a.query,now,es,settings);break;
   case 'used':s.expressions.used(chat,a.rows,a.text,now);break;
   case 'expressionPrune':s.expressions.prune(now,{...es,...a.settings});break;
   case 'decorate':value=decorate(a.response,a.choices,a.max);break;
   case 'choices':value=decorationChoices(s,chat,now,{...defaults.agent.emoji,...a.settings},()=>a.random??0);break;
   case 'usage':s.db.prepare('INSERT OR REPLACE INTO decoration_usage VALUES(?,?)').run(chat,now);break;
   case 'personality':value=personalityContext({...defaults.agent,...a.agent,personality:{...defaults.agent.personality,...a.agent?.personality}},()=>a.random??0);break;
   case 'prune':s.prune(now,a.days??30,500);break;
   case 'dump':value={memory:s.db.prepare('SELECT * FROM memory_layers ORDER BY chat,subject,layer,slot').all().map(r=>clean({...r,sources:JSON.parse(r.sources),keywords:JSON.parse(r.keywords)})),revisions:s.db.prepare('SELECT m.chat,m.subject,m.layer,m.slot,r.revision,r.text,r.sources,r.updated,r.replaced FROM memory_revisions r JOIN memory_layers m ON m.id=r.memory_id ORDER BY m.chat,m.subject,m.layer,m.slot,r.revision').all().map(r=>({...r,sources:JSON.parse(r.sources)})),expressions:s.db.prepare('SELECT * FROM expressions ORDER BY chat,subject,kind,term').all().map(r=>({...r,sources:JSON.parse(r.sources)})),history:s.db.prepare('SELECT * FROM messages ORDER BY chat,id').all()};break;
   default:throw Error(`unknown op ${a.op}`);
  }
  out.push({ok:value});
 }catch(e){out.push({error:e.message});}
}
console.log(JSON.stringify(out));s.close();
