use super::{named, num, rows, text, truth, request::Request, SimResult};
use crate::data::{EngineData, BUILD_LABEL};
use serde_json::{json, Value};
use std::sync::Arc;
use wfcore_lib::{
    mods::{self, ClassifiedEffect, EffectInput, ModdedFields, ModsInput, ModsPrep, Num},
    pymath::psum, replay::{Stats, Types}, tables::{self, Core},
};

#[derive(Clone)]
pub(super) struct Effect {
    pub upgrade: String,
    pub operation: String,
    pub value: f64,
    dtype: Option<String>,
    symbol: Option<String>,
    valid_type: Option<String>,
    source: String,
}

impl Effect {
    fn export(raw: &Value, source: &str) -> Self {
        let string = |key: &str| raw[key].as_str().filter(|v| !v.is_empty()).map(str::to_owned);
        Self {
            upgrade: string("upgradeType").unwrap_or_else(|| "NONE".into()),
            operation: string("operation").unwrap_or_else(|| "ADD".into()),
            value: num(&raw["valuePerRank"]), dtype: string("damageType"), symbol: string("symbolFilter"),
            valid_type: string("validType"), source: source.into(),
        }
    }

    pub fn last_multishot(value: f64) -> Self {
        Self::export(&json!({"upgradeType":"WEAPON_FIRE_ITERATIONS", "valuePerRank": value}),
            "incarnon:last-round-multishot")
    }

    fn classify(&self, index: usize, weapon: &Value) -> ClassifiedEffect {
        let fits = self.valid_type.as_ref().is_none_or(|name| {
            let name = name.trim_end_matches('/');
            text(&weapon["path"]).trim_end_matches('/') == name
                || rows(&weapon["ancestry"]).iter().any(|v| text(v).trim_end_matches('/') == name)
        });
        ClassifiedEffect {
            index, status: if fits { 0 } else { 2 }, message: None, unsupported: None,
            row: Arc::new(EffectInput {
                upgrade: self.upgrade.clone(), operation: self.operation.clone(), value: self.value,
                damage_type: self.dtype.clone(), symbol: self.symbol.clone(), source: Some(self.source.clone()),
                runtime_kind: None, runtime_damage_type: None, runtime_chance: None,
                runtime_threshold: None, runtime_multiplier: 1.0,
            }.into()),
        }
    }
}

/// 脚本明确读取的玩家状态。状态未给出时，该进化效果保持关闭。
fn actor_gate(condition: &Value, state: Option<&serde_json::Map<String, Value>>) -> Option<bool> {
    let script = &condition["runtimeScripts"]["apply"];
    let state_at = |key| state.and_then(|v| v.get(key)).unwrap_or(&Value::Null);
    let name = text(&script["Script"]);
    if script["Function"] == "StartMod" {
        return match name {
            "/Lotus/Scripts/Mods/ChannelAbilityMod.lua" => Some(truth(state_at("channeling"))),
            "/Lotus/Scripts/Mods/RequireOvershields.lua" => Some(truth(state_at("overshields"))),
            "/Lotus/Scripts/Mods/RequireArmor.lua" => script["_minArmorRequired"].as_f64().map(|min| num(state_at("frameArmor")) >= min),
            "/Lotus/Scripts/Mods/RequireEnergyPool.lua" => script["_minEnergyPoolRequired"].as_f64().map(|min| num(state_at("energyPool")) >= min),
            "/Lotus/Scripts/Mods/RequireSprintSpeed.lua" => script["_minSpeedMultiplier"].as_f64().map(|min| num(state_at("sprintMultiplier")) >= min),
            _ => None,
        };
    }
    if name == "/Lotus/Upgrades/Mods/ModOnAimGlideOrSlide.lua" && script["Function"] == "ModApplied"
        && script["_mustMatchWeapon"] == 1 && script["_upgradeType"] == "WEAPON_AMMO_CONSUME_RATE"
        && script["_stackingMode"] == "ADD" && rows(&condition["effects"]).len() == 1
        && (num(&condition["effects"][0]["valuePerRank"]) - num(&script["_value"])).abs() < 1e-8 {
        return Some(truth(state_at("aimGliding")) || truth(state_at("sliding")));
    }
    None
}

