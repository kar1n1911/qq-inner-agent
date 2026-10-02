//! 稀疏召回；调用方必须先限制 chat/subject，排序不扩大隔离边界。
use crate::config::Memory;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
#[path = "memory_unicode.rs"]
mod unicode;
pub type Token = Vec<u16>;
fn contains(ranges: &[(u32, u32)], c: char) -> bool {
    ranges
        .binary_search_by(|&(a, b)| {
            if (c as u32) < a {
                std::cmp::Ordering::Greater
            } else if (c as u32) > b {
                std::cmp::Ordering::Less
            } else {
                std::cmp::Ordering::Equal
            }
        })
        .is_ok()
}
/// 保留 UTF-16 码元 token，连扩展汉字切出的孤立代理项也能无损比较。
/// 属性表精确匹配 JS 的 L/N/Han，不能用 is_alphanumeric 的 Alphabetic 近似。
pub fn tokens(text: &str) -> Vec<Token> {
    let lower = text.to_lowercase();
    let mut out = Vec::new();
    for word in lower
        .split(|c| !contains(unicode::LETTER_NUMBER, c))
        .filter(|s| !s.is_empty())
    {
        let units: Vec<u16> = word.encode_utf16().collect();
        if word.chars().any(|c| contains(unicode::HAN, c)) {
            out.extend(units.windows(2).map(|s| s.to_vec()));
        } else if units.len() > 1 {
            out.push(units);
        }
    }
    out
}
fn n(v: &Value, key: &str, default: f64) -> f64 {
    v[key].as_f64().unwrap_or(default)
}
fn s<'a>(v: &'a Value, key: &str) -> &'a str {
    v[key].as_str().unwrap_or("")
}
#[derive(Default, Debug)]
pub struct RankingStats {
    pub tokenizations: usize,
    pub overlaps: usize,
}
pub fn rank_memories(
    rows: &[Value],
    query: &str,
    now: f64,
    settings: &Memory,
    require_match: bool,
) -> Vec<Value> {
    rank_with_stats(rows, query, now, settings, require_match).0
}
pub fn rank_with_stats(
    rows: &[Value],
    query: &str,
    now: f64,
    settings: &Memory,
    require_match: bool,
) -> (Vec<Value>, RankingStats) {
    let mut stats = RankingStats::default();
    let mut tokenize = |text: &str| {
        stats.tokenizations += 1;
        tokens(text)
    };
    let mut seen = HashSet::new();
    let q: Vec<_> = tokenize(query)
        .into_iter()
        .filter(|t| seen.insert(t.clone()))
        .collect();
    let docs: Vec<_> = rows
        .iter()
        .map(|r| {
            let keywords = r["keywords"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(" ")
                })
                .unwrap_or_default();
            let terms = tokenize(&format!("{} {} {}", s(r, "slot"), keywords, s(r, "text")));
            let length = terms.len();
            let mut counts = HashMap::new();
            for t in terms {
                *counts.entry(t).or_insert(0usize) += 1;
            }
            let overlap: HashSet<_> = tokenize(s(r, "text")).into_iter().collect();
            (counts, length, overlap)
        })
        .collect();
    let average = (docs.iter().map(|d| d.1).sum::<usize>() as f64 / docs.len().max(1) as f64)
        .max(f64::MIN_POSITIVE);
    let df: Vec<_> = q
        .iter()
        .map(|t| docs.iter().filter(|d| d.0.contains_key(t)).count() as f64)
        .collect();
    let mut scored = Vec::new();
    let mut sets = Vec::new();
    for (row, (counts, length, set)) in rows.iter().zip(docs.iter()) {
        let lexical = q
            .iter()
            .zip(&df)
            .map(|(t, df)| {
                let tf = *counts.get(t).unwrap_or(&0) as f64;
                (1. + (docs.len() as f64 - df + 0.5) / (df + 0.5)).ln() * tf * 2.2
                    / (tf + 1.2 * (0.25 + 0.75 * *length as f64 / average))
            })
            .sum::<f64>();
        if s(row, "layer") != "owner_note"
            && (n(row, "confidence", 0.6) < settings.min_confidence
                || (require_match && lexical <= 0.))
        {
            continue;
        }
        let mut r = row.clone();
        r["recall"] = json!({"lexical":lexical,"recency":2f64.powf(-(now-n(row,"updated",0.)).max(0.)/(settings.recall_half_life_days*86400.)),"confidence":n(row,"confidence",0.6),"importance":n(row,"importance",0.5)});
        r["saliency"] = json!(0.);
        scored.push(r);
        sets.push(set);
    }
    for (signal, weight) in [
        ("lexical", 3.),
        ("recency", 1.),
        ("importance", 1.),
        ("confidence", 1.),
    ] {
        let mut order: Vec<usize> = (0..scored.len()).collect();
        order.sort_by(|&a, &b| {
            n(&scored[b]["recall"], signal, 0.)
                .total_cmp(&n(&scored[a]["recall"], signal, 0.))
                .then_with(|| s(&scored[a], "id").cmp(s(&scored[b], "id")))
        });
        let mut rank = 1;
        for (i, &index) in order.iter().enumerate() {
            if i > 0 && scored[index]["recall"][signal] != scored[order[i - 1]]["recall"][signal] {
                rank = i + 1;
            }
            scored[index]["saliency"] =
                json!(n(&scored[index], "saliency", 0.) + weight / (20. + rank as f64));
        }
    }
    // 每候选保存最大重叠度；选中一项仅更新一次，贪心循环完全不分词。
    let mut penalty = vec![0f64; scored.len()];
    let mut pending: Vec<_> = (0..scored.len()).collect();
    let mut selected: Vec<usize> = Vec::new();
    while !pending.is_empty() {
        let score = |i: usize| {
            n(&scored[i], "saliency", 0.)
                + if s(&scored[i], "layer") == "owner_note" {
                    0.04
                } else {
                    0.
                }
                - 0.08 * penalty[i]
        };
        pending.sort_by(|&a, &b| {
            score(b)
                .total_cmp(&score(a))
                .then_with(|| s(&scored[a], "id").cmp(s(&scored[b], "id")))
        });
        let next = pending.remove(0);
        if selected.iter().any(|&i| {
            ["subject", "layer", "text"]
                .iter()
                .all(|k| scored[i][k] == scored[next][k])
        }) {
            continue;
        }
        selected.push(next);
        for &i in &pending {
            if scored[i]["subject"] == scored[next]["subject"] {
                stats.overlaps += 1;
                let (x, y) = (sets[i], sets[next]);
                if !x.is_empty() && !y.is_empty() {
                    penalty[i] = penalty[i].max(
                        x.intersection(y).count() as f64 / ((x.len() * y.len()) as f64).sqrt(),
                    );
                }
            }
        }
    }
    (
        selected.into_iter().map(|i| scored[i].clone()).collect(),
        stats,
    )
}
