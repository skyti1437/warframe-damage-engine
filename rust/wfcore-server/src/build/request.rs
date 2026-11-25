use super::{json, rows, text, truth, Map, SimCatalog, SimResult, Value, BENCHMARK_TARGET};
use std::collections::HashSet;

#[derive(Clone)]
pub(super) struct Scope {
    pub profile: &'static str,
    pub metric: String,
    pub event: bool,
    pub element: Value,
    pub required: Vec<String>,
    pub forbidden: Vec<String>,
    pub min: Option<usize>,
    pub max: Option<usize>,
    pub include_arcanes: bool,
}

impl Scope {
    pub fn parse(body: &Map<String, Value>, catalog: &SimCatalog, melee: bool) -> SimResult<Self> {
        let get = |key| body.get(key).unwrap_or(&Value::Null);
        let profile = match text(get("profile")).trim().to_ascii_lowercase().as_str() {
            "" if get("profile").is_null() || get("profile").is_string() => "standard",
            "ultra" => "ultra",
            "fast" => "fast",
            "standard" => "standard",
            "deep" | "full" => "deep",
            _ => return Err("profile must be one of ultra, fast, standard, deep".into()),
        };
        let metrics = rows(&catalog.build_options["metrics"][if melee { "melee" } else { "gun" }]);
        let default = text(&metrics[0]["id"]);
        let requested = if truth(get("metric")) { text(get("metric")) } else { default };
        let metric = if requested == "killsPerMinute" { default } else { requested };
        if !metrics.iter().any(|row| row["id"] == metric) {
            return Err("metric is unavailable for this weapon type".into());
        }
        let element = body.get("elementTarget").or_else(|| body.get("element")).unwrap_or(&Value::Null);
        let element = if element.is_null() || element == "" {
            Value::Null
        } else {
            rows(&catalog.build_options["elementTargets"]).iter().find(|row| row["id"] == *element)
                .ok_or("elementTarget is not supported")?.clone()
        };
        let paths = |key| -> SimResult<Vec<String>> {
            let raw = get(key);
            if raw.is_null() { return Ok(Vec::new()); }
            raw.as_array().ok_or_else(|| format!("{key} must be an array of MOD paths"))?.iter().map(|value| {
                let path = text(value).trim();
                if catalog.mod_at(path).is_none() {
                    return Err(format!("{key} contains unsupported MOD path: {path}"));
                }
                Ok(path.to_owned())
            }).collect()
        };
        let mut required = paths("requiredMods")?;
        for path in paths("pinnedMods")? {
            if !required.contains(&path) { required.push(path); }
        }
        let mut forbidden = paths("forbiddenMods")?;
        for path in paths("excludedMods")? {
            if !forbidden.contains(&path) { forbidden.push(path); }
        }
        let unique: HashSet<_> = required.iter().collect();
        if unique.len() != required.len() || forbidden.iter().any(|path| unique.contains(path)) {
            return Err("required MOD paths must be unique and cannot be forbidden".into());
        }
        let fill = |key| -> SimResult<Option<usize>> {
            let value = get(key);
            if value.is_null() { return Ok(None); }
            value.as_u64().filter(|n| *n <= 8).map(|n| Some(n as usize))
                .ok_or_else(|| format!("{key} must be an integer between 0 and 8"))
        };
        let (min, max) = (fill("minFill")?, fill("maxFill")?);
        if min.zip(max).is_some_and(|(lo, hi)| lo > hi) { return Err("minFill must not exceed maxFill".into()); }
        let include_arcanes = get("autoArcane") == true || get("optimizeWithArcanes") != false;
        Ok(Self { profile, metric: metric.into(), event: matches!(metric, "attackDps" | "sustainedDps"),
            element, required, forbidden, min, max, include_arcanes })
    }
}

const ENHANCEMENTS: [&str; 7] = [
    "eximus", "archimedea", "empowered", "playerCount", "magneticStacks", "noOverguard", "shieldOverride",
];

