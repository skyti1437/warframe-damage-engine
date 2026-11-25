use super::{array, first, first_text, fold, number, object, sort_named, text, truth};
use crate::data::{corrections::merge, EngineData};
use indexmap::IndexMap;
use serde_json::{json, Map, Value};
use std::collections::{HashMap, HashSet};

pub(super) struct ModCatalog {
    pub mods: Vec<Value>,
    pub utility: Vec<Value>,
    pub aliases: Map<String, Value>,
    pub variants: Vec<Value>,
    pub replacements: Map<String, Value>,
}

pub(super) fn rows(data: &EngineData) -> ModCatalog {
    let rules = &data.rules.mods;
    let shells: HashMap<_, _> = data.mods.values()
        .filter(|mod_| truth(&mod_["modSet"]) && mod_["setPieces"].as_i64().is_some())
        .map(|mod_| (text(&mod_["modSet"]), mod_)).collect();
    let supported: HashSet<_> = array(&rules["supportedUpgrades"]).iter().map(text).collect();
    let runtime: HashSet<_> = array(&rules["runtimePaths"]).iter().map(text).collect();
    let mut canonical = IndexMap::new();
    let mut all = IndexMap::new();
    let mut paths = IndexMap::<_, Vec<_>>::new();
    for (path, mod_) in &data.mods {
        let overrides = &data.weapon_mechanics_overrides["mods"][path];
        let compatible = text(&mod_["compatibility"]);
        let supported_effect = |effect: &Value| {
            if effect["supported"] == true {
                !truth(&effect["supportedBy"]) || array(&effect["supportedBy"]).iter().any(|by| by == "weapon")
            } else {
                supported.contains(text(&effect["upgradeType"]).to_ascii_uppercase().as_str())
            }
        };
        let effects = array(&mod_["effects"]);
        let admitted = effects.iter().any(supported_effect)
            || mod_["nameEn"] == "Primed Reach"
            || effects.iter().any(|e| text(&e["upgradeType"]).eq_ignore_ascii_case("WEAPON_INIT_DAMAGE_MOD"))
            || truth(overrides) || runtime.contains(path.as_str());
        if mod_["selectable"] == false
            || (path.contains("/PvPMods/") && overrides.is_null())
            || fold(compatible).contains("powersuit") || fold(path).contains("/aura/")
            || path == "/Lotus/Upgrades/Mods/Rifle/Expert/SniperReloadDamageModExpert"
            || (!admitted && (!truth(&mod_["support"]["weapon"]) || mod_["supported"] == false))
            || (effects.is_empty() && fold(path).contains("submod"))
        {
            continue;
        }
        let effects: Vec<_> = effects.iter().map(|effect| {
            let mut effect = effect.clone();
            if effect["supported"] == false && supported.contains(text(&effect["upgradeType"]).to_ascii_uppercase().as_str()) {
                effect["supported"] = json!(true);
                effect["supportedBy"] = json!(["weapon"]);
            }
            effect
        }).collect();
        let notes: Vec<_> = effects.iter().filter(|effect| effect["supported"] == false)
            .map(|effect| text(&effect["upgradeType"])).collect();
        let set_id = text(&mod_["modSet"]);
        let set = &rules["sets"][set_id];
        let shell = shells.get(set_id).copied().unwrap_or(&Value::Null);
        let indexed = &data.mod_art["mods"][path];
        let name_zh = first_text(&[&mod_["nameZh"], &mod_["name"]]);
        let name_en = first_text(&[&mod_["nameEn"], &mod_["name"]]);
        let mut description = text(&mod_["description"]).to_owned();
        if truth(&indexed["description"]) {
            description = if indexed["description"].is_array() {
                array(&indexed["description"]).iter().map(text).collect::<Vec<_>>().join("；")
            } else {
                text(&indexed["description"]).to_owned()
            };
        }
        let art = indexed.get("art").filter(|art| truth(art));
        let row = json!({
            "path": path, "nameZh": name_zh, "nameEn": name_en, "nameInternal": text(&mod_["nameInternal"]),
            "compatibility": compatible, "compatibilityTags": array(&mod_["compatibilityTags"]),
            "incompatibilityTags": array(&mod_["incompatibilityTags"]),
            "polarity": mod_["polarity"], "rarity": fallback(&mod_["rarity"], "COMMON"),
            "maxRank": mod_["maxRank"], "baseDrain": mod_["baseDrain"], "modFamily": mod_["modFamily"],
            "modSet": (!set_id.is_empty()).then_some(set_id), "modSetValues": array(&mod_["modSetValues"]),
            "setMaxPieces": first(&[&shell["setPieces"], &set["maxPieces"], &mod_["setMaxPieces"]]),
            "setName": first(&[&set["name"], &mod_["setName"]]),
            "setDescription": first(&[&set["description"], &mod_["setDescription"]]),
            "setDesc": first(&[&set["description"], &mod_["setDesc"], &mod_["setDescription"]]),
            "setDescriptionTag": first(&[&set["descriptionTag"], &mod_["setDescriptionTag"]]),
            "setEffects": array(first(&[&set["effects"], &mod_["setEffects"]])),
            "setStats": array(first(&[&set["setStats"], &set["effects"], &mod_["setStats"], &mod_["setEffects"]])),
            "setConditionalEffects": array(first(&[&set["conditionalEffects"], &mod_["setConditionalEffects"]])),
            "setDisplayValues": object(first(&[&set["displayValues"], &mod_["setDisplayValues"]])),
            "setRuntime": object(first(&[&set["runtime"], &mod_["setRuntime"]])),
            "setSource": first(&[&set["sourcePath"], &mod_["setSource"]]),
            "source": first(&[&set["sourcePath"], &mod_["source"], &mod_["setSource"]]),
            "setSupported": (!set_id.is_empty()).then(|| array(&rules["supportedSets"]).iter().any(|v| v == set_id)),
            "category": category(mod_), "slotKind": fallback(&mod_["slotKind"], "weapon"),
            "type": fallback(first(&[&mod_["type"], &mod_["slotKind"]]), "weapon"),
            "sourceType": fallback(&mod_["sourceType"], "Weapon Mod"),
            "sourceCompatibility": first_text(&[&mod_["sourceCompatibility"], &mod_["compatibility"]]),
            "description": description, "unsupportedNotes": (!notes.is_empty()).then_some(notes),
            "effects": effects, "conditionalEffects": array(&mod_["conditionalEffects"]),
            "mechanicOverride": overrides, "iconAsset": art, "art": art,
            "effectsZhByRank": indexed["effectsZhByRank"], "effectsZh": indexed["effectsZh"],
        });
        let identity = (if name_zh.is_empty() { name_en } else { name_zh }, compatible);
        paths.entry(identity).or_default().push(path.as_str());
        canonical.entry(identity).and_modify(|current: &mut &str| {
            if preference(path, &row) < preference(current, &all[*current]) {
                *current = path;
            }
        }).or_insert(path.as_str());
        all.insert(path.as_str(), row);
    }
    let canonical_paths: HashSet<_> = canonical.values().copied().collect();
    let utility = utility_rows(data, &all, &canonical_paths);
    let utility_paths: HashSet<_> = utility.iter().map(|row| text(&row["path"])).collect();
    let mut mods: Vec<_> = canonical_paths.iter().filter(|path| !utility_paths.contains(**path))
        .map(|path| all[*path].clone()).collect();
    let mut variants: Vec<_> = all.iter().filter(|(path, _)| !canonical_paths.contains(**path))
        .map(|(_, row)| row.clone()).collect();
    // 派系标记属于普通选择器和分阶变体，扩展槽已在上方合并。
    for row in mods.iter_mut().chain(&mut variants) {
        if array(&row["effects"]).iter().any(|effect| {
            text(&effect["upgradeType"]).eq_ignore_ascii_case("GAMEPLAY_FACTION_DAMAGE")
        }) {
            row["factionDamage"] = json!(true);
        }
    }
    sort_named(&mut mods);
    sort_named(&mut variants);
    let aliases: Map<_, _> = paths.into_iter().flat_map(|(identity, paths)| {
        let path = canonical[&identity];
        paths.into_iter().map(move |alias| (alias.to_owned(), json!(path)))
    }).collect();
    let replacements = legacy_replacements(data, &all, &utility, &aliases);
    ModCatalog { mods, utility, aliases, variants, replacements }
}

