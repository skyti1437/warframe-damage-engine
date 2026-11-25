//! 武器赋能的面板期望与手动状态。回放单独消费导出的触发事件。

use super::{arcane_status, identity, merge, num, rows, text, truth, Request, SimResult};
use super::loadout::{cap, compatible, query, ranked, terms};
use super::request::SimCatalog;
use super::weapon::{is_weapon_effect, Effect};
use crate::data::EngineData;
use serde_json::{json, Value};
use std::collections::HashSet;
use wfcore_lib::mods::ModdedFields;

pub(super) struct Arcane<'a> {
    pub record: &'a Value,
    pub rank: u64,
    pub stacks: Option<u64>,
    pub uptime: Option<f64>,
}

pub(super) struct Equipped<'a> {
    pub cards: Vec<Arcane<'a>>,
    pub ignored: Vec<String>,
}

pub(super) struct Snapshot {
    pub effects: Vec<Effect>,
    pub states: Vec<Value>,
    pub notes: Vec<String>,
    pub skipped: Vec<String>,
}

pub(super) fn archgun_scope(path: &str, base: &str) -> bool {
    path.starts_with("/Lotus/Upgrades/CosmeticEnhancers/") &&
        matches!(base, "/Lotus/Weapons/Tenno/LotusLongGun" | "/Lotus/Weapons/Tenno/Pistol/LotusPistol")
}

fn applies(arcane: &Value, req: &Request<'_>, frame: bool) -> bool {
    let weapon = req.weapon;
    let path = text(&arcane["path"]);
    let base = text(&arcane["compatibility"]);
    if arcane["equipmentType"] == "Kitgun Arcane" && !rows(&weapon["ancestry"]).iter()
        .any(|v| text(v).to_ascii_lowercase().contains("modular")) { return false; }
    if compatible(arcane, weapon) || weapon["slot"] == "arch_gun" && archgun_scope(path, base) { return true; }
    if !frame { return false; }
    let in_ancestry = |path: &str| std::iter::once(&weapon["path"]).chain(rows(&weapon["ancestry"]))
        .any(|v| text(v).trim_end_matches('/') == path) || weapon["slot"] == "arch_gun" && archgun_scope("/Lotus/Upgrades/CosmeticEnhancers/", path);
    if path.ends_with("/GolemArcaneBonusDamageOnWallLatch") {
        return ["/Lotus/Weapons/Tenno/LotusLongGun", "/Lotus/Weapons/Tenno/Pistol/LotusPistol"].into_iter().any(in_ancestry);
    }
    if path.ends_with("/AmmoEfficiencyOnPistolHeadshot") {
        return in_ancestry("/Lotus/Weapons/Tenno/Pistol/LotusPistol");
    }
    rows(&arcane["effects"]).iter().chain(rows(&arcane["conditionalEffects"]).iter().flat_map(|v| rows(&v["effects"])))
        .any(|e| {
            let kind = text(&e["upgradeType"]);
            let valid = text(&e["validType"]).trim_end_matches('/');
            (kind.starts_with("WEAPON_") || kind.starts_with("RUNTIME_"))
                && (kind != "WEAPON_MELEE_DAMAGE" || req.melee) && (valid.is_empty() || in_ancestry(valid))
        }) || runtime(arcane, 0).is_some()
}

