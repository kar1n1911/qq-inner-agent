//! SillyTavern character-card reader. No prompt assembly or runtime wiring lives here.
use futures_util::future::BoxFuture;
use super::{ports::PersonaPort, types::{ChatAddress, HostPermission, IoError, PersonaCapabilities, PersonaSource, PortKind, PortManifest, PortReply, TransportKind, VersionReq}};
use serde_json::{json, Map, Value};
use std::{collections::HashMap, fs, path::{Path, PathBuf}};

const MAX_CARD_BYTES: u64 = 2 * 1024 * 1024;

#[derive(Clone, Debug)]
pub struct ParsedCard {
    /// Normalized V2-shaped card, including unrecognized `extensions` keys.
    pub normalized: Value,
    /// Parsed for later policy work; never copied to `PersonaSource`.
    pub character_book: Option<Value>,
}

impl ParsedCard {
    pub fn runtime_source(&self) -> PersonaSource { self.source(None) }

    pub fn import_preview(&self) -> ImportPreview {
        ImportPreview {
            source: self.source(None),
            system_prompt_candidate: string(&self.normalized["data"], "system_prompt"),
            character_book: self.character_book.clone(),
        }
    }

    fn source(&self, initial_persona: Option<String>) -> PersonaSource {
        let data = &self.normalized["data"];
        PersonaSource {
            initial_persona,
            name: string(data, "name"),
            description: string(data, "description"),
            personality: string(data, "personality"),
            scenario: string(data, "scenario"),
            examples: string(data, "mes_example").into_iter().collect(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct ImportPreview {
    /// This is only a candidate, not part of the runtime persona payload.
    pub system_prompt_candidate: Option<String>,
    /// Retained for inspection; there is no memory/prompt injection in this plugin.
    pub character_book: Option<Value>,
    source: PersonaSource,
}

impl ImportPreview {
    /// The caller must obtain human confirmation before invoking this operation.
    /// The confirmed text is placed only in the persona field, never a base field.
    pub fn confirm(mut self, human_confirmed: bool) -> Result<PersonaSource, IoError> {
        if !human_confirmed { return Err(IoError::new("persona_import_confirmation_required")); }
        self.source.initial_persona = self.system_prompt_candidate;
        Ok(self.source)
    }
}

fn string(data: &Value, key: &str) -> Option<String> {
    data.get(key).and_then(Value::as_str).filter(|s| !s.is_empty()).map(str::to_owned)
}

pub fn parse_card(bytes: &[u8]) -> Result<ParsedCard, IoError> {
    if bytes.len() as u64 > MAX_CARD_BYTES { return Err(IoError::new("persona_card_too_large")); }
    let json_bytes = if bytes.starts_with(b"\x89PNG\r\n\x1a\n") { png_chara(bytes)? } else { bytes.to_vec() };
    let raw: Value = serde_json::from_slice(&json_bytes).map_err(|_| IoError::new("invalid_persona_card"))?;
    let normalized = match raw.get("spec").and_then(Value::as_str) {
        Some("chara_card_v3") => return Err(IoError::new("persona_v3_unsupported")),
        Some("chara_card_v2") => {
            if !raw["spec_version"].as_str().is_some_and(|s| s.starts_with('2'))
                || !raw["data"].is_object() { return Err(IoError::new("invalid_persona_card")); }
            raw
        }
        Some(_) => return Err(IoError::new("persona_card_version_unsupported")),
        None => {
            let Some(fields) = raw.as_object() else { return Err(IoError::new("invalid_persona_card")); };
            if fields.get("name").and_then(Value::as_str).is_none() { return Err(IoError::new("invalid_persona_card")); }
            let mut data: Map<String, Value> = fields.clone();
            data.entry("extensions").or_insert_with(|| json!({}));
            json!({"spec":"chara_card_v2", "spec_version":"2.0", "data":data})
        }
    };
    let book = normalized["data"].get("character_book").filter(|v| !v.is_null()).cloned();
    Ok(ParsedCard { normalized, character_book: book })
}

fn png_chara(bytes: &[u8]) -> Result<Vec<u8>, IoError> {
    let mut offset = 8usize;
    while offset < bytes.len() {
        let header = bytes.get(offset..offset + 8).ok_or_else(|| IoError::new("invalid_persona_png"))?;
        let len = u32::from_be_bytes(header[..4].try_into().unwrap()) as usize;
        let end = offset.checked_add(12).and_then(|n| n.checked_add(len)).ok_or_else(|| IoError::new("invalid_persona_png"))?;
        let chunk = bytes.get(offset + 4..end - 4).ok_or_else(|| IoError::new("invalid_persona_png"))?;
        let expected = u32::from_be_bytes(bytes[end - 4..end].try_into().unwrap());
        if crc32(chunk) != expected { return Err(IoError::new("invalid_persona_png_crc")); }
        let kind = &header[4..8];
        if kind == b"tEXt" {
            let data = &chunk[4..];
            if let Some(separator) = data.iter().position(|b| *b == 0) {
                if &data[..separator] == b"chara" { return decode_base64(&data[separator + 1..]); }
            }
        }
        if kind == b"IEND" { break; }
        offset = end;
    }
    Err(IoError::new("persona_chara_missing"))
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 { crc = (crc >> 1) ^ if crc & 1 == 1 { 0xedb8_8320 } else { 0 }; }
    }
    !crc
}

fn decode_base64(input: &[u8]) -> Result<Vec<u8>, IoError> {
    if input.is_empty() || input.len() % 4 != 0 { return Err(IoError::new("invalid_persona_base64")); }
    let mut out = Vec::with_capacity(input.len() / 4 * 3);
    for (index, group) in input.chunks_exact(4).enumerate() {
        let last = index == input.len() / 4 - 1;
        let mut digits = [0u8; 4];
        let mut padding = 0;
        for (i, byte) in group.iter().enumerate() {
            digits[i] = match byte {
                b'A'..=b'Z' => byte - b'A', b'a'..=b'z' => byte - b'a' + 26,
                b'0'..=b'9' => byte - b'0' + 52, b'+' => 62, b'/' => 63,
                b'=' if last && i >= 2 => { padding += 1; 0 },
                _ => return Err(IoError::new("invalid_persona_base64")),
            };
        }
        if group[2] == b'=' && group[3] != b'=' { return Err(IoError::new("invalid_persona_base64")); }
        if padding == 2 && digits[1] & 0x0f != 0 || padding == 1 && digits[2] & 0x03 != 0 {
            return Err(IoError::new("invalid_persona_base64"));
        }
        out.push((digits[0] << 2) | (digits[1] >> 4));
        if padding < 2 { out.push((digits[1] << 4) | (digits[2] >> 2)); }
        if padding == 0 { out.push((digits[2] << 6) | digits[3]); }
    }
    Ok(out)
}

pub struct SillyTavernPersona {
    manifest: PortManifest,
    root: PathBuf,
    cards: HashMap<ChatAddress, PathBuf>,
}

impl SillyTavernPersona {
    pub fn new(name: &str, root: PathBuf, cards: HashMap<ChatAddress, PathBuf>) -> Result<Self, IoError> {
        if name.is_empty() || name.contains(':') { return Err(IoError::new("invalid_port_manifest")); }
        let root = root.canonicalize().map_err(|_| IoError::new("persona_root_unavailable"))?;
        Ok(Self { manifest: PortManifest {
            name: name.into(), kind: PortKind::Persona, transport: TransportKind::InProcess,
            core_api_version: VersionReq { major: 1, min_minor: 0, max_minor: None },
            host_permissions: vec![HostPermission::ReadFile { root: root.to_string_lossy().into_owned() }],
            dependencies: vec![], config_fields: vec![],
        }, root, cards })
    }

    pub fn read_file(&self, path: &Path) -> Result<ParsedCard, IoError> {
        let path = path.canonicalize().map_err(|_| IoError::new("persona_card_unavailable"))?;
        if !path.starts_with(&self.root) { return Err(IoError::new("persona_card_outside_root")); }
        let metadata = fs::metadata(&path).map_err(|_| IoError::new("persona_card_unavailable"))?;
        if !metadata.is_file() || metadata.len() > MAX_CARD_BYTES { return Err(IoError::new("persona_card_too_large")); }
        parse_card(&fs::read(path).map_err(|_| IoError::new("persona_card_unavailable"))?)
    }
}

impl PersonaPort for SillyTavernPersona {
    fn manifest(&self) -> &PortManifest { &self.manifest }
    fn capabilities(&self) -> PersonaCapabilities {
        PersonaCapabilities {
            supported: vec!["v1_read".into(), "v2_read".into(), "png_chara_read".into(), "character_book_parse".into()],
            unsupported: vec!["v3_read".into(), "character_book_injection".into(), "post_history_instructions".into()],
        }
    }
    fn load<'a>(&'a self, chat: &'a ChatAddress) -> BoxFuture<'a, PortReply<PersonaSource>> {
        Box::pin(async move {
            let Some(path) = self.cards.get(chat) else { return PortReply::Failed { code: "persona_card_unbound".into(), uncertain: false }; };
            match self.read_file(path) {
                Ok(card) => PortReply::Ok(card.runtime_source()),
                Err(error) => PortReply::Failed { code: error.code, uncertain: false },
            }
        })
    }
}