fn preference<'a>(path: &'a str, row: &Value) -> (bool, std::cmp::Reverse<i64>, &'a str) {
    let lower = fold(path);
    let tiered = ["/beginner/", "/intermediate/", "/expert/"].iter().any(|part| lower.contains(part));
    (tiered, std::cmp::Reverse(row["maxRank"].as_i64().filter(|rank| *rank != 0).unwrap_or(-1)), path)
}

fn fallback<'a>(value: &'a Value, default: &'a str) -> &'a str {
    value.as_str().filter(|s| !s.is_empty()).unwrap_or(default)
}

fn category(mod_: &Value) -> &str {
    let kind = text(&mod_["slotKind"]);
    if matches!(kind, "aura" | "exilus" | "warframe") { return kind; }
    if truth(&mod_["modSet"]) { return "set"; }
    let effects = array(&mod_["effects"]);
    if effects.iter().any(|effect| matches!(text(&effect["damageType"]).to_ascii_uppercase().as_str(),
        "DT_FIRE" | "DT_FREEZE" | "DT_ELECTRICITY" | "DT_POISON" | "DT_MAGNETIC" |
        "DT_RADIATION" | "DT_VIRAL" | "DT_CORROSIVE" | "DT_EXPLOSION" | "DT_GAS")
    ) { return "element"; }
    let kinds: HashSet<_> = effects.iter().map(|effect| text(&effect["upgradeType"]).to_ascii_uppercase()).collect();
    for (category, keys) in [
        ("critical", &["WEAPON_CRIT_CHANCE", "WEAPON_CRIT_DAMAGE", "WEAPON_NOCRIT_DAMAGE"][..]),
        ("multishot", &["WEAPON_FIRE_ITERATIONS"]),
        ("status", &["WEAPON_PROC_CHANCE", "WEAPON_PROC_DAMAGE", "WEAPON_PROC_TIME", "WEAPON_DAMAGE_IF_VICTIM_PROC_ACTIVE"]),
        ("speed", &["WEAPON_FIRE_RATE"]),
        ("handling", &["WEAPON_RELOAD_SPEED", "WEAPON_CLIP_MAX", "WEAPON_AMMO_MAX"]),
        ("damage", &["WEAPON_DAMAGE_AMOUNT", "WEAPON_MELEE_DAMAGE", "WEAPON_PERCENT_BASE_DAMAGE_ADDED", "GAMEPLAY_FACTION_DAMAGE"]),
    ] {
        if keys.iter().any(|key| kinds.contains(*key)) { return category; }
    }
    "utility"
}

