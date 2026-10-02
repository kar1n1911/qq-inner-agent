// 从生产 JS 所用 Unicode 正则生成属性区间；无需 Rust 新依赖。
import fs from 'node:fs';
const ranges = re => {
 const out=[]; let start=-1;
 for(let i=0;i<=0x110000;i++) {
  const yes=i<0x110000 && re.test(String.fromCodePoint(i));
  if(yes && start<0) start=i;
  if(!yes && start>=0){out.push(`(${start},${i-1})`);start=-1;}
 }
 return out.join(',\n');
};
fs.writeFileSync(new URL('../src/memory_unicode.rs',import.meta.url),`// 自动生成：node ${process.versions.node}，Unicode ${process.versions.unicode}；见 tools/gen-memory-unicode.mjs。\npub const LETTER_NUMBER: &[(u32,u32)] = &[${ranges(/[\p{L}\p{N}]/u)}];\npub const HAN: &[(u32,u32)] = &[${ranges(/\p{Script=Han}/u)}];\n`);