impl<'a> Equipped<'a> {
    pub fn resolve(data: &'a EngineData, catalog: &SimCatalog, req: &Request<'_>) -> SimResult<Self> {
        let raw = req.body.get("arcanes").map_or(Some(&[][..]), |v| v.as_array().map(Vec::as_slice))
            .filter(|v| v.len() <= 4).ok_or("arcanes 必须为至多 4 项的数组")?;
        let mut result = Self { cards: Vec::new(), ignored: Vec::new() };
        let mut seen = HashSet::new();
        let mut counts = [0, 0];
        for item in raw {
            let path = query(item);
            let meta = catalog.arcanes.get(path).ok_or("arcanes 必须使用已支持的赋能路径")?;
            if !seen.insert(path) { return Err("不能重复装备同一赋能".into()); }
            let frame = meta["slotKind"] == "warframe";
            counts[frame as usize] += 1;
            if counts[1] > 2 || counts[0] > if req.weapon["slot"] == "arch_gun" { 2 } else { 1 } {
                return Err("赋能数量超出武器或战甲槽位限制".into());
            }
            let record = data.arcanes.get(path).ok_or("赋能缺少导出记录")?;
            if !applies(record, req, frame) {
                if !frame { return Err("赋能与所选武器不兼容".into()); }
                result.ignored.push(format!("arcane-not-for-this-weapon:{path}"));
                continue;
            }
            if item.get("rank") == Some(&Value::Null) { return Err("赋能等级必须为整数".into()); }
            let rank = ranked(&item["rank"], meta["maxRank"].as_u64().unwrap_or(0), "赋能等级")?;
            if item.get("triggerRate").is_some() { return Err("赋能触发率由面板推导".into()); }
            let stacks = if item["stacks"].is_null() { None } else {
                let stacks = item["stacks"].as_u64().ok_or("赋能 stacks 必须为非负整数")?;
                let max = if arcane_status::enervate(record, rank) { u64::MAX }
                    else { runtime(record, rank).map_or_else(|| cap(&meta["trigger"]), |r| r.cap) };
                Some(stacks.min(max))
            };
            let uptime = if item["uptime"].is_null() { None } else {
                Some(item["uptime"].as_f64().filter(|v| (0.0..=1.0).contains(v)).ok_or("赋能 uptime 必须在 0 到 1 之间")?)
            };
            result.cards.push(Arcane { record, rank, stacks, uptime });
        }
        Ok(result)
    }

    pub fn needs_stats(&self) -> bool {
        self.cards.iter().any(|c| c.stacks.is_none() && c.uptime.is_none() && rate_kind(c.record, None).is_some())
    }

