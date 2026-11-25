//! 异常和命中驱动的赋能从已核验的脚本字段生成内核参数。

use super::{num, rows, text};
use serde_json::{json, Value};

pub(super) const STATUS_POOL: [&str; 13] = ["DT_IMPACT", "DT_PUNCTURE", "DT_SLASH", "DT_FIRE", "DT_FREEZE",
    "DT_ELECTRICITY", "DT_POISON", "DT_EXPLOSION", "DT_CORROSIVE", "DT_MAGNETIC", "DT_VIRAL", "DT_GAS", "DT_RADIATION"];

const ROOT: &str = "/Lotus/Upgrades/CosmeticEnhancers/";
const IGNORED_ALT: &str = "/Lotus/Weapons/Tenno/Pistols/SundialGun/SundialPistol";

#[derive(Clone, Copy)]
pub(super) enum StatusArcane {
    Encumber(f64),
    Cryogenic(f64, u64),
    Empowered(f64),
    Duplicate(f64),
    Fortifier(f64, f64, f64),
}

impl StatusArcane {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Encumber(_) => "encumber",
            Self::Cryogenic(..) => "cryogenic",
            Self::Empowered(_) => "empowered",
            Self::Duplicate(_) => "duplicate",
            Self::Fortifier(..) => "fortifier",
        }
    }
}

pub(super) fn status(record: &Value, rank: u64) -> Option<StatusArcane> {
    if rank > 5 { return None; }
    let path = text(&record["path"]).strip_prefix(ROOT)?;
    let script = &record["runtimeScripts"]["apply"];
    let conditions = rows(&record["conditionalEffects"]);
    let condition = |name| conditions.len() == 1 && rows(&conditions[0]["conditions"]) == [json!(name)];
    let fx = rows(&record["effects"]);
    let r = rank as f64;
    let close = |a: &Value, b: f64, tol: f64| a.as_f64().is_some_and(|v| (v - b).abs() <= tol);
    match path {
        "Utility/ExtraProcOnProc" if fx.len() == 1 && fx[0]["upgradeType"] == "WEAPON_PROC_ON_PROC_CHANCE" &&
            fx[0]["operation"] == "ADD" && close(&fx[0]["valuePerRank"], 0.04, 1e-8) =>
            Some(StatusArcane::Encumber(0.04 * (r + 1.0))),
        "Offensive/SecondaryColdOnPuncture" if script["Script"] == "AoeOnCondition.lua" && script["Function"] == "ApplyUpgrade" &&
            script["_radiusBase"] == 10 && script["_radiusPerLevel"] == 1 && script["_damageBase"] == 0 &&
            script["_damagePerLevel"] == 0 && script["_damageProcTypes"] == json!(["DT_FREEZE"]) &&
            script["_numDamageProcsToSpreadAtLevel"] == json!([1, 1, 2, 2, 3, 3]) && condition("OnPunctureProcCondition") =>
            Some(StatusArcane::Cryogenic(num(&script["_numDamageProcsToSpreadAtLevel"][rank as usize]), 10 + rank)),
        "Zariman/SecondaryOnStatusProcBonusDamage" if script["Script"] == "SecondaryOnStatusProcBonusDamage.lua" &&
            script["Function"] == "ModApplied" && script["_damageAmounts"] == json!([250, 350, 450, 550, 650, 750]) &&
            condition("/Lotus/Upgrades/CosmeticEnhancers/Offensive/OnStatusProcALLCondition") =>
            Some(StatusArcane::Empowered(num(&script["_damageAmounts"][rank as usize]))),
        "Offensive/ReplayLightCritHits" if script["Script"] == "ReplayLightCritHits.lua" && script["Function"] == "OnCritHit" &&
            close(&record["trigger"]["chance"], 0.25, 1e-6) && fx.len() == 1 && close(&fx[0]["valuePerRank"], 0.15, 1e-6) &&
            condition("OnMeleeCritConditionNoIntermediates") => Some(StatusArcane::Duplicate((0.25 + 0.15 * r).min(1.0))),
        "Offensive/OverguardSteal" if script["Script"] == "OverguardLifesteal.lua" && script["Function"] == "ApplyUpgrade" &&
            close(&script["_overguardSteal"], 0.01, 1e-8) && script["_overguardLimit"] == 15000 => {
            let fx = rows(&record["extraEffects"]);
            (fx.len() == 1 && fx[0]["upgradeType"] == "WEAPON_DAMAGE_AMOUNT_TO_OVERGUARD" && fx[0]["operation"] == "MULTIPLY"
                && fx[0]["valuePerRank"] == 3).then_some(StatusArcane::Fortifier(3.0 + r, 0.01, 15000.0))
        }
        _ => None,
    }
}

