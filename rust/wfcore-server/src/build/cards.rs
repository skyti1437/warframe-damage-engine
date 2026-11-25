use super::{json, loadout, rows, text, truth, EngineData, Map, Request, SimCatalog, SimResult, Value};
use super::weapon::{self, Effect};
use std::collections::HashSet;

pub(super) const ELEMENTS: [&str; 4] = ["DT_FIRE", "DT_FREEZE", "DT_ELECTRICITY", "DT_POISON"];

pub(super) struct Card<'a> {
    pub raw: &'a Value,
    pub path: &'a str,
    pub family: String,
    pub rank: u64,
    pub drain: i64,
    pub effects: Vec<Effect>,
    pub skipped: Vec<String>,
    pub elemental: u8,
    pub reserved: bool,
    pub set: String,
}

pub(super) fn elemental(effects: &[Effect]) -> u8 {
    effects.iter().filter(|e| e.value > 0.0).filter_map(|e| {
        ELEMENTS.iter().position(|key| e.dtype.as_deref() == Some(key))
    }).fold(0, |mask, index| mask | (1 << index))
}

pub(super) fn matching_drain(drain: i64, polarity: &Value) -> i64 {
    if !truth(polarity) || polarity == "AP_UNIVERSAL" { drain } else { (drain + 1) / 2 }
}

// The optimizer admits panel effects and explicit replay-only cards. Projectile
// runtime adapters are still applied when a card is equipped in another slot.
pub(super) fn panel_effect(upgrade: &str) -> bool {
    weapon::is_weapon_effect(upgrade) &&
        !matches!(upgrade, "RUNTIME_IMPACT_PROC_CONVERSION" | "RUNTIME_INDEPENDENT_PROC")
}

impl<'a> Card<'a> {
    pub fn new(raw: &'a Value, rank: u64, req: &Request<'_>) -> Self {
        let path = text(&raw["path"]);
        let mut stacks = Map::new();
        for condition in rows(&raw["conditionalEffects"]).iter().filter(|c| loadout::kill_gate(c)) {
            for key in loadout::keys(condition) {
                stacks.insert(key.to_owned(), json!(loadout::cap(condition)));
            }
        }
        let mut skipped = Vec::new();
        let effects = loadout::expand(raw, rank, req, &stacks, &mut skipped);
        let family = ["modFamily", "nameTag", "nameEn", "nameInternal"].iter()
            .find_map(|key| raw[*key].as_str().filter(|v| !v.is_empty())).unwrap_or(path).to_lowercase();
        Self { raw, path, family, rank, drain: (raw["baseDrain"].as_i64().unwrap_or(0) + rank as i64).max(0),
            elemental: elemental(&effects), effects, skipped, reserved: false, set: loadout::set_key(&raw["modSet"]) }
    }

    pub fn cost(&self) -> i64 { matching_drain(self.drain, &self.raw["polarity"]) }
}

pub(super) fn candidates<'a>(data: &'a EngineData, catalog: &SimCatalog, req: &Request<'_>,
    weapon_type: &str, route: Option<&str>, fixed_exilus: Option<&Card<'_>>) -> (Vec<Card<'a>>, usize) {
    let mut pool = Vec::new();
    let mut rejected = 0;
    let tree = &data.mechanics["melee-mechanics"]["weapons"][req.weapon["meleePath"].as_str()
        .unwrap_or(text(&req.weapon["path"]))]["tree"];
    let hammer = rows(&req.weapon["compatTags"]).iter().any(|tag| text(tag).eq_ignore_ascii_case("HAMMERS_STANCE"))
        || text(tree).ends_with("/HammerMeleeTree");
    let reserve_reach = weapon_type == "melee" && !truth(&req.weapon["isExalted"]) && route != Some("glaive") && !hammer;
    for raw in data.mods.values() {
        let path = text(&raw["path"]);
        if raw["selectable"] != true || !truth(&raw["compatibility"]) || !loadout::compatible(raw, req.weapon)
            || truth(&raw["catalogExclusion"])
            || (path.contains("/PvPMods/") &&
                super::super::mod_runtime::double_tap(raw, raw["maxRank"].as_u64().unwrap_or(0)).is_none()) {
            continue;
        }
        let mut card = Card::new(raw, raw["maxRank"].as_u64().unwrap_or(0), req);
        let active = card.effects.iter().any(|e| panel_effect(&e.upgrade))
            || rows(&raw["conditionalEffects"]).iter().any(|c| {
                (loadout::combo_gate(c) || weapon::condition_kind(c) == Some("on-glaive-throw"))
                    && rows(&c["effects"]).iter().any(|e| panel_effect(text(&e["upgradeType"])))
            });
        if !active { rejected += 1; }
        card.reserved = reserve_reach && raw["nameEn"] == "Primed Reach";
        if (active || card.reserved) && catalog.mods.contains_key(path) && !catalog.utility.contains_key(path)
            && fixed_exilus.is_none_or(|exilus| exilus.family != card.family) {
            pool.push(card);
        }
    }
    pool.sort_by_key(|c| c.path);
    (pool, rejected)
}

