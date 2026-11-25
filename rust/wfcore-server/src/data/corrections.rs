use super::{invalid, EngineData, Result};
use crate::catalog::{array, entries, number, text, truth};
use serde_json::{json, Value};

pub(crate) const DARK_SPLIT: &str =
    "/Lotus/Weapons/Tenno/Melee/Swords/DarkSword/DarkSwordDaggerHybridWeapon";

pub(super) fn apply(engine: &mut EngineData, wiki_weapon_overrides: &Value) -> Result<()> {
    for (path, weapon) in &mut engine.weapons {
        if path == "/Lotus/Weapons/ClanTech/Energy/Railgun" {
            if let Some(modes) = weapon["fireModes"].as_array_mut() {
                for mode in modes {
                    if text(&mode["trigger"]).contains("Charge")
                        && number(&mode["fireRate"]).unwrap_or(0.0) <= 0.0
                        && number(&mode["fireRateRaw"]).unwrap_or(0.0) <= 0.0
                    {
                        mode["fireRate"] = json!(1.0);
                        mode["fireRateRaw"] = json!(60.0);
                    }
                }
            }
        }
        apply_wiki(weapon, &wiki_weapon_overrides["weapons"][path])?;
    }
    // Each override is consumed by a single exported enemy. Moving the fields
    // keeps the original merge order and releases the duplicate override table.
    let Value::Object(overrides) = engine.enemy_overrides["data"].take() else { unreachable!("validated by loader") };
    for (path, patch) in overrides {
        if let (Some(enemy), Value::Object(patch)) = (engine.enemies.get_mut(&path).and_then(Value::as_object_mut), patch) {
            enemy.extend(patch);
        }
    }
    for (path, arcane) in &mut engine.arcanes {
        merge(arcane, &engine.rules.arcanes["supplements"][path]);
    }
    apply_mod_releases(engine);
    apply_disposition(engine);
    apply_client_updates(engine)?;
    add_dark_split_forms(engine);
    Ok(())
}

pub(crate) fn merge(target: &mut Value, patch: &Value) {
    if let (Some(target), Some(patch)) = (target.as_object_mut(), patch.as_object()) {
        target.extend(patch.iter().map(|(key, value)| (key.clone(), value.clone())));
    }
}

fn apply_wiki(weapon: &mut Value, overrides: &Value) -> Result<()> {
    for (index, fields) in entries(&overrides["modes"]) {
        let index: usize = index.parse()?;
        let Some(mode) = weapon["fireModes"].as_array_mut().and_then(|modes| modes.get_mut(index)) else {
            continue;
        };
        let mut applied = Vec::new();
        if let Some(multishot) = fields.get("multishot") {
            mode["multishot"] = multishot.clone();
            applied.push("multishot");
        }
        if let Some(damage_types) = fields.get("damageTypes") {
            let types = entries(damage_types)
                .map(|(key, value)| Ok((key.clone(), json!(number(value).ok_or_else(|| invalid("Wiki 伤害类型必须为数值"))?))))
                .collect::<Result<serde_json::Map<_, _>>>()?;
            mode["damage"] = damage_packet(Value::Object(types), &mode["damage"]);
            applied.push("damage");
        }
        if let Some(proc_chance) = fields.get("procChance") {
            mode["damage"]["procChance"] = proc_chance.clone();
            applied.push("procChance");
        }
        mode["wikiOverride"] = json!(applied);
    }
    Ok(())
}

fn damage_packet(types: Value, base: &Value) -> Value {
    let total: f64 = entries(&types).filter_map(|(_, value)| number(value)).sum();
    let shares = entries(&types).filter(|_| total != 0.0)
        .map(|(key, value)| (key.clone(), json!(number(value).unwrap_or_default() / total)))
        .collect::<serde_json::Map<_, _>>();
    let mut packet = json!({});
    merge(&mut packet, base);
    merge(&mut packet, &json!({"types": types, "total": total, "shares": shares}));
    packet
}

