//! 手动模拟请求的编排，以及 HTTP 与离线批处理共用的响应契约。

mod damage;
mod request;
mod target;
mod timeline;
mod weapon;

pub(crate) use request::SimCatalog;
use request::Request;
use target::{HitLocation, Target, BENCHMARK_TARGET};
use weapon::{Evolution, Weapon};

use crate::data::{EngineData, BUILD_LABEL, SCHEMA_VERSION};
use serde_json::{json, Map, Value};
use std::collections::BTreeSet;
use wfcore_lib::{pymath::psum, replay::Types, tables};

type SimResult<T> = Result<T, String>;
const ST_STACK_NAMES: [&str; 6] = ["heat", "viral", "corrosive", "magnetic", "cold", "puncture"];

pub(crate) fn run(data: &EngineData, catalog: &SimCatalog, fields: &Map<String, Value>) -> SimResult<Value> {
    let req = Request::parse(data, catalog, fields)?;
    let weapon = Weapon::new(data, &req);
    let evolution = Evolution::resolve(data, &req)?;
    let target = Target::new(&req.target, data);
    let defs = target.defenses(req.level, req.steel, req.strip, req.elemental_strip, &req.enhancements);
    let profile = weapon.profile(req.mode);
    let mode = &weapon.modes[req.mode];
    let mut types = Types::new();
    let forced = req.forced.iter().map(|(name, v)| (types.id(name), *v)).collect();
    let probe = weapon.reduce(req.mode, mode, &profile, &evolution.effects, req.scenario.cycle, 0, false)?;
    let probe_core = weapon::core(&probe, &mut types, target.faction, target.bracket);
    let count = tables::expected_status_procs(&probe_core, &forced, false).iter().filter(|(_, v)| *v > 0.0).count();
    let fields = if count > 0 { weapon.reduce(req.mode, mode, &profile, &evolution.effects, req.scenario.cycle, count, false)? }
        else { probe };
    let core = weapon::core(&fields, &mut types, target.faction, target.bracket);
    let at_location = |hit| {
        let direct = damage::direct(&fields, &core, &types, &target, &defs, hit);
        let kill = damage::kill_metrics(&fields, &core, &types, &target, &defs, hit);
        let (status, steady) = damage::status_and_steady(&fields, &core, &types, &target, &defs, hit, &forced);
        json!({"targetDamage": direct, "killMetrics": kill, "statusProcs": status["expectedProcs"],
            "dot": status["dot"], "statusDamage": status, "statusSustained": steady})
    };
    let mut result = at_location(HitLocation::BODY);
    if req.aim > 0.0 {
        let head = at_location(target.head);
        let body = result;
        result = mix(&body, &head, req.aim);
        result["headshotMix"] = json!({"chance": req.aim, "body": body["targetDamage"], "head": head["targetDamage"]});
    }
    let (replay_target, replay_level, replay_steel, replay_enhancements) = if truth(&req.target["health"]) {
        (&req.target, req.level, req.steel, req.enhancements.clone())
    } else {
        (data.enemies.get(BENCHMARK_TARGET).ok_or("缺少回放基准目标")?, 170, true,
            json!({"eximus": false, "noOverguard": true}))
    };
    let replay_target = Target::new(replay_target, data);
    let timeline = timeline::calculate(&weapon, &evolution, &fields, &req, &replay_target,
        replay_level, replay_steel, &replay_enhancements)?;
    let mut unsupported: BTreeSet<String> = fields.unsupported.iter().chain(&evolution.notes)
        .chain(&req.enhancement_notes).cloned().collect();
    for list in [&result["targetDamage"]["unsupported"], &result["killMetrics"]["unsupported"],
        &result["statusDamage"]["unsupported"], &timeline["unsupported"]] {
        unsupported.extend(rows(list).iter().filter_map(Value::as_str).map(str::to_owned));
    }
    if req.damage_model == "panel" {
        result["targetDamage"] = Value::Null;
        result["killMetrics"] = Value::Null;
    }
    let mut scenario = json!({"aiming": req.scenario.aiming, "heavyAttack": req.scenario.heavy,
        "comboTier": req.scenario.combo, "victimStatusCount": count, "conditionalStacks": {},
        "distance": req.scenario.distance, "cloaked": req.scenario.cloaked, "nearestEnemyDistance": req.scenario.nearest,
        "headshot": false, "hitLocation": "body"});
    if req.scenario.spread { scenario["modelSpread"] = json!(true); }
    let selection = json!({"weapon": identity(req.weapon), "enemy": identity(&req.target), "kitgunParts": [],
        "mods": [], "setPieces": {}, "fireMode": req.mode, "scenario": scenario, "targetLevel": req.level,
        "armourStrip": req.strip, "elementalArmourStrip": req.elemental_strip, "steelPath": req.steel,
        "enemyEnhancements": req.enhancements, "slotCount": req.slot_count, "arcanes": [], "stance": null, "exilus": null});
    let modded = damage::panel(&fields, &core, &types);
    let mut skipped = fields.skipped.clone();
    if req.damage_model == "panel" { skipped.push("panel-model-target-defenses-not-applied".into()); }
    let wiki = &data.enemy_overrides["wikiEnhancements"];
    result.as_object_mut().unwrap().extend(json!({
        "buildLabel": BUILD_LABEL, "schemaVersion": SCHEMA_VERSION,
        "exportDir": data.dataset_root.join("data/export").join(BUILD_LABEL),
        "selection": selection,
        "loadoutValidation": {"status": "checked", "valid": true, "capacity": 0, "capacityLimit": 60,
            "remaining": 60, "slotCount": req.slot_count, "polaritySource": req.polarity_source,
            "slotPolarities": req.slots, "assignments": [], "compatibility": "checked-from-export-ancestry",
            "polarity": "checked", "polarityDetails": {"status": "checked", "source": req.polarity_source, "slots": req.slots}},
        "moddedStats": modded, "unsupported": unsupported, "skipped": fields.skipped,
        "damageModel": req.damage_model, "exalted": null, "enemyEnhancements": req.enhancements,
        "probability": probability(&modded), "arcanes": [], "stance": null, "exilus": null,
        "modelLayers": {"moddedStats": "panel", "targetDamage": "expected", "statusProcs": "expected",
            "statusSustained": "steady", "statusSustainedDps": "steady", "dotSustainedDps": "steady",
            "nonDotStatusSustainedDps": "steady", "steadyDirectDps": "steady", "totalSustainedDps": "steady",
            "killTimeline": "steady", "benchmarkTimeline": "windowed"},
        "specialSlotPolarities": {"exilus": req.exilus_polarity}, "innateElement": null,
        "innateElementCandidates": [], "incarnonChoices": evolution.choices, "incarnonPolicy": evolution.policy,
        "diagnostics": {"unsupported": unsupported, "skipped": skipped, "capacity": 0, "polarity": "checked",
            "enemyEnhancements": req.enhancements, "wikiEnhancements": {"schema": wiki["schema"],
            "status": wiki["overguard"]["status"].as_str().unwrap_or("manifest-only"),
            "unsupported": rows(&wiki["unsupported"])}},
        "capacity": 0, "capacityLimit": 60, "benchmarkTimeline": timeline,
    }).as_object().unwrap().clone());
    for (key, source) in [
        ("steadyDirectDps", "directDps"), ("dotSustainedDps", "dotDps"),
        ("nonDotStatusSustainedDps", "nonDotDps"), ("statusSustainedDps", "statusDps"),
        ("totalSustainedDps", "totalDps"), ("statusRampSeconds", "steadyStateSeconds"),
        ("magneticBreakPotential", "magneticBreakPotential"),
    ] {
        result[key] = json!(num(&result["statusSustained"][source]));
    }
    if let Some(timeline) = damage::kill_timeline(&result) {
        result["killTimeline"] = timeline;
    }
    Ok(result)
}

