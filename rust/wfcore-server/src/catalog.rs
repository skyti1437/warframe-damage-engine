mod arcanes;
pub(crate) mod enemies;
mod modular;
mod mods;
pub(crate) mod riven;
mod weapons;

use crate::data::{EngineData, Result, BUILD_LABEL, SCHEMA_VERSION};
use serde_json::{json, Map, Value};

pub fn build(data: &EngineData) -> Result<Value> {
    let stances = stance_rows(data);
    let (weapons, hard_fails, counts) = weapons::rows(data, &stances)?;
    let mods::ModCatalog { mods, utility, aliases, variants, replacements } = mods::rows(data);
    let enemies = enemies::rows(data);
    let arcanes = arcanes::rows(data);
    Ok(json!({
        "buildLabel": BUILD_LABEL, "schemaVersion": SCHEMA_VERSION,
        "manualCounts": {"mods": mods.len(), "enemies": enemies.len(), "arcanes": arcanes.len()},
        "counts": counts, "weapons": weapons, "manualMods": mods, "enemies": enemies,
        "arcanes": arcanes, "stances": stances, "utilityMods": utility,
        "statsByType": riven::stats(data),
        "exaltedWeapons": data.exalted_weapons,
        "knownHardFails": hard_fails,
        "enemyEnhancements": object(&data.enemy_overrides["wikiEnhancements"]),
        "buildOptions": build_options(data),
        "rivenDisposition": data.riven_disposition,
        "manualModAliases": aliases, "manualModVariants": variants,
        "modSets": data.rules.mods["sets"],
        "kitgunParts": modular::kitguns(&data.kitguns),
        "zawParts": modular::zaws(&data.zaws),
        "legacyModReplacements": replacements,
        "sentinel": data.rules.display["sentinel"],
    }))
}

const GUN_METRICS: &[(&str, &str)] = &[
    ("sustainedDps", "持续 DPS"), ("burstDps", "爆发 DPS"), ("shotDamage", "单次伤害"),
];
const MELEE_METRICS: &[(&str, &str)] = &[("attackDps", "攻击 DPS"), ("heavyDamage", "重击伤害")];
const INNATE_MIN: f64 = 0.25;
const INNATE_MAX: f64 = 0.60;

fn build_options(data: &EngineData) -> Value {
    let metric_rows = |metrics: &[(&str, &str)]| {
        metrics.iter().map(|(id, label)| json!({"id": id, "label": label})).collect::<Vec<_>>()
    };
    let elements = [
        ("heat", "DT_FIRE", "火焰"), ("cold", "DT_FREEZE", "冰冻"),
        ("electricity", "DT_ELECTRICITY", "电击"), ("toxin", "DT_POISON", "毒素"),
        ("blast", "DT_EXPLOSION", "爆炸"), ("radiation", "DT_RADIATION", "辐射"),
        ("gas", "DT_GAS", "毒气"), ("magnetic", "DT_MAGNETIC", "磁力"),
        ("viral", "DT_VIRAL", "病毒"), ("corrosive", "DT_CORROSIVE", "腐蚀"),
    ];
    json!({
        "metrics": {"gun": metric_rows(GUN_METRICS), "melee": metric_rows(MELEE_METRICS)},
        "slotCount": {"min": 1, "max": 8, "default": 8},
        "elementTargets": elements.map(|(id, damage_type, label)| json!({"id": id, "damageType": damage_type, "label": label})),
        "innateElement": {"min": INNATE_MIN, "max": INNATE_MAX, "default": INNATE_MAX},
        "clearElementValue": null,
        "damageModels": ["panel", "target", "custom"],
        "customDamageModel": true,
        "limitations": [
            "60 capacity, Catalyst and matching Forma are assumed for automatic builds",
            "element target constrains final damage type, not status weighting",
            "enemy resistances, status DOT, Arcanes and Incarnon choices are not optimized",
        ],
        "externalBuffs": data.rules.display["externalBuffs"],
    })
}

fn stance_rows(data: &EngineData) -> Vec<Value> {
    data.stances.iter().filter_map(|row| {
        let best = array(&row["combos"]).iter()
            .filter_map(|combo| positive(&combo["duration"]).and(positive(&combo["dpsMultiplier"])).map(|dps| (dps, combo)))
            // 倍率相同时取导出表中先出现的连段。
            .reduce(|best, next| if next.0 > best.0 { next } else { best })?.1;
        let mut row = row.clone();
        row["bestComboData"] = best.clone();
        Some(row)
    }).collect()
}

pub(crate) fn array(value: &Value) -> &[Value] {
    value.as_array().map(Vec::as_slice).unwrap_or_default()
}

pub(crate) fn entries(value: &Value) -> impl Iterator<Item = (&String, &Value)> {
    value.as_object().into_iter().flatten()
}

pub(crate) fn number(value: &Value) -> Option<f64> {
    value.as_f64().filter(|v| v.is_finite())
}

fn positive(value: &Value) -> Option<f64> {
    number(value).filter(|v| *v > 0.0)
}

fn object(value: &Value) -> Map<String, Value> {
    value.as_object().cloned().unwrap_or_default()
}

pub(crate) fn text(value: &Value) -> &str {
    value.as_str().unwrap_or_default()
}

pub(crate) fn truth(value: &Value) -> bool {
    match value {
        Value::Null | Value::Bool(false) => false,
        Value::Number(number) => number.as_f64() != Some(0.0),
        Value::String(text) => !text.is_empty(),
        Value::Array(items) => !items.is_empty(),
        Value::Object(fields) => !fields.is_empty(),
        Value::Bool(true) => true,
    }
}

fn first<'a>(values: &[&'a Value]) -> &'a Value {
    values.iter().copied().find(|v| truth(v)).or_else(|| values.last().copied()).unwrap_or(&Value::Null)
}

fn first_text<'a>(values: &[&'a Value]) -> &'a str {
    text(first(values))
}

// 目录的中英文名称采用 Unicode 小写，保留当前导出中会影响排序的大小写折叠。
fn fold(value: &str) -> String {
    value.chars().flat_map(|ch| match ch {
        'ß' | 'ẞ' => "ss".chars().collect::<Vec<_>>(),
        'ς' => vec!['σ'],
        _ => ch.to_lowercase().collect(),
    }).collect()
}

fn sort_named(rows: &mut [Value]) {
    rows.sort_by_cached_key(|row| (fold(text(&row["nameZh"])), text(&row["path"]).to_owned()));
}

// Python 的默认通用浮点格式最多显示六位有效数字。
fn general(value: f64) -> String {
    let exponential = format!("{value:.5e}");
    let (mantissa, exponent) = exponential.split_once('e').unwrap();
    let exponent: i32 = exponent.parse().unwrap();
    if !(-4..6).contains(&exponent) {
        format!("{}e{}{:02}", mantissa.trim_end_matches('0').trim_end_matches('.'),
            if exponent < 0 { '-' } else { '+' }, exponent.abs())
    } else {
        let decimals = (5 - exponent) as usize;
        let fixed = format!("{value:.decimals$}");
        if decimals == 0 { fixed } else { fixed.trim_end_matches('0').trim_end_matches('.').to_owned() }
    }
}
