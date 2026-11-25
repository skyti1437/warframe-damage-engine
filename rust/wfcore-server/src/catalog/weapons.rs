use super::{array, entries, first_text, fold, positive, text, truth, GUN_METRICS, INNATE_MAX, INNATE_MIN, MELEE_METRICS};
use crate::data::{corrections::{merge, DARK_SPLIT}, invalid, EngineData, Result};
use serde_json::{json, Value};
use std::collections::HashMap;

const RIVEN_SLOTS: &[&str] = &["primary", "secondary", "melee"];

pub(super) fn rows(data: &EngineData, stances: &[Value]) -> Result<(Vec<Value>, Vec<Value>, Value)> {
    let mut rows = Vec::new();
    let mut hard_fails = Vec::new();
    for (path, weapon) in &data.weapons {
        let slot = text(&weapon["slot"]);
        if !(RIVEN_SLOTS.contains(&slot) || slot == "arch_gun" || slot == "companion") || path == DARK_SPLIT {
            continue;
        }
        let weapon_type = if slot == "companion" { "companion" } else { weapon_type(weapon) };
        let riven_type = riven_type(weapon);
        let exalted = exalted_payload(data, weapon);
        let variant_type = weapon["variantType"].as_str().filter(|value| !value.is_empty()).unwrap_or("VT_NORMAL");
        let modes = display_modes(weapon, &data.rules.weapons["exalted"][path]["modePatch"]);
        let fire_modes = fire_modes(data, weapon, &modes, weapon_type)?;
        let issue = match fire_modes.first() {
            None => json!("weapon has no fire modes"),
            Some(mode) if mode["available"] == false && mode["issue"].is_null() => json!("non-damage ability action"),
            Some(mode) => mode["issue"].clone(),
        };
        let build_issue = if issue.is_null() { &data.rules.weapons["buildIssues"][path] } else { &issue };
        let metric = if weapon_type == "melee" { "attackDps" } else { "sustainedDps" };
        let mut analysis_issue = issue.clone();
        if analysis_issue.is_null() {
            let availability = fire_modes.first().map(|mode| &mode["metricAvailability"][metric]).unwrap_or(&Value::Null);
            if availability["available"] != true {
                analysis_issue = json!(availability["issue"].as_str().unwrap_or("default analysis metric is unavailable"));
            } else if !exalted.is_null() {
                analysis_issue = json!("显赫武器没有紫卡");
            }
        }
        let polarities = array(&weapon["polarities"]);
        let stance = stance_slot(data, weapon, weapon_type, stances);
        let innate = array(&weapon["innateElementCandidates"]).iter().filter(|value| !text(value).is_empty()).collect::<Vec<_>>();
        let sources = array(&weapon["innateElementSources"]);
        let innate_element = if innate.is_empty() {
            Value::Null
        } else {
            json!({"candidates": innate, "sources": sources, "min": INNATE_MIN, "max": INNATE_MAX, "default": INNATE_MAX})
        };
        let row = json!({
            "path": path, "slot": slot, "nameZh": text(&weapon["nameZh"]),
            "nameEn": text(&weapon["nameEn"]), "nameInternal": text(&weapon["nameInternal"]),
            "weaponType": weapon_type, "rivenType": riven_type,
            "ancestry": array(&weapon["ancestry"]), "compatTags": array(&weapon["compatTags"]),
            "polarities": polarities, "variantType": variant_type,
            "variantGroupPath": variant_group(data, weapon, path),
            "disposition": positive(&weapon["omegaAttenuation"]), "fireModes": fire_modes,
            "available": issue.is_null(), "issue": issue,
            "analysisAvailable": analysis_issue.is_null(), "analysisIssue": analysis_issue,
            "buildAvailable": build_issue.is_null(), "buildIssue": build_issue,
            "hiddenFromBuildPicker": issue.is_null() && !build_issue.is_null(),
            "incarnon": incarnon(data, weapon)?,
            "innateElementCandidates": array(&weapon["innateElementCandidates"]),
            "innateElementSources": sources, "innateElement": innate_element,
            "isExalted": stance["builtIn"] == true || !exalted.is_null(), "exalted": exalted,
            "specialSlots": {
                "stance": stance,
                "exilus": {"slotPolarity": polarities.get(if weapon_type == "melee" { 9 } else { 8 }),
                    "kind": if weapon_type == "melee" { "melee" } else { slot }},
                "weaponArcane": {"limit": 1}, "frameArcane": {"limit": 2},
            },
        });
        if !issue.is_null() {
            hard_fails.push(json!({"path": path, "slot": slot, "nameZh": row["nameZh"], "nameEn": row["nameEn"], "reason": issue}));
        }
        rows.push(row);
    }

    let mut groups = HashMap::new();
    for row in &rows {
        if row["exalted"].is_object() && row["variantType"] == "VT_NORMAL" {
            groups.entry(exalted_key(row)).or_insert_with(|| row["path"].clone());
        }
    }
    for row in &mut rows {
        if row["exalted"].is_object() {
            row["variantGroupPath"] = groups.get(&exalted_key(row)).unwrap_or(&row["path"]).clone();
        }
    }
    rows.sort_by_cached_key(|row| (
        text(&row["slot"]).to_owned(),
        fold(first_text(&[&row["nameZh"], &row["nameEn"], &row["nameInternal"]])),
        text(&row["path"]).to_owned(),
    ));
    let count = |field: &str| rows.iter().filter(|row| row[field] == true).count();
    let analysis = count("analysisAvailable");
    let build = count("buildAvailable");
    let counts = json!({
        "weapons": rows.len(), "available": count("available"), "knownHardFails": hard_fails.len(),
        "analysisAvailable": analysis, "analysisUnavailable": rows.len() - analysis,
        "buildAvailable": build, "buildUnavailable": rows.len() - build,
        "buildPickerWeapons": rows.len() - count("hiddenFromBuildPicker"),
        "buildPickerAvailable": rows.iter().filter(|r| r["buildAvailable"] == true && r["hiddenFromBuildPicker"] == false).count(),
    });
    Ok((rows, hard_fails, counts))
}