pub(super) struct Evolution {
    pub choices: Value,
    pub policy: Value,
    pub effects: Vec<Effect>,
    pub notes: Vec<String>,
    pub last_multishot: f64,
    pub last_burst: bool,
    pub sources: Vec<Trigger>,
}

pub(super) struct Trigger {
    pub id: String,
    pub label: String,
    pub kind: &'static str,
    pub effects: Vec<Effect>,
    pub cap: f64,
    pub duration: f64,
    pub chance: f64,
    pub fixed: bool,
    pub hold: bool,
}

fn condition_kind(value: &Value) -> Option<&'static str> {
    let keys = rows(&value["conditions"]);
    let tag = &value["conditionTag"];
    let candidates = if keys.is_empty() { std::slice::from_ref(tag) } else { keys };
    candidates.iter().find_map(|key| {
        let key = text(key).rsplit('/').next().unwrap_or_default().to_ascii_lowercase();
        match key.as_str() {
            "onreloadfromemptyclipcondition" | "onreloadfromemptycondition_description" => Some("on-reload-empty"),
            "onreloadarchgunfromemptyclipcondition" => Some("on-reload-empty-complete"),
            "onreloadshotguncondition" | "onreloadriflecondition" | "onreloadpistolcondition" => Some("on-reload-complete"),
            "onreloadcondition" => Some("on-reload"),
            _ => None,
        }
    })
}

impl Evolution {
    pub fn resolve(data: &EngineData, req: &Request<'_>) -> SimResult<Self> {
        let mut result = Self {
            choices: json!({}), policy: json!({}), effects: Vec::new(), notes: Vec::new(),
            last_multishot: 0.0, last_burst: false, sources: Vec::new(),
        };
        let incarnon = &req.weapon["incarnon"];
        let tiers = rows(&incarnon["tiers"]);
        let mut explicit = std::collections::BTreeMap::new();
        if let Some(requested) = req.choices.as_object() {
            for (key, value) in requested {
                let Some(tier) = key.trim().strip_prefix("tier").unwrap_or(key).parse::<u64>().ok() else { continue; };
                let want = if let Some(v) = value.as_str() { v.trim().to_owned() } else { value.to_string() };
                if want.is_empty() { continue; }
                if explicit.insert(tier, want).is_some() { return Err(format!("重复的灵化阶段 tier{tier}")); }
                if !tiers.iter().any(|row| row["tier"] == tier && (2..=5).contains(&tier)
                    && (row["selectable"] == true || rows(&row["branches"]).len() > 1)) {
                    return Err(format!("此武器无法选择灵化阶段 tier{tier}"));
                }
            }
        }
        if tiers.is_empty() { return Ok(result); }
        let defaults = &data.incarnon_defaults;
        let adapter = text(&incarnon["adapter"]);
        let key = adapter.trim_end_matches('/').rsplit('/').next().unwrap_or_default();
        result.policy = json!({"adapter": adapter, "strategyKey": key, "sampledAt": defaults["sampledAt"],
            "gameBuild": defaults["gameBuild"], "matched": false});
        let mut policy_route = &Value::Null;
        if defaults["gameBuild"] != BUILD_LABEL {
            result.policy["reason"] = json!("build-mismatch");
            result.notes.push("incarnon-policy-build-mismatch".into());
        } else if key.is_empty() {
            result.policy["reason"] = json!("weapon-adapter-missing");
            result.notes.push("incarnon-policy-adapter-missing".into());
        } else if let Some((name, strategy)) = defaults["strategies"].as_object().and_then(|v|
            v.iter().find(|(name, strategy)| name.eq_ignore_ascii_case(key) && strategy.is_object())) {
            policy_route = &strategy["route"];
            let builds = rows(&strategy["builds"]);
            let sources = if rows(&strategy["sources"]).is_empty() {
                let base = text(&defaults["sourceBase"]).trim_end_matches('/');
                let mut sources: Vec<_> = builds.iter().filter(|b| !b["id"].is_null())
                    .map(|b| format!("{base}/{}/", b["id"])).collect();
                if truth(&defaults["regionalSource"]) { sources.push(text(&defaults["regionalSource"]).into()); }
                json!(sources)
            } else { strategy["sources"].clone() };
            result.policy.as_object_mut().unwrap().extend(json!({
                "strategyKey": name, "objective": strategy["objective"], "confidence": strategy["confidence"],
                "sources": sources, "builds": builds, "alternatives": rows(&strategy["alternatives"]),
                "sourceKind": defaults["sourceKind"].as_str().unwrap_or("whole-build-votes"),
                "selectionMode": "community-default", "matched": true
            }).as_object().unwrap().clone());
        } else {
            result.policy["reason"] = json!("strategy-not-found");
            result.notes.push(format!("incarnon-policy-{key}-not-found"));
        }
        let mut applied = Vec::new();
        for tier in tiers {
            let number = tier["tier"].as_u64().unwrap_or(0);
            let branches = rows(&tier["branches"]);
            if number == 1 {
                for branch in branches {
                    result.add_actor_effects(branch, req.scenario.state);
                }
                continue;
            }
            if number < 2 || branches.is_empty() { continue; }
            let tier_key = format!("tier{number}");
            let policy_id = policy_route[&tier_key].as_str().or_else(|| policy_route[number.to_string()].as_str());
            let requested = explicit.get(&number).map(String::as_str);
            let wanted = requested.or(policy_id);
            let picked = wanted.and_then(|id| branches.iter().enumerate().find_map(|(index, b)| {
                (b["id"] == id || b["path"] == id
                    || id.parse::<u64>().ok() == Some(b["index"].as_u64().unwrap_or(index as u64 + 1)))
                    .then_some(b)
            }));
            if requested.is_some() && picked.is_none() {
                return Err(format!("灵化阶段 {tier_key} 没有选项 {}", requested.unwrap()));
            }
            let branch = if let Some(branch) = picked { branch } else if branches.len() == 1 { &branches[0] } else {
                return Err(format!("灵化阶段 {tier_key} 需要指定可用的进化选项"));
            };
            let id = text(&branch["id"]);
            if requested.is_none() && policy_id == Some(id) { applied.push(tier_key.clone()); }
            result.choices[&tier_key] = json!(id);
            result.add_branch(branch, req.scenario.policy == "assumed_max", req.scenario.state,
                &data.mechanics["infinite-conditions"]["sources"]);
        }
        result.policy["appliedTiers"] = json!(applied);
        result.notes.sort_unstable();
        result.notes.dedup();
        Ok(result)
    }

