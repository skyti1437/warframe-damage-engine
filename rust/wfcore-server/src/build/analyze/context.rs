use super::{json, num, request, text, EngineData, Map, SimCatalog, SimResult, Value};
use crate::simulate::BENCHMARK_TARGET;

pub(super) fn benchmark(fields: &Map<String, Value>, data: &EngineData, catalog: &SimCatalog)
    -> SimResult<(Value, Map<String, Value>)> {
    let supplied = fields.get("benchmarkTarget").filter(|target| !target.is_null());
    let raw = supplied.map(|v| v.as_object().ok_or("benchmarkTarget must be an object"))
        .transpose()?.cloned().unwrap_or_default();
    let mut target = if raw.is_empty() {
        json!({"enemy": BENCHMARK_TARGET, "level": 170, "steelPath": true, "eximus": false,
            "enemyEnhancements": {"eximus": false, "noOverguard": true}, "windowSeconds": 30.0,
            "armourStrip": 0.0, "headshotChance": 0.0})
    } else { Value::Object(raw.clone()) };
    let mut body = request::normalize(fields)?;
    let snapshot = raw.get("defenseSnapshot").or_else(|| raw.get("customDefense"))
        .or_else(|| fields.get("defenseSnapshot")).or_else(|| fields.get("customDefense")).filter(|v| !v.is_null());
    let damage_model = raw.get("damageModel").or_else(|| fields.get("damageModel")).filter(|v| !v.is_null());
    if damage_model.is_some_and(|v| !matches!(v.as_str(), Some("custom" | "target"))) {
        return Err("analysis damageModel must be target or custom".into());
    }
    if snapshot.is_some() || damage_model.is_some_and(|v| v == "custom") {
        let snapshot = snapshot.ok_or("defenseSnapshot must be an object")?;
        crate::simulate::request::custom_target(snapshot)?;
        body.insert("damageModel".into(), json!("custom"));
        body.insert("enemy".into(), json!(BENCHMARK_TARGET));
        body.insert("defenseSnapshot".into(), snapshot.clone());
        target["enemy"] = json!("__custom_enemy__");
    } else {
        if !catalog.enemies.contains_key(text(&body["enemy"])) {
            return Err("benchmarkTarget.enemy must be a portable enemy path".into());
        }
        body.insert("damageModel".into(), json!("target"));
        target["enemy"] = body["enemy"].clone();
    }
    for alias in ["armorStrip", "elementalArmorStrip"] { target.as_object_mut().unwrap().remove(alias); }
    for (field, low, high) in [("armourStrip", 0.0, 1.0), ("elementalArmourStrip", 0.0, 1.0),
        ("headshotChance", 0.0, 1.0), ("windowSeconds", 0.1, 600.0)] {
        if field == "headshotChance" && !raw.contains_key(field) && !fields.contains_key(field)
            && fields.get("scenario").is_none_or(|scenario| scenario.get(field).is_none()) {
            continue;
        }
        if let Some(value) = body.get(field) {
            let value = value.as_f64().filter(|v| (low..=high).contains(v))
                .ok_or_else(|| format!("{field} must be between {low} and {high}"))?;
            target[field] = json!(value);
        }
    }
    for (field, fallback) in [("level", json!(170)), ("steelPath", json!(true))] {
        if target.get(field).is_some() || fields.contains_key(field) {
            let value = raw.get(field).or_else(|| fields.get(field)).unwrap_or(&fallback);
            let valid = if field == "level" { value.as_u64().is_some() } else { value.is_boolean() };
            if !valid { return Err(format!("{field} has invalid value")); }
            target[field] = value.clone();
        }
    }
    if target.get("hitLocation").is_some_and(|value| !matches!(value.as_str(), Some("body" | "head"))) {
        return Err("benchmarkTarget.hitLocation must be body or head".into());
    }
    for field in ["eximus", "archimedea", "empowered"] {
        if raw.get(field).is_some_and(|value| !value.is_boolean()) { return Err(format!("benchmarkTarget.{field} must be boolean")); }
    }
    let enhancements = &body["enemyEnhancements"];
    let nested_enhancements = |map: &Map<String, Value>| map.contains_key("enemyEnhancements");
    if nested_enhancements(fields) || nested_enhancements(&raw) || !enhancements.as_object().unwrap().is_empty() {
        let mut normalized = enhancements.clone();
        if normalized.get("shieldOverride").is_some() {
            let shield = normalized["shieldOverride"].as_f64().filter(|value| *value >= 0.0);
            match shield {
                Some(value) => normalized["shieldOverride"] = json!(value),
                None if normalized["shieldOverride"].is_null() => { normalized.as_object_mut().unwrap().remove("shieldOverride"); }
                None => return Err("enemyEnhancements.shieldOverride must be non-negative".into()),
            }
        }
        target["enemyEnhancements"] = normalized;
    }
    let fallback: Map<_, _> = ["eximus", "archimedea", "empowered", "playerCount"].into_iter()
        .filter_map(|key| target.get(key).map(|value| (key.to_owned(), value.clone()))).collect();
    let enhancements = target["enemyEnhancements"].as_object().filter(|values| !values.is_empty())
        .unwrap_or(&fallback);
    body.insert("enemyEnhancements".into(), Value::Object(enhancements.clone()));
    body.insert("level".into(), target.get("level").cloned().unwrap_or_else(|| {
        data.enemy(text(&target["enemy"])).and_then(|mut record| record.as_object_mut()?.shift_remove("baseLevel")).unwrap_or(json!(1))
    }));
    body.insert("steelPath".into(), target.get("steelPath").cloned().unwrap_or(json!(false)));
    body.insert("headshotChance".into(), json!(num(&target["headshotChance"])));
    body.insert("windowSeconds".into(), json!(target["windowSeconds"].as_f64().unwrap_or(30.0)));
    Ok((target, body))
}

