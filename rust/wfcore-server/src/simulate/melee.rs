//! 从导出的攻击、动画事件和武器重击属性构造逐击循环。

use super::{num, rows, text, truth, Request, SimResult};
use crate::data::EngineData;
use serde_json::{json, Value};
use std::borrow::Cow;
use wfcore_lib::replay::{HitSpec, MeleeHit, MeleeSegment, MeleeSpec, Types};

pub(super) const ROUTES: [&str; 5] = ["combo", "slam", "slamBullet", "slamContinuous", "glaive"];
const CONTEXTS: [&str; 4] = ["CC_GROUND", "CC_GROUND_BRANCH_A", "CC_GROUND_BRANCH_B", "CC_GROUND_BRANCH_C"];

fn get(value: &Value, default: f64) -> f64 { value.as_f64().unwrap_or(default) }
fn nonzero(value: &Value, default: f64) -> f64 { value.as_f64().filter(|v| *v != 0.0).unwrap_or(default) }

#[derive(Clone)]
struct Hit {
    kind: &'static str,
    at: f64,
    atten: f64,
    proc_atten: f64,
    procs: Value,
    combo_hits: f64,
    pct: Value,
    damage_type: Option<String>,
    crit: Option<f64>,
    crit_mult: Option<f64>,
    can_crit: bool,
    heavy: bool,
    combo_mult: bool,
    consumes: bool,
    radial: bool,
    slam: bool,
    phase: u8,
    throw: bool,
    bounce: bool,
    no_co: bool,
}

impl Hit {
    fn new(kind: &'static str, at: f64, props: &Value) -> Self {
        Self { kind, at, atten: get(&props["atten"], 1.0), proc_atten: get(&props["procAtten"], 1.0),
            procs: props["extraProcs"].clone(), combo_hits: get(&props["comboHits"], 1.0),
            pct: props["damagePct"].clone(), damage_type: None, crit: None, crit_mult: None,
            can_crit: true, heavy: false, combo_mult: false, consumes: false, radial: false,
            slam: false, phase: 0, throw: false, bounce: false, no_co: false }
    }
}

struct Segment {
    fixed: f64,
    anim: f64,
    charge: bool,
    flight: bool,
    hit: Option<Hit>,
}

impl Segment {
    fn new(fixed: f64, anim: f64, hit: Option<Hit>) -> Self {
        Self { fixed, anim, hit, charge: false, flight: false }
    }
}

pub(super) struct Schedule {
    pub route: String,
    label: String,
    combo: Option<String>,
    stance: Option<String>,
    pub notes: Vec<String>,
    segments: Vec<Segment>,
    combo_spec: Value,
    crit: f64,
    crit_mult: f64,
}

fn wait_fraction(attack: &Value) -> Option<f64> {
    rows(&attack["events"]).iter().filter(|e| text(&e["event"]).starts_with("WaitStart"))
        .filter_map(|e| e["t"].as_f64()).reduce(f64::min)
}

fn combo_segments(attacks: &[&Value], slams: &[Value]) -> Vec<Segment> {
    let mut result = Vec::new();
    for (index, attack) in attacks.iter().enumerate() {
        let duration = nonzero(&attack["duration"], 0.5);
        let end = if index + 1 < attacks.len() { duration * wait_fraction(attack).unwrap_or(1.0) } else { duration };
        let events = rows(&attack["hits"]);
        let swings = rows(&attack["swings"]);
        let mut hits: Vec<_> = (0..events.len().max(1)).map(|i| {
            let at = events.get(i).map_or(0.5, |event| get(&event["t"], 0.5));
            Hit::new("sweep", at, swings.get(i).filter(|v| truth(v)).unwrap_or(attack))
        }).collect();
        for event in rows(&attack["events"]) {
            let name = text(&event["event"]);
            if name.is_empty() || name == "GroundAttackEffect" { continue; }
            if let Some(slam) = slams.iter().rev().find(|s| s["event"] == name && get(&s["radius"], 1.0) > 0.0) {
                let mut hit = Hit::new("slam", num(&event["t"]), &Value::Null);
                hit.atten = nonzero(&slam["attenuation"], 1.0);
                hit.procs = slam["forcedProcs"].clone();
                hit.can_crit = false;
                hit.radial = true;
                hit.slam = true;
                hits.push(hit);
            }
        }
        hits.sort_by(|a, b| a.at.total_cmp(&b.at));
        let mut previous: f64 = 0.0;
        for hit in hits {
            let at = (hit.at.max(0.0) * duration).min(end);
            result.push(Segment::new(0.0, (at - previous).max(0.0), Some(hit)));
            previous = previous.max(at);
        }
        result.push(Segment::new(0.0, (end - previous).max(0.0), None));
    }
    result
}