    pub fn snapshot(&self, req: &Request<'_>, fields: Option<&ModdedFields>, elapsed: Option<f64>) -> SimResult<Snapshot> {
        let mut out = Snapshot { effects: Vec::new(), states: Vec::new(), notes: Vec::new(), skipped: Vec::new() };
        for (index, card) in self.cards.iter().enumerate() {
            let arcane = card.record;
            let path = text(&arcane["path"]);
            let trigger = &arcane["trigger"];
            let runtime = runtime(arcane, card.rank);
            let status = arcane_status::status(arcane, card.rank);
            let enervate = arcane_status::enervate(arcane, card.rank);
            let crux = arcane_status::crux(arcane);
            let compression = arcane_status::compression(arcane, card.rank);
            let implicit = card.stacks.is_none() && card.uptime.is_none();
            let (stacks, uptime) = if implicit {
                runtime.as_ref().map_or((0, 1.0), |r| r.default(req))
            } else { (card.stacks.unwrap_or(0), card.uptime.unwrap_or(1.0)) };
            let duration = num(&trigger["duration"]);
            let chance = trigger["chance"].as_f64().unwrap_or(1.0);
            let cap = cap(trigger);
            let cond = condition_text(trigger);
            let kinds = modeled_kinds(arcane);
            let kind = rate_kind(arcane, Some(req));
            let inferred = if implicit && runtime.is_none() {
                fields.zip(kind).map(|(f, kind)| {
                    let hits = f.fire_rate.f().max(0.0) * if f.multishot == 0.0 { 1.0 } else { f.multishot.max(0.0) };
                    hits * match kind { "crit" => f.crit_chance.f().clamp(0.0, 1.0), "headshot" => req.aim, _ => 1.0 }
                })
            } else { None };
            if implicit && runtime.is_none() && kind.is_none() && !truth(&arcane["effects"]) && kinds.is_empty() && crux.is_none() && compression.is_none() {
                out.notes.push(format!("arcane-state-{index}-trigger-rate-required"));
            }
            let (stacks, effect_share, expected_stacks, expected_uptime) = if let Some(rate) = inferred.filter(|_| !enervate) {
                if duration <= 0.0 || truth(&trigger["durationScalesWithRank"]) || !(0.0..=1.0).contains(&chance) {
                    return Err("赋能缺少可计算的触发持续时间".into());
                }
                let mean = rate * chance * elapsed.map_or(duration, |time| duration.min(time.max(0.0)));
                let p0 = (-mean).exp();
                let (mut p, mut cumulative, mut expected) = (p0, p0, 0.0);
                for count in 1..=cap {
                    expected += (1.0 - cumulative).max(0.0);
                    p *= mean / count as f64;
                    cumulative = (cumulative + p).min(1.0);
                }
                let expected = expected.min(cap as f64);
                let stacks = expected.ceil() as u64;
                (stacks, if stacks > 0 { expected / stacks as f64 } else { 0.0 }, expected, 1.0 - p0)
            } else { (stacks, uptime, stacks as f64 * uptime, if stacks > 0 { uptime } else { 0.0 }) };
            let mut state = merge(identity(arcane), json!({"query": path, "rank": card.rank, "stacks": stacks,
                "uptime": if inferred.is_some() && !enervate { expected_uptime } else { uptime },
                "mode": if inferred.is_some() { "expected" } else { "manual" },
                "triggerRate": inferred.unwrap_or(0.0), "expectedStacks": expected_stacks, "expectedUptime": expected_uptime,
                "active": false, "effectCount": 0, "applied": false}));
            if trigger.is_object() {
                state = merge(state, json!({"triggerChance": chance, "durationSeconds": duration, "maxStacks": cap,
                    "stackMode": text(&trigger["stackMode"]), "triggerCondition": cond}));
            }
            if inferred.is_some() { state["triggerSource"] = json!("derived-from-manual-stats"); }
            if truth(&arcane["unresolvedReferences"]) {
                out.notes.push(format!("arcane-state-{index}-has-unresolved-references"));
                out.states.push(state);
                continue;
            }
            let mut skipped = Vec::new();
            let mut effects = if let Some(status) = status {
                let share = if card.stacks == Some(0) { 0.0 } else { uptime };
                state["mode"] = json!("events");
                state["eventModel"] = json!(status.kind());
                state["active"] = json!(share > 0.0);
                state["applied"] = json!(true);
                state["effectCount"] = json!(1);
                state["expectedUptime"] = json!(share);
                Vec::new()
            } else if enervate {
                state["mode"] = json!(if card.stacks.is_some() { "explicit-fixed" } else { "events" });
                state["durationSeconds"] = Value::Null;
                state["maxStacks"] = Value::Null;
                state["stackMode"] = json!("joint-probability-reset");
                state["resetAfter"] = json!(card.rank + 1);
                vec![Effect::export(&json!({"upgradeType": "WEAPON_CRIT_CHANCE", "valuePerRank": 0.1 * stacks as f64 * effect_share}), path)]
            } else if let Some((damage, ammo)) = compression {
                let share = if card.stacks == Some(0) { 0.0 } else { effect_share };
                vec![Effect::export(&json!({"upgradeType": "RUNTIME_PRIMARY_COMPRESSION",
                    "valuePerRank": damage * share, "runtimeChance": ammo * share}), path)]
            } else if let Some(runtime) = runtime {
                runtime.effects(path, stacks, effect_share)
            } else if crux.is_none() && truth(&arcane["runtimeScripts"]) {
                skipped.push(format!("{path}: unsupported[runtime-script]"));
                Vec::new()
            } else {
                let mut effects = if crux.is_none() { terms(&arcane["effects"], path, card.rank, 1.0) } else { Vec::new() };
                for raw in rows(&arcane["extraEffects"]) {
                    if card.rank < raw["unlockRank"].as_u64().unwrap_or(0) { continue; }
                    let mut effect = Effect::export(raw, path);
                    if truth(&raw["scalesWithRank"]) { effect.value *= card.rank as f64 + 1.0; }
                    effects.push(effect);
                }
                for condition in crux.as_ref().map(std::slice::from_ref).unwrap_or_else(|| rows(&arcane["conditionalEffects"])) {
                    let source = condition["sourcePath"].as_str().unwrap_or(path);
                    if truth(&condition["runtimeScripts"]) {
                        skipped.push(format!("{source}: unsupported[runtime-script]"));
                        continue;
                    }
                    let active = stacks.min(super::loadout::cap(condition));
                    if active == 0 { continue; }
                    effects.extend(terms(&condition["effects"], source, card.rank, active as f64)
                        .into_iter().map(|mut e| { e.value *= effect_share; e.with_condition(condition) }));
                }
                effects
            };
            let before = effects.len();
            effects.retain(|e| is_weapon_effect(&e.upgrade));
            if effects.len() < before { out.notes.push(format!("arcane-state-{index}-unsupported-effects-skipped")); }
            if !skipped.is_empty() { out.notes.push(format!("arcane-state-{index}-runtime-effects-skipped")); }
            out.skipped.extend(skipped);
            if !effects.is_empty() {
                state["active"] = json!(expected_stacks != 0.0 || enervate);
                state["applied"] = json!(true);
                state["effectCount"] = json!(effects.len());
                if let Some((damage, ammo)) = compression {
                    let share = if card.stacks == Some(0) { 0.0 } else { effect_share };
                    state = merge(state, json!({"mode": "conditional", "triggerCondition": "aiming",
                        "eventModel": "primary-compression", "active": share > 0.0, "expectedUptime": share,
                        "conditionRequired": true,
                        "triggerDescription": "While aiming: lose 80% of the audited attack radius; continuous damage and ammo-efficiency bonuses per metre lost. Unknown profiles are excluded.",
                        "damagePerMeter": damage, "ammoEfficiencyPerMeter": ammo}));
                }
                out.effects.extend(effects);
            } else if status.is_some() {
                // 事件赋能的状态已在上方填充。
            } else if !kinds.is_empty() || crux.is_some() {
                state["applied"] = json!(true);
                state["active"] = json!(expected_stacks != 0.0);
                state["mode"] = json!(if implicit { "events" } else { "explicit-fixed" });
                state["eventModel"] = json!("conditional-effects");
                state["eventKinds"] = if crux.is_some() { json!(["on-headshot"]) } else { json!(kinds) };
            } else {
                state["error"] = json!("no supported static effect");
                out.notes.push(format!("arcane-state-{index}-no-supported-effects"));
            }
            out.states.push(state);
        }
        if elapsed.is_none() { out.notes.extend(self.ignored.iter().cloned()); }
        out.notes.sort_unstable();
        out.notes.dedup();
        Ok(out)
    }
}

