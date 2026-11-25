use super::{json, num, rows, text, EngineData, SimResult, Value};
use indexmap::IndexSet;
use std::collections::{BTreeSet, HashMap};

pub(super) fn assemble(data: &EngineData, base: &Value, raw: &Value) -> SimResult<Option<Value>> {
    if raw.is_null() { return Ok(None); }
    let parts = raw.as_array().filter(|list| (1..=3).contains(&list.len())).ok_or("kitgunParts must be an array of 1–3 paths")?;
    let paths = parts.iter().map(|value| {
        let path = text(value).trim().replace('\\', "/").trim_end_matches('/').to_owned();
        if path.is_empty() { return Err("kitgunParts must contain non-empty paths".to_owned()); }
        Ok(path)
    }).collect::<SimResult<Vec<_>>>()?;
    let legacy: HashMap<_, _> = ["strikes", "grips", "links"].into_iter().flat_map(|key| {
        data.zaws[key].as_object().into_iter().flatten().filter_map(|(path, item)| {
            item["legacyPath"].as_str().map(|old| (old, path.as_str()))
        })
    }).collect();
    let is_legacy = paths.iter().any(|path| legacy.get(path.as_str()).is_some_and(|p| !data.zaws["strikes"][*p].is_null()));
    let parts: IndexSet<_> = paths.iter().map(|path| {
        if is_legacy { legacy.get(path.as_str()).copied().unwrap_or(path) } else { path.as_str() }
    }).collect();
    if parts.len() != 3 { return Err("assembly requires exactly three distinct component paths".into()); }
    if parts.iter().any(|path| data.zaws["strikes"][*path].is_object()) {
        zaw(base, &parts, &data.zaws).map(Some)
    } else {
        kitgun(base, &parts, &data.kitguns).map(Some)
    }
}

fn part<'a>(table: &'a Value, paths: &IndexSet<&str>) -> SimResult<(&'a str, &'a Value)> {
    let mut selected = table.as_object().into_iter().flatten().filter(|(path, _)| paths.contains(path.as_str()));
    let (path, row) = selected.next().ok_or("missing compatible component path")?;
    if selected.next().is_some() { return Err("expected one component for each slot".into()); }
    Ok((path, row))
}

fn part_override<'a>(component: &'a Value, chamber: &Value) -> SimResult<&'a Value> {
    rows(&component["overrides"]).iter().find(|row| {
        row["barrelType"] == chamber["barrelType"] && row["gunType"] == chamber["gunType"]
    }).ok_or_else(|| format!("Kitgun component has no {}/{} override", chamber["barrelType"], chamber["gunType"]))
}

fn name(record: &mut Value, part: &Value, suffix: &str, fallback: &str, zh: &str) {
    let en = part["nameEn"].as_str().filter(|v| !v.is_empty()).unwrap_or(suffix);
    record["nameInternal"] = json!(format!("{}{suffix}", en.replace(' ', "")));
    record["name"] = record["nameInternal"].clone();
    record["nameEn"] = json!(format!("{en} {suffix}"));
    record["nameZh"] = json!(format!("{} {zh}", part["nameZh"].as_str().filter(|value| !value.is_empty()).unwrap_or(fallback)));
}

fn tags(record: &mut Value, extra: impl Iterator<Item = String>) {
    let tags: BTreeSet<_> = rows(&record["compatTags"]).iter().map(|v| text(v).to_owned()).chain(extra).collect();
    record["compatTags"] = json!(tags);
}

fn round(value: f64, places: usize) -> f64 {
    format!("{value:.places$}").parse().unwrap_or(value)
}

