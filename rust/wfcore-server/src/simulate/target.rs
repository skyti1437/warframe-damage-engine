use super::{named, num, rows, text, truth};
use crate::{catalog::enemies::{field, find_head}, data::EngineData};
use serde_json::{json, Value};
use std::collections::BTreeSet;
use wfcore_lib::{
    pymath::pow, replay::Types, sustained::{armour_reduction, magnetic_defense, SustainedTarget},
    tables::{Location, TargetTables, Vector, CINEMATIC, ELECTRICITY, EXPLOSION, FIRE, GAS, POISON},
};

pub(super) const BENCHMARK_TARGET: &str = "/Lotus/Types/Enemies/Duviri/Avatars/DuviriMeleeAvatar";

#[derive(Clone)]

pub(super) struct Target<'a> {
    pub record: &'a Value,
    pub bracket: f64,
    pub faction: &'a str,
    pub deltas: Vec<(String, f64)>,
    pub armour_bypass: Vec<String>,
    pub shield_bypass: Vec<String>,
    pub head: HitLocation,
}

#[derive(Clone, Copy)]
pub(super) struct HitLocation {
    pub data: Location,
    pub chance: Option<f64>,
    pub unsupported: Option<&'static str>,
}

impl HitLocation {
    pub const BODY: Self = Self {
        data: Location { hit_multiplier: 1.0, hit_critical_multiplier: 1.0, head: false },
        chance: None, unsupported: None,
    };

    fn head(enemy: &Value) -> Self {
        let raw = ["hitProxies", "hitProxy", "weakpoints", "weakPoints"].iter()
            .find_map(|key| enemy.get(*key));
        let proxy = raw.and_then(find_head);
        let mut result = Self { data: Location { head: true, ..Self::BODY.data }, ..Self::BODY };
        let nonnegative = |value: &Value| value.as_f64().filter(|v| *v >= 0.0);
        let fallback = ["headshotMultiplier", "headMultiplier"].iter()
            .find_map(|key| nonnegative(&enemy[*key]));
        let Some(proxy) = proxy else {
            if let Some(multiplier) = fallback {
                result.data.hit_multiplier = multiplier;
            } else {
                result.unsupported = Some(if raw.is_some() { "hit-location-head-invalid" } else { "hit-location-head-unavailable" });
            }
            return result;
        };
        let atten = field(proxy, &["damageatten"]);
        let direct = atten.and_then(nonnegative).map(|v| v * 1.5)
            .or_else(|| nonnegative(proxy))
            .or_else(|| field(proxy, &["damagemultiplier", "multiplier"]).and_then(nonnegative))
            .or_else(|| atten.is_none().then_some(1.5))
            .or_else(|| field(proxy, &["headshotmultiplier", "headmultiplier", "weakpointmultiplier",
                "damagescale", "scale", "value"]).and_then(nonnegative));
        if let Some(direct) = direct {
            result.data.hit_multiplier = direct;
            result.data.hit_critical_multiplier = field(proxy, &["criticalmultiplier"]).and_then(nonnegative).unwrap_or(1.0);
            result.chance = field(proxy, &["criticalchance"]).and_then(nonnegative);
            if field(proxy, &["damagemultipliers"]).is_some() {
                result.unsupported = Some("hit-location-damage-multipliers-unapplied");
            }
        } else {
            result.unsupported = Some("hit-location-head-invalid");
        }
        result
    }

    pub fn metadata(self) -> Value {
        let mut info = json!({"hitLocation": if self.data.head { "head" } else { "body" },
            "hitMultiplier": self.data.hit_multiplier});
        if self.data.head && !matches!(self.unsupported, Some("hit-location-head-invalid" | "hit-location-head-unavailable")) {
            info["weakpointMultiplier"] = json!(self.data.hit_multiplier);
            info["headcritMultiplier"] = json!(self.data.hit_critical_multiplier);
            if let Some(chance) = self.chance { info["headcritChance"] = json!(chance); }
        }
        info
    }
}

pub(super) struct Defenses {
    pub raw: Value,
    pub full: [f64; 3],
    pub armour: f64,
    pub armour_base: f64,
    pub armour_strip: f64,
    pub elemental_strip: f64,
    pub magnetic: i64,
    pub shielded: bool,
    pub unsupported: Vec<String>,
}