fn weapon_type(weapon: &Value) -> &'static str {
    let ancestry = fold(&array(&weapon["ancestry"]).iter().map(text).collect::<Vec<_>>().join(" "));
    let slot = text(&weapon["slot"]);
    if matches!(slot, "melee" | "arch_melee" | "space_melee" | "archmelee" | "spacemelee") {
        "melee"
    } else if ancestry.contains("shotgun") {
        "shotgun"
    } else if slot == "companion" && ancestry.contains("/lotus/weapons/tenno/melee/playermeleeweapon") {
        "melee"
    } else if slot == "secondary" || (slot == "companion" && ancestry.contains("/lotus/weapons/tenno/pistol/lotuspistol")) {
        "pistol"
    } else if ancestry.contains("archwing") || ancestry.contains("archgun") {
        "archgun"
    } else {
        "rifle"
    }
}

fn riven_type(weapon: &Value) -> &'static str {
    let path = fold(text(&weapon["path"]));
    if truth(&weapon["kitgunParts"]) || path.starts_with("/lotus/weapons/solarisunited/primary/lotusmodularprimary")
        || path.starts_with("/lotus/weapons/solarisunited/secondary/lotusmodularsecondary")
    {
        "pistol"
    } else {
        weapon_type(weapon)
    }
}

fn variant_group<'a>(data: &'a EngineData, weapon: &'a Value, own_path: &'a str) -> &'a str {
    if truth(&weapon["variantType"]) {
        for path in array(&weapon["ancestry"]).iter().map(text).filter(|path| *path != own_path) {
            if data.weapons.get(path).is_some_and(|candidate| {
                RIVEN_SLOTS.contains(&text(&candidate["slot"])) && !truth(&candidate["variantType"])
                    && array(&candidate["fireModes"]).iter().any(|mode| truth(&mode["damage"]["types"]))
            }) {
                return path;
            }
        }
    }
    own_path
}

fn exalted_key(row: &Value) -> (String, String) {
    (text(&row["exalted"]["family"]).to_owned(), text(&row["slot"]).to_owned())
}

fn exalted_payload(data: &EngineData, weapon: &Value) -> Value {
    let block = &weapon["exalted"];
    if !block.is_object() {
        return Value::Null;
    }
    let rule = &data.rules.weapons["exalted"][text(&weapon["path"])];
    json!({
        "family": block["family"], "tier": block["tier"], "abilitySlot": block["abilitySlot"],
        "headshotIssue": rule["headshotIssue"], "headshotLocked": rule["headshotLocked"],
        "frames": array(&rule["frames"]),
    })
}

