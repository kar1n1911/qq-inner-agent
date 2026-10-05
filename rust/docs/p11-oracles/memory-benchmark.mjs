// 可复跑的同输入基准；计时不包含启动、建库和预热。
import fs from 'node:fs';
import {Store} from '../../../src/store.mjs';
import {defaults} from '../../../src/config.mjs';
const input=JSON.parse(fs.readFileSync(0,'utf8'));
const source=fs.readFileSync(new URL('../../../src/memory-ranking.mjs',import.meta.url),'utf8').replace('export function tokens(text) {','export function tokens(text) { globalThis.tokenizations++;');
const {rankMemories}=await import(`data:text/javascript;base64,${Buffer.from(source).toString('base64')}`);
const timings=[],ranking=[];let memory,ranked,tokenizations;
for(let repeat=0;repeat<3;repeat++) {
 const s=new Store(':memory:');
 for(const m of input.seed)s.memory.capture(m,m.ts,defaults.agent.memory);
 const start=performance.now();
 for(const m of input.messages)s.memory.capture(m,m.ts,defaults.agent.memory);
 s.memory.configure(3000,defaults.agent.memory);
 timings.push(performance.now()-start);
 memory=s.db.prepare('SELECT * FROM memory_layers ORDER BY chat,subject,layer,slot').all().map(r=>{delete r.id;return {...r,sources:JSON.parse(r.sources),keywords:JSON.parse(r.keywords)};});s.close();
 globalThis.tokenizations=0;const rankStart=performance.now();ranked=rankMemories(input.rows,'园艺 rareword',3000,defaults.agent.memory);ranking.push(performance.now()-rankStart);tokenizations=globalThis.tokenizations;
}
console.log(JSON.stringify({captureMs:timings.sort((a,b)=>a-b)[1],rankingMs:ranking.sort((a,b)=>a-b)[1],tokenizations,memory,ranked}));
