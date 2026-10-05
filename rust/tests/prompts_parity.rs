//! `prompts.rs` 与 `src/prompts.mjs` 的逐字一致性测试。
//!
//! 提示词是**行为的一部分**:改一个字就可能改变模型输出。因此 Rust 侧的常量由脚本从 JS
//! 生成,并由这里的测试保证它不会悄悄漂移。Node 不可用时跳过。
use qq_inner_core::prompts::{
    articulation_for, articulation_for_with_rules, compose_prompt, ReplyLanguage, ARTICULATION,
    BOUNDARY, EVALUATION, FORECAST, FORMATION, IDENTITY, ORIENTATION, OUTPUT_CONTRACT, RULES,
};
use serde_json::Value;

#[test]
fn every_prompt_constant_matches_the_frozen_javascript_source() {
    // 期望值固化自 2026-10-05 的 JS 源(src/prompts.mjs + orientation.mjs),不再依赖 node。
    let js: Value =
        serde_json::from_str(include_str!("golden/prompts.json")).expect("frozen golden");

    let text = |key: &str| js[key].as_str().expect("string field");
    assert_eq!(
        qq_inner_core::prompts::LEARNING_REVIEW,
        text("learningReview")
    );
    assert_eq!(IDENTITY, text("identity"));
    assert_eq!(OUTPUT_CONTRACT, text("outputContract"));
    for (name, rule) in RULES {
        assert_eq!(*rule, js["rules"][name].as_str().unwrap());
    }
    let mut selections = vec![vec![]];
    selections.extend(RULES.iter().map(|(name, _)| vec![*name]));
    selections.push(RULES.iter().map(|(name, _)| *name).collect());
    for (task, contract) in [FORMATION, EVALUATION, ARTICULATION, FORECAST]
        .iter()
        .enumerate()
    {
        for (index, disabled) in selections.iter().enumerate() {
            assert_eq!(
                compose_prompt(contract, disabled),
                js["composed"][task][index].as_str().unwrap()
            );
        }
    }
    assert_eq!(BOUNDARY, text("boundary"));
    assert_eq!(FORMATION, text("formation"));
    assert_eq!(EVALUATION, text("evaluation"));
    assert_eq!(ARTICULATION, text("articulation"));
    assert_eq!(FORECAST, text("forecast"));
    assert_eq!(ORIENTATION, text("orientation"));
    assert_eq!(
        articulation_for_with_rules("en", &["language"]).unwrap(),
        text("disabledLanguage")
    );
    assert_eq!(
        articulation_for_with_rules("en", &["antiAi", "decorations"]).unwrap(),
        text("disabledReplyRules")
    );
    for language in ["auto", "zh-CN", "en"] {
        let expected = js["variants"][language].as_str().expect("variant");
        assert_eq!(
            articulation_for(language).as_deref(),
            Ok(expected),
            "articulation_for({language}) diverged"
        );
    }
}

#[test]
fn an_unknown_reply_language_is_rejected_like_javascript() {
    assert_eq!(articulation_for("fr"), Err("Invalid reply language"));
    assert_eq!(articulation_for(""), Err("Invalid reply language"));
    assert_eq!(ReplyLanguage::parse("fr"), None);
    assert_eq!(ReplyLanguage::parse("zh-CN"), Some(ReplyLanguage::ZhCn));
}

/// 模型输出的 JSON 契约:字段名与任务标识变了,解析就会整批失败。
#[test]
fn the_model_output_contract_is_still_stated() {
    let all = [
        OUTPUT_CONTRACT,
        FORMATION,
        EVALUATION,
        ARTICULATION,
        FORECAST,
    ];
    for clue in [
        "TASK: FORM",
        "TASK: EVALUATE",
        "TASK: ARTICULATE",
        "TASK: FORECAST",
        "\"allocation\":\"self|other|open\"",
        "\"emoji\":null,\"faceId\":null",
        "\"motivation\":1.0",
        "\"shouldSend\":true",
        "只输出指定的 JSON 对象",
    ] {
        assert!(
            all.iter().any(|prompt| prompt.contains(clue)),
            "the prompt set should still state: {clue}"
        );
    }
}

/// 更像真人的约束不能在移植过程中丢失。
#[test]
fn the_human_like_constraints_survive() {
    for clue in [
        "lengthTarget",
        "tiny",
        "优先 face",
        "只在情绪节拍上使用",
        "不同看法",
    ] {
        let present = [
            compose_prompt(FORMATION, &[]),
            compose_prompt(ARTICULATION, &[]),
        ]
        .iter()
        .any(|prompt| prompt.contains(clue));
        assert!(present, "the prompt set should still state: {clue}");
    }
    // 引擎还不支持"只发表情",提示词里就不能出现这个措辞。
    assert!(
        !ARTICULATION.contains("空文本"),
        "prompt must not invite an empty-text reply yet"
    );
}

#[test]
fn pre_refactor_json_examples_are_unchanged() {
    let golden: Value =
        serde_json::from_str(include_str!("../../test/fixtures/prompt-contracts.json")).unwrap();
    for (name, contract) in [
        ("formation", FORMATION),
        ("evaluation", EVALUATION),
        ("articulation", ARTICULATION),
        ("forecast", FORECAST),
    ] {
        for example in golden[name].as_array().unwrap() {
            assert!(contract.contains(example.as_str().unwrap()));
        }
        assert!(!contract.contains("persona"));
    }
}

#[test]
fn responsibility_rule_keeps_the_completed_task_two_boundary() {
    let rule = qq_inner_core::prompts::RESPONSIBILITY;
    for clue in [
        "涉及对方决策或利益",
        "第三方的具体言行",
        "无害的日常描写或情绪状态",
        "不主动冒充真人",
    ] {
        assert!(rule.contains(clue));
    }
}
