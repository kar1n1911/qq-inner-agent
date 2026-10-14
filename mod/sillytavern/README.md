# SillyTavern PersonaPort adapter

`adapter.rs` reads V1 JSON and V2 JSON/PNG `tEXt` `chara` cards. V1 is normalized to a V2-shaped `data` object. The card's unknown `extensions` remain in `ParsedCard.normalized`; `character_book` is parsed and retained but never copied to `PersonaSource` or a prompt. V3, malformed cards, missing bindings and unavailable files fail explicitly.

`PersonaPort::load` is the **runtime** path. It returns only name, description, personality, scenario and examples. It never returns the card's `system_prompt`, `post_history_instructions`, creator notes, tags, greeting or book. The **import** path produces `ImportPreview`; after human confirmation, `confirm(true)` puts the candidate `system_prompt` into `PersonaSource.initial_persona`, a persona field below the kernel's immutable base. `confirm(false)` fails. This adapter does not compose the kernel prompt or send greetings.

`SillyTavernPersona::new` takes an already selected card root and chat-to-file bindings. `read_file` canonicalizes both root and file path, rejects files outside the root, and limits cards to 2 MiB. The manifest requests read access to the selected root; the host must still enforce its own grant. This plugin is deliberately not wired into runtime discovery or prompt assembly. The package version, loader entry, root grant and binding configuration remain TBD.