impl<'a> Target<'a> {
    pub fn new(record: &'a Value, data: &EngineData) -> Self {
        let embedded = record.get("factionDamageModifiers").filter(|v| !v.is_null())
            .or_else(|| record.get("factionModifiers")).filter(|v| v.is_object());
        let deltas = named(embedded.unwrap_or(&data.rules.factions[text(&record["factionResistance"])]));
        let suffix = text(&record["path"]).rsplit('/').next().unwrap_or_default();
        let demolisher = (suffix.contains("Disruption") || suffix.starts_with("Demo")
            || suffix.starts_with("PNWNarmerDemo")) && !suffix.contains("ShieldDrone");
        let bracket = record["factionBracket"].as_f64().unwrap_or(if demolisher { 0.8 } else { 1.0 });
        let types = |key: &str, defaults: &[&str]| -> Vec<String> {
            if record[key].is_array() { rows(&record[key]).iter().filter_map(Value::as_str).map(str::to_owned).collect() }
            else { defaults.iter().map(|v| (*v).to_owned()).collect() }
        };
        Self {
            record, bracket, faction: text(&record["faction"]), deltas,
            armour_bypass: types("armourBypassDamageTypes", &["DT_CINEMATIC", "DT_HEALTH_DRAIN", "DT_FINISHER"]),
            shield_bypass: types("shieldBypassDamageTypes", &["DT_POISON", "DT_HEALTH_DRAIN"]),
            head: HitLocation::head(record),
        }
    }

    pub fn defenses(&self, level: u64, steel: bool, strip: f64, elemental: f64, selected: &Value) -> Defenses {
        let mut unsupported = BTreeSet::new();
        let enemy = self.record;
        let mut pool = |key: &str, required: bool| {
            let value = enemy.get(key).map_or(Some(0.0), Value::as_f64).filter(|v| *v >= 0.0);
            match value {
                None => { unsupported.insert(format!("enemy-{key}-invalid")); 0.0 }
                Some(v) => {
                    if required && v == 0.0 { unsupported.insert(format!("enemy-{key}-missing")); }
                    v
                }
            }
        };
        let mut health = pool("health", true);
        let mut shield = pool("shield", false);
        let base_level = enemy["baseLevel"].as_f64().filter(|v| *v >= 0.0);
        if base_level.is_none() {
            unsupported.insert(if rows(&enemy["baseLevelCandidates"]).len() > 1 {
                "enemy-base-level-ambiguous"
            } else { "enemy-base-level-missing" }.to_owned());
        }
        let shield_override = selected["shieldOverride"].as_f64();
        let shield_type = shield_override.filter(|v| *v > 0.0)
            .map(|_| if shield > 0.0 && truth(&enemy["shieldType"]) { text(&enemy["shieldType"]) } else { "RK_SHIELD" });
        let standard = !["path", "name", "nameEn", "nameZh", "nameInternal"].iter().any(|key| {
            let name = text(&enemy[*key]).to_ascii_lowercase();
            ["prosecutor", "warden", "archwing"].iter().any(|word| name.contains(word))
        });
        let eximus = selected["eximus"] == true;
        let empowered = selected["empowered"] == true;
        let standard_eximus = eximus && standard;
        if eximus && !standard { unsupported.insert("eximus-special-variant".into()); }
        let scaled_level = level as f64;
        let ex_scale = eximus_scale(scaled_level);
        if standard_eximus {
            let coefficient = if shield > 0.0 || truth(&enemy["armour"]) { 0.25 } else { 0.375 };
            health = (health * 1.1).max(coefficient * (health + 900.0) * ex_scale);
        }
        shield = shield_override.unwrap_or(shield);
        if standard_eximus && shield > 0.0 { shield *= ex_scale.max(1.1); }
        let faction = text(&enemy["factionResistance"]);
        let shield_curve = shield_curve(faction);
        let shield_level = base_level.or_else(|| {
            (enemy["path"] == "/Lotus/Types/Enemies/Orokin/RifleSpacemanAvatar"
                && enemy["damageController"] == "/Lotus/Types/Enemies/Corpus/Spaceman/AIWeek/RifleSpacemanDamageController").then_some(1.0)
        });
        let aura_armour = num(&selected["auraArmourReduction"]);
        let aura_shield = num(&selected["auraShieldReduction"]);
        let shield_native = shield_level.filter(|_| shield > 0.0 && !eximus && !empowered)
            .map(|base| native_shield(shield, (scaled_level - base).max(0.0), shield_curve, steel, aura_shield))
            .filter(|v| v.is_finite());
        let delta = base_level.map_or(0.0, |base| (scaled_level - base).max(0.0));
        let steel_multiplier = if steel { 2.5 } else { 1.0 };
        health *= scaling(delta, health_curve(faction));
        health *= steel_multiplier;
        shield = shield_native.unwrap_or_else(|| (shield * scaling(delta, shield_curve)) * steel_multiplier);
        let armour_base = if num(&enemy["armour"]) > 0.0 {
            let scale = if enemy["usesNpcArmourCurve"] == false { 1.0 }
                else { scaling(delta, [0.005, 1.75, 0.4, 0.75]) };
            (num(&enemy["armour"]) * scale).clamp(200.0, 2700.0)
        } else { 0.0 };
        let armour = (armour_base * (1.0 - strip)) * (1.0 - elemental) * (1.0 - aura_armour);
        let mut overguard = num(&enemy["overguard"]) * overguard_scale(scaled_level);
        if standard_eximus {
            overguard = overguard.max(12.0 * overguard_scale(scaled_level));
        } else if truth(&enemy["hasOverguard"]) && overguard <= 0.0 {
            unsupported.insert("enemy-overguard".into());
        }
        let empowerment = if empowered { 2.0 + 0.5 * selected["playerCount"].as_f64().unwrap_or(1.0) } else { 1.0 };
        health *= empowerment;
        shield *= empowerment;
        if shield_native.is_none() { shield *= 1.0 - aura_shield; }
        if selected["archimedea"] == true { overguard += health * 0.5; }
        if selected["noOverguard"] == true { overguard = 0.0; }
        if ["damageAttenuation", "hasDamageAttenuation", "dynamicDamageResistance"].iter().any(|key| truth(&enemy[*key])) {
            unsupported.insert("enemy-damage-attenuation".into());
        }
        unsupported.extend(rows(&enemy["unsupportedDefenses"]).iter().filter_map(Value::as_str).map(str::to_owned));
        let unsupported: Vec<_> = unsupported.into_iter().collect();
        let magnetic = selected["magneticStacks"].as_i64().unwrap_or(0);
        let mut enhancements = json!({"eximus": eximus, "archimedea": selected["archimedea"] == true,
            "empowered": empowered, "playerCount": selected["playerCount"],
            "magneticStacks": magnetic, "noOverguard": selected["noOverguard"] == true,
            "healthScale": 1.0, "auraArmourReduction": 0.0, "auraShieldReduction": 0.0});
        if let Some(v) = shield_override { enhancements["shieldOverride"] = json!(v); }
        let mut raw = json!({"level": level, "baseLevel": base_level, "steelPath": steel,
            "steelPathMultiplier": steel_multiplier, "steelPathShieldMultiplier": steel_multiplier,
            "steelPathArmorMultiplier": 1.0, "health": health, "shield": shield, "armour": armour,
            "armourReduction": armour_reduction(armour), "elementalArmourStrip": elemental,
            "overguard": overguard, "overguardMultiplier": if standard_eximus { overguard_scale(scaled_level) } else { 0.0 },
            "magneticStacks": magnetic, "empoweredMultiplier": empowerment, "enhancements": enhancements,
            "unsupported": unsupported});
        if let Some(kind) = shield_type { raw["shieldType"] = json!(kind); }
        let shielded = if shield_override.is_some() { shield > 0.0 } else { truth(&enemy["shield"]) };
        Defenses { raw, full: [overguard, shield, health], armour, armour_base, armour_strip: strip,
            elemental_strip: elemental, magnetic, shielded, unsupported }
    }