pub(super) struct Riven {
    pub raw: Value,
    pub lines: Vec<Value>,
    pub effects: Vec<Effect>,
    pub skipped: Vec<String>,
    pub notes: Vec<String>,
    pub polarity: Value,
}

impl Riven {
    pub fn parse(data: &EngineData, raw: &Value) -> SimResult<Option<Self>> {
        if raw.is_null() { return Ok(None); }
        if !raw.is_object() { return Err("riven must be an object".into()); }
        if let Some(rank) = raw.get("rank") {
            if rank.as_u64().is_none_or(|rank| rank > 8) { return Err("riven.rank must be an integer between 0 and 8".into()); }
        }
        let list = raw["stats"].as_array().filter(|stats| (1..=4).contains(&stats.len()))
            .ok_or("riven.stats must contain between one and four stats")?;
        let mut seen = HashSet::new();
        let mut lines = Vec::new();
        let mut valid = Vec::new();
        let mut notes = Vec::new();
        let mut negatives = 0;
        for stat in list {
            let stat = stat.as_object().ok_or("riven stats must be objects")?;
            let code = stat.get("code").and_then(Value::as_str).unwrap_or("").trim().to_ascii_uppercase();
            if code.is_empty() || !seen.insert(code.clone()) { return Err("riven stat codes must be non-empty and unique".into()); }
            let rule = &data.rules.riven["traits"][&code];
            let negative = stat.get("negative").map_or(Ok(false), |v| v.as_bool().ok_or("riven stat negative must be boolean"))?;
            negatives += usize::from(negative);
            if !rule.is_null() && rule[if negative { 3 } else { 2 }] == false {
                return Err(format!("riven stat {code} cannot be rolled with this sign"));
            }
            let number = |key| stat.get(key).map_or(Some(0.0), Value::as_f64)
                .filter(|n| n.is_finite() && *n >= 0.0).ok_or("riven stat values must be finite and non-negative");
            let (value, low, high) = (number("value")?, number("low")?, number("high")?);
            if high > 0.0 && low > high { return Err("riven stat low exceeds high".into()); }
            let label = stat.get("label").and_then(Value::as_str).filter(|x| !x.is_empty())
                .or_else(|| rule[0].as_str()).unwrap_or(&code);
            let unit = stat.get("unit").and_then(Value::as_str).filter(|x| !x.is_empty()).unwrap_or(text(&rule[1]));
            lines.push(json!({"code": code, "label": label, "unit": unit, "value": value, "low": low, "high": high, "negative": negative}));
            if rule.is_null() { notes.push(format!("riven-stat-{code}-unknown")); }
            valid.push(json!({"code": code, "value": value, "negative": negative}));
        }
        if negatives > 1 || list.len() - negatives > 3 {
            return Err("riven supports at most three positive stats and one negative stat".into());
        }
        let (effects, _) = loadout::riven_terms(data, &json!(valid))?;
        let skipped: Vec<_> = lines.iter().filter_map(|line| {
            let code = text(&line["code"]);
            data.rules.riven["traits"][code].is_null().then(|| format!("riven:{code}: unsupported[no-handler]"))
        }).collect();
        let polarity = match text(&raw["polarity"]) {
            "AP_ATTACK" | "AP_DEFENSE" | "AP_TACTIC" => raw["polarity"].clone(),
            _ => json!("AP_ATTACK"),
        };
        Ok(Some(Self { raw: raw.clone(), lines, effects, skipped, notes, polarity }))
    }
}

pub(super) fn utility<'a>(data: &'a EngineData, req: &Request<'_>, row: &Value, rank: u64) -> Option<Card<'a>> {
    if row["catalogJoined"] != true { return None; }
    data.mods.get(text(&row["path"])).map(|raw| Card::new(raw, rank, req))
}

pub(super) fn utility_fits(data: &EngineData, req: &Request<'_>, row: &Value) -> bool {
    row["slotKind"] == if req.melee { "melee" } else { text(&req.weapon["slot"]) } &&
        if row["catalogJoined"] == true {
            data.mods.get(text(&row["path"])).is_some_and(|mod_row| loadout::compatible(mod_row, req.weapon))
        } else {
            req.melee && row["sourceKind"] == "tennokai"
        }
}

pub(super) fn utility_at_rank(row: &Value, rank: u64, slot: &Value) -> Value {
    let raw = row["drain"].as_i64().unwrap_or(0) - utility_rank(row) as i64 + rank as i64;
    let (drain, relation) = loadout::adjust_drain(raw, &row["polarity"], slot);
    super::super::merge(row.clone(), json!({"rank": rank, "rawDrain": raw, "drain": drain,
        "slotPolarity": slot, "polarityRelation": relation}))
}

pub(super) fn utility_rank(row: &Value) -> u64 {
    row["rank"].as_u64().or_else(|| row["maxRank"].as_u64()).unwrap_or(0)
}
