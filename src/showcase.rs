// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

use std::path::PathBuf;

use serde_json::Value;
use uuid::Uuid;

// Kept to 31 bits, so every reader of the value, signed or not, takes it
// as the same number.
const SEED_MASK: u32 = 0x7FFF_FFFF;

// Built in, so a showcase runs with nothing but `enabled = true`. Both are
// 4v4s of Petra, recorded from real matches.
const PRESETS: [(&str, &[u8]); 2] = [
    (
        "preset:mainland-4v4",
        include_bytes!("showcase/presets/mainland-4v4.json"),
    ),
    (
        "preset:islands-4v4",
        include_bytes!("showcase/presets/islands-4v4.json"),
    ),
];

struct Template {
    // The file's path, or the preset's name.
    label: String,
    json: Value,
}

// The showcase's match settings. Checked once at startup, so a bad file
// stops the server there instead of failing one match after another.
pub struct Showcase {
    templates: Vec<Template>,
    next: usize,
}

impl Showcase {
    // No paths means the built-in presets. They go through the same checks
    // as a file, so a broken one stops the server at startup too.
    pub fn load(paths: &[PathBuf]) -> Result<Showcase, String> {
        let mut templates = Vec::new();
        if paths.is_empty() {
            for (name, data) in PRESETS {
                templates.push(parse(name.to_string(), data)?);
            }
        }
        for path in paths {
            let label = path.display().to_string();
            let data = std::fs::read(path)
                .map_err(|error| format!("failed to read showcase template '{label}': {error}"))?;
            templates.push(parse(label, &data)?);
        }
        // Shuffled once, so a restarted server does not open with the same
        // map every time, and then taken in turn, so none repeats before all
        // have run.
        for i in (1..templates.len()).rev() {
            let j = (random_u64() % (i as u64 + 1)) as usize;
            templates.swap(i, j);
        }
        Ok(Showcase { templates, next: 0 })
    }

    // Fresh seeds are what make a template a new game: with the same ones
    // the map generates identically and every AI plays the same match again.
    pub fn next_start(&mut self) -> Vec<u8> {
        let template = &self.templates[self.next % self.templates.len()];
        self.next = self.next.wrapping_add(1);
        let mut json = template.json.clone();
        let seed = random_u64() as u32 & SEED_MASK;
        let ai_seed = random_u64() as u32 & SEED_MASK;
        if let Some(settings) = json.get_mut("settings").and_then(Value::as_object_mut) {
            settings.insert("Seed".to_string(), seed.into());
            settings.insert("AISeed".to_string(), ai_seed.into());
        }
        if let Some(root) = json.as_object_mut() {
            root.insert(
                "matchID".to_string(),
                format!("{:016X}", random_u64()).into(),
            );
        }
        tracing::info!(
            template = %template.label,
            seed,
            ai_seed,
            "showcase: next match"
        );
        serde_json::to_vec(&json).expect("a JSON value always serializes")
    }
}

fn parse(label: String, data: &[u8]) -> Result<Template, String> {
    let json: Value = serde_json::from_slice(data)
        .map_err(|error| format!("failed to parse showcase template '{label}': {error}"))?;
    check(&json).map_err(|error| format!("showcase template '{label}' {error}"))?;
    Ok(Template { label, json })
}

// A slot without an AI would wait for a player the showcase never has.
fn check(json: &Value) -> Result<(), String> {
    let players = json
        .get("settings")
        .and_then(|s| s.get("PlayerData"))
        .and_then(Value::as_array)
        .ok_or("has no settings.PlayerData list")?;
    if players.is_empty() {
        return Err("has no players".to_string());
    }
    for (i, player) in players.iter().enumerate() {
        let ai = player.get("AI").and_then(Value::as_str).unwrap_or_default();
        if ai.is_empty() {
            return Err(format!("has player {} without an AI", i + 1));
        }
    }
    Ok(())
}

fn random_u64() -> u64 {
    Uuid::new_v4().as_u128() as u64
}
