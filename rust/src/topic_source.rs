//! 外部话题：独立网络预算，短期共享来源缓存，群级滑动窗口限额。
use anyhow::{ensure, Result};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    io::Read,
    time::Duration,
};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct Settings {
    #[serde(default)]
    pub enabled: bool,
    pub github: Vec<String>,
    /// RSS/论坛须提供 RSS 订阅地址，不解析任意 HTML 页面。
    pub feeds: Vec<String>,
    pub interval_hours: f64,
    pub max_per_hour: usize,
    pub max_requests: usize,
    pub max_items: usize,
    pub max_chars: usize,
    pub max_total_chars: usize,
    pub cache_hours: f64,
    pub threshold: f64,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            enabled: false,
            github: vec![],
            feeds: vec![],
            interval_hours: 1.,
            max_per_hour: 1,
            max_requests: 4,
            max_items: 3,
            max_chars: 600,
            max_total_chars: 1800,
            cache_hours: 1.,
            threshold: 0.15,
        }
    }
}
impl Settings {
    pub fn enabled(&self) -> bool {
        self.enabled && (!self.github.is_empty() || !self.feeds.is_empty())
    }
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.interval_hours.is_finite()
                && self.interval_hours > 0.
                && self.cache_hours.is_finite()
                && self.cache_hours > 0.,
            "invalid topicSource periods"
        );
        ensure!(
            self.threshold.is_finite() && self.threshold > 0. && self.threshold <= 1.,
            "invalid topicSource threshold"
        );
        ensure!(
            [
                self.max_per_hour,
                self.max_requests,
                self.max_items,
                self.max_chars,
                self.max_total_chars
            ]
            .iter()
            .all(|n| *n > 0 && *n <= 100_000),
            "invalid topicSource budget"
        );
        ensure!(
            self.github.len() + self.feeds.len() <= 100
                && self
                    .github
                    .iter()
                    .all(|s| !s.trim().is_empty() && s.len() <= 200)
                && self.feeds.iter().all(|s| valid_url(s)),
            "invalid topicSource sources"
        );
        Ok(())
    }
}
fn valid_url(s: &str) -> bool {
    s.starts_with("http://") || s.starts_with("https://")
}
// 查询参数逐字节百分号编码，topic: 与空格不能改变 URL 的结构。
fn query_encode(s: &str) -> String {
    s.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Item {
    pub title: String,
    pub description: String,
    pub tags: Vec<String>,
    pub url: String,
}
#[derive(Clone, Debug, Serialize)]
pub struct Match {
    pub item: Item,
    pub source: String,
    pub score: f64,
    pub interests: Vec<String>,
}
fn tokens(s: &str) -> BTreeSet<String> {
    let normalized: String = s
        .chars()
        .map(|c| match c {
            '\u{FF01}'..='\u{FF5E}' => char::from_u32(c as u32 - 0xFEE0).unwrap(),
            _ => c,
        })
        .collect();
    crate::memory::text::terms(&normalized)
        .into_iter()
        .filter(|s| {
            ![
                "the", "and", "this", "that", "https", "http", "com", "是的", "这个",
            ]
            .contains(&s.as_str())
        })
        .collect()
}
pub fn interests(traits: &str, messages: &[String]) -> BTreeSet<String> {
    let mut out = tokens(traits);
    let mut counts = BTreeMap::new();
    for m in messages {
        for t in tokens(m) {
            *counts.entry(t).or_insert(0usize) += 1;
        }
    }
    let mut counts: Vec<_> = counts.into_iter().filter(|(_, n)| *n >= 2).collect();
    counts.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    out.extend(counts.into_iter().take(32).map(|(t, _)| t));
    out
}
pub fn relevance(item: &Item, interests: &BTreeSet<String>) -> (f64, Vec<String>) {
    let terms = tokens(&format!(
        "{} {} {}",
        item.title,
        item.description,
        item.tags.join(" ")
    ));
    let hits: Vec<_> = terms.intersection(interests).cloned().collect();
    let score = hits.len() as f64 / ((terms.len().max(1) * interests.len().max(1)) as f64).sqrt();
    (score, hits)
}
#[derive(Default)]
pub struct Sources {
    cache: HashMap<String, (f64, Vec<Item>)>,
    attempts: HashMap<String, Vec<f64>>,
}
impl Sources {
    pub fn collect(
        &mut self,
        cfg: &Settings,
        chat: &str,
        now: f64,
        interests: &BTreeSet<String>,
        mut fetch: impl FnMut(&str, usize) -> Result<Vec<Item>>,
    ) -> Vec<Match> {
        if !cfg.enabled() || interests.is_empty() || !now.is_finite() || cfg.validate().is_err() {
            return vec![];
        }
        let times = self.attempts.entry(chat.into()).or_default();
        if times
            .last()
            .is_some_and(|t| now < *t || now - *t < cfg.interval_hours * 3600.)
        {
            return vec![];
        }
        times.retain(|t| now - *t < 3600.);
        if times.len() >= cfg.max_per_hour {
            return vec![];
        }
        times.push(now);
        self.cache
            .retain(|_, (t, _)| now >= *t && now - *t < cfg.cache_hours * 3600.);
        let urls = cfg
            .github
            .iter()
            .map(|q| {
                format!("https://api.github.com/search/repositories?q={}&sort=updated&order=desc&per_page={}", query_encode(q), cfg.max_items.min(100))
            })
            .chain(cfg.feeds.iter().cloned())
            .collect::<BTreeSet<_>>();
        let mut requests = 0;
        let mut remaining = cfg.max_total_chars;
        let mut out = vec![];
        let mut seen = BTreeSet::new();
        for source in urls {
            if !self.cache.contains_key(&source) {
                if requests >= cfg.max_requests {
                    continue;
                }
                requests += 1;
                // 失败也缓存，避免坏源在多个群之间被反复重试。
                let items = fetch(&source, cfg.max_items).unwrap_or_default();
                self.cache.insert(source.clone(), (now, items));
            }
            for item in self.cache[&source].1.iter().take(cfg.max_items) {
                let Some(item) = bounded(item, cfg.max_chars.min(remaining)) else {
                    continue;
                };
                let size = item.title.chars().count()
                    + item.description.chars().count()
                    + item.tags.iter().map(|s| s.chars().count()).sum::<usize>()
                    + item.url.chars().count();
                remaining -= size;
                let (score, hits) = relevance(&item, interests);
                if score >= cfg.threshold && !hits.is_empty() && seen.insert(item.url.clone()) {
                    out.push(Match {
                        item,
                        source: source.clone(),
                        score,
                        interests: hits,
                    });
                    if out.len() >= cfg.max_items {
                        return out;
                    }
                }
                if remaining == 0 {
                    return out;
                }
            }
        }
        out
    }
}
fn bounded(item: &Item, cap: usize) -> Option<Item> {
    if !valid_url(&item.url) || item.url.chars().count() >= cap {
        return None;
    }
    let mut left = cap - item.url.chars().count();
    let mut cut = |s: &str| {
        let out: String = s.chars().take(left).collect();
        left -= out.chars().count();
        out
    };
    Some(Item {
        title: cut(&item.title),
        description: cut(&item.description),
        tags: item
            .tags
            .iter()
            .map(|s| cut(s))
            .filter(|s| !s.is_empty())
            .collect(),
        url: item.url.clone(),
    })
}
pub fn fetch(url: &str, limit: usize) -> Result<Vec<Item>> {
    let response = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(10))
        .build()
        .get(url)
        .set("User-Agent", "qq-inner-agent-topic-source")
        .set(
            "Accept",
            "application/vnd.github+json, application/rss+xml, application/xml",
        )
        .call()?;
    let mut body = String::new();
    response
        .into_reader()
        .take(262_145)
        .read_to_string(&mut body)?;
    ensure!(body.len() <= 262_144, "topic source body too large");
    parse(&body, limit)
}
pub fn parse(body: &str, limit: usize) -> Result<Vec<Item>> {
    if body.trim_start().starts_with('{') {
        let v: serde_json::Value = serde_json::from_str(body)?;
        return Ok(v["items"]
            .as_array()
            .into_iter()
            .flatten()
            .take(limit)
            .map(|v| Item {
                title: v["full_name"].as_str().unwrap_or_default().into(),
                description: v["description"].as_str().unwrap_or_default().into(),
                tags: v["topics"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|t| t.as_str().map(str::to_owned))
                    .collect(),
                url: v["html_url"].as_str().unwrap_or_default().into(),
            })
            .collect());
    }
    // RSS 子集：不展开 DTD/外部实体，论坛使用其 RSS 入口。
    ensure!(
        !body.contains("<!DOCTYPE") && !body.contains("<!ENTITY"),
        "unsupported feed entities"
    );
    Ok(elements(body, "item")
        .into_iter()
        .take(limit)
        .map(|s| Item {
            title: field(s, "title"),
            description: field(s, "description"),
            tags: elements(s, "category").into_iter().map(clean).collect(),
            url: field(s, "link"),
        })
        .collect())
}
fn elements<'a>(s: &'a str, tag: &str) -> Vec<&'a str> {
    let open = format!("<{tag}");
    let close = format!("</{tag}>");
    let mut rest = s;
    let mut out = vec![];
    while let Some(start) = rest.find(&open) {
        rest = &rest[start + open.len()..];
        if !rest.starts_with('>') && !rest.starts_with(char::is_whitespace) {
            continue;
        }
        let Some(a) = rest.find('>') else { break };
        rest = &rest[a + 1..];
        let Some(b) = rest.find(&close) else { break };
        out.push(&rest[..b]);
        rest = &rest[b + close.len()..];
    }
    out
}
fn field(s: &str, tag: &str) -> String {
    elements(s, tag)
        .first()
        .map_or_else(String::new, |s| clean(s))
}
fn clean(s: &str) -> String {
    let s = s
        .trim()
        .strip_prefix("<![CDATA[")
        .and_then(|s| s.strip_suffix("]]>"))
        .unwrap_or(s);
    let mut tag = false;
    let text: String = s
        .chars()
        .filter(|c| {
            if *c == '<' {
                tag = true;
                false
            } else if *c == '>' {
                tag = false;
                false
            } else {
                !tag
            }
        })
        .collect();
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
        .trim()
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;
    fn settings() -> Settings {
        Settings {
            enabled: true,
            feeds: vec!["https://example.org/rss".into()],
            ..Default::default()
        }
    }
    fn item(title: &str) -> Item {
        Item {
            title: title.into(),
            description: String::new(),
            tags: vec![],
            url: "https://example.org/1".into(),
        }
    }
    #[test]
    fn matching_normalizes_and_rejects_unrelated() {
        let interests = interests(
            "ＥＳＰ３２ 无线电",
            &["diy project".into(), "DIY tools".into(), "cats".into()],
        );
        assert!(
            interests.contains("esp32") && interests.contains("diy") && !interests.contains("cats")
        );
        assert!(relevance(&item("ESP32 DIY"), &interests).0 > 0.15);
        assert_eq!(relevance(&item("kitten adoption"), &interests).0, 0.);
        let mut sources = Sources::default();
        assert!(sources
            .collect(&settings(), "group:1", 0., &interests, |_, _| Ok(vec![
                item("kitten adoption")
            ]))
            .is_empty());
        let cfg = Settings {
            threshold: 1.,
            ..settings()
        };
        assert!(Sources::default()
            .collect(&cfg, "group:1", 0., &interests, |_, _| Ok(vec![item(
                "ESP32 DIY"
            )]))
            .is_empty());
    }
    #[test]
    fn throttle_sliding_hour_cache_and_expiry() {
        let cfg = Settings {
            interval_hours: 0.1,
            max_per_hour: 2,
            cache_hours: 2.,
            ..settings()
        };
        let interests = interests("esp32", &[]);
        let mut sources = Sources::default();
        let mut calls = 0;
        for (chat, now, expected) in [
            ("g1", 0., 1),
            ("g1", 100., 0),
            ("g1", 360., 1),
            ("g1", 720., 0),
            ("g2", 720., 1),
            ("g1", 3600., 1),
            ("g1", 7200., 1),
        ] {
            let result = sources.collect(&cfg, chat, now, &interests, |_, _| {
                calls += 1;
                Ok(vec![item("esp32")])
            });
            assert_eq!(result.len(), expected, "{chat} {now}");
        }
        assert_eq!(calls, 2);
        assert!(sources
            .collect(&cfg, "g1", 7199., &interests, |_, _| panic!(
                "clock rollback"
            ))
            .is_empty());
    }
    #[test]
    fn budgets_truncate_unicode_without_cutting_urls() {
        let cfg = Settings {
            max_items: 2,
            max_chars: 32,
            max_total_chars: 55,
            max_requests: 1,
            feeds: vec!["https://a.org/rss".into(), "https://b.org/rss".into()],
            ..settings()
        };
        let interests = interests("esp32", &[]);
        let mut calls = 0;
        let mut first = item("esp32 中文🙂中文🙂中文🙂中文🙂");
        first.description = "ignored".repeat(100);
        let mut second = item("esp32");
        second.url = "https://example.org/2".into();
        let result = Sources::default().collect(&cfg, "g", 0., &interests, |_, _| {
            calls += 1;
            Ok(vec![first.clone(), second.clone(), item("esp32")])
        });
        assert_eq!(calls, 1);
        assert_eq!(result.len(), 1);
        let v = &result[0].item;
        assert_eq!(v.title.chars().count() + v.url.chars().count(), 32);
        assert_eq!(v.url, first.url);
        assert!(v.description.is_empty());
        let cfg = Settings {
            max_items: 1,
            ..settings()
        };
        assert_eq!(
            Sources::default()
                .collect(&cfg, "g", 0., &interests, |_, _| Ok(vec![
                    item("esp32"),
                    second.clone()
                ]))
                .len(),
            1
        );
    }
    #[test]
    fn disabled_and_failed_sources_do_not_retry() {
        let interests = interests("esp32", &[]);
        let mut sources = Sources::default();
        assert!(sources
            .collect(&Settings::default(), "g", 0., &interests, |_, _| panic!(
                "disabled"
            ))
            .is_empty());
        assert!(sources
            .collect(&settings(), "g", 0., &interests, |_, _| anyhow::bail!(
                "offline"
            ))
            .is_empty());
        assert!(sources
            .collect(&settings(), "other", 1., &interests, |_, _| panic!(
                "cached failure"
            ))
            .is_empty());
    }
    #[test]
    fn rss_and_github_parsing() {
        let rss = r#"<rss><channel><item><title><![CDATA[ESP32 &amp; DIY]]></title><description>&lt;b&gt;radio&lt;/b&gt;</description><link>https://example.org/1</link><category domain="test">sdr</category></item></channel></rss>"#;
        let parsed = parse(rss, 1).unwrap();
        assert_eq!(parsed[0].title, "ESP32 & DIY");
        assert_eq!(parsed[0].tags, vec!["sdr"]);
        let github = r#"{"items":[{"full_name":"diy/esp32","description":null,"topics":["sdr"],"html_url":"https://github.com/diy/esp32"}]}"#;
        let parsed = parse(github, 1).unwrap();
        assert_eq!(parsed[0].title, "diy/esp32");
        assert_eq!(parsed[0].tags, vec!["sdr"]);
        assert!(parse("<!DOCTYPE rss><rss/>", 1).is_err());
    }
    #[test]
    fn ureq_fetches_mock_feed() {
        use std::{
            io::{Read, Write},
            net::TcpListener,
        };
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0; 4096];
            let n = stream.read(&mut request).unwrap();
            assert!(String::from_utf8_lossy(&request[..n]).contains("qq-inner-agent-topic-source"));
            let body =
                "<rss><item><title>esp32</title><link>https://example.org/1</link></item></rss>";
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .unwrap();
        });
        assert_eq!(
            fetch(&format!("http://{addr}/rss"), 1).unwrap()[0].title,
            "esp32"
        );
        server.join().unwrap();
    }
    #[test]
    fn master_switch_requires_sources_and_skips_fetch_when_disabled() {
        let legacy: Settings = serde_json::from_value(
            serde_json::json!({"feeds": ["https://example.org/rss"]}),
        ).unwrap();
        assert!(!legacy.enabled);
        assert!(!legacy.enabled());
        assert!(!Settings { enabled: true, ..Default::default() }.enabled());
        assert!(settings().enabled());
        assert!(Settings { enabled: true, github: vec!["esp32".into()], ..Default::default() }.enabled());
        let disabled = Settings { enabled: false, ..settings() };
        assert!(Sources::default().collect(&disabled, "g", 0., &interests("esp32", &[]), |_, _| {
            panic!("disabled source must not fetch")
        }).is_empty());
    }
    #[test]
    fn config_defaults_and_validation() {
        let default = Settings::default();
        assert!(!default.enabled());
        assert!(default.validate().is_ok());
        let mut config = crate::config::defaults();
        config["agent"]["topicSource"] =
            serde_json::json!({"enabled":true,"github":["esp32","topic:sdr"],"maxItems":2});
        assert!(crate::config::validate(&config).is_ok());
        for bad in [
            serde_json::json!({"enabled":"true"}),
            serde_json::json!({"maxItems":0}),
            serde_json::json!({"threshold":0}),
            serde_json::json!({"feeds":["file:///etc/passwd"]}),
            serde_json::json!({"maxPerHour":1.5}),
        ] {
            config["agent"]["topicSource"] = bad;
            assert!(crate::config::validate(&config).is_err());
        }
    }
}
