//! 文本工具：对应 `src/store.mjs` 的 `terms()` 与 `similarity()`。
//!
//! JS 用的是 Unicode 属性正则：
//! ```text
//! /[\p{L}\p{N}]{2,}/gu     长度 ≥2 的字母/数字连续段
//! /[\p{Script=Han}]+/gu    汉字连续段，再取逐字二元组
//! ```
//!
//! Rust 标准库没有 Unicode Script 属性，也不带正则引擎。为了不引入新依赖（部署主机上没有
//! cmake/make/pkg-config，而且本项目约定不增加依赖），这里：
//!
//! * `[\p{L}\p{N}]` 用 `char::is_alphanumeric()`（≈ `\p{Alphabetic} ∪ \p{N}`）；
//! * `\p{Script=Han}` 用下面的显式码点区间表。
//!
//! 两处都可能与 ICU 有细微出入，因此 `tests/text_parity.rs` 会把一批"刁钻"语料同时交给
//! 真实 JS 与这里实现，逐一比对。**改动这里必须让该测试保持通过。**
//!
//! ## 已知且**有意**的差异：BMP 以外的汉字
//!
//! JS 的二元组用 `run.slice(i, i + 2)` 切分，按 **UTF-16 码元**计数。扩展 B 区及以上的汉字
//! 是代理对（2 个码元），`slice(i, i + 2)` 会切出**半个字符**（孤立代理项），产生的 key 是
//! 非法字符串。Rust 这里按 `char`（标量值）切分，得到的是合法的双字词。
//!
//! 也就是说这不是"移植误差"，而是 **JS 侧的缺陷**；对正常使用（常用汉字都在 BMP 内）没有
//! 影响。刻意不模仿该缺陷——模仿它反而会在 Rust 里制造非法字符串。
use std::collections::HashSet;

/// Unicode `Script=Han` 的主要码点区间（CJK 统一表意文字及其扩展、兼容表意文字、部首等）。
fn is_han(c: char) -> bool {
    matches!(c as u32,
        0x2E80..=0x2EFF   // CJK Radicals Supplement
        | 0x2F00..=0x2FDF // Kangxi Radicals
        | 0x3005          // 々  ideographic iteration mark
        | 0x3007          // 〇  ideographic number zero
        | 0x3021..=0x3029 // Hangzhou numerals
        | 0x3038..=0x303B // CJK strokes
        | 0x3400..=0x4DBF // Extension A
        | 0x4E00..=0x9FFF // Unified Ideographs
        | 0xF900..=0xFAFF // Compatibility Ideographs
        | 0x20000..=0x2A6DF // Extension B
        | 0x2A700..=0x2EBEF // Extensions C–F
        | 0x2F800..=0x2FA1F // Compatibility Ideographs Supplement
        | 0x30000..=0x3134F // Extension G
        | 0x31350..=0x323AF // Extension H
    )
}

/// 复刻 `terms()`：小写化后取长度 ≥2 的字母/数字连续段，再补上所有汉字二元组。
pub fn terms(text: &str) -> HashSet<String> {
    let lowered = text.to_lowercase();
    let mut out = HashSet::new();

    // 第一趟：`[\p{L}\p{N}]{2,}`。注意该正则的匹配段可以混合汉字与拉丁字母
    //（例如 "abc中文" 是一整段），这里的行为一致。
    let mut run = String::new();
    let flush = |run: &mut String, out: &mut HashSet<String>| {
        if run.chars().count() >= 2 {
            out.insert(std::mem::take(run));
        } else {
            run.clear();
        }
    };
    for c in lowered.chars() {
        if c.is_alphanumeric() {
            run.push(c);
        } else {
            flush(&mut run, &mut out);
        }
    }
    flush(&mut run, &mut out);

    // 第二趟：`[\p{Script=Han}]+`，对每个汉字连续段取相邻两字。
    let chars: Vec<char> = lowered.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if !is_han(chars[i]) {
            i += 1;
            continue;
        }
        let start = i;
        while i < chars.len() && is_han(chars[i]) {
            i += 1;
        }
        for j in start..i.saturating_sub(1) {
            out.insert(chars[j..j + 2].iter().collect());
        }
    }

    out
}

/// 复刻 `similarity()`：交集大小除以两者大小的几何平均。
pub fn similarity(a: &str, b: &str) -> f64 {
    let (x, y) = (terms(a), terms(b));
    if x.is_empty() || y.is_empty() {
        return 0.0;
    }
    let shared = x.iter().filter(|t| y.contains(*t)).count();
    shared as f64 / ((x.len() * y.len()) as f64).sqrt()
}
