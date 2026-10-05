//! revision 的六组固化金标准已由真实 JS 实现生成，无需再次运行 Node。
//! 缺文件与空文件同哈希；非 ASCII 按 UTF-8；断言精确比较完整 SHA-256。
//! 无 Node 的零跳过验证方法见 golden/README.md，所有用例无条件执行。
use qq_inner_core::settings::revision;
use std::fs;
use std::path::{Path, PathBuf};

type RevisionCase<'a> = (&'a str, &'a [(&'a str, &'a str)], &'a str);
const CASES: &[RevisionCase<'_>] = &[
    (
        "both",
        &[
            ("config.json", "{\"a\":1}"),
            ("secrets.json", "{\"k\":\"v\"}"),
        ],
        "72140a075499bd712ed16fdba4c27bff81b55755508822414d6070f51d01ab04",
    ),
    (
        "config-only",
        &[("config.json", "{\"a\":1}")],
        "5262ceb474c2a60b289fb487a34645a71bac60339b3165738a93ad55587ceabd",
    ),
    (
        "secrets-only",
        &[("secrets.json", "{\"k\":\"v\"}")],
        "87a2bae92110433bda8318e9f8fde84dc57dc04c7a9f72b3a1564cb759795fbc",
    ),
    (
        // 两个空输入之间仅留下一个 0x00 分隔字节的 SHA-256。
        "neither",
        &[],
        "6e340b9cffb37a989ca544e6bb780a2c78901d3fb33738768511a30617afa01d",
    ),
    (
        "unicode",
        &[
            ("config.json", "{\"n\":\"中文\",\"e\":\"🙂\"}"),
            ("secrets.json", "{\"t\":\"令牌\"}"),
        ],
        "34ab02a42a3be460f01f949b4807d4a131e75bb39b50afc18940d97c8b6917d8",
    ),
    (
        "empty-files",
        &[("config.json", ""), ("secrets.json", "")],
        "6e340b9cffb37a989ca544e6bb780a2c78901d3fb33738768511a30617afa01d",
    ),
];

fn materialize(base: &Path, name: &str, files: &[(&str, &str)]) -> PathBuf {
    let dir = base.join(name);
    fs::create_dir_all(&dir).expect("create fixture directory");
    for (file, content) in files {
        fs::write(dir.join(file), content).expect("write fixture file");
    }
    dir
}

#[test]
fn revision_matches_the_javascript_golden_values() {
    let base = std::env::temp_dir().join("qq-inner-revision-golden");
    let _ = fs::remove_dir_all(&base);

    for (name, files, expected) in CASES {
        let dir = materialize(&base, name, files);
        let actual = revision(&dir).expect("revision should succeed");
        assert_eq!(
            &actual, expected,
            "fixture {name} diverged from the JS reference"
        );
    }

    let _ = fs::remove_dir_all(&base);
}

#[test]
fn a_missing_file_and_an_empty_file_are_indistinguishable() {
    let base = std::env::temp_dir().join("qq-inner-revision-empty");
    let _ = fs::remove_dir_all(&base);
    let missing = materialize(&base, "missing", &[]);
    let empty = materialize(&base, "empty", &[("config.json", ""), ("secrets.json", "")]);
    assert_eq!(
        revision(&missing).unwrap(),
        revision(&empty).unwrap(),
        "absent and empty files must hash the same, as the JS implementation does"
    );
    let _ = fs::remove_dir_all(&base);
}