    fn add_actor_effects(&mut self, branch: &Value, state: Option<&serde_json::Map<String, Value>>) {
        for condition in rows(&branch["conditionalEffects"]) {
            if actor_gate(condition, state) == Some(true) {
                let path = condition["sourcePath"].as_str().unwrap_or(text(&branch["path"]));
                self.effects.extend(rows(&condition["effects"]).iter()
                    .filter(|v| is_weapon_effect(text(&v["upgradeType"])))
                    .map(|v| Effect::export(v, path)));
            }
        }
    }

    fn add_branch(&mut self, branch: &Value, assumed: bool, state: Option<&serde_json::Map<String, Value>>,
                  infinite_sources: &Value) {
        let path = text(&branch["path"]);
        let id = text(&branch["id"]);
        let conditions = rows(&branch["conditionalEffects"]);
        let mut unknown_conditions = false;
        for (index, condition) in conditions.iter().enumerate() {
            let source = condition["sourcePath"].as_str().unwrap_or(path);
            let effects = rows(&condition["effects"]);
            if actor_gate(condition, state).is_some() || effects.iter().any(|row| row["upgradeType"] == "WEAPON_LAST_MULTISHOT") {
                continue;
            }
            if text(&condition["runtimeScripts"]["apply"]["Script"]).ends_with("/OnLastBurstCondition.lua") {
                self.last_burst = true;
            }
            if let Some(kind) = condition_kind(condition).filter(|_| {
                !truth(&condition["runtimeScripts"]) && !effects.is_empty() &&
                    effects.iter().all(|v| is_weapon_effect(text(&v["upgradeType"])))
            }) {
                let hold = truth(&condition["isInfinite"]) || infinite_sources.get(source).is_some();
                self.sources.push(Trigger {
                    id: format!("{source}:{index}:0"),
                    label: branch["nameZh"].as_str().or_else(|| branch["nameEn"].as_str())
                        .filter(|s| !s.is_empty()).map_or_else(|| format!("灵化 {id}"), str::to_owned),
                    kind, effects: effects.iter().map(|v| Effect::export(v, source)).collect(),
                    cap: condition["maxStacks"].as_f64().filter(|v| *v > 0.0).unwrap_or(1.0),
                    duration: if hold { 0.0 } else { num(&condition["duration"]) },
                    chance: condition["chance"].as_f64().unwrap_or(1.0), fixed: assumed, hold,
                });
            } else if !effects.is_empty() {
                unknown_conditions = true;
            }
        }
        for raw in rows(&branch["effects"]) {
            let effect = Effect::export(raw, path);
            if effect.upgrade == "WEAPON_LAST_MULTISHOT" {
                self.last_multishot += effect.value;
                continue;
            }
            let conditional = conditions.iter().find(|c| truth(&c["effects"]) &&
                c["sourcePath"].as_str().unwrap_or(path) == raw["sourcePath"].as_str().unwrap_or(path));
            if let Some(condition) = conditional {
                if actor_gate(condition, state).is_some() { continue; }
                if !truth(&condition["conditions"]) && !truth(&condition["runtimeScripts"]) {
                    if is_weapon_effect(&effect.upgrade) { self.effects.push(effect); }
                } else if condition_kind(condition).is_none() || truth(&condition["runtimeScripts"]) {
                    self.notes.push(format!("incarnon-{id}-conditional-source-excluded"));
                }
            } else if is_weapon_effect(&effect.upgrade) {
                self.effects.push(effect);
            } else {
                self.notes.push(format!("incarnon-{id}-{}-not-evaluated", effect.upgrade));
            }
        }
        if unknown_conditions { self.notes.push(format!("incarnon-{id}-conditional-effects-not-applied")); }
        if truth(&branch["runtimeScripts"]) && !conditions.iter().any(|condition| actor_gate(condition, state).is_some()) {
            self.notes.push(format!("incarnon-{id}-runtime-scripts-not-applied"));
        }
        self.add_actor_effects(branch, state);
        if truth(&branch["unresolvedReferences"]) { self.notes.push(format!("incarnon-{id}-unresolved-references")); }
    }
}