fn loop_rate(segments: &[Segment]) -> f64 {
    let seconds = segments.iter().map(|s| s.fixed + s.anim).sum::<f64>();
    let damage = segments.iter().filter_map(|s| s.hit.as_ref()).map(|h| h.atten).sum::<f64>();
    if seconds > 0.0 { damage / seconds } else { 0.0 }
}

fn record<'a>(reference: &'a Value, weapon: &Value) -> Cow<'a, Value> {
    let base = &reference["weapons"][weapon["meleePath"].as_str().unwrap_or(text(&weapon["path"]))];
    let hand = &reference["zaws"][text(&weapon["zawStrike"])][text(&weapon["zawHanded"])];
    if !base.is_object() || !hand.is_object() { return Cow::Borrowed(base); }
    let mut row = base.clone();
    row["tree"] = hand["tree"].clone();
    row["slams"] = json!(rows(&hand["slams"]));
    Cow::Owned(row)
}

fn has_tag(weapon: &Value, tag: &str) -> bool {
    rows(&weapon["compatTags"]).iter().any(|v| text(v).eq_ignore_ascii_case(tag))
}

fn default_route(weapon: &Value, row: &Value) -> &'static str {
    if has_tag(weapon, "GLAIVES_STANCE") && truth(&row["glaive"]) { "glaive" }
    else if has_tag(weapon, "HAMMERS_STANCE") { "slamBullet" }
    else { "combo" }
}

fn charge_time(reference: &Value, row: &Value) -> (f64, bool) {
    let charge = |v: &Value| v["heavy"]["chargeTime"].as_f64().filter(|v| (v - 2.0).abs() > 1e-6);
    if let Some(value) = charge(row) { return (value, false); }
    let mut siblings: Vec<_> = reference["weapons"].as_object().into_iter().flat_map(|m| m.values())
        .filter(|w| w["tree"] == row["tree"]).filter_map(charge).collect();
    siblings.sort_by(f64::total_cmp);
    (siblings.get(siblings.len() / 2).copied().unwrap_or(0.7), true)
}

