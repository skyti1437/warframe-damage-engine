use super::{json, num, rows, text, truth, EngineData, SimResult, Value};
use super::weapon::Effect;
use indexmap::IndexMap;

pub(super) struct Buffs {
    pub effects: Vec<Effect>,
    pub armour_reduction: f64,
    pub shield_reduction: f64,
}

// Keep the gameplay operation beside its catalog id. The catalog owns ranks,
// strength, family, scope and display metadata.
fn kinds(id: &str) -> &[&str] {
    match id {
        "roar" | "roar-helminth" => &["faction"],
        "eclipse" | "eclipse-helminth" | "molecular-prime" | "final-damage" => &["final"],
        "vex-armor" | "shooting-gallery" | "octavia-amp" | "metamorphosis" | "rifle-amp" |
        "pistol-amp" | "shotgun-amp" | "dead-eye" | "void-strike" | "arbitration" | "vigorous-swap" | "base-damage" => &["base"],
        "haste-mote" => &["fire-rate", "fire-rate"],
        "warcry" | "warcry-helminth" | "fire-rate" => &["fire-rate"],
        "fireball-frenzy" => &["element:DT_FIRE"],
        "freeze-force" => &["element:DT_FREEZE"],
        "shock-trooper" => &["element:DT_ELECTRICITY"],
        "smite-infusion" => &["element:DT_RADIATION"],
        "venom-dose" => &["element:DT_CORROSIVE"],
        "nourish" | "nourish-helminth" => &["element:DT_VIRAL"],
        "toxic-lash" => &["extra-hit:DT_POISON"],
        "xatas-whisper" => &["extra-hit:DT_VOID"],
        "resupply" => &["extra-hit:select"],
        "covenant" => &["crit-flat", "crit-head-flat"],
        "wrathful-advance" | "wrathful-advance-helminth" => &["crit-flat"],
        "hakchum" => &["final:DT_FREEZE"],
        "steel-charge" => &["melee-damage"],
        "worthy-comradery" => &["weakpoint-crit"],
        "ready-steel" => &["initial-combo"],
        "corrosive-projection" => &["enemy-armour"],
        "shield-disruption" => &["enemy-shield"],
        "arcane-crepuscular" => &["final-crit-damage"],
        "crit-chance" => &["crit-chance"],
        "crit-damage" => &["crit-damage"],
        "status" => &["status"],
        "multishot" => &["multishot"],
        "reload" => &["reload"],
        "magazine" => &["magazine"],
        _ => &[],
    }
}

fn effect_kind(kind: &str) -> (&str, &str) {
    let upgrade = match kind {
        "base" => "WEAPON_DAMAGE_AMOUNT",
        "melee-damage" => "WEAPON_MELEE_DAMAGE",
        "faction" => "GAMEPLAY_FACTION_DAMAGE",
        "final" => "RUNTIME_FINAL_DAMAGE",
        "element" => "WEAPON_PERCENT_BASE_DAMAGE_ADDED",
        "extra-hit" => "RUNTIME_EXTRA_HIT",
        "crit-chance" | "crit-flat" => "WEAPON_CRIT_CHANCE",
        "crit-head-flat" | "weakpoint-crit" => "WEAPON_CRIT_CHANCE_WEAKPOINT",
        "crit-damage" => "WEAPON_CRIT_DAMAGE",
        "final-crit-damage" => "RUNTIME_FINAL_CRIT_DAMAGE",
        "status" => "WEAPON_PROC_CHANCE",
        "multishot" => "WEAPON_FIRE_ITERATIONS",
        "fire-rate" => "WEAPON_FIRE_RATE",
        "reload" => "WEAPON_RELOAD_SPEED",
        "magazine" => "WEAPON_CLIP_MAX",
        "initial-combo" => "WEAPON_MELEE_COMBO_INITIAL_BONUS",
        _ => unreachable!("registered external buff kind"),
    };
    let operation = match kind {
        "crit-flat" | "crit-head-flat" | "final-crit-damage" | "initial-combo" | "extra-hit" => "ADD",
        _ => "STACKING_MULTIPLY",
    };
    (upgrade, operation)
}