fn display_modes(weapon: &Value, patch: &Value) -> Vec<Value> {
    if patch["replace"] == true {
        return array(&patch["modes"]).to_vec();
    }
    let mut modes = array(&weapon["fireModes"]).to_vec();
    for (mode, changes) in modes.iter_mut().zip(array(&patch["modes"])) {
        merge(mode, changes);
    }
    modes
}

fn fire_modes(data: &EngineData, weapon: &Value, modes: &[Value], weapon_type: &str) -> Result<Vec<Value>> {
    let path = text(&weapon["path"]);
    let metrics = if weapon_type == "melee" { MELEE_METRICS } else { GUN_METRICS };
    modes.iter().enumerate().filter(|(_, mode)| mode.is_object()).map(|(index, mode)| {
        let damage = &mode["damage"];
        let auxiliary = mode["behaviorKind"] == "auxiliary-melee"
            && matches!(text(&weapon["slot"]), "primary" | "secondary" | "arch_gun");
        let action = mode["behaviorKind"] == "ability-action";
        let available = truth(&damage["types"]) && !auxiliary && !action;
        let issue = if available { Value::Null } else if action { mode["unavailableReason"].clone() }
        else if auxiliary { json!(format!("fire mode {index} is auxiliary melee, outside nonmelee scope")) }
        else { json!(format!("fire mode {index} has no direct damage types")) };
        let mut availability = serde_json::Map::new();
        if available {
            let baselines = array(&data.rules.metrics[path][index]);
            if baselines.len() != metrics.len() {
                return Err(invalid(format!("缺少 {path} 开火模式 {index} 的冻结基线")));
            }
            for ((metric, _), sample) in metrics.iter().zip(baselines) {
                let row = if sample.is_number() {
                    json!({"baseline": sample, "available": true, "issue": null})
                } else {
                    let coverage = sample[2] == true;
                    let mut row = json!({"baseline": sample[0], "available": coverage, "issue": sample[1]});
                    if coverage {
                        row["requiresCoverage"] = json!(true);
                    }
                    row
                };
                availability.insert((*metric).to_owned(), row);
            }
        }
        Ok(json!({
            "index": index, "trigger": text(&mode["trigger"]),
            "isAlternateFire": truth(&mode["isAlternateFire"]),
            "isIncarnonForm": available && is_incarnon(weapon, modes, mode, index),
            "damageTotal": positive(&damage["total"]), "available": available,
            "issue": issue, "metricAvailability": availability,
        }))
    }).collect()
}

fn is_incarnon(weapon: &Value, modes: &[Value], mode: &Value, index: usize) -> bool {
    if !truth(&weapon["isIncarnon"]) {
        return false;
    }
    if let Some(form) = mode.get("isIncarnonForm") {
        return truth(form);
    }
    match text(&weapon["slot"]) {
        "melee" | "arch_melee" => false,
        _ if weapon["path"] == "/Lotus/Weapons/Thanotech/EntratiWristGun/EntratiWristGunWeapon" => index > 0,
        _ => modes.len() > 1 && index == modes.len() - 1,
    }
}

fn incarnon(data: &EngineData, weapon: &Value) -> Result<Vec<Value>> {
    let incarnon = &weapon["incarnon"];
    let adapter = text(&incarnon["adapter"]).trim_end_matches(['/', '\\']).rsplit(['/', '\\']).next().unwrap_or("");
    let route = entries(&data.incarnon_defaults["strategies"])
        .find(|(key, _)| key.eq_ignore_ascii_case(adapter)).map(|(_, row)| &row["route"]).unwrap_or(&Value::Null);
    let mut rows = Vec::new();
    for tier in array(&incarnon["tiers"]) {
        let number = tier["tier"].as_u64().or_else(|| text(&tier["tier"]).trim_start_matches("tier").parse().ok()).unwrap_or_default();
        let branches = array(&tier["branches"]);
        if !(1..=5).contains(&number) || branches.is_empty() {
            continue;
        }
        let selectable = number != 1 && (tier["selectable"] == true || branches.len() > 1);
        let mut options = Vec::new();
        for branch in branches.iter().filter(|branch| branch.is_object()) {
            let path = text(&branch["path"]);
            let id = first_text(&[&branch["id"], &branch["path"]]);
            let name = first_text(&[&data.incarnon_strings["data"][text(&branch["nameTag"])], &branch["id"]]);
            let rule = &data.rules.incarnon[format!("{number}:{path}")];
            if rule.is_null() {
                return Err(invalid(format!("缺少灵化路线 {path} 的冻结展示规则")));
            }
            let details = array(&rule[0]).iter().map(|row| json!({"text": row[0], "calculation": row[1], "modeled": row[2]})).collect::<Vec<_>>();
            let description = array(&rule[0]).iter().map(|row| text(&row[0])).collect::<Vec<_>>().join("；");
            let unsupported = array(&rule[1]);
            options.push(json!({
                "id": id, "index": branch["index"].as_u64().filter(|i| *i != 0).unwrap_or(options.len() as u64 + 1),
                "label": if name.is_empty() { "未知路线" } else { name },
                "description": if description.is_empty() { "该路线的效果由游戏内脚本实现（无静态词条）" } else { &description },
                "complete": unsupported.is_empty() && !details.is_empty(), "details": details, "unsupported": unsupported,
            }));
        }
        rows.push(json!({
            "tier": number, "label": format!("进化 {number}"), "selectable": selectable,
            "recommended": text(&route[format!("tier{number}")]), "options": options,
        }));
    }
    Ok(rows)
}