fn build(reference: &Value, weapon: &Value, route: Option<&str>, stance: Option<&str>,
         combo: Option<&str>, distance: Option<f64>, heavy: bool) -> SimResult<Option<Schedule>> {
    let row = &record(reference, weapon);
    if !truth(row) { return Ok(None); }
    let mut route = route.unwrap_or_else(|| default_route(weapon, row));
    if route == "slamContinuous" && !has_tag(weapon, "HAMMERS_STANCE") && !text(&row["tree"]).ends_with("/HammerMeleeTree") {
        return Err("连续震地需要锤类武器".into());
    }
    let attacks = &reference["attacks"];
    let slams = rows(&row["slams"]);
    let find_slam = |name| slams.iter().rev().find(|v| v["event"] == name);
    let mut notes = Vec::new();
    if row["combo"]["recoveryPerSecond"].is_null() { notes.push("melee-initial-combo-recovery-rate-unverified".into()); }
    notes.push("melee-combo-recovery-event-integration-and-impact-consumption".into());
    if route == "glaive" && !truth(&row["glaive"]) {
        notes.push("melee-route-glaive-unavailable-using-combo".into());
        route = "combo";
    }
    if route.starts_with("slam") && find_slam("HeavySlam").or_else(|| find_slam("MeleeSlam")).is_none() {
        notes.push("melee-route-slam-unavailable-using-combo".into());
        route = "combo";
    }
    let trees = &reference["trees"];
    let tree_path = [stance, row["stance"].as_str(), row["tree"].as_str()].into_iter().flatten()
        .find(|path| CONTEXTS.iter().any(|c| truth(&trees[*path]["contexts"][*c])));
    let tree = tree_path.map_or(&Value::Null, |path| &trees[path]);
    let mut selected_combo = None;
    let mut segments = Vec::new();
    let damage = &row["damage"];
    let label = match route {
        "combo" if heavy => {
            let path = weapon["heavyAttack"]["meleeTree"].as_str().unwrap_or(text(&row["tree"]));
            let heavy_tree = if truth(&trees[path]) { &trees[path] } else { tree };
            let (charge, guessed) = charge_time(reference, row);
            for path in rows(&heavy_tree["contexts"]["CC_GROUND_HEAVY"]) {
                let attack = &attacks[text(path)];
                if attack.is_null() { continue; }
                let mut strike = combo_segments(&[attack], slams);
                for hit in strike.iter_mut().filter_map(|s| s.hit.as_mut()) {
                    hit.heavy = true;
                    hit.combo_mult = true;
                    hit.combo_hits = 0.0;
                }
                strike.iter_mut().rev().find_map(|s| s.hit.as_mut()).unwrap().consumes = true;
                segments.push(Segment { charge: true, ..Segment::new(charge, 0.0, None) });
                segments.extend(strike);
            }
            if guessed { notes.push("heavy-wind-up-from-class".into()); }
            selected_combo = Some("CC_GROUND_HEAVY".to_owned());
            "heavy-attack"
        }
        "combo" => {
            let mut best = 0.0;
            for context in CONTEXTS {
                if combo.is_some_and(|c| CONTEXTS.contains(&c) && c != context) { continue; }
                let chain: Vec<_> = rows(&tree["contexts"][context]).iter()
                    .filter_map(|p| attacks.get(text(p))).collect();
                if chain.is_empty() { continue; }
                let candidate = combo_segments(&chain, slams);
                let rate = loop_rate(&candidate);
                if segments.is_empty() || rate > best {
                    best = rate;
                    segments = candidate;
                    selected_combo = Some(context.to_owned());
                }
            }
            let Some(label) = selected_combo.as_deref() else { return Ok(None); };
            label
        }
        "slam" | "slamBullet" | "slamContinuous" => {
            let is_heavy = find_slam("HeavySlam").is_some();
            let slam = find_slam(if is_heavy { "HeavySlam" } else { "MeleeSlam" }).unwrap();
            let attack = text(&row[if is_heavy { "heavySlamAttack" } else { "slamAttack" }]);
            let atten = nonzero(&attacks[attack]["atten"], if is_heavy { 3.0 } else { 2.0 });
            let strike = &attacks[text(&tree["aerial"]["BP_CROUCH"])];
            let mut recovery = nonzero(&strike["duration"], 1.0) * wait_fraction(strike).filter(|v| *v != 0.0).unwrap_or(1.0);
            let gravity = 9.8 + 9.0;
            let height: f64 = if route == "slamBullet" { 10.0 } else { 5.0 };
            let fall = (2.0 * height / gravity).sqrt();
            let wait = if route == "slamContinuous" {
                notes.push("continuous-slam-user-cadence-0.2s-minimum-height-no-recovery".into());
                recovery = 0.0;
                0.2
            } else {
                let speed = 2.5 * (2.0_f64 * gravity * 5.0).sqrt();
                let rise = if route == "slamBullet" { (speed - (speed * speed - 2.0 * gravity * height).max(0.0).sqrt()) / gravity }
                    else { fall };
                notes.push(format!("slam-cycle: rise {rise:.2}s + free-fall {fall:.2}s from {height} m (plunge speed not in data)"));
                rise + fall
            };
            // 导出的震地攻击提供中心命中的倍率，Slams 条目提供径向类型和强制异常。
            let mut hit = Hit::new("slam", 0.0, &Value::Null);
            hit.atten = atten;
            hit.procs = slam["forcedProcs"].clone();
            hit.damage_type = slam["damageType"].as_str().map(str::to_owned);
            hit.combo_hits = if is_heavy { 0.0 } else { 1.0 };
            hit.heavy = is_heavy;
            hit.combo_mult = is_heavy;
            hit.consumes = is_heavy;
            hit.radial = true;
            hit.slam = true;
            hit.phase = 3;
            segments.push(Segment::new(wait, 0.0, Some(hit)));
            segments.push(Segment::new(0.0, recovery, None));
            if is_heavy { "heavy-slam" } else { "slam" }
        }
        _ => {
            let glaive = &row["glaive"];
            let projectile = if truth(&glaive["turbo"]) { &glaive["turbo"] } else { &glaive["projectile"] };
            let (charge, guessed) = row["heavy"]["chargeDuration"].as_f64().filter(|v| *v != 0.0)
                .map_or_else(|| charge_time(reference, row), |v| (v, false));
            if guessed { notes.push("heavy-wind-up-from-class".into()); }
            let distance = distance.unwrap_or(10.0);
            let flight = distance / nonzero(&projectile["speed"], 55.0);
            let throw_path = text(&tree["contexts"]["CC_GROUND_HEAVY"][0]);
            let anim = nonzero(&attacks[throw_path]["duration"], 0.7);
            notes.push(format!("glaive-cycle: wind-up {charge}s + throw {anim:.2}s + flight {flight:.2}s ({distance} m)"));
            let amount = num(&damage["amount"]);
            let atten = |value| if amount != 0.0 { value / amount } else { 1.0 };
            let proc = |v: &Value| if num(&damage["procChance"]) != 0.0 { num(v) / num(&damage["procChance"]) } else { 1.0 };
            let blast = num(&projectile["explosion"]);
            let mut direct = Hit::new("glaiveDirect", 0.0, &Value::Null);
            direct.crit = projectile["crit"].as_f64();
            direct.crit_mult = projectile["critMultiplier"].as_f64();
            direct.atten = atten(num(&projectile["amount"]));
            direct.proc_atten = proc(&projectile["procChance"]);
            direct.procs = projectile["forcedProcs"].clone();
            direct.throw = true;
            let mut bounce = direct.clone();
            bounce.kind = "glaiveBounce";
            bounce.atten = atten(blast);
            bounce.proc_atten = proc(projectile.get("explosionProcChance").unwrap_or(&projectile["procChance"]));
            bounce.combo_hits = 0.0;
            bounce.procs = Value::Null;
            bounce.throw = false;
            bounce.radial = true;
            bounce.damage_type = Some("DT_EXPLOSION".into());
            bounce.bounce = true;
            bounce.no_co = true;
            let mut explosion = bounce.clone();
            explosion.kind = "glaiveExplosion";
            explosion.atten = atten(blast * nonzero(&projectile["heavyRecallAtten"], 1.0));
            explosion.procs = projectile["heavyRecallProcs"].clone();
            explosion.bounce = false;
            explosion.heavy = true;
            explosion.combo_mult = true;
            explosion.consumes = true;
            segments.extend([
                Segment { charge: true, ..Segment::new(charge, anim, None) },
                Segment { flight: true, ..Segment::new(flight, 0.0, Some(direct)) },
                Segment::new(0.0, 0.0, Some(bounce)),
                Segment::new(0.0, 0.0, Some(explosion)),
            ]);
            "heavy-throw-detonate"
        }
    }.to_owned();
    Ok((!segments.is_empty()).then(|| Schedule {
        route: route.into(), label, combo: selected_combo, stance: tree_path.map(str::to_owned),
        notes, segments, combo_spec: row["combo"].clone(), crit: num(&damage["crit"]), crit_mult: num(&damage["critMultiplier"]),
    }))
}

