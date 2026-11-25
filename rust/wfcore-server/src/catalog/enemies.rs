use super::{array, entries, first_text, fold, general, number, object, text, truth};
use crate::data::EngineData;
use indexmap::IndexMap;
use serde_json::{json, Value};
use std::collections::HashMap;

#[derive(PartialEq, Eq, Hash)]
struct Identity {
    faction: String,
    zh: String,
    en: String,
    pools: [u64; 4],
    materials: [String; 3],
}

type Preference = (bool, bool, bool, bool, usize);

pub(super) fn rows(data: &EngineData) -> Vec<Value> {
    let mut unique: IndexMap<Identity, (Preference, Value)> = IndexMap::new();
    for (path, enemy) in &data.enemies {
        let path = path.trim();
        let (Some(health), Some(shield), Some(armour), Some(level)) = (
            number(&enemy["health"]).filter(|v| *v > 0.0),
            number(&enemy["shield"]).filter(|v| *v >= 0.0),
            number(&enemy["armour"]).filter(|v| *v >= 0.0),
            number(&enemy["baseLevel"]),
        ) else { continue };
        let health_type = text(&enemy["healthType"]).trim();
        let armour_type = first_text(&[&enemy["armourType"], &enemy["armorType"]]).trim();
        let shield_type = text(&enemy["shieldType"]).trim();
        let faction = text(&enemy["faction"]).trim();
        let resistance = text(&enemy["factionResistance"]).trim();
        if [path, health_type, armour_type, shield_type, resistance].contains(&"")
            || !truth(&enemy["damageController"])
        {
            continue;
        }
        let numeric_overguard = number(&enemy["overguard"]).is_some_and(|v| v > 0.0);
        let mut unsupported = Vec::new();
        let note = if numeric_overguard {
            Some("innate-scaled")
        } else if truth(&enemy["overguard"]) || truth(&enemy["hasOverguard"]) {
            unsupported.push("enemy-overguard");
            Some("missing-value")
        } else { None };
        if ["damageAttenuation", "hasDamageAttenuation", "dynamicDamageResistance"]
            .iter().any(|key| truth(&enemy[*key]))
        {
            unsupported.push("enemy-damage-attenuation");
        }
        unsupported.sort_unstable();
        let multiplier = head_multiplier(enemy);
        let name_zh = first_text(&[&enemy["nameZh"], &enemy["name"]]);
        let name_en = first_text(&[&enemy["nameEn"], &enemy["name"]]);
        let name_zh = if name_zh.is_empty() { path } else { name_zh };
        let name_en = if name_en.is_empty() { path } else { name_en };
        let complete = unsupported.is_empty();
        let mut row = json!({
            "path": path, "nameZh": name_zh, "nameEn": name_en, "nameInternal": text(&enemy["nameInternal"]),
            "faction": faction, "factionResistance": resistance,
            "baseLevel": level, "health": health, "shield": shield, "armour": armour,
            "healthType": health_type, "armourType": armour_type, "shieldType": shield_type,
            "hasHeadProxy": multiplier.is_some(),
            "headshotStatus": if multiplier.is_some() {"supported"} else {"not-applicable"},
            "scalingSupported": enemy["defenseSources"]["baseLevel"] != "default-1",
            "complete": complete, "unsupported": unsupported,
        });
        row.as_object_mut().unwrap().extend(image_fields(data, path));
        if let Some(note) = note {
            row["overguardNote"] = json!(note);
        }
        if truth(&enemy["baseLevelCandidates"]) {
            row["baseLevelCandidates"] = enemy["baseLevelCandidates"].clone();
        }
        if let Some(multiplier) = multiplier {
            row["headshotMultiplier"] = json!(format!("{multiplier:.4}").parse::<f64>().unwrap());
        }
        let embedded = if enemy["factionDamageModifiers"].is_null() { &enemy["factionModifiers"] } else { &enemy["factionDamageModifiers"] };
        let deltas = if embedded.is_object() {
            entries(embedded).filter_map(|(key, value)| number(value).map(|v| (key.clone(), json!(v)))).collect()
        } else {
            object(&data.rules.factions[resistance])
        };
        if !deltas.is_empty() {
            row["factionModifiers"] = Value::Object(deltas);
        }
        for key in ["factionDamageModifiers", "damageController", "hitProxies", "usesNpcArmourCurve",
            "overguard", "hasOverguard", "damageAttenuation", "hasDamageAttenuation",
            "dynamicDamageResistance", "defenseSources"] {
            if !enemy[key].is_null() {
                row[key] = enemy[key].clone();
            }
        }
        let identity = Identity {
            faction: fold(faction), zh: fold(name_zh), en: fold(name_en),
            pools: [health, shield, armour, level].map(|v| if v == 0.0 { 0 } else { v.to_bits() }),
            materials: [health_type, armour_type, shield_type].map(str::to_owned),
        };
        let lowered = fold(path);
        let preference = (!complete, lowered.contains("leader"), lowered.contains("ally"),
            lowered.contains("/pnw"), path.chars().count());
        match unique.entry(identity) {
            indexmap::map::Entry::Vacant(entry) => { entry.insert((preference, row)); }
            indexmap::map::Entry::Occupied(mut entry) if preference < entry.get().0 => {
                entry.insert((preference, row));
            }
            _ => {}
        }
    }
    let mut counts = HashMap::new();
    for key in unique.keys() {
        *counts.entry((key.faction.clone(), key.zh.clone())).or_insert(0) += 1;
    }
    let mut rows = Vec::with_capacity(unique.len());
    for (key, (_, row)) in &mut unique {
        let name = text(&row["nameZh"]);
        row["displayName"] = if counts[&(key.faction.clone(), key.zh.clone())] > 1 {
            let [level, health, shield, armour] = ["baseLevel", "health", "shield", "armour"]
                .map(|field| general(number(&row[field]).unwrap()));
            json!(format!("{name} · Lv{level} · H{health}/S{shield}/A{armour} · {}", text(&row["path"])))
        } else { json!(name) };
        rows.push(((&key.faction, &key.zh, text(&row["path"]).to_owned()), row));
    }
    rows.sort_by(|(left, _), (right, _)| left.cmp(right));
    rows.into_iter().map(|(_, row)| row.take()).collect()
}