fn is_weapon_effect(upgrade: &str) -> bool {
    mods::supports_effect(upgrade)
}

pub(super) struct Weapon<'a> {
    pub data: &'a EngineData,
    pub req: &'a Request<'a>,
    pub path: &'a str,
    pub modes: &'a [Value],
    pub reference: &'a Value,
    pub incarnon: &'a Value,
}

fn numeric(value: &Value, default: Num) -> Num {
    if let Some(v) = value.as_i64() { Num::I(v) }
    else if let Some(v) = value.as_f64() { Num::F(v) }
    else { default }
}

fn nonzero(value: &Value, default: f64) -> f64 {
    value.as_f64().filter(|v| *v != 0.0).unwrap_or(default)
}

impl<'a> Weapon<'a> {
    pub fn new(data: &'a EngineData, req: &'a Request<'a>) -> Self {
        let path = text(&req.weapon["path"]);
        let reference = data.mechanics.get("combat-reference").unwrap_or(&Value::Null);
        Self { data, req, path, modes: rows(&req.weapon["fireModes"]),
            reference, incarnon: &reference["incarnon"][path] }
    }

    pub fn is_incarnon(&self, index: usize) -> bool {
        truth(&self.req.weapon["isIncarnon"]) && self.modes[index]["isIncarnonForm"].as_bool()
            .unwrap_or(index > 0 && index + 1 == self.modes.len())
    }