pub(super) fn normalize_special_slots(body: &mut Map<String, Value>) -> SimResult<()> {
    if let Some(value) = body.get_mut("specialSlotPolarities").filter(|v| !v.is_null()) {
        let slots = value.as_object_mut().ok_or("specialSlotPolarities must be an object")?;
        slots.retain(|key, _| matches!(key.as_str(), "stance" | "exilus"));
        for value in slots.values_mut() {
            if value.is_null() || matches!(text(value), "" | "AP_NONE" | "NONE" | "NULL") && value.is_string() {
                *value = json!("AP_UNIVERSAL");
            } else if let Some(token) = value.as_str() {
                *value = json!(token.trim().to_ascii_uppercase());
            }
        }
    }
    Ok(())
}

pub(super) fn normalize(original: &Map<String, Value>) -> SimResult<Map<String, Value>> {
    let mut body = original.clone();
    let target = body.remove("benchmarkTarget").unwrap_or(Value::Null);
    if !target.is_null() && !target.is_object() { return Err("benchmarkTarget must be an object".into()); }
    if original.get("scenario").is_some_and(|value| !value.is_null() && !value.is_object()) {
        return Err("scenario must be an object".into());
    }
    let mut scenario = body.remove("scenario").and_then(|v| v.as_object().cloned()).unwrap_or_default();
    for key in ENHANCEMENTS.into_iter().chain(["headshotChance"]) {
        if let Some(value) = scenario.remove(key) { body.insert(key.into(), value); }
    }
    for (key, alias) in [("armourStrip", "armorStrip"), ("elementalArmourStrip", "elementalArmorStrip")] {
        let mut value = None;
        for source in [&body, &scenario].into_iter().chain(target.as_object()) {
            if let Some(raw) = source.get(key).or_else(|| source.get(alias)) { value = Some(raw.clone()); }
        }
        body.remove(alias);
        scenario.remove(key);
        scenario.remove(alias);
        if let Some(value) = value { body.insert(key.into(), value); }
    }
    if target.get("enemy").is_none() {
        if let Some(path) = target.get("path") { body.insert("enemy".into(), path.clone()); }
    }
    for key in ["enemy", "level", "steelPath", "headshotChance", "windowSeconds", "hitLocation"] {
        if let Some(value) = target.get(key) { body.insert(key.into(), value.clone()); }
    }
    let mut enhancements = Map::new();
    for key in ENHANCEMENTS {
        if let Some(value) = body.remove(key) { enhancements.insert(key.into(), value); }
    }
    for source in [&body, target.as_object().unwrap_or(&Map::new())] {
        if !std::ptr::eq(source, &body) {
            for key in ENHANCEMENTS {
                if let Some(value) = source.get(key) { enhancements.insert(key.into(), value.clone()); }
            }
        }
        if let Some(value) = source.get("enemyEnhancements").or_else(|| source.get("enhancements")).filter(|v| !v.is_null()) {
            enhancements.extend(value.as_object().ok_or("enemyEnhancements must be an object")?.clone());
        }
    }
    body.insert("enemyEnhancements".into(), Value::Object(enhancements));
    if body.get("enemy").is_none() { body.insert("enemy".into(), json!(BENCHMARK_TARGET)); }
    if body.get("headshotChance").is_none() && body.get("hitLocation").is_some_and(|v| v == "head") {
        body.insert("headshotChance".into(), json!(1.0));
    } else {
        body.remove("hitLocation");
    }
    if body.get("slotCount").is_none() {
        if let Some(slots) = body.remove("slots") { body.insert("slotCount".into(), slots); }
    }
    body.insert("scenario".into(), Value::Object(scenario));
    // The optimizer chooses ordinary slots and polarity independently of the manual loadout.
    body.remove("slotPolarities");
    body.remove("mods");
    normalize_special_slots(&mut body)?;
    Ok(body)
}