fn identity(record: &Value) -> Value {
    Value::Object(["path", "nameInternal", "nameEn", "nameZh"].into_iter().filter_map(|key|
        record.get(key).filter(|v| !v.is_null()).map(|v| (key.to_owned(), v.clone()))).collect())
}

fn mix(left: &Value, right: &Value, weight: f64) -> Value {
    match (left, right) {
        (Value::Number(a), Value::Number(b)) => json!(a.as_f64().unwrap() * (1.0 - weight) + b.as_f64().unwrap() * weight),
        (Value::Number(a), Value::Null) => json!(a.as_f64().unwrap() * (1.0 - weight)),
        (Value::Null, Value::Number(b)) => json!(b.as_f64().unwrap() * weight),
        (Value::Object(a), Value::Object(b)) => Value::Object(a.keys().chain(b.keys().filter(|k| !a.contains_key(*k))).map(|key| {
            (key.clone(), mix(a.get(key).unwrap_or(&Value::Null), b.get(key).unwrap_or(&Value::Null), weight))
        }).collect()),
        (Value::Array(a), Value::Array(b)) if a.len() == b.len() => Value::Array(
            a.iter().zip(b).map(|(left, right)| mix(left, right, weight)).collect()),
        (Value::Array(_), Value::Array(_)) => left.clone(),
        _ if weight >= 0.5 => right.clone(),
        _ => left.clone(),
    }
}