    pub fn profile(&self, index: usize) -> Value {
        let mode = &self.modes[index];
        let root = &self.reference["auditedAttacks"][self.path];
        let selected = if index == 0 { root } else { &root["modes"][index.to_string()] };
        let mut profile = if self.is_incarnon(index) && !truth(&selected["allowIncarnon"]) {
            json!({})
        } else { selected.as_object().map_or_else(|| json!({}), |m| Value::Object(m.clone())) };
        let cadence = &self.reference["cadenceProfiles"][self.path];
        if cadence["fireMode"] == index {
            profile.as_object_mut().unwrap().extend(cadence.as_object().unwrap().clone());
        }
        let total = psum(named(&mode["damage"]["types"]).into_iter().map(|(_, v)| v));
        if profile["ammoCost"].as_f64().is_some_and(|v| v != num(&mode["ammoCost"]))
            || (truth(&profile["projectile"]) && profile["projectile"] != mode["damage"]["projectile"])
            || profile["directTotal"].as_f64().is_some_and(|v| (v - total).abs() > 0.02) {
            profile = json!({});
        }
        let charge = self.mode_profile("charge-profiles", index);
        if !charge.is_null() && !truth(&profile["bow"]) {
            if !profile["charge"].is_object() {
                profile["charge"] = json!({"seconds": charge["seconds"], "beforeFire": charge["beforeFire"]});
            }
            profile["charge"]["scalesWithFireRate"] = json!(charge["chargeModifier"] == "WEAPON_FIRE_RATE");
        }
        if !self.is_incarnon(index) {
            let battery = self.mechanics("battery-profiles");
            if !battery.is_null() { profile["battery"] = battery.clone(); }
            if profile["reloadPerShell"].is_null() {
                if let Some(shell) = self.mechanics("shell-reload-profiles").as_object() {
                    profile.as_object_mut().unwrap().extend(shell.clone());
                }
            }
        }
        profile
    }

    pub fn mechanics(&self, dataset: &str) -> &Value {
        &self.data.mechanics.get(dataset).unwrap_or(&Value::Null)["weapons"][self.path]
    }

    pub fn mode_profile(&self, dataset: &str, index: usize) -> &Value {
        let imported = &self.mechanics(dataset)["modes"][index.to_string()];
        let mode = &self.modes[index];
        if imported["trigger"] == mode["trigger"] &&
            imported["behaviour"].as_u64().is_none_or(|v| mode["index"] == v) &&
            (num(&imported["fireRate"]) - num(&mode["fireRate"])).abs() <= 1e-6 {
            imported
        } else { &Value::Null }
    }

