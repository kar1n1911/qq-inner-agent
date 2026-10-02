//! `sending.rs` 与 `src/sending.mjs` 的一致性测试。
//!
//! 硬编码的期望值是用**真实的 JS 实现**跑出来后记录在这里的；另外还有一个直接调用
//! Node 的交叉验证测试（Node 不可用时跳过），用来覆盖参数矩阵而不必逐个抄写。
use qq_inner_core::sending::{
    forecast_result, sending_probability, Forecast, Outcomes, ResponseMode, SendingSettings,
    Timing, Veto,
};
use serde_json::{json, Value};
use std::fs;
use std::process::Command;

fn settings() -> SendingSettings {
    SendingSettings {
        proactive_probability: 0.8,
        addressed_probability: 1.0,
        settle_seconds: 15.0,
        recovery_seconds: 300.0,
        burst_scale: 6.0,
        max_negative_probability: 0.4,
    }
}

fn timing(proactive: bool, age: f64, gap: f64, recent_humans: f64, score: f64) -> Timing {
    Timing {
        proactive,
        age,
        gap,
        recent_humans,
        score,
    }
}

fn forecast(should_send: bool, negative: f64, mode: ResponseMode) -> Forecast {
    Forecast {
        should_send,
        outcomes: Outcomes {
            reply: 0.6,
            silence: 0.3,
            negative,
        },
        response_mode: mode,
        plan: "答一句".into(),
    }
}

/// 期望值逐位取自 JS：`sendingProbability` 在同样输入下的输出。
#[test]
fn probability_matches_the_javascript_golden_values_bit_for_bit() {
    let cases = [
        (timing(true, 15.0, 300.0, 6.0, 5.0), 0.36000000000000004_f64),
        (timing(true, 0.0, 0.0, 0.0, 1.0), 0.0),
        (timing(true, 7.5, 150.0, 3.0, 3.0), 0.075),
        (timing(true, 900.0, 999.0, 99.0, 9.0), 0.04114285714285714),
        (timing(false, 1.0, 1.0, 1.0, 1.0), 1.0),
    ];
    for (t, expected) in cases {
        let got = sending_probability(&settings(), &t, &forecast(true, 0.1, ResponseMode::Answer));
        // 精确比较而不是 epsilon：因子相乘顺序一旦与 JS 不同，最后一位就会变。
        assert_eq!(
            got.probability.to_bits(),
            expected.to_bits(),
            "probability diverged for {t:?}: got {}",
            got.probability
        );
    }
}

#[test]
fn factors_match_the_javascript_golden_values() {
    let got = sending_probability(
        &settings(),
        &timing(true, 7.5, 150.0, 3.0, 3.0),
        &forecast(true, 0.1, ResponseMode::Answer),
    );
    assert_eq!(got.factors.base.to_bits(), 0.8_f64.to_bits());
    assert_eq!(got.factors.settle.to_bits(), 0.5_f64.to_bits());
    assert_eq!(got.factors.recovery.to_bits(), 0.5_f64.to_bits());
    assert_eq!(got.factors.pace.to_bits(), (2.0_f64 / 3.0).to_bits());
    assert_eq!(got.factors.motivation.to_bits(), 0.625_f64.to_bits());
    assert_eq!(got.factors.forecast.to_bits(), 0.9_f64.to_bits());
}

#[test]
fn a_non_proactive_turn_keeps_only_the_addressed_base() {
    let got = sending_probability(
        &settings(),
        &timing(false, 1.0, 1.0, 1.0, 1.0),
        &forecast(true, 0.1, ResponseMode::Answer),
    );
    assert_eq!(got.factors.base, 1.0);
    assert_eq!(got.factors.settle, 1.0);
    assert_eq!(got.factors.recovery, 1.0);
    assert_eq!(got.factors.pace, 1.0);
    assert_eq!(got.factors.motivation, 1.0);
    // The forecast factor is only neutralised for non-proactive turns; the veto is not.
    assert_eq!(got.factors.forecast, 1.0);
    assert_eq!(got.probability, 1.0);
}

#[test]
fn vetoes_apply_regardless_of_who_is_being_answered() {
    let withhold = forecast(false, 0.1, ResponseMode::Wait);
    let risky = forecast(true, 0.7, ResponseMode::Answer);
    for proactive in [true, false] {
        let t = timing(proactive, 15.0, 300.0, 6.0, 5.0);
        let a = sending_probability(&settings(), &t, &withhold);
        assert_eq!(a.veto, Some(Veto::ForecastWithhold));
        assert_eq!(a.probability, 0.0);
        let b = sending_probability(&settings(), &t, &risky);
        assert_eq!(b.veto, Some(Veto::ForecastRisk));
        assert_eq!(b.probability, 0.0);
    }
}