pub(super) fn evaluation_target(requested: &Value, req: &super::Request<'_>) -> Value {
    let fallback: Map<_, _> = ["eximus", "archimedea", "empowered", "playerCount"].into_iter()
        .filter_map(|key| requested.get(key).map(|value| (key.to_owned(), value.clone()))).collect();
    let enhancements = requested["enemyEnhancements"].as_object().filter(|values| !values.is_empty())
        .unwrap_or(&fallback);
    let hit_location = requested["hitLocation"].as_str().unwrap_or("body");
    json!({"enemyPath": req.target["path"], "enemyName": req.target.get("nameEn").filter(|value| !text(value).is_empty())
        .unwrap_or(&req.target["name"]), "level": requested["level"], "steelPath": req.steel,
        "enemyEnhancements": enhancements, "armourStrip": num(&requested["armourStrip"]),
        "elementalArmourStrip": num(&requested["elementalArmourStrip"]), "hitLocation": hit_location,
        "headshotChance": num(&requested["headshotChance"]), "windowSeconds": req.window})
}

pub(super) fn scenario(req: &super::Request<'_>, stance: &Value) -> Value {
    let scenario = &req.scenario;
    let nested = |key| req.body.get("scenario").and_then(|v| v.get(key)).or_else(|| req.body.get(key)).unwrap_or(&Value::Null);
    let mut state = scenario.state.cloned().unwrap_or_default();
    for key in ["frameArmor", "frameShield", "energyPool", "sprintMultiplier", "energySpent", "energySpendPerSecond",
        "selfShieldBreakAt", "selfShieldBreakInterval", "selfOverguardBreakAt", "selfOverguardBreakInterval",
        "selfShieldHitAt", "selfShieldHitInterval", "equipFromPrimaryAt", "equipFromPrimaryInterval",
        "abilityUseAt", "abilityUseInterval", "meleeTargetDistance"] {
        if let Some(value) = state.get_mut(key) { *value = json!(num(value)); }
    }
    if state.get("zymosChildOverlap").is_some_and(|v| v == "off") { state.remove("zymosChildOverlap"); }
    if let Some(path) = stance["path"].as_str() {
        state.entry("meleeStance").or_insert(json!(path));
    }
    let mut result = json!({"aiming":scenario.aiming, "incarnonCycle":scenario.cycle, "heavyAttack":scenario.heavy,
        "triggerPolicy":scenario.policy, "distance":scenario.distance, "cloaked":scenario.cloaked,
        "nearestEnemyDistance":scenario.nearest, "comboTier":scenario.combo,
        "detonationDelaySeconds":num(nested("detonationDelaySeconds")),
        "detonationBatchSize":nested("detonationBatchSize").as_u64().unwrap_or(1), "combatState":state});
    if scenario.spread { result["modelSpread"] = json!(true); }
    result
}