    pub fn reduce(&self, index: usize, mode: &Value, profile: &Value, effects: &[Effect],
                  cycle: bool, status_count: usize, part: bool) -> SimResult<ModdedFields> {
        if let Some(reason) = mode["unavailableReason"].as_str().filter(|_| mode["behaviorKind"] == "ability-action") {
            return Err(reason.to_owned());
        }
        if mode["behaviorKind"] == "auxiliary-melee" { return Err("此模式是远程武器的辅助近战攻击".into()); }
        let damage = &mode["damage"];
        let base = named(&damage["types"]);
        if base.is_empty() { return Err("射击模式缺少伤害数据".into()); }
        let scaling = nonzero(&mode["chargeDamageMultiplier"], 1.0);
        let continuous = mode["trigger"] == "/Lotus/Language/Menu/Loadout_TriggerContinous";
        let native_multishot = nonzero(&mode["multishot"], 1.0);
        let proc = num(&damage["procChance"]);
        let force = forced(&damage["forcedProcs"]);
        let is_incarnon = self.is_incarnon(index);
        let pool = self.incarnon["pool"].as_f64().filter(|v| *v > 0.0);
        let radial = matches!(text(&damage["source"]), "ExplosiveAttack" | "RadialDamage" | "EmbedDeathAttack" | "StackingAttack");
        let charge = &profile["charge"];
        let battery = &profile["battery"];
        let reload_each = truth(self.mode_profile("reload-after-shot", index));
        let rr = rows(&mode["falloffRange"]);
        let falloff = rr.first().and_then(Value::as_f64).zip(rr.get(1).and_then(Value::as_f64))
            .zip(mode["falloffMinDamage"].as_f64()).map(|((start, end), min)| (start, end, min));
        let mut notes = Vec::new();
        if !part && truth(&damage["components"]) && !["radial", "radials", "lingering", "pureRadial"]
            .iter().any(|k| truth(&profile[*k])) {
            notes.push(("projectile-component-damage-unapplied".into(),
                format!("{}: unsupported[projectile-component-damage-unapplied]", self.path)));
        }
        let co = &self.reference["coProfiles"][self.path][index.to_string()];
        let co = if (num(&co["directTotal"]) - psum(base.iter().map(|(_, v)| *v))).abs() <= 0.02
            && co["projectile"] == damage["projectile"] { co } else { &Value::Null };
        let passive = self.mechanics("weapon-passives");
        let mut installed = effects.to_vec();
        for source in rows(&passive["innate"]) {
            if truth(&source["fireModes"]) && !rows(&source["fireModes"]).iter().any(|v| v == index) { continue; }
            installed.extend(rows(&source["effects"]).iter()
                .filter(|effect| is_weapon_effect(text(&effect["upgradeType"])))
                .map(|effect| Effect::export(effect, text(&source["path"]))));
        }
        let copy = &mode["multishotCopy"][if part { "radial" } else { "direct" }];
        let prep = ModsPrep {
            base: base.into_iter().map(|(name, value)| (mods::intern(&name), value * scaling)).collect(),
            aiming: self.req.scenario.aiming,
            co_base_fraction: co["baseFraction"].as_f64().unwrap_or(1.0),
            gunco_present: truth(co), gunco_base_fraction: co["baseFraction"].as_f64(),
            gunco_independent: co["behavior"] == "independent",
            gunco_exclude: rows(&co["excludeFlatSources"]).iter().filter_map(Value::as_str).map(str::to_owned).collect(),
            direct_source_stack_damage: truth(&profile["directTakesVictimStackDamage"]) && !part,
            notes, distance: if part { None } else { self.req.scenario.distance }, falloff,
            speed_falloff: !rr.is_empty(),
            innate_multishot: native_multishot, native_ignore_fire_iterations: truth(&mode["nativeIgnoreFireIterations"]),
            per_pellet_proc: proc / native_multishot.max(1.0),
            incarnon_form: is_incarnon, incarnon_pool: pool,
            incarnon_cycle_seconds: pool.filter(|_| cycle && is_incarnon)
                .map(|_| num(&self.incarnon["enterSeconds"]) + num(&self.incarnon["exitSeconds"])),
            incarnon_cycle: cycle,
            base_magazine: nonzero(&self.req.weapon["magazineSize"], nonzero(&profile["magazine"], if self.req.weapon["slot"] == "arch_gun" { 0.0 } else { 6.0 })),
            reload_every_shot: reload_each && !is_incarnon,
            reload_base: if num(&mode["reloadTime"]) == 0.0 { Num::F(0.0) } else { numeric(&mode["reloadTime"], Num::F(0.0)) },
            shell: profile["reloadPerShell"].as_f64().map(|per| (num(&profile["reloadStart"]), per, num(&profile["reloadEnd"]))),
            battery: battery["regenPerSecond"].as_f64().map(|regen| (numeric(&battery["delayEmptySeconds"], Num::F(0.0)),
                regen, battery["emptyAnimationSeconds"].as_f64(), num(&battery["delayPartialSeconds"]))),
            crit_chance: numeric(&damage["critChance"], Num::F(0.0)),
            crit_multiplier: numeric(&damage["critMultiplier"], Num::F(1.0)),
            fire_rate: if num(&mode["fireRate"]) == 0.0 { Num::F(0.0) } else { numeric(&mode["fireRate"], Num::F(0.0)) },
            charge_scales: charge["scalesWithFireRate"].as_bool().unwrap_or(self.req.weapon["slot"] != "arch_gun"),
            charge_seconds: num(&charge["seconds"]), bow_draw: if truth(&profile["bow"]) { Some(num(&profile["drawSeconds"])) } else { None },
            continuous, radial_only: radial,
            ignore_headshot_bonus: Some(!radial && !part &&
                (truth(&damage["ignoreHeadshotBonus"]) || truth(&damage["impactIgnoreHeadshotBonus"]))),
            copy: truth(copy).then(|| (copy["scale"].as_f64().unwrap_or(1.0),
                copy.get("critChance").map(|v| numeric(v, Num::F(0.0))), copy["critMultiplier"].as_f64(), copy["procChance"].as_f64())),
            super_crit: passive["superCritOnStatus"]["chance"].as_f64().filter(|_| !is_incarnon)
                .map(|v| (v, num(&passive["superCritOnStatus"]["critChance"]))),
            super_status: passive["superStatusOnCrit"]["chance"].as_f64().filter(|_| !is_incarnon)
                .map(|v| (v, num(&passive["superStatusOnCrit"]["statusChance"]))),
            orb_stacks: 1, forced: force, ammo_cost: nonzero(&mode["ammoCost"], 1.0), ..ModsPrep::default()
        };
        let result = mods::apply_mods(&prep, ModsInput {
            effects: installed.iter().enumerate().map(|(index, e)| e.classify(index, self.req.weapon)).collect(),
            skipped: Vec::new(), victim_status_count: status_count as f64, need_spec: false,
        }, std::iter::empty::<Result<_, std::convert::Infallible>>()).unwrap();
        Ok(result.fields)
    }
}