impl Buffs {
    pub fn resolve(data: &EngineData, raw: &Value, weapon: &Value, melee: bool) -> SimResult<Self> {
        let roster = rows(&data.rules.display["externalBuffs"]);
        let items = if raw.is_null() { &[][..] } else {
            raw.as_array().filter(|v| v.len() <= 40).ok_or("externalBuffs must be an array with at most 40 entries")?.as_slice()
        };
        let mut rows_out = Vec::new();
        let mut families = IndexMap::<_, (_, f64, _)>::new();
        let mut drift = 0.0;
        for item in items {
            if !item.is_object() { return Err("externalBuffs entries must be objects".into()); }
            let id = match text(&item["id"]) {
                "mallet" => "octavia-amp",
                "vigoruous-swap" => "vigorous-swap",
                "unairu-wisp" | "flash-accelerant" => continue,
                other => other,
            };
            let spec = roster.iter().find(|row| row["id"] == id).ok_or("externalBuffs contains an unknown id")?;
            let base = num(&spec["base"]);
            let value = if !item["value"].is_null() {
                item["value"].as_f64().filter(|n| (0.0..=50.0).contains(n)).ok_or("externalBuffs.value is out of range")?
            } else if spec["squad"] == true {
                let count = item.get("count").map_or(Some(1), Value::as_u64)
                    .filter(|n| (1..=4).contains(n)).ok_or("externalBuffs.count must be between 1 and 4")?;
                base * count as f64
            } else if spec["strength"] == true {
                let strength = item.get("strength").map_or(Some(1.0), Value::as_f64)
                    .filter(|n| (0.0..=10.0).contains(n)).ok_or("externalBuffs.strength is out of range")?;
                base * strength
            } else { base };
            let value = spec["maximum"].as_f64().map_or(value, |limit| value.min(limit));
            if id == "coaction-drift" { drift = value; continue; }
            if let Some(family) = spec["family"].as_str().filter(|family| !family.is_empty()) {
                let row = families.entry(family).or_insert((spec, value, item));
                if value > row.1 { *row = (spec, value, item); }
            } else {
                rows_out.push((spec, value, item));
            }
        }
        let sniper = rows(&weapon["ancestry"]).iter().any(|v| v == "/Lotus/Weapons/Tenno/Rifle/LotusSniperRifle");
        let mut result = Self { effects: Vec::new(), armour_reduction: 0.0, shield_reduction: 0.0 };
        for (spec, value, item) in rows_out.into_iter().chain(families.into_values()) {
            let id = text(&spec["id"]);
            let source = format!("external:{id}");
            let value = if spec["group"] == "aura" { value * (1.0 + drift) } else { value };
            for (kind, row) in kinds(id).iter().zip(rows(&spec["effects"])) {
                let mut amount = value * num(&row["scale"]);
                match *kind {
                    "enemy-armour" => { result.armour_reduction = (result.armour_reduction + amount).min(1.0); continue; }
                    "enemy-shield" => { result.shield_reduction = (result.shield_reduction + amount).min(1.0); continue; }
                    _ => {}
                }
                let (kind, mut dtype) = kind.split_once(':').map_or((*kind, None), |(left, right)| (left, Some(right)));
                if dtype == Some("select") {
                    let candidates = rows(&spec["elements"]);
                    dtype = candidates.iter().find(|v| *v == &item["element"]).or(candidates.first()).map(text);
                }
                if kind == "extra-hit" {
                    if melee && truth(&spec["meleeScale"]) { amount *= num(&spec["meleeScale"]); }
                    if sniper && truth(&spec["sniperScale"]) { amount *= num(&spec["sniperScale"]); }
                }
                let (upgrade, operation) = effect_kind(kind);
                let forced = matches!(id, "toxic-lash" | "resupply").then_some(1.0);
                let applies = rows(&row["applies"]);
                for valid in applies.iter().map(Some).chain(applies.is_empty().then_some(None)) {
                    result.effects.push(Effect::export(&json!({"upgradeType":upgrade, "operation":operation,
                        "valuePerRank":amount, "damageType":dtype, "validType":valid, "runtimeChance":forced}), &source));
                }
            }
        }
        Ok(result)
    }
}