pub(super) fn stance(data: &EngineData, catalog: &[Value], req: &Request<'_>) -> SimResult<Value> {
    let requested = req.body.get("stancePath").filter(|v| !v.is_null() && *v != "");
    if !req.melee {
        if requested.is_some() { return Err("远程武器没有架势槽".into()); }
        return Ok(Value::Null);
    }
    let reference = &data.mechanics["melee-mechanics"];
    let mut candidates: Vec<_> = catalog.iter().filter(|s| has_tag(req.weapon, text(&s["compatTag"]))).map(|s| {
        let path = text(&s["path"]);
        let schedule = build(reference, req.weapon, Some("combo"), Some(path), None, None, false).ok().flatten();
        let rate = schedule.as_ref().filter(|r| r.stance.as_deref() == Some(path))
            .map_or_else(|| num(&s["bestComboData"]["dpsMultiplier"]), |r| loop_rate(&r.segments));
        (s, rate)
    }).collect();
    let preferred = if default_route(req.weapon, &record(reference, req.weapon)) == "glaive" {
        Some("/Lotus/Weapons/Tenno/Melee/MeleeTrees/GlaiveCmbTwoMeleeTree")
    } else { None };
    let name = |v: &Value| v["nameZh"].as_str().unwrap_or(text(&v["nameEn"])).to_lowercase();
    candidates.sort_by(|(a, ar), (b, br)| (a["path"].as_str() != preferred).cmp(&(b["path"].as_str() != preferred))
        .then_with(|| br.total_cmp(ar)).then_with(|| name(a).cmp(&name(b))));
    let Some((first, _)) = candidates.first() else {
        return Ok(json!({"locked": false, "builtIn": false, "path": null, "slotPolarity": req.stance_polarity,
            "capacityGain": 0, "dpsMultiplier": 1.0, "statusRateMultiplier": 1.0, "compatiblePaths": []}));
    };
    let selected = if let Some(path) = requested {
        candidates.iter().find(|(s, _)| s["path"] == *path).map(|(s, _)| *s).ok_or("此武器不兼容请求的架势")?
    } else { first };
    let rank = selected["maxRank"].as_i64().unwrap_or(0);
    let raw = selected["baseDrain"].as_i64().unwrap_or(0) - rank;
    let (drain, relation) = super::loadout::adjust_drain(raw, &selected["polarity"], &req.stance_polarity);
    let combo = &selected["bestComboData"];
    Ok(super::merge(selected.clone(), json!({"rank": rank, "locked": false, "builtIn": false,
        "slotPolarity": req.stance_polarity, "capacityGain": -drain, "matchedCapacityGain": -raw * 2,
        "polarityRelation": relation, "dpsMultiplier": combo["dpsMultiplier"], "statusRateMultiplier": combo["statusRateMultiplier"],
        "bestCombo": combo, "compatiblePaths": candidates.iter().map(|(s, _)| &s["path"]).collect::<Vec<_>>()})))
}

