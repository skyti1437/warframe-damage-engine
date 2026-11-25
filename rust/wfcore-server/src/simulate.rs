//! 手动模拟请求的编排，以及 HTTP 与离线批处理共用的响应契约。

#[path = "build/mod.rs"]
pub(crate) mod build;

mod arcane;
mod arcane_status;
mod damage;
mod events;
mod loadout;
mod magnetic;
mod melee;
mod mod_runtime;
mod request;
mod target;
mod timeline;
mod weapon;

pub(crate) use request::SimCatalog;
use request::Request;
use target::{HitLocation, Target, BENCHMARK_TARGET};
use weapon::{Effect, Evolution, Weapon};

use crate::data::{EngineData, BUILD_LABEL, SCHEMA_VERSION};
use serde_json::{json, Map, Value};
use std::collections::BTreeSet;
use wfcore_lib::{mods::ModdedFields, pymath::psum, replay::Types, tables};

type SimResult<T> = Result<T, String>;
const ST_STACK_NAMES: [&str; 6] = ["heat", "viral", "corrosive", "magnetic", "cold", "puncture"];

pub(crate) fn run(data: &EngineData, catalog: &SimCatalog, fields: &Map<String, Value>) -> SimResult<Value> {
    let path = text(fields.get("weapon").unwrap_or(&Value::Null)).trim();
    let meta = catalog.weapons.get(path).ok_or("weapon 必须是已导出的武器路径")?;
    let raw_weapon = data.weapon(path).ok_or("weapon 缺少战斗数据")?;
    if meta["available"] == false {
        return Err(meta["issue"].as_str().unwrap_or("weapon 不可用").into());
    }
    let req = Request::parse_weapon(data, catalog, fields, &raw_weapon)?;
    let weapon = Weapon::new(data, &req);
    let evolution = Evolution::resolve(data, &req)?;
    let stance = melee::stance(data, &catalog.stances, &req)?;
    let schedule = melee::Schedule::resolve(data, &req, &stance)?;
    let loadout = loadout::Loadout::resolve(data, catalog, &req, &stance)?;
    let arcanes = arcane::Equipped::resolve(data, catalog, &req)?;
    let stance_damage = stance["dpsMultiplier"].as_f64().unwrap_or(1.0);
    let stance_status = stance["statusRateMultiplier"].as_f64().unwrap_or(1.0);
    let target = Target::new(&req.target, data);
    let defs = target.defenses(req.level, req.steel, req.strip, req.elemental_strip, &req.enhancements);
    let mut types = Types::new();
    let forced: tables::Vector = req.forced.iter().map(|(name, v)| (types.id(name), *v)).collect();
    let snapshot = |effects: &[Effect], skipped: &[String], types: &mut Types|
        -> SimResult<(ModdedFields, Value, usize)> {
        let (probe, metadata) = weapon.panel_fields(effects, 0.0, skipped)?;
        let core = weapon::core(&probe, types, target.faction, target.bracket);
        let count = tables::expected_status_procs(&core, &forced, false).iter().filter(|(_, v)| *v > 0.0).count();
        let (fields, metadata) = if count == 0 { (probe, metadata) } else {
            weapon.panel_fields(effects, count as f64, skipped)?
        };
        Ok((fields, metadata, count))
    };
    let event_fields = if arcanes.needs_stats() {
        Some(snapshot(&loadout.terms, &loadout.skipped, &mut types)?.0)
    } else { None };
    let arcane = arcanes.snapshot(&req, event_fields.as_ref(), None)?;
    let effects = loadout.effects(&evolution.effects, &arcane.effects, true);
    let mut skipped = loadout.skipped.clone();
    skipped.extend(arcane.skipped.iter().chain(&loadout.riven_skipped).cloned());
    let (fields, heavy, count) = snapshot(&effects, &skipped, &mut types)?;
    let core = weapon::core(&fields, &mut types, target.faction, target.bracket);
    let at_location = |hit| {
        let mut direct = damage::direct(&fields, &core, &types, &target, &defs, hit);
        direct["dps"] = json!(num(&direct["dps"]) * stance_damage);
        let kill = damage::kill_metrics(&fields, &core, &types, &target, &defs, hit);
        let (status, steady) = damage::status_and_steady(&fields, &core, &types, &target, &defs, hit, &forced, stance_status, stance_damage);
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
    let benchmark_target;
    let (replay_target, replay_level, replay_steel, replay_enhancements) = if truth(&req.target["health"]) {
        (&req.target, req.level, req.steel, req.enhancements.clone())
    } else {
        benchmark_target = data.enemy(BENCHMARK_TARGET).ok_or("缺少回放基准目标")?;
        (&benchmark_target, 170, true,
            json!({"eximus": false, "noOverguard": true}))
    };
    let replay_target = Target::new(replay_target, data);
    let replay_effects = events::Effects::prepare(&req, &loadout.cards, &arcanes, effects.clone(), &evolution,
        schedule.as_ref().map(|s| s.route.as_str()), &data.mechanics["infinite-conditions"]["sources"],
        weapon.innate);
    let timeline = timeline::calculate(&weapon, &evolution, &fields, &req, &replay_target,
        replay_level, replay_steel, &replay_enhancements, &replay_effects, schedule.as_ref(), &arcanes, stance_status, stance_damage)?;
    let mut unsupported: BTreeSet<String> = fields.unsupported.iter().chain(&evolution.notes)
        .chain(&req.enhancement_notes).chain(&loadout.notes).chain(&arcane.notes).cloned().collect();
    for list in [&result["targetDamage"]["unsupported"], &result["killMetrics"]["unsupported"],
        &result["statusDamage"]["unsupported"], &timeline["unsupported"]] {
        unsupported.extend(rows(list).iter().filter_map(Value::as_str).map(str::to_owned));
    }
    if req.damage_model == "panel" {
        result["targetDamage"] = Value::Null;
        result["killMetrics"] = Value::Null;
    }
    let mut scenario = json!({"aiming": req.scenario.aiming, "heavyAttack": req.scenario.heavy,
        "comboTier": req.scenario.combo, "victimStatusCount": count, "conditionalStacks": loadout.conditional_stacks,
        "distance": req.scenario.distance, "cloaked": req.scenario.cloaked, "nearestEnemyDistance": req.scenario.nearest,
        "headshot": false, "hitLocation": "body"});
    if req.scenario.spread { scenario["modelSpread"] = json!(true); }
    let selection = json!({"weapon": identity(req.weapon), "enemy": identity(&req.target), "kitgunParts": [],
        "mods": loadout.selection, "setPieces": loadout.set_pieces, "fireMode": req.mode, "scenario": scenario, "targetLevel": req.level,
        "armourStrip": req.strip, "elementalArmourStrip": req.elemental_strip, "steelPath": req.steel,
        "enemyEnhancements": req.enhancements, "slotCount": req.slot_count, "arcanes": arcane.states, "stance": stance, "exilus": loadout.exilus});
    let mut modded = damage::panel(&fields, &core, &types);
    if heavy.is_object() { modded = merge(modded, heavy); }
    modded["stanceMultiplier"] = json!(stance_damage);
    for key in ["sustainedDps", "burstDps"] { modded[key] = json!(num(&modded[key]) * stance_damage); }
    let mut result_skipped = fields.skipped.clone();
    result_skipped.extend(loadout.notes.iter().filter(|n| n.starts_with("exilus-mod-not-compatible-with-weapon:")).cloned());
    let mut skipped = result_skipped.clone();
    let mut special_slots = json!({"exilus": req.exilus_polarity});
    if req.melee { special_slots["stance"] = req.stance_polarity.clone(); }
    if req.damage_model == "panel" { skipped.push("panel-model-target-defenses-not-applied".into()); }
    let wiki = &data.enemy_overrides["wikiEnhancements"];
    result.as_object_mut().unwrap().extend(json!({
        "buildLabel": BUILD_LABEL, "schemaVersion": SCHEMA_VERSION,
        "exportDir": data.dataset_root.join("data/export").join(BUILD_LABEL),
        "selection": selection,
        "loadoutValidation": loadout.validation,
        "moddedStats": modded, "unsupported": unsupported, "skipped": result_skipped,
        "damageModel": req.damage_model, "exalted": null, "enemyEnhancements": req.enhancements,
        "probability": probability(&modded), "arcanes": arcane.states, "stance": stance, "exilus": loadout.exilus,
        "modelLayers": {"moddedStats": "panel", "targetDamage": "expected", "statusProcs": "expected",
            "statusSustained": "steady", "statusSustainedDps": "steady", "dotSustainedDps": "steady",
            "nonDotStatusSustainedDps": "steady", "steadyDirectDps": "steady", "totalSustainedDps": "steady",
            "killTimeline": "steady", "benchmarkTimeline": "windowed"},
        "specialSlotPolarities": special_slots, "innateElement": null,
        "innateElementCandidates": [], "incarnonChoices": evolution.choices, "incarnonPolicy": evolution.policy,
        "diagnostics": {"unsupported": unsupported, "skipped": skipped, "capacity": loadout.base_capacity, "polarity": "checked",
            "enemyEnhancements": req.enhancements, "wikiEnhancements": {"schema": wiki["schema"],
            "status": wiki["overguard"]["status"].as_str().unwrap_or("manifest-only"),
            "unsupported": rows(&wiki["unsupported"])}},
        "capacity": loadout.validation["capacity"], "capacityLimit": loadout.validation["capacityLimit"], "benchmarkTimeline": timeline,
    }).as_object().unwrap().clone());
    for (key, source) in [
        ("steadyDirectDps", "directDps"), ("dotSustainedDps", "dotDps"),
        ("nonDotStatusSustainedDps", "nonDotDps"), ("statusSustainedDps", "statusDps"),
        ("totalSustainedDps", "totalDps"), ("statusRampSeconds", "steadyStateSeconds"),
        ("magneticBreakPotential", "magneticBreakPotential"),
    ] {
        result[key] = json!(num(&result["statusSustained"][source]));
    }
    if let Some(timeline) = damage::kill_timeline(&result, &stance) {
        result["killTimeline"] = timeline;
    }
    if !arcanes.cards.is_empty() {
        let duration = result["killTimeline"]["killTime"].as_f64().filter(|v| *v > 0.0).unwrap_or(30.0).min(30.0);
        let dps = |arcane_effects: &[Effect]| -> SimResult<f64> {
            let effects = loadout.effects(&evolution.effects, arcane_effects, false);
            let mut types = Types::new();
            for (name, _) in &req.forced { types.id(name); }
            let (fields, _, _) = snapshot(&effects, &[], &mut types)?;
            let core = weapon::core(&fields, &mut types, target.faction, target.bracket);
            let (_, steady) = damage::status_and_steady(&fields, &core, &types, &target, &defs, HitLocation::BODY,
                &forced, stance_status, stance_damage);
            let total = num(&steady["totalDps"]);
            Ok(if total != 0.0 { total } else { damage::raw_sustained_dps(&fields, &core) })
        };
        let baseline = dps(&[])?;
        let mut points = Vec::new();
        for index in 0..=40 {
            let elapsed = duration * index as f64 / 40.0;
            let arcane = arcanes.snapshot(&req, event_fields.as_ref().or(Some(&fields)), Some(elapsed))?;
            let value = dps(&arcane.effects)?;
            points.push(json!({"time": elapsed, "expectedDps": value, "dps": value, "baselineDps": baseline,
                "gainPercent": if baseline > 0.0 { (value / baseline - 1.0) * 100.0 } else { 0.0 },
                "expectedStacks": arcane.states.iter().map(|v| num(&v["expectedStacks"])).collect::<Vec<_>>(),
                "uptime": arcane.states.iter().map(|v| num(&v["expectedUptime"])).collect::<Vec<_>>(),
                "arcanes": arcane.states.iter().map(|v| json!({"query": v["query"], "expectedStacks": v["expectedStacks"],
                    "uptime": v["expectedUptime"]})).collect::<Vec<_>>(),
                "unsupported": arcane.notes}));
        }
        result["timeline"] = json!({"model": "manual-expected-target-dps", "durationSeconds": duration, "points": points,
            "assumptions": ["on-hit/on-crit/on-headshot rates use the selected modded stats",
                "triggerRate uses the exported chance and duration Poisson approximation",
                "target defenses and status ticks are held at each snapshot"]});
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