fn kitgun_damage(damage: &mut Value, delta: f64) {
    let count = damage["types"].as_object().map_or(0, |v| v.len());
    let types: serde_json::Map<_, _> = damage["types"].as_object().into_iter().flatten().filter_map(|(key, value)| {
        let amount = round((num(value) + delta / count as f64).max(0.0), 6);
        (amount != 0.0).then(|| (key.clone(), json!(amount)))
    }).collect();
    let total = types.values().map(num).sum::<f64>();
    let shares: serde_json::Map<_, _> = types.iter().map(|(key, value)| (key.clone(), json!(num(value) / total))).collect();
    damage["total"] = json!(total);
    damage["types"] = json!(types);
    damage["shares"] = json!(shares);
}

fn kitgun(base: &Value, parts: &IndexSet<&str>, rules: &Value) -> SimResult<Value> {
    let (_, barrel) = part(&rules["barrels"], parts)?;
    let (_, grip) = part(&rules["handles"], parts)?;
    let (_, loader) = part(&rules["clips"], parts)?;
    let slot = text(&base["slot"]);
    if !matches!(slot, "primary" | "secondary") || grip["slot"] != slot {
        return Err("Kitgun grip does not match the weapon slot".into());
    }
    let grip = part_override(grip, barrel)?;
    let loader = part_override(loader, barrel)?;
    let source_modes = rows(&barrel[format!("{slot}Modes")]);
    if source_modes.is_empty() { return Err("Kitgun chamber has no fire mode for this slot".into()); }
    let mut weapon = base.clone();
    name(&mut weapon, barrel, "Kitgun", "组合枪", "组合枪");
    weapon["magazineSize"] = loader["magazineSize"].clone();
    weapon["omegaAttenuation"] = if slot == "primary" && !barrel["primaryOmegaAttenuation"].is_null() {
        barrel["primaryOmegaAttenuation"].clone()
    } else { barrel["omegaAttenuation"].clone() };
    tags(&mut weapon, rows(&barrel["compatTags"]).iter().map(|v| text(v).to_owned()));
    weapon["kitgunParts"] = json!(parts.iter().copied().collect::<Vec<_>>());
    let mut modes = Vec::new();
    let mut unsupported = BTreeSet::new();
    for (index, mode) in source_modes.iter().enumerate() {
        let mut mode = mode.clone();
        mode["index"] = json!(index);
        mode["fireRateRaw"] = grip["fireRate"].clone();
        mode["fireRate"] = json!(num(&grip["fireRate"]) / 60.0);
        mode["reloadTime"] = loader["reloadTime"].clone();
        if num(&grip["traceDistance"]) != 0.0 { mode["traceDistance"] = grip["traceDistance"].clone(); }
        unsupported.extend(rows(&mode["unsupported"]).iter().map(|v| text(v).to_owned()));
        let damage = &mut mode["damage"];
        if !damage.is_object() { return Err("Kitgun chamber fire mode has no direct damage".into()); }
        kitgun_damage(damage, num(&grip["damage"]));
        if damage["radial"].is_object() {
            kitgun_damage(&mut damage["radial"], num(&grip["damage"]));
            if !super::truth(&damage["types"]) && num(&damage["radial"]["total"]) > 0.0 {
                return Err("radial-only Kitgun damage is unavailable".into());
            }
            unsupported.insert("kitgun-radial-damage-unapplied".into());
        }
        for key in ["critChance", "critMultiplier", "procChance"] { damage[key] = loader[key].clone(); }
        modes.push(mode);
    }
    weapon["fireModes"] = json!(modes);
    weapon["kitgunUnsupported"] = json!(unsupported);
    Ok(weapon)
}