#[test]
fn forecast_result_normalises_and_drops_unknown_outcome_keys() {
    let value = json!({
        "shouldSend": true,
        "outcomes": { "reply": 0.6, "silence": 0.3, "negative": 0.1, "extra": 9 },
        "responseMode": "ask",
        "plan": "  空格  "
    });
    let got = forecast_result(&value).expect("valid forecast");
    assert_eq!(got.response_mode, ResponseMode::Ask);
    assert_eq!(got.plan, "空格");
    let rendered = serde_json::to_value(&got).unwrap();
    assert_eq!(
        rendered["outcomes"],
        json!({ "reply": 0.6, "silence": 0.3, "negative": 0.1 })
    );
    assert!(rendered["outcomes"].get("extra").is_none());
}

#[test]
fn invalid_forecasts_are_rejected_with_the_javascript_error_code() {
    let bad = [
        // shouldSend 不是布尔
        json!({"shouldSend": "yes", "outcomes": {"reply":0.6,"silence":0.3,"negative":0.1}, "responseMode":"answer", "plan":"x"}),
        // 三项之和偏离 1 超过 0.02
        json!({"shouldSend": true, "outcomes": {"reply":0.6,"silence":0.3,"negative":0.2}, "responseMode":"answer", "plan":"x"}),
        // 取值越界
        json!({"shouldSend": true, "outcomes": {"reply":1.2,"silence":-0.2,"negative":0.0}, "responseMode":"answer", "plan":"x"}),
        // wait 却要求发送
        json!({"shouldSend": true, "outcomes": {"reply":0.6,"silence":0.3,"negative":0.1}, "responseMode":"wait", "plan":"x"}),
        // plan 全空白
        json!({"shouldSend": true, "outcomes": {"reply":0.6,"silence":0.3,"negative":0.1}, "responseMode":"answer", "plan":"   "}),
        // plan 超过 400 个 UTF-16 码元
        json!({"shouldSend": true, "outcomes": {"reply":0.6,"silence":0.3,"negative":0.1}, "responseMode":"answer", "plan":"x".repeat(401)}),
        // 和恰好偏离 0.02 以上（0.7+0.3+0.1）
        json!({"shouldSend": true, "outcomes": {"reply":0.7,"silence":0.3,"negative":0.1}, "responseMode":"answer", "plan":"x"}),
        // 缺少 outcomes
        json!({"shouldSend": true, "responseMode": "answer", "plan": "x"}),
    ];
    for (index, value) in bad.iter().enumerate() {
        let err = forecast_result(value).expect_err(&format!("case {index} should be rejected"));
        assert_eq!(err.to_string(), "invalid_forecast");
    }
}

#[test]
fn the_two_percent_tolerance_boundary_is_inclusive() {
    // JS 判定为 abs(sum-1) > 0.02 才非法，因此恰好 0.02 仍然通过。
    let exactly = json!({"shouldSend": true, "outcomes": {"reply":0.5,"silence":0.32,"negative":0.20}, "responseMode":"answer", "plan":"x"});
    assert!((0.5 + 0.32 + 0.20 - 1.0f64).abs() > 0.02);
    assert!(forecast_result(&exactly).is_err(), "0.02 偏离应被拒绝");
    let inside = json!({"shouldSend": true, "outcomes": {"reply":0.5,"silence":0.31,"negative":0.18}, "responseMode":"answer", "plan":"x"});
    assert!((0.5 + 0.31 + 0.18 - 1.0f64).abs() < 0.02);
    assert!(forecast_result(&inside).is_ok(), "0.01 偏离应通过");
}

#[test]
fn a_plan_of_exactly_four_hundred_units_is_accepted() {
    let value = json!({
        "shouldSend": true,
        "outcomes": {"reply":0.7,"silence":0.2,"negative":0.1},
        "responseMode": "answer",
        "plan": "a".repeat(400)
    });
    assert!(forecast_result(&value).is_ok());
}