fn utility_rows(data: &EngineData, manual: &IndexMap<&str, Value>, canonical: &HashSet<&str>) -> Vec<Value> {
    let rules = &data.rules.mods;
    let mut rows = data.utility_mods.clone();
    let existing: HashSet<_> = rows.iter().map(|row| text(&row["path"]).to_owned()).collect();
    rows.extend(array(&rules["utilityPaths"]).iter().map(text).filter_map(|path| {
        let mod_ = data.mods.get(path).filter(|row| row["selectable"] == true && !existing.contains(path))?;
        let kind = utility_slot(path, text(&mod_["compatibility"]))?;
        let rank = mod_["maxRank"].as_i64().unwrap_or_default();
        let base = mod_["baseDrain"].as_i64().unwrap_or_default();
        let indexed_art = text(&data.mod_art["mods"][path]["art"]);
        let fixup_icon = text(&rules["utilityIcons"][path]).trim_start_matches('/');
        let icon = text(&mod_["icon"]);
        let file = if !fixup_icon.is_empty() && data.root.join("frontend/assets/mods/art").join(fixup_icon).is_file() {
            fixup_icon
        } else {
            icon.trim_start_matches('/')
        };
        let icon_asset = if !indexed_art.is_empty() {
            json!(indexed_art)
        } else if !file.is_empty() {
            json!(format!("/assets/mods/art/{file}"))
        } else { Value::Null };
        let lines = utility_lines(mod_, &rules["utilityEffectLabels"], rank);
        let description = lines.as_ref().and_then(|lines| lines.last())
            .map(|lines| lines.join("；")).unwrap_or_else(|| text(&mod_["description"]).to_owned());
        Some(json!({
            "path": path, "nameEn": text(&mod_["nameEn"]), "nameZh": text(&mod_["nameZh"]),
            "slotKind": kind, "exilus": true, "polarity": mod_["polarity"],
            "rarity": fallback(&mod_["rarity"], "COMMON"), "description": description,
            "effectsZh": lines.as_ref().and_then(|lines| lines.last()), "effectsZhByRank": lines,
            "effects": array(&mod_["effects"]), "conditionalEffects": array(&mod_["conditionalEffects"]),
            "drain": (base + rank).max(0), "rank": rank, "icon": icon, "iconAsset": icon_asset,
            "sourceKind": "export-fixup", "catalogJoined": true,
        }))
    }));
    rows.into_iter().map(|sidecar| {
        let path = text(&sidecar["path"]);
        let exported = data.mods.get(path).unwrap_or(&Value::Null);
        let mut row = json!({});
        merge(&mut row, exported);
        if let Some(mod_) = manual.get(path).filter(|_| canonical.contains(path)) { merge(&mut row, mod_); }
        merge(&mut row, &sidecar);
        let set_id = text(&exported["modSet"]);
        if !set_id.is_empty() {
            let set = &rules["sets"][set_id];
            merge(&mut row, &json!({
                "modSet": set_id, "modSetValues": array(&exported["modSetValues"]),
                "setMaxPieces": set["maxPieces"], "setName": set["name"], "setNameZh": set["nameZh"],
                "setDescription": set["description"], "setDesc": first(&[&set["setDesc"], &set["description"]]),
                "setStats": array(first(&[&set["setStats"], &set["effects"]])),
                "setConditionalEffects": array(&set["conditionalEffects"]),
                "setDisplayValues": object(&set["displayValues"]), "setRuntime": object(&set["runtime"]),
                "setSource": set["sourcePath"],
            }));
        }
        row["category"] = json!(if set_id.is_empty() { "exilus" } else { "set" });
        row["type"] = json!("weapon-exilus");
        row["sourceType"] = json!("Weapon Exilus");
        if row["maxRank"].is_null() {
            row["maxRank"] = row.get("rank").cloned().unwrap_or(json!(0));
        }
        row
    }).collect()
}