fn add_dark_split_forms(engine: &mut EngineData) {
    let Some(base) = engine.weapons.get(DARK_SPLIT).filter(|base| !truth(&base["fireModes"])).cloned() else {
        return;
    };
    for (suffix, melee_path, name, tag, puncture, slash, radiation, speed, crit, multiplier, proc) in [
        ("HeavyBlade", "Single", "暗黑分合剑 · 巨刃形态", "HEAVY_BLADE_STANCE",
            78.0, 52.0, 100.0, 55.0, 0.15, 2.0, 0.25),
        ("DualSwords", "Duals", "暗黑分合剑 · 双剑形态", "DUAL_SWORDS_STANCE",
            56.0, 28.0, 32.0, 70.0, 0.25, 2.5, 0.15),
    ] {
        let path = format!("{DARK_SPLIT}#{suffix}");
        let internal = format!("DarkSwordDaggerHybridWeapon{suffix}");
        let mut form = base.clone();
        form.as_object_mut().unwrap().shift_remove("nameTag");
        let damage = damage_packet(
            json!({"DT_PUNCTURE": puncture, "DT_SLASH": slash, "DT_RADIATION": radiation}),
            &json!({"critChance": crit, "critMultiplier": multiplier, "procChance": proc}),
        );
        merge(&mut form, &json!({
            "path": path, "name": internal, "nameInternal": internal, "nameZh": name,
            "nameEn": format!("Dark Split-Sword ({suffix})"),
            "ancestry": [path, DARK_SPLIT, "/Lotus/Weapons/Tenno/Melee/PlayerMeleeWeapon",
                "/Lotus/Types/Game/LotusMeleeWeapon"],
            "compatTags": [tag],
            "omegaAttenuation": number(&base["omegaAttenuation"]).filter(|v| *v != 0.0).unwrap_or(1.3),
            "meleePath": format!("/Lotus/Weapons/Tenno/Melee/Swords/DarkSword/DarkSwordDagger{melee_path}"),
            "fireModes": [{
                "ammoCost": 0, "ammoType": "", "chargeTime": null, "damage": damage,
                "falloffMinDamage": 1, "falloffRange": [0, 0], "fireRate": speed / 60.0,
                "fireRateRaw": speed, "index": 0, "isAlternateFire": false,
                "multishot": 1, "reloadTime": 1, "trigger": ""
            }]
        }));
        engine.weapons.insert(path, form);
    }
}

fn apply_mod_releases(engine: &mut EngineData) {
    let rules = &engine.rules.mods;
    let mut patches = Vec::new();
    for (path, row) in &engine.mods {
        if let Some(base_path) = rules["unreleased"][path].as_str() {
            if let Some(base) = engine.mods.get(base_path) {
                if row["maxRank"].as_i64() == Some(10)
                    && base["maxRank"].as_i64().is_some_and(|rank| rank < 10)
                    && row["rarity"] != "LEGENDARY" && truth(&row["nameTag"]) && truth(&row["modFamily"])
                    && ["nameTag", "modFamily", "compatibility"].iter().all(|key| row[*key] == base[*key])
                {
                    patches.push((path.clone(), json!({"selectable": false, "catalogExclusion": {
                        "reason": "unreleased-tier-clone", "basePath": base_path, "evidence": "expert-catalog-20260927"
                    }})));
                }
            }
        }
        if let Some(canonical) = rules["duplicate"][path].as_str() {
            if let Some(base) = engine.mods.get(canonical) {
                if base["selectable"] == true && truth(&row["modFamily"])
                    && array(&rules["equivalentFields"]).iter().all(|key| row[text(key)] == base[text(key)])
                {
                    patches.push((path.clone(), json!({"optimizerDuplicateOf": canonical})));
                }
            }
        }
        let evidence = &rules["archgunReleases"][path];
        if evidence.is_object() && row["nameEn"] == evidence["nameEn"]
            && row["maxRank"].as_i64() == Some(10) && row["selectable"] == false
            && row["compatibility"] == "/Lotus/Weapons/Tenno/Archwing/Primary/ArchGun"
            && !truth(&row["isSubUpgrade"]) && truth(&row["icon"])
            && array(&row["effects"]).iter().any(|effect| {
                effect["upgradeType"] == "WEAPON_PERCENT_BASE_DAMAGE_ADDED"
                    && effect["damageType"] == evidence["element"] && effect["operation"] == "ADD"
                    && number(&effect["valuePerRank"]) == Some(0.17)
            })
        {
            patches.push((path.clone(), json!({"selectable": true, "releaseOverride": evidence})));
        }
    }
    for (path, patch) in patches {
        merge(engine.mods.get_mut(&path).unwrap(), &patch);
    }
}

fn apply_disposition(engine: &mut EngineData) {
    let setting = &engine.mechanics["riven-disposition"];
    let Some(value) = number(&setting["value"]).filter(|value| *value > 0.0 && setting["mode"] == "uniform") else {
        return;
    };
    engine.riven_disposition = json!({"mode": "uniform", "value": value, "effective": setting["effective"]});
    let modular = [&mut engine.kitguns["barrels"], &mut engine.zaws["strikes"]]
        .into_iter().filter_map(Value::as_object_mut).flat_map(|rows| rows.values_mut());
    for row in engine.weapons.values_mut().chain(modular) {
        for (field, legacy) in [
            ("omegaAttenuation", "legacyOmegaAttenuation"),
            ("primaryOmegaAttenuation", "legacyPrimaryOmegaAttenuation"),
            ("primeOmegaAttenuation", "legacyPrimeOmegaAttenuation"),
        ] {
            if number(&row[field]).is_some_and(|v| v > 0.0) {
                let old = row[field].clone();
                row.as_object_mut().unwrap().entry(legacy).or_insert(old);
                row[field] = json!(value);
            }
        }
    }
    for template in engine.riven_templates["templates"].as_array_mut().into_iter().flatten() {
        for row in template["dispositions"].as_array_mut().into_iter().flatten() {
            if number(&row["attenuation"]).is_some_and(|v| v > 0.0) {
                let old = row["attenuation"].clone();
                row.as_object_mut().unwrap().entry("legacyAttenuation").or_insert(old);
                row["attenuation"] = json!(value);
            }
        }
    }
}