fn zaw(base: &Value, parts: &IndexSet<&str>, rules: &Value) -> SimResult<Value> {
    let (path, strike) = part(&rules["strikes"], parts)?;
    let (_, grip) = part(&rules["grips"], parts)?;
    let (_, link) = part(&rules["links"], parts)?;
    let mut weapon = base.clone();
    name(&mut weapon, strike, "Zaw", "Zaw", "组合近战");
    weapon["omegaAttenuation"] = strike["omegaAttenuation"].clone();
    weapon["zawParts"] = json!(parts.iter().copied().collect::<Vec<_>>());
    let two = grip["handed"].as_str().unwrap_or("two") == "two";
    let class = strike[if two { "twoHanded" } else { "oneHanded" }].as_str().unwrap_or("Sword");
    weapon["zawWeaponClass"] = json!(class);
    weapon["zawHanded"] = json!(if two { "two" } else { "one" });
    weapon["zawStrike"] = json!(path);
    let tag = match class {
        "Dagger" => "DAGGERS_STANCE", "Hammer" => "HAMMERS_STANCE", "Heavy Blade" => "HEAVY_BLADE_STANCE",
        "Machete" => "MACHETES_STANCE", "Nikana" => "NIKANAS_STANCE", "Polearm" => "POLEARMS_STANCE",
        "Rapier" => "RAPIER_STANCE", "Scythe" => "SCYTHES_STANCE", "Staff" => "STAVES_STANCE", "Sword" => "SWORDS_STANCE",
        _ => "",
    };
    tags(&mut weapon, (!tag.is_empty()).then(|| tag.to_owned()).into_iter());
    let damage = &strike["damage"];
    let bonus = num(&grip["damageBonus"]) + num(&link["damageBonus"]);
    let factor = if two { strike["twoHandedDamageFactor"].as_f64().filter(|n| *n != 0.0).unwrap_or(1.0) } else { 1.0 };
    let total = ((num(&damage["total"]) + bonus) * factor).max(0.0);
    let speed = grip["baseSpeed"].as_f64().filter(|n| *n != 0.0).unwrap_or(0.917)
        + num(&strike["speedMod"]) + num(&link["speedBonus"]);
    let scale = if bonus != 0.0 || factor != 1.0 {
        total / damage["types"].as_object().into_iter().flatten().map(|(_, v)| num(v).max(0.0)).sum::<f64>().max(1e-9)
    } else { 1.0 };
    let types: serde_json::Map<_, _> = damage["types"].as_object().into_iter().flatten().filter_map(|(key, value)| {
        value.as_f64().map(|amount| (key.clone(), json!(round(amount.max(0.0) * scale, 4))))
    }).collect();
    weapon["fireModes"] = json!([{
        "ammoCost":0, "damage":{
            "critChance": (num(&strike["critChance"]) + num(&link["critChance"])).max(0.0),
            "critMultiplier": strike["critMultiplier"].as_f64().filter(|n| *n != 0.0).unwrap_or(2.0).max(1.0),
            "procChance": (num(&strike["procChance"]) + num(&link["statusChance"])).clamp(0.0, 1.0),
            "total":total, "types":types,
        }, "falloffMinDamage":1, "falloffRange":[0,0], "fireRate":round(speed, 6).max(0.05),
        "fireRateRaw":round(speed.max(0.05) * 60.0, 3), "index":0, "isAlternateFire":false,
        "multishot":1, "reloadTime":1, "trigger":"",
    }]);
    Ok(weapon)
}

pub(super) fn innate(body: &super::Map<String, Value>, weapon: &Value) -> SimResult<Value> {
    let candidates = rows(&weapon["innateElementCandidates"]);
    let element = body.get("innateElement").or_else(|| body.get("progenitorElement")).unwrap_or(&Value::Null);
    let absent = element.is_null() || element == "";
    if candidates.is_empty() {
        if absent { return Ok(Value::Null); }
        return Err("innateElement requires a weapon with exported candidates".into());
    }
    let element = if absent { candidates.iter().find(|value| *value == "DT_FIRE").unwrap_or(&candidates[0]) } else { element };
    if !candidates.contains(element) { return Err("innateElement must be one of the selected weapon candidates".into()); }
    let value = body.get("innateElementValue").or_else(|| body.get("progenitorBonus")).map_or(Some(0.60), Value::as_f64)
        .filter(|n| (0.25..=0.60).contains(n)).ok_or("innateElementValue must be between 0.25 and 0.60")?;
    Ok(json!({"damageType":element, "value":value, "percent":value * 100.0}))
}
