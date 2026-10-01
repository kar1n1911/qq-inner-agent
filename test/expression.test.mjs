import test from 'node:test';
import assert from 'node:assert/strict';
import {defaults,merge,validate} from '../src/config.mjs';
import {Store} from '../src/store.mjs';
import {Engine} from '../src/engine.mjs';
import {OneBot} from '../src/onebot.mjs';
import {parseExpressions,personalityContext,decorationChoices,decorate} from '../src/expression.mjs';
const a={chat:'group:10',id:'a',sender:'20',text:'今天又咕咕了',ts:1000,self:false};
const b={...a,id:'b',sender:'21',ts:1001};
const pattern={subject:'group',kind:'jargon',term:'咕咕',meaning:'推迟原先约定',situation:'轻松调侃延期，不用于严肃求助',example:'咕咕',confidence:.9,sourceIds:['a','b']};
const settings=defaults.agent.expression;
test('expression evidence must be literal, human, in scope and attributed',()=>{
 const parse=(v,h=[a,b])=>parseExpressions([v],h,'group:10','20');
 assert.equal(parse(pattern)[0].sources.length,2);
 for(const v of [{...pattern,term:'不存在'},{...pattern,example:'编造原话'},{...pattern,subject:'person:21'},{...pattern,subject:'person:20'},{...pattern,confidence:NaN}])assert.throws(()=>parse(v));
 assert.throws(()=>parse(pattern,[{...a,self:true},b]));assert.throws(()=>parse(pattern,[{...a,chat:'group:11'},b]));
});
test('pending jargon needs repeated multi-author evidence, stays scoped and respects use cooldown',()=>{
 const s=new Store(':memory:');
 const apply=(history,ids,now)=>s.expressions.apply('group:10',parseExpressions([{...pattern,sourceIds:ids}],history,'group:10','20'),now,settings);
 const context=(chat='group:10',sender='20',now=1010,options=settings)=>s.expressions.context(chat,sender,'咕咕',now,options,defaults.agent.memory);
 try{
  apply([a],['a'],1000);assert.equal(context().length,0);
  apply([b],['b'],1001);const rows=context();assert.equal(rows.length,1);
  assert.equal(context('group:11').length,0);assert.equal(context('private:20').length,0);
  s.expressions.used('group:10',rows,'那就别咕咕啦',1010);assert.equal(context().length,0);
  assert.equal(context('group:10','20',2811).length,1);
  assert.equal(context('group:10','20',2811,{...settings,useLearned:false}).length,0);
  s.resetLearning('group:10',2812,'group');assert.equal(context('group:10','20',2813).length,0);
 }finally{s.close();}
});
test('personal expression never becomes another speaker style and changed meanings need new evidence',()=>{
 const s=new Store(':memory:');const c={...a,id:'c',ts:1002};
 try{
  const p={...pattern,subject:'person:20',sourceIds:['a','c']};
  s.expressions.apply('group:10',parseExpressions([p],[a,c],'group:10','20'),1002,settings);
  assert.equal(s.expressions.context('group:10','21','咕咕',1003,settings,defaults.agent.memory).length,0);
  s.expressions.apply('group:10',parseExpressions([{...p,meaning:'错误新解释'}],[a,c],'group:10','20'),1004,settings);
  assert.equal(s.db.prepare('SELECT meaning FROM expressions').get().meaning,pattern.meaning);
  s.expressions.prune(1002+settings.retentionDays*86400,settings);assert.equal(s.db.prepare('SELECT count(*) n FROM expressions').get().n,0);
 }finally{s.close();}
});
test('persona variants preserve custom identity and optional decoration is bounded and allowlisted',()=>{
 const agent=merge(defaults.agent,{persona:'自定义身份',personality:{variants:['轻松一点'],variantProbability:1}});
 assert.equal(personalityContext(agent,()=>0).identity,'自定义身份');assert.equal(personalityContext(agent,()=>0).variant,'轻松一点');
 assert.deepEqual(decorate({text:'你好',emoji:'未知',faceId:'999'},{symbols:['🙂'],faceIds:['14']},10),{text:'你好',faceId:null,decorated:false});
 assert.equal(decorate({text:'你好',faceId:'14'},{symbols:[],faceIds:['14']},10).faceId,'14');
 assert.ok([...decorate({text:'你好',emoji:'x'.repeat(24)},{symbols:['x'.repeat(24)],faceIds:[]},10).text].length<=10);
 for(const config of [{emoji:{faceIds:['file:///secret']}},{personality:{variantProbability:2}},{expression:{maxPerReply:0}}])assert.throws(()=>validate(merge(defaults,{agent:config})));
});
test('QQ face is sent as a typed segment, while CQ-looking text stays inert',async()=>{
 const bot=new OneBot(defaults.onebot,'test');bot.connected=true;bot.online=true;let sent;
 bot.call=async(action,params)=>{sent=params;return{message_id:1};};
 await bot.send('group:10','[CQ:image,file=secret]','14');
 assert.deepEqual(sent.message,[{type:'text',data:{text:'[CQ:image,file=secret]'}},{type:'face',data:{id:'14'}}]);
 await assert.rejects(bot.send('group:10','x','image'),/invalid_face/);
});
test('engine supplies personality and sends optional face once without extra model stages',async()=>{
 const s=new Store(':memory:');const calls=[],sent=[];
 const config=merge(defaults,{agent:{allowedUsers:['20'],quietHours:null,sending:{enabled:false},emoji:{enabled:true,probability:1,faceIds:['14']}}});
 const transport={selfId:'99',connected:true,online:true,send:async(...args)=>{sent.push(args);return{message_id:'sent'};}};
 const provider={json:async(sys,p)=>{calls.push(p);if(sys.includes('TASK: FORM'))return{allocation:'self',candidates:[{kind:'system2',text:'打招呼'}]};if(sys.includes('TASK: EVALUATE'))return{ratings:p.candidates.map(c=>({id:c.id,motivation:5,relevance:5,originality:5}))};return{text:'你好',faceId:'14'};}};
 const engine=new Engine(config,s,provider,transport,{now:()=>1000,expressionRandom:()=>0});
 try{
  engine.ingest({post_type:'message',message_type:'private',user_id:20,message_id:'hi',time:1000,message:'你好'});await engine.cycle('private:20');
  assert.equal(calls.length,3);assert.ok(calls[0].personality.replyStyle);assert.deepEqual(calls[2].decorations.faceIds,['14']);
  assert.deepEqual(sent,[['private:20','你好','14']]);
  assert.deepEqual(decorationChoices(s,'private:20',1001,config.agent.emoji,()=>0),{symbols:[],faceIds:[]});
 }finally{s.close();}
});
