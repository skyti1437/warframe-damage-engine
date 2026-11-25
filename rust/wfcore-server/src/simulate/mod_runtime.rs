//! 只识别参照实现核验过的 MOD 脚本和参数。

use super::{loadout::terms, num, rows, text, Request};
use super::weapon::Effect;
use serde_json::{json, Value};

pub(super) const DOUBLE_TAP: &str = "/Lotus/Upgrades/Mods/PvPMods/Rifle/MoreDamageonTripleTapRifleMod";
pub(super) const CONSECUTIVE_DAMAGE: &str = "RUNTIME_CONSECUTIVE_HIT_DAMAGE";

pub(super) fn double_tap(record: &Value, rank: u64) -> Option<(f64, f64, f64)> {
    let script = &record["runtimeScripts"]["apply"];
    if record["path"] != DOUBLE_TAP || record["maxRank"] != 3 || rank > 3 ||
        script["Script"] != "/Lotus/Upgrades/CosmeticEnhancers/Offensive/BonusDamageOnNextHit.lua" ||
        script["Function"] != "ApplyUpgrade" || (num(&script["_bonusDamagePercent"]) - 0.05).abs() > 1e-5 ||
        (num(&script["_multTimeWindow"]) - 0.5).abs() > 1e-5 {
        return None;
    }
    let per_hit = 0.05 * (rank + 1) as f64;
    Some((per_hit, (4.0 / per_hit + 1e-9).floor(), 0.5 * (rank + 1) as f64))
}

