//! Plugin-boundary checks for io-port-plan §7.3.1; runtime prompt wiring is separate.
#[path = "../../mod/sillytavern/adapter.rs"]
mod sillytavern;

use qq_inner_core::engine::io::{ChatAddress, ChatKind, PersonaPort, PortReply};
use serde_json::{json, Value};
use sillytavern::{parse_card, SillyTavernPersona};
use std::{collections::HashMap, fs};

fn card() -> Value {
    json!({"spec":"chara_card_v2","spec_version":"2.0","data":{
        "name":"Foreign card","description":"description-only","personality":"curious",
        "scenario":"at a station","mes_example":"Example dialogue",
        "system_prompt":"CARD SYSTEM OVERRIDE","post_history_instructions":"LATE OVERRIDE",
        "creator_notes":"NOT FOR PROMPT","character_book":{"entries":[{"keys":["secret"],"content":"BOOK ONLY"}]},
        "extensions":{"unknown_vendor_key":{"keep":true}}
    }})
}

fn png_card(card: &[u8]) -> Vec<u8> {
    fn crc32(bytes: &[u8]) -> u32 {
        let mut crc = !0u32;
        for byte in bytes { crc ^= u32::from(*byte); for _ in 0..8 { crc = (crc >> 1) ^ if crc & 1 == 1 { 0xedb8_8320 } else { 0 }; } }
        !crc
    }
    let alphabet = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut encoded = Vec::new();
    for chunk in card.chunks(3) {
        let n = (u32::from(chunk[0]) << 16) | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8) | u32::from(*chunk.get(2).unwrap_or(&0));
        encoded.push(alphabet[((n >> 18) & 63) as usize]); encoded.push(alphabet[((n >> 12) & 63) as usize]);
        encoded.push(if chunk.len() > 1 { alphabet[((n >> 6) & 63) as usize] } else { b'=' });
        encoded.push(if chunk.len() > 2 { alphabet[(n & 63) as usize] } else { b'=' });
    }
    let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut chunk = b"tEXtchara\0".to_vec(); chunk.extend(encoded);
    png.extend(((chunk.len() - 4) as u32).to_be_bytes()); png.extend(&chunk); png.extend(crc32(&chunk).to_be_bytes());
    png.extend(0u32.to_be_bytes()); png.extend(b"IEND"); png.extend(crc32(b"IEND").to_be_bytes());
    png
}

#[test]
fn v1_v2_png_and_v3() {
    let card = card();
    let upgraded = parse_card(&serde_json::to_vec(&card["data"]).unwrap()).unwrap();
    assert_eq!(upgraded.normalized["spec"], "chara_card_v2");
    assert_eq!(upgraded.normalized["data"]["extensions"]["unknown_vendor_key"]["keep"], true);
    let direct = parse_card(&serde_json::to_vec(&card).unwrap()).unwrap();
    let embedded = parse_card(&png_card(&serde_json::to_vec(&card).unwrap())).unwrap();
    assert_eq!(direct.runtime_source(), embedded.runtime_source());
    assert_eq!(embedded.runtime_source().scenario.as_deref(), Some("at a station"));
    assert_eq!(embedded.runtime_source().examples, ["Example dialogue"]);
    assert_eq!(parse_card(br#"{"spec":"chara_card_v3","data":{}}"#).unwrap_err().code, "persona_v3_unsupported");
}

#[tokio::test]
async fn import_confirmation_and_runtime_payload_boundary() {
    let card = card();
    let parsed = parse_card(&serde_json::to_vec(&card).unwrap()).unwrap();
    let base = "KERNEL BASE: boundary and output contract";
    let preview = parsed.import_preview();
    assert_eq!(preview.system_prompt_candidate.as_deref(), Some("CARD SYSTEM OVERRIDE"));
    assert_eq!(preview.character_book, card["data"].get("character_book").cloned());
    assert_eq!(preview.clone().confirm(false).unwrap_err().code, "persona_import_confirmation_required");
    let approved = preview.confirm(true).unwrap();
    assert_eq!(approved.initial_persona.as_deref(), Some("CARD SYSTEM OVERRIDE"));
    let approved_payload = serde_json::to_string(&approved).unwrap();
    assert!(!approved_payload.contains("LATE OVERRIDE"));
    assert!(!approved_payload.contains("BOOK ONLY"));
    assert_eq!(base, "KERNEL BASE: boundary and output contract");

    let root = std::env::temp_dir().join(format!("qq-inner-sillytavern-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
    fs::create_dir(&root).unwrap();
    let path = root.join("card.png"); fs::write(&path, png_card(&serde_json::to_vec(&card).unwrap())).unwrap();
    let chat = ChatAddress::new(ChatKind::Group, "onebot", "123").unwrap();
    let port = SillyTavernPersona::new("sillytavern", root.clone(), HashMap::from([(chat.clone(), path)])).unwrap();
    let runtime = port.load(&chat).await.into_result().unwrap();
    let payload = serde_json::to_string(&runtime).unwrap();
    assert_eq!(runtime.initial_persona, None);
    assert!(!payload.contains("CARD SYSTEM OVERRIDE"));
    assert!(!payload.contains("BOOK ONLY"));
    assert!(!payload.contains("LATE OVERRIDE"));
    assert_eq!(base, "KERNEL BASE: boundary and output contract");
    assert_eq!(parsed.character_book, card["data"].get("character_book").cloned());
    assert!(port.capabilities().unsupported.contains(&"v3_read".to_string()));
    assert!(port.capabilities().unsupported.contains(&"character_book_injection".to_string()));
    assert!(matches!(port.load(&ChatAddress::new(ChatKind::Group, "onebot", "missing").unwrap()).await, PortReply::Failed { code, .. } if code == "persona_card_unbound"));
    fs::remove_dir_all(root).unwrap();
}
