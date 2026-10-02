//! `activity::activity_probability` 与 `src/activity.mjs` 的一致性测试。
//!
//! **这里不能用"逐位相等"**:曲线含 `exp()`，而 V8 自带 fdlibm 移植、Rust 走系统 libm，
//! 同一个 `exp` 输入的结果可以差若干 ULP（实测约 8 ULP，量级 1e-17）。纯 `+ - * /` 的
//! 计算可以逐位对齐（见 `sending_parity.rs`），超越函数不行。因此这里用 1e-12 的绝对容差 ——
//! 比任何行为上有意义的差异都小好几个数量级。
use qq_inner_core::activity::activity_probability;
use qq_inner_core::config::{Rhythm, Schedule};
use serde_json::{json, Value};
use std::fs;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};

static SEQ: AtomicUsize = AtomicUsize::new(0);

fn node_available() -> bool {
    Command::new("node").arg("--version").output().is_ok_and(|o| o.status.success())
}

fn schedule(enabled: bool, active_start: &str, inactive_start: &str, timezone: &str) -> Schedule {
    Schedule {
        enabled,
        active_start: active_start.into(),
        inactive_start: inactive_start.into(),
        timezone: timezone.into(),
    }
}

fn rhythm(sigma: f64, day: f64, edge: f64, center: f64) -> Rhythm {
    Rhythm {
        enabled: true,
        day_probability: day,
        edge_probability: edge,
        center_probability: center,
        sigma,
        active_min_seconds: 300.0,
        active_max_seconds: 1200.0,
        rest_min_seconds: 600.0,
        rest_max_seconds: 2400.0,
    }
}

/// 覆盖跨午夜与不跨午夜两种窗口、三种时区、以及若干时刻。
fn cases() -> Vec<(f64, Schedule, Rhythm)> {
    let schedules = [
        schedule(true, "08:00", "23:00", "Europe/Stockholm"),
        schedule(true, "22:00", "06:00", "Asia/Shanghai"),
        schedule(true, "08:30", "22:15", "+08:00"),
        schedule(true, "00:00", "23:59", "UTC"),
        schedule(false, "08:00", "23:00", "Europe/Stockholm"),
    ];
    let rhythms = [rhythm(0.22, 0.85, 0.65, 0.02), rhythm(0.05, 0.9, 0.5, 0.1), rhythm(1.0, 0.8, 0.7, 0.0)];
    // 2026 年若干个时刻，含两次欧洲夏令时切换的前后。
    let instants = [
        1_767_225_600.0, // 2026-01-01T00:00:00Z
        1_772_000_000.0,
        1_774_747_800.0, // 2026-03-29T01:30:00Z
        1_781_524_800.0, // 2026-06-15T12:00:00Z
        1_785_000_000.0,
        1_792_891_800.0, // 2026-10-25T01:30:00Z
        1_798_761_600.0, // 2026-12-31T00:00:00Z
        1_767_225_600.5, // 带小数秒，验证秒级精度确实参与定位
    ];
    let mut out = Vec::new();
    for s in &schedules {
        for r in &rhythms {
            for now in instants {
                out.push((now, s.clone(), r.clone()));
            }
        }
    }
    out
}

#[test]
fn activity_probability_matches_javascript_within_a_tiny_tolerance() {
    if !node_available() {
        eprintln!("SKIP: node unavailable");
        return;
    }
    let cases = cases();
    let payload: Vec<Value> = cases
        .iter()
        .map(|(now, schedule, rhythm)| json!({ "now": now, "schedule": schedule, "rhythm": rhythm }))
        .collect();

    let script = r#"
import { activityProbability } from './src/activity.mjs';
import { readFileSync } from 'node:fs';
const cases = JSON.parse(readFileSync(process.argv[1], 'utf8'));
process.stdout.write(JSON.stringify(cases.map(c => activityProbability(c.now, c.schedule, c.rhythm))));
"#;
    let dir = std::env::temp_dir().join(format!(
        "qq-inner-activity-{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, AtomicOrdering::Relaxed)
    ));
    fs::create_dir_all(&dir).expect("create temp dir");
    let payload_file = dir.join("cases.json");
    fs::write(&payload_file, serde_json::to_string(&payload).unwrap()).unwrap();
    let out = Command::new("node")
        .args(["--input-type=module", "-e", script])
        .arg(&payload_file)
        .current_dir(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap())
        .output()
        .expect("run node");
    let _ = fs::remove_dir_all(&dir);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let js: Vec<Value> = serde_json::from_slice(&out.stdout).expect("node output");
    assert_eq!(js.len(), cases.len());

    for (index, (now, schedule, rhythm)) in cases.iter().enumerate() {
        let expected = js[index].as_f64().expect("js number");
        let got = activity_probability(*now, schedule, rhythm);
        assert!(
            (got - expected).abs() <= 1e-12,
            "case {index}: now={now} tz={} schedule={}..{} sigma={} -> rust {got}, js {expected}",
            schedule.timezone,
            schedule.active_start,
            schedule.inactive_start,
            rhythm.sigma
        );
    }
}

#[test]
fn a_disabled_schedule_always_returns_the_day_probability() {
    let s = schedule(false, "08:00", "23:00", "Europe/Stockholm");
    let r = rhythm(0.22, 0.85, 0.65, 0.02);
    for now in [1_767_225_600.0, 1_781_524_800.0, 1_798_761_600.0] {
        assert_eq!(activity_probability(now, &s, &r), r.day_probability);
    }
}

#[test]
fn the_middle_of_the_rest_window_reaches_the_center_probability() {
    // 时间表 08:00–23:00，静默窗口是 23:00–08:00，共 540 分钟，正中是本地 03:30。
    let s = schedule(true, "08:00", "23:00", "UTC");
    let r = rhythm(0.22, 0.85, 0.65, 0.02);
    let middle = activity_probability(1_781_494_200.0, &s, &r); // 2026-06-15T03:30:00Z
    assert!(
        (middle - r.center_probability).abs() < 1e-12,
        "the midpoint should hit centerProbability exactly, got {middle}"
    );
    // 靠近窗口边缘时 gaussian 趋近 edge，dip 趋近 0，结果趋近 edgeProbability。
    let near_edge = activity_probability(1_781_494_200.0 - 265.0 * 60.0, &s, &r); // 约 23:05
    assert!(near_edge > middle, "the edges must be more active than the middle");
    assert!(near_edge <= r.edge_probability + 1e-12);
    // 活跃窗口内直接返回白天的基准概率。
    let active = activity_probability(1_781_524_800.0, &s, &r); // 12:00 UTC 在 08:00–23:00 内
    assert_eq!(active, r.day_probability);
}