impl Schedule {
    pub fn resolve(data: &EngineData, req: &Request<'_>, stance: &Value) -> SimResult<Option<Self>> {
        if !req.melee || (req.mode != 0 && !req.scenario.heavy) { return Ok(None); }
        let state = |key| req.scenario.state.and_then(|s| s.get(key)).unwrap_or(&Value::Null);
        build(&data.mechanics["melee-mechanics"], req.weapon, state("meleeRoute").as_str(),
            state("meleeStance").as_str().filter(|s| !s.is_empty()).or(stance["path"].as_str()), state("meleeCombo").as_str(),
            state("meleeTargetDistance").as_f64(), req.scenario.heavy)
    }

    pub fn spec(&self, tier_keys: Vec<(usize, String, f64)>) -> MeleeSpec {
        let combo = &self.combo_spec;
        let base_hits = nonzero(&combo["baseHits"], 20.0);
        MeleeSpec { base_hits, tier_step: nonzero(&combo["tierStep"], base_hits), cap: nonzero(&combo["cap"], 220.0),
            base_multiplier: nonzero(&combo["baseMultiplier"], 1.0), recovery_per_second: num(&combo["recoveryPerSecond"]),
            tier_keys, segments: self.segments.iter().enumerate().map(|(i, segment)| MeleeSegment {
                fixed: segment.fixed, anim: segment.anim, charge_scaled: segment.charge, flight_scaled: segment.flight,
                hit: segment.hit.as_ref().map(|h| MeleeHit { id: i as i64, heavy: h.heavy,
                    combo_hits: h.combo_hits, consumes: h.consumes, phase: h.phase }),
            }).collect() }
    }

    pub fn hit(&self, id: i64, types: &mut Types) -> HitSpec {
        let h = self.segments[id as usize].hit.as_ref().unwrap();
        let pct = super::named(&h.pct).iter().map(|(name, v)| (types.id(name), *v)).collect();
        let target = h.damage_type.as_deref().filter(|t| !matches!(*t, "DT_PHYSICAL" | "DT_ANY")).map(|t| types.id(t));
        let extra_procs = rows(&h.procs).iter().filter_map(|v| super::weapon::proc_type(text(v)).map(|t| types.id(t))).collect();
        HitSpec { atten: h.atten, slam: h.slam, combo_multiplier: h.combo_mult, pct, target,
            crit_chance: h.crit.filter(|_| self.crit != 0.0).map(|v| v / self.crit),
            crit_multiplier: h.crit_mult.filter(|_| self.crit_mult != 0.0).map(|v| v / self.crit_mult),
            can_crit: h.can_crit, extra_procs, proc_atten: h.proc_atten, radial: h.radial, duplicate: None,
            throw: h.throw, bounce: h.bounce, no_co: h.no_co }
    }

    pub fn payload(&self, events: &[wfcore_lib::replay::MeleeEvent]) -> Value {
        let round = wfcore_lib::replay::round_decimal;
        let events: Vec<_> = events.iter().map(|event| json!({
            "time": round(event.time, 6), "kind": self.segments[event.id as usize].hit.as_ref().unwrap().kind,
            "combo": round(event.combo, 3), "comboBefore": round(event.before, 6),
            "comboRegenerated": round(event.regenerated, 6), "heavyMultiplier": event.multiplier,
        })).collect();
        json!({"route": self.route, "label": self.label, "combo": self.combo, "stance": self.stance, "events": events})
    }
}
