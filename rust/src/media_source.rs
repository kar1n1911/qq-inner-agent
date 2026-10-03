//! 来源可公开性：所有证据在本地处理，严禁上传素材或联网反向图搜。
use crate::settings::sha256;
use serde::{Deserialize, Serialize};
use std::{collections::HashSet, fs, io::Read, path::Path};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceTier {
    Public,
    Private,
    #[default]
    Unknown,
}
impl SourceTier {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Public => "public",
            Self::Private => "private",
            Self::Unknown => "unknown",
        }
    }
}
/// 人工只能明确指定 public/private；None 表示清除覆盖，回到本地证据。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Override {
    Public,
    Private,
}
impl Override {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Public => "public",
            Self::Private => "private",
        }
    }
}
#[derive(Debug, Clone, Copy, Default)]
pub struct Evidence {
    pub distinct_senders: u64,
    pub public_corpus_match: bool,
    pub manual_override: Option<Override>,
}
#[derive(Debug, PartialEq, Eq)]
pub struct Assessment {
    pub tier: SourceTier,
    /// 传播度只表示群内通用程度；群友的脸也可能被全群使用，不能据此放行。
    pub widespread: bool,
}
/// 纯函数；未验证的 sub_type/sticker/emoji 标志不属于“正面确认公开”的证据。
pub fn classify(e: Evidence, widespread_threshold: u64) -> Assessment {
    let tier = match e.manual_override {
        Some(Override::Private) => SourceTier::Private,
        Some(Override::Public) => SourceTier::Public,
        None if e.public_corpus_match => SourceTier::Public,
        None => SourceTier::Unknown,
    };
    Assessment {
        tier,
        widespread: e.distinct_senders >= widespread_threshold.max(2),
    }
}
/// 纯准入判定，尚不实现跨群检索/共享。unknown 与 private 一样禁止出群。
/// 私聊永不参与群间共享；群内使用不受来源档位限制。
pub fn can_use(tier: SourceTier, source_chat: &str, target_chat: &str) -> bool {
    let group = |chat: &str| {
        chat.strip_prefix("group:")
            .is_some_and(|id| !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit()))
    };
    source_chat == target_chat
        || (tier == SourceTier::Public && group(source_chat) && group(target_chat))
}
/// 运维明确标记为公开的本地语料目录；递归散列普通文件，不跟随符号链接。
/// 缺失目录视为空，不下载语料，不相信文件名中的哈希。读取失败由调用方保守降为空。
pub fn corpus_hashes(directory: &Path, max_file_bytes: u64) -> std::io::Result<HashSet<String>> {
    let mut hashes = HashSet::new();
    let mut pending = vec![directory.to_owned()];
    while let Some(path) = pending.pop() {
        let meta = match fs::symlink_metadata(&path) {
            Ok(meta) => meta,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e),
        };
        if meta.is_symlink() {
            continue;
        }
        if meta.is_dir() {
            for entry in fs::read_dir(path)? {
                pending.push(entry?.path());
            }
        } else if meta.is_file() && meta.len() > 0 && meta.len() <= max_file_bytes {
            let mut data = Vec::new();
            fs::File::open(path)?
                .take(max_file_bytes.saturating_add(1))
                .read_to_end(&mut data)?;
            if !data.is_empty() && data.len() as u64 <= max_file_bytes {
                hashes.insert(sha256(&data));
            }
        }
    }
    Ok(hashes)
}