    pub fn tables(&self, types: &Types, defenses: Option<&Defenses>, forced: Vector) -> TargetTables {
        let damage = [HitLocation::BODY.data, self.head.data];
        let dot_multiplier = if let Some(defenses) = defenses {
            [CINEMATIC, FIRE, POISON, ELECTRICITY, GAS, EXPLOSION].into_iter()
                .map(|d| (d, self.multiplier(defenses, types.name(d), defenses.armour))).collect()
        } else { Vec::new() };
        TargetTables { damage, dot: damage, dot_head_eligible: [false, !matches!(self.head.unsupported,
            Some("hit-location-head-invalid" | "hit-location-head-unavailable"))],
            target_faction: !self.faction.is_empty(), dot_multiplier, forced_procs: forced }
    }

    pub fn resistance(&self, dtype: &str) -> f64 {
        (1.0 + self.deltas.iter().find(|(name, _)| name == dtype).map_or(0.0, |(_, v)| *v)).max(0.0)
    }

    pub fn multiplier(&self, defenses: &Defenses, dtype: &str, armour: f64) -> f64 {
        let [overguard, shield, _] = defenses.full;
        if overguard > 0.0 { return overguard_multiplier(dtype, defenses.magnetic); }
        let bypass = self.shield_bypass.iter().any(|v| v == dtype);
        let mut value = self.resistance(dtype);
        if (!defenses.shielded || bypass) && !self.armour_bypass.iter().any(|v| v == dtype) {
            value *= 1.0 - armour_reduction(armour);
        }
        if shield > 0.0 && !bypass { value *= magnetic_defense(defenses.magnetic); }
        value
    }

