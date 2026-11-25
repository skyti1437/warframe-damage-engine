use super::{cards::Riven, json, loadout, num, rows, text, truth, EngineData, SimResult, Value};
use crate::catalog::riven::template;

#[derive(Clone)]
pub(super) struct Trait<'a> {
    pub code: &'a str,
    pub label: &'a str,
    pub unit: &'a str,
    pub default_base: f64,
    pub template_base: Option<f64>,
    pub positive: bool,
    pub negative: bool,
    pub fusion: bool,
    pub modeled: bool,
    pub upgrade: &'a str,
}

impl Trait<'_> {
    pub fn line(&self, value: f64, low: f64, high: f64, negative: bool) -> Value {
        json!({"code": self.code, "label": self.label, "unit": self.unit, "value": value,
            "low": low, "high": high, "negative": negative})
    }
}

pub(super) fn traits<'a>(data: &'a EngineData, kind: &str, weapon: &Value) -> Vec<Trait<'a>> {
    let rules = &data.rules.riven;
    let Some(bases) = rules["types"][kind].as_object() else { return Vec::new(); };
    let stats = rows(&template(&data.riven_templates, kind)["stats"]);
    let all_stats = || rows(&data.riven_templates["templates"]).iter()
        .filter(move |template| template["weaponType"] == kind).flat_map(|t| rows(&t["stats"]));
    let sentinel = text(&weapon["slot"]).eq_ignore_ascii_case("companion");
    let gun = kind != "melee";
    let mut traits: Vec<_> = bases.iter().map(|(code, row)| {
        let rule = &rules["traits"][code];
        let fusion = rows(&rules["fusionCodes"]).iter().any(|v| v == code);
        let stat = stats.iter().find(|stat| stat["tag"] == row["faceTag"]);
        let stat = stat.or_else(|| matches!(code.as_str(), "E" | "PA").then(|| {
            all_stats().find(|stat| stat["tag"] == row["faceTag"])
        }).flatten());
        let fusion_available = !fusion || all_stats().any(|stat| {
            stat["tag"] == row["faceTag"] && (stat["isAdvancedTrait"] == true || stat["fusionOnly"] == true)
        });
        let can_roll = |index, flag: &str, restrictions: &str| rule[index] == true && fusion_available
            && stat.is_none_or(|stat| stat[flag] != false && !(sentinel && rows(&stat[restrictions]).iter().any(|v| v == "NotSentinel")));
        let unit = text(&rule[1]);
        let default_base = num(&row["defaultBase"]);
        let template_base = if gun && fusion { Some(default_base) } else {
            stat.and_then(|stat| {
                let upgrades = rows(&stat["upgrades"]);
                let upgrade = if upgrades.len() == 1 { upgrades.first() }
                    else if code == "0" && upgrades.len() == 2 { upgrades.iter().find(|u| !truth(&u["validModifiers"])) }
                    else { None };
                let upgrade = upgrade.filter(|u| (truth(&row["valueTag"]) || (fusion && kind == "melee"))
                    && u["upgradeType"] == rule[4])?;
                upgrade["rawValue"].as_f64().map(|value| value.abs() * if unit == "flat" { 90.0 } else { 9000.0 })
            })
        };
        Trait { code, label: text(&rule[0]), unit, default_base, template_base,
            positive: can_roll(2usize, "canBeBuff", "buffRestrictions"),
            negative: can_roll(3usize, "canBeCurse", "curseRestrictions"),
            fusion, modeled: rows(&data.rules.mods["supportedUpgrades"]).contains(&rule[4]),
            upgrade: text(&rule[4]) }
    }).collect();
    traits.sort_by_key(|line| line.code);
    traits
}

pub(super) fn structure(positives: usize, curse: bool, negative: bool) -> f64 {
    match (positives, curse, negative) {
        (3, _, true) => 0.75,
        (_, _, true) => 0.495,
        (3, true, false) => 0.9375,
        (_, true, false) => 1.2375,
        (3, false, false) => 0.75,
        _ => 0.99,
    }
}

pub(super) fn neutral(melee: bool, code: &str) -> bool {
    if melee { matches!(code, "X" | "E") } else { matches!(code, "Z" | "H" | "M" | "E") }
}

pub(super) fn card(data: &EngineData, lines: Vec<Value>, rank: u64, polarity: &str) -> SimResult<Riven> {
    let stats: Vec<_> = lines.iter().map(|line| json!({
        "code": line["code"], "value": line["value"], "negative": line["negative"]
    })).collect();
    let (effects, _) = loadout::riven_terms(data, &json!(stats))?;
    let skipped = lines.iter().filter(|line| {
        let rule = &data.rules.riven["traits"][text(&line["code"])];
        !rows(&data.rules.mods["supportedUpgrades"]).contains(&rule[4])
    }).map(|line| format!("riven:{}: unsupported[no-handler]", text(&line["code"]))).collect();
    Ok(Riven { raw: json!({"polarity": polarity, "stats": stats}), lines, effects, skipped,
        notes: Vec::new(), polarity: json!(polarity), drain: 10 + rank as i64 })
}