fn forced(raw: &Value) -> Vec<(String, f64)> {
    [("PT_KNOCKBACK", "DT_IMPACT"), ("PT_FRAILTY", "DT_PUNCTURE"), ("PT_BLEEDING", "DT_SLASH"),
     ("PT_IMMOLATION", "DT_FIRE"), ("PT_CHILLED", "DT_FREEZE"), ("PT_ELECTROCUTION", "DT_ELECTRICITY"),
     ("PT_POISONED", "DT_POISON"), ("PT_FLASHBANG", "DT_EXPLOSION"), ("PT_RAD_TOX", "DT_RADIATION"),
     ("PT_ASPHYXIATION", "DT_GAS"), ("PT_MAGNETIZED", "DT_MAGNETIC"), ("PT_INFECTED", "DT_VIRAL"),
     ("PT_CAUSTIC_BURN", "DT_CORROSIVE")].into_iter().filter_map(|(proc, dtype)| {
        let count = rows(raw).iter().filter(|value| *value == proc).count();
        (count > 0).then(|| (dtype.to_owned(), count as f64))
    }).collect()
}

pub(super) fn core(fields: &ModdedFields, types: &mut Types, faction: &str, bracket: f64) -> Core {
    let damage_types = fields.damage_types.0.iter().map(|&(id, value)| (types.id(&mods::name(id)), value)).collect();
    let mut vector = |items: &[(String, f64)]| items.iter().map(|(name, v)| (types.id(name), *v)).collect();
    let proc_duration_types = vector(&fields.proc_duration_types);
    let proc_max_stacks = vector(&fields.proc_max_stacks);
    let native_forced_procs = vector(&fields.native_forced_procs);
    let independent_proc_chances = vector(&fields.independent_proc_chances);
    let faction_mod = |name| fields.faction_damage_multipliers.iter().find(|(k, _)| k == name).map_or(1.0, |(_, v)| *v);
    Core {
        damage_types, dot_base_damage: Some(fields.dot_base_damage),
        crit_chance: fields.crit_chance.f(), crit_multiplier: fields.crit_multiplier,
        noncrit_damage_multiplier: fields.noncrit_damage_multiplier, double_crit_chance: fields.double_crit_chance,
        falloff_multiplier: fields.falloff_multiplier, faction_all: faction_mod(""), faction_target: faction_mod(faction),
        faction_bracket: bracket, multishot: fields.multishot, continuous: fields.continuous,
        radial_only: fields.radial_only, ignore_headshot_bonus: fields.ignore_headshot_bonus.unwrap_or(false),
        area_direct: fields.area_direct, bodyshot_crit_multiplier: fields.bodyshot_crit_multiplier,
        weakpoint_crit_chance_bonus: fields.weakpoint_crit_chance_bonus.f(),
        base_crit_chance: fields.base_crit_chance.f(), weakpoint_crit_chance_relative: fields.weakpoint_crit_chance_relative.f(),
        headshot_bonus_multiplier: fields.headshot_bonus_multiplier, weakpoint_damage_bonus: fields.weakpoint_damage_bonus,
        status_chance: fields.status_chance, proc_duration_multiplier: fields.proc_duration_multiplier,
        proc_duration_types, proc_max_stacks, proc_damage_multiplier: fields.proc_damage_multiplier,
        native_forced_procs, independent_proc_chances,
        crit_proc_chances: fields.crit_proc_chances.map(|v| vec![(tables::SLASH, v.f())]).unwrap_or_default(),
        impact_proc_conversions: fields.impact_proc_conversions.iter().map(|(name, v)| (types.id(name), v.clone())).collect(),
        fire_rate: fields.fire_rate.f(),
        dot_elemental_multipliers: fields.dot_elemental_multipliers.iter().map(|(name, v)| (types.id(name), *v)).collect(),
        extra_hits: fields.extra_hits.iter().map(|(name, v, forced)| (types.id(name), *v, *forced)).collect(),
        low_crit_threshold: fields.low_crit_threshold, low_crit_damage_base: fields.low_crit_damage_base,
        low_crit_damage_bonus: fields.low_crit_damage_bonus,
    }
}