pub(super) const ENERVATE_ASSUMPTIONS: [&str; 5] = [
    "enervate-grouped-hit-any-big-crit", "enervate-gain-then-reset-after-damage",
    "enervate-minimum-interval-30hz", "enervate-state-target-correlation-expected",
    "enervate-blast-hit-probability-with-cooldown-branches",
];

pub(super) fn enervate(record: &Value, rank: u64) -> bool {
    if rank > 5 || text(&record["path"]).strip_prefix(ROOT) != Some("Offensive/SecondaryCritOnHit") { return false; }
    let script = &record["runtimeScripts"]["apply"];
    script["Script"] == "CritOnHit.lua" && script["Function"] == "OnHit" &&
        script["_critChanceBase"] == 0.1 && script["_critChancePerLevel"] == 0 &&
        script["_numTieredCritsToResetBase"] == 1 && script["_numTieredCritToResetPerLevel"] == 1 &&
        script["_resetCritTier"] == 1 && script["_ignoreAltFireWeapons"] == json!([IGNORED_ALT])
}

pub(super) fn ignore_selected(path: &str, mode: usize) -> bool { path == IGNORED_ALT && mode > 0 }

pub(super) fn crux(record: &Value) -> Option<Value> {
    if text(&record["path"]).strip_prefix(ROOT) != Some("Offensive/StatusAndAmmoEfficiencyOnWeakpointHit") { return None; }
    let script = &record["runtimeScripts"]["apply"];
    let expected = [("_ammoEfficiencyBase", -0.01), ("_ammoEfficiencyPerLevel", -0.01),
        ("_statusChanceBase", 0.05), ("_statusChancePerLevel", 0.05), ("_duration", 10.0), ("_maxStacks", 10.0)];
    if script["Script"] != "StatusAndAmmoEfficiencyOnWeakpointHit.lua" || script["Function"] != "ApplyUpgrade" ||
        !expected.iter().all(|(k, expected)| script[*k].as_f64().is_some_and(|v| (v - expected).abs() <= 1e-5)) {
        return None;
    }
    Some(json!({"sourcePath": record["path"], "conditionTag": "OnWeakpointHitCondition", "conditions": ["OnWeakpointHitCondition"],
        "duration": 10, "maxStacks": 10, "stackMode": "reset", "effects": [
            {"upgradeType": "WEAPON_PROC_CHANCE", "operation": "STACKING_MULTIPLY", "valuePerRank": 0.05},
            {"upgradeType": "WEAPON_AMMO_CONSUME_RATE", "operation": "STACKING_MULTIPLY", "valuePerRank": -0.01},
        ]}))
}

pub(super) fn compression(record: &Value, rank: u64) -> Option<(f64, f64)> {
    if rank > 5 || text(&record["path"]).strip_prefix(ROOT) != Some("Utility/ConvergeRadialAttackOnAim") { return None; }
    let script = &record["runtimeScripts"]["apply"];
    let expected = [("_damageMultPerMeterLostBase", 0.5), ("_damageMultPerMeterLostPerLevel", 0.1),
        ("_ammoEfficiencyPerMeterLostBase", 0.03), ("_ammoEfficiencyPerMeterLostPerLevel", 0.005),
        ("_explosionRadiusBase", -0.8), ("_explosionRadiusPerLevel", 0.0)];
    if text(&script["Script"]).rsplit('/').next() != Some("ConvergeRadialAttackOnAim.lua")
        || script["Function"] != "OnBehaviorChange" || !expected.iter().all(|(k, v)| {
            script[*k].as_f64().is_some_and(|n| (n - v).abs() <= 1e-5)
        }) {
        return None;
    }
    let rank = rank as f64;
    Some((0.5 + 0.1 * rank, 0.03 + 0.005 * rank))
}