fn image_fields(data: &EngineData, path: &str) -> serde_json::Map<String, Value> {
    let source = &data.enemy_art["enemies"][path];
    if data.enemy_art["schemaVersion"] != 1 || !source.is_object() {
        return object(&json!({"imageAsset": null, "imageStatus": "missing"}));
    }
    let mut row = json!({
        "imageAsset": null, "imageStatus": source.get("status").cloned().unwrap_or(json!("missing")),
        "imageSourceType": source["iconSourceType"], "imageInherited": truth(&source["inherited"]),
    });
    let asset = text(&source["asset"]);
    if asset.starts_with("/assets/enemies/art/Lotus/Interface/")
        && !asset.contains(['\\', '?', '#', '%'])
        && !asset[1..].split('/').any(|part| matches!(part, "" | "." | ".."))
    {
        let frontend = data.dataset_root.join("frontend");
        let found = frontend.join(&asset[1..]).canonicalize().ok().is_some_and(|p| {
            p.starts_with(frontend.join("assets/enemies/art")) && p.is_file()
        });
        if found { row["imageAsset"] = json!(asset); }
        else { row["imageStatus"] = json!("file-missing"); }
    }
    object(&row)
}

fn field<'a>(value: &'a Value, names: &[&str]) -> Option<&'a Value> {
    entries(value).find_map(|(key, value)| {
        names.contains(&fold(key).replace('_', "").as_str()).then_some(value)
    })
}

fn hit_value(value: &Value) -> Option<f64> {
    number(value).filter(|v| *v >= 0.0)
}

const HEAD_ALIASES: &[&str] = &["head", "headshot", "head_hit", "headshot_hit", "weakpoint", "weak_point"];

fn normalized(value: &str) -> String {
    fold(value.trim()).replace(['-', ' '], "_")
}

fn find_head(value: &Value) -> Option<&Value> {
    for (key, nested) in entries(value) {
        if HEAD_ALIASES.contains(&normalized(key).as_str()) {
            if nested.is_object() {
                if field(nested, &["part"]).and_then(Value::as_str)
                    .is_some_and(|part| !HEAD_ALIASES.contains(&normalized(part).as_str())) {
                    continue;
                }
                return Some(nested);
            }
            if hit_value(nested).is_some() {
                return Some(nested);
            }
        }
    }
    for key in ["part", "location", "hitlocation", "name", "type", "tag", "id"] {
        if let Some(label) = field(value, &[key]).and_then(Value::as_str).filter(|s| !s.is_empty()) {
            if HEAD_ALIASES.iter().any(|alias| normalized(label).contains(alias)) {
                return Some(value);
            }
            break;
        }
    }
    entries(value).map(|(_, value)| value).chain(array(value)).find_map(find_head)
}

fn head_multiplier(enemy: &Value) -> Option<f64> {
    let raw = ["hitProxies", "hitProxy", "weakpoints", "weakPoints"].iter()
        .find_map(|key| enemy.get(*key));
    let Some(proxy) = raw.and_then(find_head) else {
        return ["headshotMultiplier", "headMultiplier"].iter().find_map(|key| hit_value(&enemy[*key]));
    };
    if let Some(value) = hit_value(proxy) {
        return Some(value);
    }
    if let Some(value) = field(proxy, &["damageatten"]).and_then(hit_value) {
        return field(proxy, &["damagemultipliers"]).is_none().then_some(1.5 * value);
    }
    if field(proxy, &["damagemultipliers"]).is_some() {
        return None;
    }
    if let Some(value) = field(proxy, &["damagemultiplier", "multiplier"]).and_then(hit_value) {
        return Some(value);
    }
    if field(proxy, &["damageatten"]).is_none() {
        return Some(1.5);
    }
    ["damagemultiplier", "multiplier", "headshotmultiplier", "headmultiplier",
        "weakpointmultiplier", "damagescale", "scale", "value"]
        .iter().find_map(|key| field(proxy, &[*key]).and_then(hit_value))
        .or_else(|| proxy.as_object().filter(|obj| obj.len() == 1).and_then(|obj| obj.values().next()).and_then(hit_value))
}