fn node_available() -> bool {
    Command::new("node")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

/// 直接把参数矩阵交给真实的 JS 实现，逐位比较概率与因子。
#[test]
fn probability_matches_node_across_a_parameter_matrix() {
    if !node_available() {
        eprintln!("SKIP: node unavailable");
        return;
    }
    let settings = settings();
    let mut cases: Vec<Value> = Vec::new();
    let mut expected_inputs: Vec<(Timing, Forecast)> = Vec::new();
    for &proactive in &[true, false] {
        for &age in &[0.0, 1.0, 7.5, 15.0, 900.0] {
            for &gap in &[0.0, 30.0, 300.0, 5000.0] {
                for &recent in &[0.0, 1.0, 6.0, 99.0] {
                    for &score in &[1.0, 2.5, 5.0] {
                        for &negative in &[0.0, 0.1, 0.45, 0.9] {
                            let t = timing(proactive, age, gap, recent, score);
                            let f = forecast(true, negative, ResponseMode::Answer);
                            expected_inputs.push((t, f.clone()));
                            cases.push(json!({
                                "settings": settings,
                                "timing": t,
                                "forecast": f,
                            }));
                        }
                    }
                }
            }
        }
    }

    // 传二进制位而不是十进制：serde_json 解析某些十进制浮点会差 1 ULP
    //（例如 "0.0012000000000000001"），那会让"逐位一致"的断言出现假失败。
    let script = r#"
import { sendingProbability } from './src/sending.mjs';
import { readFileSync } from 'node:fs';
const cases = JSON.parse(readFileSync(process.argv[1], 'utf8'));
const bits = x => { const b = new DataView(new ArrayBuffer(8)); b.setFloat64(0, x); return b.getBigUint64(0).toString(); };
process.stdout.write(JSON.stringify(cases.map(c => {
  const r = sendingProbability(c.settings, c.timing, c.forecast);
  return { p: bits(r.probability), f: Object.fromEntries(Object.entries(r.factors).map(([k, v]) => [k, bits(v)])) };
})));
"#;
    // 通过临时文件而不是 stdin 传递：矩阵有近两千个用例，管道会在 Node 启动前写满。
    let dir = std::env::temp_dir().join(format!("qq-inner-sending-{}", std::process::id()));
    fs::create_dir_all(&dir).expect("create case directory");
    let case_file = dir.join("cases.json");
    fs::write(&case_file, serde_json::to_string(&cases).unwrap()).expect("write cases");
    let out = Command::new("node")
        .args(["--input-type=module", "-e", script])
        .arg(&case_file)
        // 仓库根（rust/ 的上一级），这样 ./src/sending.mjs 才解析得到。
        .current_dir(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .unwrap(),
        )
        .output()
        .expect("run node");
    let _ = fs::remove_dir_all(&dir);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let js: Vec<Value> = serde_json::from_slice(&out.stdout).expect("node output");

    assert_eq!(js.len(), expected_inputs.len());
    for (index, ((t, f), expected)) in expected_inputs.iter().zip(js.iter()).enumerate() {
        let got = sending_probability(&settings, t, f);
        let js_probability: u64 = expected["p"].as_str().unwrap().parse().unwrap();
        assert_eq!(
            got.probability.to_bits(),
            js_probability,
            "case {index} ({t:?}, negative={}): rust={} js_bits={js_probability}",
            f.outcomes.negative,
            got.probability
        );
        for key in [
            "base",
            "settle",
            "recovery",
            "pace",
            "motivation",
            "forecast",
        ] {
            let js_factor: u64 = expected["f"][key].as_str().unwrap().parse().unwrap();
            let rust_factor = match key {
                "base" => got.factors.base,
                "settle" => got.factors.settle,
                "recovery" => got.factors.recovery,
                "pace" => got.factors.pace,
                "motivation" => got.factors.motivation,
                _ => got.factors.forecast,
            };
            assert_eq!(
                rust_factor.to_bits(),
                js_factor,
                "case {index} factor {key} diverged"
            );
        }
    }
}

/// 记录一个影响比对方式的事实：serde_json 解析十进制浮点时不保证正确舍入
///（实测 `"0.0012000000000000001"` 会比正确值少 1 ULP），所以任何"与 JS 逐位一致"
/// 的测试都应该传二进制位，而不是比较解析回来的十进制字面量。
///
/// 这里不对 serde_json 的行为做断言（它可能随版本修正），只钉住"正确答案是什么"。
#[test]
fn the_decimal_float_round_trip_is_why_the_matrix_transfers_bits() {
    let decimal = "0.0012000000000000001"; // JS `JSON.stringify` 对该 double 的输出
    let via_std: f64 = decimal.parse().unwrap();
    assert_eq!(
        via_std.to_bits(),
        4_563_176_846_121_054_818,
        "the correctly-rounded value of {decimal} must stay pinned"
    );
    let via_serde: f64 = serde_json::from_str(decimal).unwrap();
    assert!(
        (via_serde - via_std).abs() <= f64::EPSILON * via_std.abs(),
        "serde_json must at least stay within one ULP"
    );
}