fn utility_slot(path: &str, compatibility: &str) -> Option<&'static str> {
    let compatible = fold(compatibility.trim_end_matches('/'));
    if compatible.ends_with("playermeleeweapon") { return Some("melee"); }
    if ["lotuslonggun", "lotusshotgun", "lotusbow"].iter().any(|s| compatible.ends_with(s)) { return Some("primary"); }
    if ["lotuspistol", "lotusshortgun"].iter().any(|s| compatible.ends_with(s)) { return Some("secondary"); }
    if path.split('/').any(|part| part == "EmpoweredHeavyMelee") { return Some("melee"); }
    match path.split_once("/Mods/")?.1.split('/').next()? {
        "Melee" => Some("melee"),
        "Rifle" | "Shotgun" => Some("primary"),
        "Pistol" => Some("secondary"),
        _ => None,
    }
}

fn utility_lines(mod_: &Value, labels: &Value, max_rank: i64) -> Option<Vec<Vec<String>>> {
    let mut lines = None;
    for effect in array(&mod_["effects"]) {
        let Some(label) = labels[text(&effect["upgradeType"])].as_str() else { continue; };
        let Some(value) = number(&effect["valuePerRank"]) else { continue; };
        let levels = lines.get_or_insert_with(|| vec![Vec::new(); (max_rank + 1) as usize]);
        for (rank, texts) in levels.iter_mut().enumerate() {
            let amount = value * (rank + 1) as f64;
            let formatted = if amount.abs() <= 10.0 {
                format!("{:+.1}% {label}", amount * 100.0).replace(".0%", "%")
            } else {
                format!("{amount:+.1} {label}")
            };
            texts.push(formatted);
        }
    }
    lines
}

fn legacy_replacements(data: &EngineData, manual: &IndexMap<&str, Value>, utility: &[Value], aliases: &Map<String, Value>) -> Map<String, Value> {
    data.mods.iter().filter_map(|(path, row)| {
        let excluded = &row["catalogExclusion"];
        let base_path = excluded["basePath"].as_str()?;
        let row = utility.iter().find(|row| row["path"] == base_path)
            .or_else(|| manual.get(base_path))
            .or_else(|| manual.get(text(aliases.get(base_path)?)))?;
        row["maxRank"].as_i64()?;
        Some((path.clone(), json!({
            "path": row["path"], "maxRank": row["maxRank"], "nameZh": row["nameZh"],
            "nameEn": row["nameEn"], "reason": excluded["reason"],
        })))
    }).collect()
}