fn apply_client_updates(engine: &mut EngineData) -> Result<()> {
    for patch in array(&engine.mechanics["client-updates"]["patches"]) {
        for path in array(&patch["paths"]).iter().map(text) {
            let mut found = false;
            match text(&patch["kind"]) {
                "arcane" => {
                    if let Some(record) = engine.arcanes.get_mut(path) {
                        apply_update(record, patch)?;
                        found = true;
                    }
                }
                "incarnon" => {
                    for weapon in engine.weapons.values_mut() {
                        let Some(incarnon) = weapon.get_mut("incarnon") else { continue };
                        for tier in incarnon["tiers"].as_array_mut().into_iter().flatten() {
                            for branch in tier["branches"].as_array_mut().into_iter().flatten() {
                                if branch["path"] == path {
                                    apply_update(branch, patch)?;
                                    found = true;
                                }
                            }
                        }
                    }
                }
                _ => {}
            }
            if !found {
                return Err(invalid(format!("客户端补丁 {} 找不到 {path}", patch["id"])));
            }
        }
    }
    Ok(())
}

fn apply_update(record: &mut Value, patch: &Value) -> Result<()> {
    let context = format!("客户端补丁 {} {}", patch["id"], record["path"]);
    for rule in array(&patch["effects"]) {
        let mut hits = 0;
        let record_fields = record.as_object_mut().unwrap();
        for (field, body) in record_fields {
            let Some(rows) = body.as_array_mut() else { continue };
            match field.as_str() {
                "effects" => update_effects(rows, rule, patch, &mut hits, &context)?,
                "conditionalEffects" => {
                    for row in rows {
                        if let Some(effects) = row["effects"].as_array_mut() {
                            update_effects(effects, rule, patch, &mut hits, &context)?;
                        }
                    }
                }
                _ => {}
            }
        }
        if hits == 0 {
            return Err(invalid(format!("{context} 缺少效果 {}", rule["upgradeType"])));
        }
    }
    if let Some(conditions) = record.get_mut("conditionalEffects").and_then(Value::as_array_mut) {
        for row in conditions {
            if let Some(stacks) = patch.get("maxStacks") {
                patch_number(&mut row["maxStacks"], stacks, &context)?;
            }
            if let Some(tag) = patch.get("conditionTag") {
                row["conditionTag"] = tag.clone();
            }
        }
    }
    if let Some(trigger) = record.get_mut("trigger").filter(|v| v.is_object()) {
        if let Some(tag) = patch.get("conditionTag") {
            trigger["conditionTag"] = tag.clone();
        }
        for (field, values) in entries(&patch["trigger"]) {
            patch_number(&mut trigger[field], values, &context)?;
        }
    } else if truth(&patch["trigger"]) {
        return Err(invalid(format!("{context} 缺少 trigger")));
    }
    for field in ["effectsZhByRank", "effectsEnByRank"] {
        if truth(&patch[field]) {
            record[field] = patch[field].clone();
        }
    }
    Ok(())
}

fn patch_number(value: &mut Value, change: &Value, context: &str) -> Result<()> {
    if !same_number(value, &change[0]) && !same_number(value, &change[1]) {
        return Err(invalid(format!("{context} 数值 {value} 不符合预期 {change}")));
    }
    *value = change[1].clone();
    Ok(())
}

fn same_number(a: &Value, b: &Value) -> bool {
    number(a).zip(number(b)).is_some_and(|(a, b)| (a - b).abs() <= 1e-9)
}

fn update_effects(rows: &mut [Value], rule: &Value, patch: &Value, hits: &mut usize, context: &str) -> Result<()> {
    for effect in rows.iter_mut().filter(|effect| effect["upgradeType"] == rule["upgradeType"]) {
        let from = &rule["from"];
        let to = &rule["to"];
        if effect["operation"] == from[0] && same_number(&effect["valuePerRank"], &from[1]) {
            effect["operation"] = to[0].clone();
            effect["valuePerRank"] = to[1].clone();
            if let Some(tag) = patch.get("locTag") {
                effect["locTag"] = tag.clone();
            }
        } else if effect["operation"] != to[0] || !same_number(&effect["valuePerRank"], &to[1]) {
            return Err(invalid(format!("{context} 效果不符合预期：{effect}")));
        }
        *hits += 1;
    }
    Ok(())
}