fn condition_text(trigger: &Value) -> String {
    std::iter::once(&trigger["conditionTag"]).chain(rows(&trigger["conditions"]))
        .filter_map(Value::as_str).filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" ")
}

fn rate_kind(arcane: &Value, req: Option<&Request<'_>>) -> Option<&'static str> {
    let kind = condition_text(&arcane["trigger"]).to_ascii_lowercase();
    if req.is_some_and(|req| (kind.contains("meleekill") && !req.melee)
        || (kind.contains("primarykill") && req.weapon["slot"] != "primary")) { return None; }
    if kind.contains("headshot") { Some("headshot") }
    else if kind.contains("weakpoint") && (kind.contains("hit") || kind.contains("kill")) { Some("hit") }
    else if kind.contains("crit") { Some("crit") }
    else if ["onhit", "on_hit", "onkill", "on_kill", "killcondition"].iter().any(|s| kind.contains(s)) { Some("hit") }
    else { None }
}

fn modeled_kinds(record: &Value) -> Vec<&str> {
    rows(&record["conditionalEffects"]).iter().map(|c| {
        if truth(&c["runtimeScripts"]) || !truth(&c["effects"]) ||
            !rows(&c["effects"]).iter().all(|e| is_weapon_effect(text(&e["upgradeType"]))) { return None; }
        super::weapon::condition_kind(c)
    }).collect::<Option<_>>().unwrap_or_default()
}

enum DefaultState {
    Inactive,
    Combat(&'static str, f64, f64),
    Flags(&'static [&'static str]),
    Headshot,
}

struct Runtime {
    cap: u64,
    default: DefaultState,
    upgrade: &'static str,
    operation: &'static str,
    per: f64,
    limit: f64,
    dtype: Option<&'static str>,
    secondary: Option<&'static str>,
    victim_stacks: bool,
}

impl Runtime {
    fn default(&self, req: &Request<'_>) -> (u64, f64) {
        let value = |key: &str| req.scenario.state.and_then(|v| v.get(key)).unwrap_or(&Value::Null);
        match self.default {
            DefaultState::Inactive => (0, 1.0),
            DefaultState::Headshot => (1, req.aim),
            DefaultState::Flags(keys) => (keys.iter().any(|key| value(key) == true) as u64, 1.0),
            DefaultState::Combat(key, floor, per) =>
                ((((num(value(key)) - floor) / per + 1e-9).floor().max(0.0) as u64).min(self.cap), 1.0),
        }
    }

    fn effects(&self, path: &str, stacks: u64, uptime: f64) -> Vec<Effect> {
        let stacks = stacks.min(self.cap) as f64;
        let value = if self.victim_stacks { self.per } else { (stacks * self.per.abs()).min(self.limit) * self.per.signum() * uptime };
        std::iter::once(self.upgrade).chain(self.secondary).map(|upgrade| Effect::export(&json!({
            "upgradeType": upgrade, "operation": self.operation, "damageType": self.dtype, "valuePerRank": value,
            "runtimeChance": self.victim_stacks.then_some(stacks * uptime),
        }), path)).collect()
    }
}

fn runtime(record: &Value, rank: u64) -> Option<Runtime> {
    use DefaultState::*;
    let path = text(&record["path"]);
    let mut result = Runtime { cap: 1, default: Inactive, upgrade: "", operation: "STACKING_MULTIPLY",
        per: 0.0, limit: f64::INFINITY, dtype: None, secondary: None, victim_stacks: false };
    let r = rank as f64;
    let script = &record["runtimeScripts"]["apply"];
    let ranked = |script: &Value, key: &str| {
        let values = rows(&script[key]);
        values.get((rank as usize).min(values.len().saturating_sub(1))).and_then(Value::as_f64)
    };
    let filename = |s: &Value| text(&s["Script"]).rsplit('/').next().unwrap_or("").to_owned();
    let has_script = |name| filename(script) == name;
    let close = |value: &Value, expected: f64| value.as_f64().is_some_and(|v| (v - expected).abs() <= 1e-5);
    match path.rsplit('/').next()? {
        "SecondaryExtraDamagePerFreezeStack" if !truth(&record["runtimeScripts"]) && rows(&record["effects"]).len() == 1
            && record["effects"][0]["upgradeType"] == "WEAPON_DAMAGE_PER_ACTIVE_PROC_STACK_ON_VICTIM"
            && close(&record["effects"][0]["valuePerRank"], 0.075) => {
            result.upgrade = "RUNTIME_COLD_STACK_DAMAGE";
            result.operation = "ADD";
            result.per = 0.075 * (r + 1.0);
            result.cap = 10;
            result.victim_stacks = true;
        }
        "FreezeEnemiesOnRoll" => {
            let script = &record["conditionalEffects"][1]["runtimeScripts"]["apply"];
            if filename(script) != "ApplyStatusToEnemiesOnRoll.lua" || script["_bonusDamageProcCount"] != 10
                || !close(&script["_damageToApplyBase"], 0.25) || !close(&script["_damageToApplyPerLevel"], 0.25) { return None; }
            result.upgrade = "RUNTIME_FROZEN_DAMAGE";
            result.operation = "ADD";
            result.per = 0.25 + 0.25 * r;
            result.victim_stacks = true;
        }
        "MeleeDamageForActiveShields" if has_script("MeleeDamageForActiveShields.lua") &&
            script["_shieldsToGetBonus"] == 200 && close(&script["_meleeDamageLimit"], 4.2) &&
            close(&script["_meleeDamagePerBonus"], 0.05) && close(&script["_meleeDamageStepPerFusionLevel"], 0.05) => {
            result.upgrade = "WEAPON_MELEE_DAMAGE";
            result.per = 0.05 + 0.05 * r;
            result.limit = 4.2;
            result.cap = (result.limit / result.per - 1e-9).ceil() as u64;
            result.default = Combat("frameShield", 0.0, 200.0);
        }
        "DamageForBonusArmour" if has_script("GiveStatForStat.lua") && script["_haveStatType"] == "AVATAR_ARMOUR" &&
            script["_giveStatType"] == "WEAPON_DAMAGE_AMOUNT" && script["_haveStatThresholdMin"] == 1000 &&
            script["_maxStacks"] == 500 && close(&script["_giveStatIncreaseBase"], 0.005) &&
            close(&script["_giveStatIncreasePerLevel"], 0.001) => {
            result.upgrade = "WEAPON_DAMAGE_AMOUNT";
            result.per = 0.005 + 0.001 * r;
            result.cap = 500;
            result.default = Combat("frameArmor", 1000.0, 1.0);
        }
        "MultishotForMaxEnergy" if has_script("GiveStatForStat.lua") && script["_haveStatType"] == "AVATAR_POWER_MAX" &&
            script["_giveStatType"] == "WEAPON_FIRE_ITERATIONS" && close(&script["_giveStatIncreaseBase"], 0.0035) &&
            close(&script["_giveStatCapBase"], 1.0) && close(&script["_giveStatCapPerLevel"], 0.5) &&
            close(&script["_haveStatThresholdMin"], 0.9) => {
            result.upgrade = "WEAPON_FIRE_ITERATIONS";
            result.per = 0.0035;
            result.limit = 1.0 + 0.5 * r;
            result.cap = (result.limit / result.per - 1e-9).ceil() as u64;
            result.default = Combat("energyPool", 0.0, 1.0);
        }
        "SecondaryOnOvershieldCritChance" if has_script("SecondaryOnOvershieldCritChance.lua")
            && script["_upgradeType"] == "WEAPON_CRIT_CHANCE" && close(&script["_bonuses"][5], 3.0) => {
            result.upgrade = "WEAPON_CRIT_CHANCE";
            result.operation = "ADD";
            result.per = ranked(script, "_bonuses")?;
            result.default = Flags(&["overshields"]);
        }
        "SecondaryCritChancePerBuff" if has_script("UpgradePerAllyBuff.lua") &&
            script["_upgradeType"] == "WEAPON_CRIT_CHANCE" && script["_operationType"] == "STACKING_MULTIPLY" &&
            close(&script["_baseValue"], 0.0334) && close(&script["_rankValue"], 0.0334) => {
            result.upgrade = "WEAPON_CRIT_CHANCE";
            result.per = 0.0334 * (r + 1.0);
            result.cap = 10;
        }
        "MultishotForCombo" if has_script("MultishotForCombo.lua") && script["_upgradeType"] == "WEAPON_CRIT_DAMAGE" &&
            script["_secondaryUpgradeType"] == "WEAPON_CRIT_CHANCE" && script["_operationType"] == "STACKING_MULTIPLY" &&
            close(&script["_valuesPerCombo"][5], 0.2) => {
            result.upgrade = "WEAPON_CRIT_CHANCE";
            result.secondary = Some("WEAPON_CRIT_DAMAGE");
            result.per = ranked(script, "_valuesPerCombo")?;
            result.cap = 12;
        }
        "CorrosiveMeleePerAbilityCast" if has_script("CorrosiveMeleePerAbilityCast.lua") &&
            script["_damageUpgradeType"] == "DT_CORROSIVE" && script["_upgradeType"] == "WEAPON_PERCENT_BASE_DAMAGE_ADDED" &&
            close(&script["_buffHardLimit"], 2.4) && close(&script["_buffStepPerCast"], 0.1) &&
            close(&script["_buffStepPerFusionLevel"], 0.1) => {
            result.upgrade = "WEAPON_PERCENT_BASE_DAMAGE_ADDED";
            result.operation = "ADD";
            result.dtype = Some("DT_CORROSIVE");
            result.per = 0.1 + 0.1 * r;
            result.limit = 2.4;
            result.cap = (result.limit / result.per - 1e-9).ceil() as u64;
        }
        "LongbowDamageOnHeadshot" if has_script("BonusDamageForNextShot.lua") && close(&script["_damageBonuses"][5], 3.0) => {
            result.upgrade = "RUNTIME_NEXT_SHOT_DAMAGE";
            result.operation = "ADD";
            result.per = ranked(script, "_damageBonuses")?;
            result.default = Headshot;
        }
        "AbilityStrengthAndCritDamageWhenInvisible" if has_script("GiveStatsWhileInvisible.lua") &&
            script["_giveStatTwoType"] == "AVATAR_CRIT_DAMAGE" && script["_giveStatTwoOperation"] == "ADD" &&
            close(&script["_giveStatTwoIncreaseBase"], 0.5) && close(&script["_giveStatTwoIncreasePerLevel"], 0.5) => {
            result.upgrade = "RUNTIME_FINAL_CRIT_DAMAGE";
            result.operation = "ADD";
            result.per = 0.5 + 0.5 * r;
        }
        "WeaponDamageOnResidualContact" => {
            let script = &record["effects"][0]["localizationScript"];
            if rows(&record["effects"]).len() != 1 || filename(script) != "ResidualTrigger.lua"
                || script["_maxStacks"] != 15 || !close(&script["_upgradePct"][5], 0.12) { return None; }
            result.upgrade = "WEAPON_DAMAGE_AMOUNT";
            result.per = ranked(script, "_upgradePct")?;
            result.cap = 15;
        }
        "AmmoEfficiencyOnSliding" if has_script("AmmoEfficiencyOnSliding.lua") && close(&script["_ammoEfficiencyIncreaseLevels"][5], 0.65) => {
            result.upgrade = "WEAPON_AMMO_CONSUME_RATE";
            result.per = -ranked(script, "_ammoEfficiencyIncreaseLevels")?;
            result.default = Flags(&["sliding", "aimGliding"]);
        }
        _ => return None,
    }
    Some(result)
}

pub(super) fn runtime_effect(record: &Value, rank: u64) -> bool { runtime(record, rank).is_some() }