pub(super) fn replay_stats(fields: &ModdedFields, core: Core) -> Stats {
    Stats {
        magazine: fields.magazine, ammo_cost: fields.ammo_cost, fire_rate: core.fire_rate,
        reload_time: fields.reload_time.f(), transform_time_scale: fields.transform_time_scale.f(),
        stack_native_fraction: fields.stack_native_fraction, stackable_hit_event: fields.stackable_hit_event,
        multishot: core.multishot, continuous: core.continuous, radial_only: core.radial_only,
        crit_chance: core.crit_chance, crit_multiplier: core.crit_multiplier,
        bodyshot_crit_multiplier: core.bodyshot_crit_multiplier, weakpoint_crit_chance_bonus: core.weakpoint_crit_chance_bonus,
        base_crit_chance: core.base_crit_chance, weakpoint_crit_chance_relative: core.weakpoint_crit_chance_relative,
        status_chance: core.status_chance, proc_duration_multiplier: core.proc_duration_multiplier,
        proc_damage_multiplier: core.proc_damage_multiplier, damage_types: core.damage_types.clone(),
        native_forced_procs: core.native_forced_procs.clone(), independent_proc_chances: core.independent_proc_chances.clone(),
        has_crit_procs: !core.crit_proc_chances.is_empty(),
        first_shot_damage_bonus: fields.first_shot_damage_bonus, cold_stack_damage: fields.cold_stack_damage.f(),
        base_damage_bracket: fields.base_damage_bracket, frozen_damage_multiplier: fields.frozen_damage_multiplier,
        bow_draw: fields.bow_draw_seconds, charge_time: fields.charge_seconds,
        initial_combo: fields.initial_combo, heavy_charge_speed: fields.heavy_charge_speed,
        projectile_speed_multiplier: fields.projectile_speed_multiplier,
        reflection_explosion_chance: fields.reflection_explosion_chance, throw_damage_bonus: fields.throw_damage_bonus.f(),
        combo_duration: fields.combo_duration, combo_extra_chance: fields.combo_extra_chance.f(),
        combo_miss_chance: fields.combo_miss_chance.f(), heavy_attack_efficiency: fields.heavy_attack_efficiency,
        victim_full_crit_chance: fields.victim_full_crit_chance.f(), victim_full_crit_damage: fields.victim_full_crit_damage,
        victim_low_health_multiplier: fields.victim_low_health_multiplier, ammo_pool_multishot_cost: fields.ammo_pool_multishot_cost,
        ammo_pool_damage_bonus: fields.ammo_pool_damage_bonus, slam_damage_ratio: fields.slam_damage_ratio,
        incarnon_charge_multiplier: fields.incarnon_charge_multiplier, core, ..Stats::default()
    }
}

pub(super) fn interval(fields: &ModdedFields) -> f64 {
    fields.bow_draw_seconds.unwrap_or_else(|| {
        let rate = fields.fire_rate.f();
        if rate > 0.0 { 1.0 / rate + fields.charge_seconds } else { f64::INFINITY }
    })
}

pub(super) fn attack_rate(fields: &ModdedFields) -> f64 {
    let interval = interval(fields);
    let fire_rate = fields.fire_rate.f();
    let cost = if fields.ammo_cost == 0.0 && fields.bow_draw_seconds.is_some() { 1.0 } else { fields.ammo_cost };
    if fields.magazine == 0.0 || fire_rate == 0.0 || cost == 0.0 {
        return if interval > 0.0 { 1.0 / interval } else { fire_rate };
    }
    let shots = fields.magazine / cost;
    shots / (shots * interval + fields.reload_time.f())
}