fn probability(stats: &Value) -> Value {
    let multishot = num(&stats["multishot"]).max(0.0);
    let crit = num(&stats["critChance"]).max(0.0);
    let distribution = |value: f64| {
        let floor = value.floor();
        let frac = value - floor;
        let mut rows = vec![(floor as i64, 1.0 - frac)];
        if frac > 1e-9 { rows.push((floor as i64 + 1, frac)); }
        (rows, json!({"value": value, "floor": floor as i64, "ceil": value.ceil() as i64}))
    };
    let (shots, shot_distribution) = distribution(multishot);
    let (tiers, crit_distribution) = distribution(crit);
    let promotion = num(&stats["doubleCritChance"]).clamp(0.0, 1.0);
    let crit_multiplier = num(&stats["critMultiplier"]).max(1.0);
    let total = num(&stats["totalDamage"]).max(0.0);
    let falloff = num(&stats["falloffMultiplier"]).max(0.0);
    let mut outcomes = Vec::new();
    for (count, chance) in shots {
        for &(tier, tier_chance) in &tiers {
            let promoted = if tier > 0 { promotion } else { 0.0 };
            let values = [(tier, tier_chance * (1.0 - promoted)), (tier + 1, tier_chance * promoted)];
            for &(tier, tier_chance) in &values[..if promoted > 0.0 { 2 } else { 1 }] {
                let multiplier = if tier == 0 { num(&stats["noncritDamageMultiplier"]).max(0.0) }
                    else { 1.0 + tier as f64 * (crit_multiplier - 1.0) };
                outcomes.push(json!({"multishot": count, "critTier": tier, "probability": chance * tier_chance,
                    "relativeDamage": multiplier * count as f64, "damage": total * falloff * multiplier * count as f64}));
            }
        }
    }
    let sum = psum(outcomes.iter().map(|row| num(&row["probability"])));
    json!({"multishot": shot_distribution, "crit": crit_distribution, "outcomes": outcomes, "probabilitySum": sum})
}

fn merge(mut left: Value, right: Value) -> Value {
    left.as_object_mut().unwrap().extend(right.as_object().unwrap().clone());
    left
}

fn num(value: &Value) -> f64 { value.as_f64().unwrap_or(0.0) }
fn text(value: &Value) -> &str { value.as_str().unwrap_or("") }
fn rows(value: &Value) -> &[Value] { value.as_array().map(Vec::as_slice).unwrap_or_default() }
fn named(value: &Value) -> Vec<(String, f64)> {
    value.as_object().into_iter().flatten().filter_map(|(key, value)| value.as_f64().map(|v| (key.clone(), v))).collect()
}
fn truth(value: &Value) -> bool {
    match value {
        Value::Null => false, Value::Bool(v) => *v, Value::Number(_) => num(value) != 0.0,
        Value::String(v) => !v.is_empty(), Value::Array(v) => !v.is_empty(), Value::Object(v) => !v.is_empty(),
    }
}

fn active_pool([guard, shield, health]: [f64; 3]) -> (&'static str, f64) {
    if guard > 0.0 { ("overguard", guard) }
    else if shield > 0.0 { ("shield", shield) }
    else { ("health", health) }
}
