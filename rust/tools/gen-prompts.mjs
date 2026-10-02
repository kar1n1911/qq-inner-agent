// 从 `src/prompts.mjs` 生成 `rust/src/prompts.rs`。
//
// 提示词是行为的一部分，手抄会引入看不见的字符差异（全角/半角、空格、换行）。因此
// Rust 侧的常量一律由本脚本生成，并用 `rust/tests/prompts_parity.rs` 兜底。
//
// 用法（仓库根目录）：node rust/tools/gen-prompts.mjs
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const here = path.dirname(fileURLToPath(import.meta.url));
const root = path.resolve(here, '..', '..');
const target = path.join(root, 'rust', 'src', 'prompts.rs');

const { boundary, formation, evaluation, articulation, forecast, articulationFor } =
  await import(path.join(root, 'src', 'prompts.mjs'));
// ORIENT 提示词住在 orientation.mjs 里，但它同样是"行为的一部分"，一并生成。
const { orientationPrompt } = await import(path.join(root, 'src', 'orientation.mjs'));

const items = [
  ['BOUNDARY', boundary],
  ['FORMATION', formation],
  ['EVALUATION', evaluation],
  ['ARTICULATION', articulation],
  ['FORECAST', forecast],
  ['ORIENTATION', orientationPrompt],
];

// raw string 用 "## 作为分隔符；内容里若出现同样的序列就必须再加长。
const guard = (label, text) => {
  if (text.includes('"##')) throw new Error(`${label} 含有 "## ，需要更长的 raw 分隔符`);
  if (text.includes('\r')) throw new Error(`${label} 含有回车符，请先统一换行`);
};

let out = `//! 对应 \`src/prompts.mjs\`。\n//!\n//! 这些字符串**属于行为的一部分**，必须与 JS 逐字一致。本文件由\n//! \`rust/tools/gen-prompts.mjs\` 从 JS 生成，并由 \`tests/prompts_parity.rs\` 保证不漂移。\n//!\n//! 改动提示词时：先改 \`src/prompts.mjs\`，再运行 \`node rust/tools/gen-prompts.mjs\`，\n//! 最后跑 \`cargo test --test prompts_parity\`。\n\n`;

for (const [name, text] of items) {
  guard(name, text);
  out += `pub const ${name}: &str = r##"${text}"##;\n\n`;
}

out += `/// 与 JS 的 \`articulationFor\` 对应的回复语言。\n#[derive(Debug, Clone, Copy, PartialEq, Eq)]\npub enum ReplyLanguage {\n    Auto,\n    ZhCn,\n    En,\n}\n\n`;
out += `impl ReplyLanguage {\n    pub fn parse(value: &str) -> Option<Self> {\n        match value {\n            "auto" => Some(Self::Auto),\n            "zh-CN" => Some(Self::ZhCn),\n            "en" => Some(Self::En),\n            _ => None,\n        }\n    }\n}\n\n`;

for (const [name, key] of [['AUTO', 'auto'], ['ZH_CN', 'zh-CN'], ['EN', 'en']]) {
  // articulationFor 的返回是 `${articulation}\n${instruction}`，这里只取语言指令部分。
  const instruction = articulationFor(key).slice(articulation.length + 1);
  guard(name, instruction);
  out += `const INSTRUCTION_${name}: &str = r##"${instruction}"##;\n`;
}

out += `\n/// 复刻 \`articulationFor\`：非法语言在 JS 里抛错，这里返回错误。\npub fn articulation_for(language: &str) -> Result<String, &'static str> {\n    let instruction = match ReplyLanguage::parse(language) {\n        Some(ReplyLanguage::Auto) => INSTRUCTION_AUTO,\n        Some(ReplyLanguage::ZhCn) => INSTRUCTION_ZH_CN,\n        Some(ReplyLanguage::En) => INSTRUCTION_EN,\n        None => return Err("Invalid reply language"),\n    };\n    Ok(format!("{ARTICULATION}\\n{instruction}"))\n}\n`;

fs.writeFileSync(target, out);
console.log(`已写入 ${path.relative(root, target)}（${out.length} 字节）`);