pub(super) fn canonical_json(value: &Value, out: &mut String) {
    match value {
        Value::Object(fields) => {
            out.push('{');
            let mut ordered: Vec<_> = fields.iter().collect();
            ordered.sort_by_key(|(key, _)| *key);
            for (index, (key, value)) in ordered.into_iter().enumerate() {
                if index > 0 { out.push(','); }
                out.push_str(&json!(key).to_string());
                out.push(':');
                canonical_json(value, out);
            }
            out.push('}');
        }
        Value::Array(values) => {
            out.push('[');
            for (index, item) in values.iter().enumerate() {
                if index > 0 { out.push(','); }
                canonical_json(item, out);
            }
            out.push(']');
        }
        Value::Number(value) if value.is_f64() => {
            let digits = format!("{:?}", value.as_f64().unwrap());
            if let Some((base, exponent)) = digits.split_once('e') {
                let exponent: i32 = exponent.parse().unwrap();
                out.push_str(&format!("{base}e{exponent:+03}"));
            } else {
                out.push_str(&digits);
            }
        }
        _ => out.push_str(&value.to_string()),
    }
}

pub(super) fn hash(value: &Value) -> String {
    let mut encoded = String::new();
    canonical_json(value, &mut encoded);
    sha256_prefix(encoded.as_bytes())
}

fn sha256_prefix(input: &[u8]) -> String {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
        0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
        0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
        0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
        0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
        0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
        0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
    ];
    let mut state: [u32; 8] = [0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a,
        0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19];
    let mut padded = input.to_vec();
    padded.push(0x80);
    padded.resize((padded.len() + 8).next_multiple_of(64) - 8, 0);
    padded.extend_from_slice(&(input.len() as u64 * 8).to_be_bytes());
    for block in padded.chunks_exact(64) {
        let mut words = [0u32; 64];
        for (word, source) in words.iter_mut().zip(block.chunks_exact(4)) {
            *word = u32::from_be_bytes(source.try_into().unwrap());
        }
        for i in 16..64 {
            let a = words[i - 15];
            let b = words[i - 2];
            let s0 = a.rotate_right(7) ^ a.rotate_right(18) ^ (a >> 3);
            let s1 = b.rotate_right(17) ^ b.rotate_right(19) ^ (b >> 10);
            words[i] = words[i - 16].wrapping_add(s0).wrapping_add(words[i - 7]).wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = state;
        for (constant, word) in K.into_iter().zip(words) {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let choice = (e & f) ^ (!e & g);
            let t1 = h.wrapping_add(s1).wrapping_add(choice).wrapping_add(constant).wrapping_add(word);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let majority = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(majority);
            [a, b, c, d, e, f, g, h] = [t1.wrapping_add(t2), a, b, c, d.wrapping_add(t1), e, f, g];
        }
        for (slot, value) in state.iter_mut().zip([a, b, c, d, e, f, g, h]) { *slot = slot.wrapping_add(value); }
    }
    format!("{:08x}{:08x}{:04x}", state[0], state[1], state[2] >> 16)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn python_context_hash_format() {
        assert_eq!(sha256_prefix(b"abc"), "ba7816bf8f01cfea4141");
        let mut encoded = String::new();
        canonical_json(&json!({"词": [1.0, 1e-7, 1e16], "a": -0.0}), &mut encoded);
        assert_eq!(encoded, r#"{"a":-0.0,"词":[1.0,1e-07,1e+16]}"#);
    }
}