    pub fn sustained(&self, defenses: &Defenses, hit: HitLocation) -> SustainedTarget {
        let [overguard, shield, _] = defenses.full;
        let location = |v: Location| (v.hit_multiplier, v.hit_critical_multiplier, v.head);
        SustainedTarget {
            armour_base: defenses.armour_base, armour_strip: defenses.armour_strip, aura_armour_reduction: 0.0,
            elemental_armour_strip: defenses.elemental_strip, overguard, shield, magnetic_stacks: defenses.magnetic,
            shielded: defenses.shielded, faction_deltas: self.deltas.clone(), armour_bypass: self.armour_bypass.clone(),
            shield_bypass: self.shield_bypass.clone(), target_faction: self.faction.to_owned(), faction_bracket: self.bracket,
            body: location(HitLocation::BODY.data), located: location(hit.data), existing_magnetic: defenses.magnetic,
            head_eligible: hit.data.head && !matches!(hit.unsupported, Some("hit-location-head-invalid" | "hit-location-head-unavailable")),
        }
    }
}

pub(super) fn overguard_multiplier(dtype: &str, magnetic: i64) -> f64 {
    (if matches!(dtype, "DT_RADIANT" | "DT_VOID") { 1.5 } else { 1.0 }) * magnetic_defense(magnetic)
}

fn health_curve(faction: &str) -> [f64; 4] {
    match faction {
        "RK_GRINEER_FACTION" | "RK_GRINEER_KUVA_FACTION" | "RK_LASRIA_FACTION" => [0.015, 2.12, 10.7332, 0.72],
        "RK_CORPUS_FACTION" | "RK_CORPUS_AMALGAM_FACTION" => [0.015, 2.12, 13.4165, 0.55],
        "RK_INFESTED_FACTION" | "RK_INFESTED_DEIMOS_FACTION" => [0.0225, 2.12, 16.0998, 0.72],
        "RK_OROKIN_FACTION" | "RK_OROKIN_EMPIRE_FACTION" => [0.015, 2.1, 10.7332, 0.685],
        "RK_PROTOINFESTED_FACTION" => [0.02, 2.12, 15.0998, 0.7],
        _ => [0.015, 2.0, 10.7332, 0.5],
    }
}

fn shield_curve(faction: &str) -> [f64; 4] {
    match faction {
        "RK_CORPUS_FACTION" | "RK_CORPUS_AMALGAM_FACTION" => [0.02, 1.76, 2.0, 0.76],
        "RK_OROKIN_FACTION" | "RK_OROKIN_EMPIRE_FACTION" => [0.02, 1.75, 2.0, 0.75],
        "RK_PROTOINFESTED_FACTION" => [0.02, 1.76, 3.5, 0.76],
        _ => [0.02, 1.75, 1.6, 0.75],
    }
}

fn scaling(delta: f64, [low, lo_exp, high, hi_exp]: [f64; 4]) -> f64 {
    let lo = low * pow(delta, lo_exp);
    let hi = high * pow(delta, hi_exp);
    let t = ((delta - 70.0) / 10.0).clamp(0.0, 1.0);
    1.0 + lo + (hi - lo) * (3.0 * pow(t, 2.0) - 2.0 * pow(t, 3.0))
}

fn native_shield(base: f64, delta: f64, curve: [f64; 4], steel: bool, aura: f64) -> f64 {
    let [lo, lo_exp, hi, hi_exp] = curve.map(|v| v as f32);
    let delta = delta as f32;
    let low = 1.0 + lo * pow(delta as f64, lo_exp as f64) as f32;
    let high = 1.0 + hi * pow(delta as f64, hi_exp as f64) as f32;
    let t = ((delta - 70.0) / 10.0).clamp(0.0, 1.0);
    let blend = (t * t) * (3.0 - 2.0 * t);
    let value = low + (high - low) * blend;
    let sp = if steel { 1.58114_f32 } else { 1.0 };
    let initial = (base as f32 * sp + 0.5).floor();
    let stored = (initial * value + 0.5).floor();
    (stored * sp * (1.0 - aura as f32) + 0.5).floor() as f64
}

fn overguard_scale(level: f64) -> f64 {
    let level = level.max(1.0);
    let low = 1.0 + 0.0015 * pow(level - 1.0, 4.0);
    let high = 1.0 + 260.0 * pow(level - 1.0, 0.9);
    if level <= 45.0 { low }
    else if level >= 50.0 { high }
    else {
        let t = ((level - 45.0) / 5.0) as f32 as f64;
        let blend = (3.0 * t * t - 2.0 * t * t * t) as f32;
        (low + ((high - low) as f32 * blend) as f64) as f32 as f64
    }
}

fn eximus_scale(level: f64) -> f64 {
    if level <= 15.0 { 1.0 }
    else if level <= 25.0 { 1.0 + 0.025 * (level - 15.0) }
    else if level <= 35.0 { 1.25 + 0.125 * (level - 25.0) }
    else if level <= 50.0 { 2.5 + (2.0 / 15.0) * (level - 35.0) }
    else if level <= 100.0 { 4.5 + 0.03 * (level - 50.0) }
    else { 6.0 }
}