pub(super) fn effects(card: &Value, rank: u64, req: &Request<'_>, skipped: &mut Vec<String>) -> Option<Vec<Effect>> {
    let path = text(&card["path"]);
    let scripts = &card["runtimeScripts"];
    let script = &scripts["apply"];
    let filename = text(&script["Script"]).rsplit('/').next()?;
    let native = rows(&card["effects"]);
    let has_script = |name: &str| native.iter().any(|e| text(&e["localizationScript"]["Script"]).ends_with(name));
    let close = |raw: &Value, expected: f64| raw.as_f64().is_some_and(|v| (v - expected).abs() <= 1e-5);
    let add = |upgrade: &str, operation: &str, value: f64| {
        Effect::export(&json!({"upgradeType": upgrade, "operation": operation, "valuePerRank": value}), path)
    };
    let normal = || terms(&card["effects"], path, rank, 1.0);
    if let Some((damage, _, _)) = double_tap(card, rank) {
        return Some(vec![add(CONSECUTIVE_DAMAGE, "ADD", damage)]);
    }
    match filename {
        "LimitMultishot.lua" | "LimitFireRate.lua" => {
            let multishot = filename == "LimitMultishot.lua";
            let canonical = ["Rifle", "Pistol", "Shotgun"].into_iter().any(|f| {
                let name = if multishot { "WeaponWeakpointCriticalChanceMod".to_owned() } else { format!("{f}SemiAutoFantasyMod") };
                path == format!("/Lotus/Upgrades/Mods/{f}/{name}")
            });
            let script_path = format!("/Lotus/Upgrades/Mods/WeaponStatOverride/{filename}");
            let expected = json!({"apply": {"Function": "OnApplyUpgrade", "Script": script_path},
                "unapply": {"Function": "OnUnapplyUpgrade", "Script": script_path}});
            if !canonical || scripts != &expected { return None; }
            let mut effects = normal();
            effects.push(add(if multishot { "RUNTIME_LOCK_MULTISHOT" } else { "RUNTIME_LOCK_FIRE_RATE" }, "SET", 1.0));
            Some(effects)
        }
        "BleedOnImpactFireRateCheck.lua" => {
            let (dtype, upgrade, internal) = match path {
                "/Lotus/Upgrades/Mods/Pistol/WeaponBleedOnImpactPistolMod" =>
                    ("DT_SLASH", "WEAPON_SLASH_PROC_ON_IMPACT", "WeaponBleedOnImpactPistolMod"),
                "/Lotus/Upgrades/Mods/Rifle/WeaponBleedOnImpactProcRifleMod" =>
                    ("DT_SLASH", "WEAPON_SLASH_PROC_ON_IMPACT", "WeaponBleedOnImpactProcRifleMod"),
                "/Lotus/Upgrades/Mods/Shotgun/WeaponMagneticOnImpactShotgunMod" =>
                    ("DT_MAGNETIC", "WEAPON_MAGNETIC_PROC_ON_IMPACT", "WeaponMagneticOnImpactShotgunMod"),
                _ => return None,
            };
            let chance = script["_baseProcChance"].get(rank as usize)?.as_f64()?;
            if card["nameInternal"] != internal || !(0.0..=1.0).contains(&chance)
                || !close(&script["_fireRateDoubleBuff"], 2.5) || script["_modProcType"] != upgrade || !has_script(filename) {
                return None;
            }
            let mut effects = normal();
            effects.retain(|e| e.upgrade != "NONE");
            effects.push(Effect::export(&json!({"upgradeType": "RUNTIME_IMPACT_PROC_CONVERSION",
                "valuePerRank": chance, "damageType": dtype, "runtimeKind": "impact_conversion", "runtimeDamageType": dtype,
                "runtimeChance": chance, "runtimeThreshold": 2.5, "runtimeMultiplier": 2.0}), path));
            Some(effects)
        }
        "IndependentStatusProcOnDamage.lua" if path == "/Lotus/Upgrades/Mods/Pistol/Event/Nightwave/NightwaveLasGooPistolAugmentMod"
            && card["nameInternal"] == "NightwaveLasGooPistolAugmentMod" && script["_procType"] == "PT_ELECTROCUTION"
            && close(&script["_statusChance"], 0.2) && native.iter().any(|e| e["upgradeType"] == "WEAPON_PERCENT_BASE_DAMAGE_ADDED"
                && e["damageType"] == "DT_ELECTRICITY" && close(&e["valuePerRank"], 0.15)) => {
            let mut effects = normal();
            effects.push(Effect::export(&json!({"upgradeType": "RUNTIME_INDEPENDENT_PROC",
                "valuePerRank": 0.2, "damageType": "DT_ELECTRICITY", "runtimeKind": "independent_proc",
                "runtimeDamageType": "DT_ELECTRICITY", "runtimeChance": 0.2}), path));
            Some(effects)
        }
        "ProjectileDamageNoEnemiesOverDistance.lua" if path == "/Lotus/Upgrades/Mods/Rifle/Event/Nightwave/NightwaveAkjagaraAugmentMod"
            && card["nameInternal"] == "NightwaveAkjagaraAugmentMod" && has_script(filename)
            && close(&script["_rangeRequired"][0], 10.0) => {
            let bonuses = rows(&script["_damageMultiplier"]);
            if bonuses.len() < 6 || !bonuses.iter().zip([0.5, 1.0, 1.5, 2.0, 2.5, 3.0]).all(|(v, x)| close(v, x)) { return None; }
            let amount = bonuses.get(rank as usize)?.as_f64()?;
            let required = num(&script["_rangeRequired"][0]);
            match req.scenario.nearest {
                Some(distance) if distance >= required => {
                    let mut e = Effect::export(&json!({"upgradeType": "WEAPON_DAMAGE_AMOUNT", "operation": "STACKING_MULTIPLY",
                        "damageType": "DT_ANY", "valuePerRank": amount}), path);
                    e.condition = Some("range-advantage".into());
                    Some(vec![e])
                }
                distance => {
                    let note = if distance.is_none() { "requires nearest_enemy_distance".into() }
                        else { format!("inactive nearest_enemy_distance<{required}") };
                    skipped.push(format!("{path}: runtime[range-advantage] {note}"));
                    Some(Vec::new())
                }
            }
        }
        _ if path == "/Lotus/Upgrades/Mods/Rifle/WeaponDamageAmountInvisibleMod"
            && card["nameInternal"] == "WeaponDamageAmountInvisibleMod" && has_script("WeaponDamageAmountInvisibleMod.lua")
            && native.iter().any(|e| e["upgradeType"] == "WEAPON_DAMAGE_AMOUNT" && e["damageType"] == "DT_ANY"
                && close(&e["valuePerRank"], 0.3) && rows(&e["validModifiers"]).iter().any(|v| v == "PM_CLOAK")) => {
            if !req.scenario.cloaked {
                skipped.push(format!("{path}: requires modifier PM_CLOAK"));
                return Some(Vec::new());
            }
            let effect = Effect::export(&json!({"upgradeType": "WEAPON_DAMAGE_AMOUNT", "operation": "STACKING_MULTIPLY",
                "damageType": "DT_ANY", "valuePerRank": 0.3 * (rank as f64 + 1.0), "conditionTag": "PM_CLOAK"}), path);
            Some(vec![effect])
        }
        _ => None,
    }
}