fn stance_slot(data: &EngineData, weapon: &Value, weapon_type: &str, stances: &[Value]) -> Value {
    if weapon_type != "melee" {
        return Value::Null;
    }
    let polarity = array(&weapon["polarities"]).get(8).unwrap_or(&Value::Null);
    let path = &weapon["path"];
    if let Some(row) = data.exalted_weapons.iter().find(|row| &row["path"] == path || array(&row["portablePaths"]).contains(path)) {
        return json!({
            "locked": true, "builtIn": true, "path": first(&row["builtInStancePath"], &row["builtInStance"]),
            "nameZh": first(&row["nameZh"], &row["name"]), "nameEn": row["name"],
            "slotPolarity": polarity, "capacityGain": 0, "dpsMultiplier": 1.0, "statusRateMultiplier": 1.0,
        });
    }
    let tags = array(&weapon["compatTags"]);
    let mut compatible: Vec<&Value> = stances.iter().filter(|row| {
        tags.iter().any(|tag| text(tag).eq_ignore_ascii_case(text(&row["compatTag"])))
    }).collect();
    let order = array(&data.rules.weapons["stanceOrders"][text(path)]);
    compatible.sort_by_cached_key(|row| {
        (order.iter().position(|path| path == &row["path"]).unwrap_or(usize::MAX),
            fold(first_text(&[&row["nameZh"], &row["nameEn"]])))
    });
    let Some(selected) = compatible.first() else {
        return json!({"locked": false, "builtIn": false, "path": null, "slotPolarity": polarity,
            "capacityGain": 0, "dpsMultiplier": 1.0, "statusRateMultiplier": 1.0, "compatiblePaths": []});
    };
    let base = selected["baseDrain"].as_i64().unwrap_or_default();
    let rank = selected["maxRank"].as_i64().unwrap_or_default();
    let card = polarity_key(&selected["polarity"]);
    let slot = polarity_key(polarity);
    let negative = base < 0;
    let (numerator, denominator, relation) = if card.is_empty() || slot.is_empty() {
        (1, 1, "unpolarized")
    } else if card == slot || card == "AP_ANY" || slot == "AP_ANY" {
        (if negative { 2 } else { 1 }, if negative { 1 } else { 2 }, "matched")
    } else {
        (if negative { 1 } else { 5 }, if negative { 2 } else { 4 }, "mismatched")
    };
    let magnitude = ((base.abs() + rank) * numerator + denominator - 1) / denominator;
    let combo = &selected["bestComboData"];
    json!({
        "locked": false, "builtIn": false, "path": selected["path"], "slotPolarity": polarity,
        "capacityGain": if negative { magnitude } else { -magnitude }, "polarityRelation": relation,
        "dpsMultiplier": combo["dpsMultiplier"], "statusRateMultiplier": combo["statusRateMultiplier"],
        "bestCombo": combo, "compatiblePaths": compatible.iter().map(|r| &r["path"]).collect::<Vec<_>>(),
    })
}

fn first<'a>(left: &'a Value, right: &'a Value) -> &'a Value {
    if truth(left) { left } else { right }
}

fn polarity_key(value: &Value) -> String {
    match text(value).trim().to_ascii_uppercase().as_str() {
        "AP_UNIVERSAL" | "AP_NONE" | "NONE" | "NULL" => String::new(),
        token => token.to_owned(),
    }
}
