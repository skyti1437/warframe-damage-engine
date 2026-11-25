//! 普通槽、特殊槽和套装共用导出记录，负载顺序同时决定元素合成顺序。

use super::{merge, num, request::{polarity, SimCatalog}, rows, text, truth, Request, SimResult};
use super::weapon::{is_weapon_effect, Effect};
use crate::data::EngineData;
use serde_json::{json, Map, Value};
use std::collections::{HashMap, HashSet};

pub(super) fn adjust_drain(raw: i64, mod_pol: &Value, slot_pol: &Value) -> (i64, &'static str) {
    let (a, b) = (polarity(mod_pol), polarity(slot_pol));
    let (factor, relation) = if a.is_null() || b.is_null() { (1.0, "unpolarized") }
        else if a == b || a == "AP_ANY" || b == "AP_ANY" { (if raw < 0 { 2.0 } else { 0.5 }, "matched") }
        else { (if raw < 0 { 0.5 } else { 1.25 }, "mismatched") };
    (((raw.abs() as f64 * factor).ceil() as i64) * raw.signum(), relation)
}

pub(super) fn compatible(card: &Value, weapon: &Value) -> bool {
    if let Some(base) = card["compatibility"].as_str().filter(|v| !v.is_empty()) {
        if !std::iter::once(&weapon["path"]).chain(rows(&weapon["ancestry"]))
            .any(|v| text(v).trim_end_matches('/').eq_ignore_ascii_case(base.trim_end_matches('/'))) { return false; }
    }
    let matches = |list: &Value| rows(list).iter().filter_map(Value::as_str)
        .any(|tag| rows(&weapon["compatTags"]).iter().any(|v| text(v).eq_ignore_ascii_case(tag)));
    (!truth(&card["compatibilityTags"]) || matches(&card["compatibilityTags"])) && !matches(&card["incompatibilityTags"])
}

pub(super) fn ranked(raw: &Value, max: u64, label: &str) -> SimResult<u64> {
    if raw.is_null() { return Ok(max); }
    raw.as_u64().filter(|v| *v <= max).ok_or_else(|| format!("{label} 必须为 0 到 {max} 之间的整数"))
}

pub(super) fn query(item: &Value) -> &str {
    item["path"].as_str().filter(|v| !v.is_empty()).unwrap_or(text(&item["query"])).trim()
}

pub(super) fn terms(raw: &Value, source: &str, rank: u64, stacks: f64) -> Vec<Effect> {
    rows(raw).iter().map(|row| {
        let mut effect = Effect::export(row, source);
        effect.value = effect.value * (rank as f64 + 1.0) * stacks;
        effect
    }).collect()
}

pub(super) fn combo_gate(condition: &Value) -> bool {
    rows(&condition["conditions"]).iter().any(|s| text(s).ends_with("/OnComboTierCondition"))
}

fn keys(condition: &Value) -> impl Iterator<Item = &str> {
    [&condition["sourcePath"], &condition["conditionTag"]].into_iter().filter_map(Value::as_str).filter(|s| !s.is_empty())
}

fn kill_gate(condition: &Value) -> bool {
    keys(condition).chain(rows(&condition["conditions"]).iter().filter_map(Value::as_str))
        .any(|key| key.to_ascii_lowercase().contains("killcondition"))
}

pub(super) fn cap(condition: &Value) -> u64 {
    condition["maxStacks"].as_u64().filter(|v| *v > 0).unwrap_or(1)
}

pub(super) struct Card<'a> {
    pub raw: &'a Value,
    pub rank: u64,
    pub compatible: bool,
    slot: usize,
    explicit_slot: bool,
    meta: Option<&'a Value>,
}

pub(super) struct Loadout<'a> {
    pub cards: Vec<Card<'a>>,
    pub validation: Value,
    pub base_capacity: i64,
    pub selection: Vec<Value>,
    pub set_pieces: Map<String, Value>,
    pub conditional_stacks: Map<String, Value>,
    pub exilus: Value,
    pub terms: Vec<Effect>,
    pub riven: Vec<Effect>,
    riven_index: Option<usize>,
    pub skipped: Vec<String>,
    pub riven_skipped: Vec<String>,
    pub notes: Vec<String>,
}

fn set_key(value: &Value) -> String {
    text(value).trim().replace('\\', "/").trim_end_matches('/').to_lowercase()
}

fn expand(card: &Value, rank: u64, req: &Request<'_>, stacks: &Map<String, Value>, skipped: &mut Vec<String>) -> Vec<Effect> {
    let source = text(&card["path"]);
    let mut effects = if truth(&card["runtimeScripts"]) {
        let Some(effects) = super::mod_runtime::effects(card, rank, req, skipped) else {
            skipped.push(format!("{source}: unsupported[runtime-script]"));
            return Vec::new();
        };
        effects
    } else { terms(&card["effects"], source, rank, 1.0) };
    for condition in rows(&card["conditionalEffects"]) {
        let condition_source = condition["sourcePath"].as_str().unwrap_or("?");
        let title = keys(condition).next().unwrap_or("?");
        if truth(&condition["runtimeScripts"]) {
            skipped.push(format!("{title}: unsupported[runtime-script]"));
            continue;
        }
        let count = keys(condition).find_map(|key| stacks.get(key).and_then(Value::as_u64))
            .or_else(|| combo_gate(condition).then_some(req.scenario.combo));
        if let Some(count) = count {
            let count = count.min(cap(condition));
            if count > 0 { effects.extend(terms(&condition["effects"], condition_source, rank, count as f64).into_iter().map(|e| e.with_condition(condition))); }
        } else {
            skipped.push(format!("condition not specified: {title}"));
        }
    }
    effects
}

impl<'a> Loadout<'a> {
    pub fn resolve(data: &'a EngineData, catalog: &'a SimCatalog, req: &Request<'_>, stance: &Value) -> SimResult<Self> {
        let mut cards = Vec::new();
        let mut families = HashMap::new();
        for (index, item) in rows(req.body.get("mods").unwrap_or(&Value::Null)).iter().enumerate() {
            let path = query(item);
            let meta = catalog.mods.get(path).ok_or("mods 必须使用已导出的普通 MOD 路径")?;
            let raw = data.mods.get(path).ok_or("MOD 缺少导出记录")?;
            if truth(&raw["catalogExclusion"]) { return Err("MOD 阶级未发布".into()); }
            if !item["setPieces"].is_null() { return Err("套装件数由已装备 MOD 推导".into()); }
            let family = ["modFamily", "nameTag", "nameEn", "nameInternal", "path"].into_iter()
                .find_map(|key| meta[key].as_str().filter(|v| !v.is_empty())).unwrap_or(path);
            let key = (family.to_lowercase(), text(&meta["compatibility"]).to_lowercase());
            if families.insert(key, path).is_some_and(|old| old != path) { return Err("同一 MOD 家族的不同卡片互斥".into()); }
            let rank = ranked(&item["rank"], raw["maxRank"].as_u64().unwrap_or(0), "MOD 等级")?;
            let explicit_slot = item.get("slot").is_some();
            let slot = if explicit_slot { item["slot"].as_u64().filter(|v| *v < req.slot_count as u64)
                .ok_or("MOD 槽位超出范围")? as usize } else { index };
            cards.push(Card { raw, rank, compatible: compatible(raw, req.weapon), slot, explicit_slot, meta: Some(meta) });
        }
        let mut exilus = Value::Null;
        let mut notes = Vec::new();
        let mut extra_drain = 0;
        let mut slots = req.slots.clone();
        let mut polarity_source = req.polarity_source;
        if let Some(item) = req.body.get("exilus").filter(|v| !v.is_null()) {
            let path = query(item);
            let meta = catalog.utility.get(path).ok_or("exilus 必须使用已导出的功能槽 MOD 路径")?;
            let max = meta["rank"].as_u64().or_else(|| meta["maxRank"].as_u64()).unwrap_or(0);
            let rank = ranked(&item["rank"], max, "exilus.rank")?;
            let raw_drain = meta["drain"].as_i64().unwrap_or(0) - max as i64 + rank as i64;
            let (drain, relation) = adjust_drain(raw_drain, &meta["polarity"], &req.exilus_polarity);
            exilus = merge(meta.clone(), json!({"rank": rank, "rawDrain": raw_drain, "drain": drain,
                "slotPolarity": req.exilus_polarity, "polarityRelation": relation}));
            let expected = if req.melee { "melee" } else { text(&req.weapon["slot"]) };
            let raw = data.mods.get(path);
            let joined = truth(&meta["catalogJoined"]);
            let fits = meta["slotKind"] == expected && if joined { raw.is_some_and(|m| compatible(m, req.weapon)) }
                else { expected == "melee" && meta["sourceKind"] == "tennokai" };
            if fits && joined {
                cards.push(Card { raw: raw.unwrap(), rank, compatible: true, slot: req.slot_count, explicit_slot: true, meta: None });
                slots.push(polarity(&req.exilus_polarity));
                polarity_source = "payload";
            } else if fits {
                extra_drain += drain;
            } else {
                exilus["incompatible"] = json!(true);
                let label = ["nameZh", "nameEn", "path"].into_iter()
                    .find_map(|key| meta[key].as_str().filter(|v| !v.is_empty())).unwrap_or(path);
                notes.push(format!("exilus-mod-not-compatible-with-weapon: {label}"));
            }
            if !joined && meta["sourceKind"] == "tennokai" { notes.push("tennokai-opportunities-not-modeled".into()); }
        }
        let limit = 60 + stance["capacityGain"].as_i64().unwrap_or(0);
        let mut used = 0;
        let mut occupied = HashSet::new();
        let mut assignments = Vec::new();
        let mut stacks = Map::new();
        let mut shells = HashMap::new();
        let mut set_pieces: Map<String, Value> = Map::new();
        for raw in data.mods.values() {
            let key = set_key(&raw["modSet"]);
            if !key.is_empty() && !raw["setPieces"].is_null() && (raw["selectable"] == false ||
                (!truth(&raw["selectable"]) && !truth(&raw["compatibility"]))) { shells.insert(key, raw); }
        }
        for card in &cards {
            if !occupied.insert(card.slot) { return Err(format!("MOD 槽位 {} 被占用多次", card.slot)); }
            let raw = card.raw;
            let base = raw["baseDrain"].as_i64().ok_or("MOD 缺少有效的容量消耗")?;
            let ranked = (base.abs() + card.rank as i64) * if base < 0 { -1 } else { 1 };
            let (drain, relation) = adjust_drain(ranked, &raw["polarity"], &slots[card.slot]);
            used += drain;
            assignments.push(json!({"slot": card.slot, "path": raw["path"], "rank": card.rank,
                "baseDrain": base, "rawDrain": ranked, "drain": drain, "modPolarity": polarity(&raw["polarity"]),
                "slotPolarity": slots[card.slot], "polarityRelation": relation}));
            if !card.compatible { continue; }
            let set = set_key(&raw["modSet"]);
            if !set.is_empty() {
                let count = set_pieces.get(&set).and_then(Value::as_u64).unwrap_or(0) + 1;
                set_pieces.insert(set, json!(count));
            }
            for condition in rows(&raw["conditionalEffects"]).iter().filter(|c| kill_gate(c)) {
                for key in keys(condition) { stacks.entry(key.to_owned()).or_insert(json!(cap(condition))); }
            }
        }
        let mut clamp_notes = Vec::new();
        for (set, pieces) in &mut set_pieces {
            if let Some(max) = shells.get(set).and_then(|v| v["setPieces"].as_u64()) {
                let actual = pieces.as_u64().unwrap();
                if actual > max {
                    clamp_notes.push(format!("{set}: setPieces clamped {actual}->{max}"));
                    *pieces = json!(max);
                }
            }
        }
        let riven = req.body.get("riven").unwrap_or(&Value::Null);
        let riven_slot = riven["slot"].as_i64();
        let (riven_effects, riven_skipped) = riven_terms(data, &riven["stats"])?;
        if riven["stats"].is_array() {
            if riven.get("rank") == Some(&Value::Null) { return Err("riven.rank 必须为整数".into()); }
            let raw = 10 + ranked(&riven["rank"], 8, "riven.rank")? as i64;
            let pol = &riven["polarity"];
            let slot_pol = riven_slot.and_then(|index| usize::try_from(index).ok())
                .and_then(|index| req.body.get("slotPolarities")?.get(index)).unwrap_or(&Value::Null);
            extra_drain += if matches!(text(pol), "AP_ATTACK" | "AP_DEFENSE" | "AP_TACTIC") {
                adjust_drain(raw, pol, slot_pol).0
            } else { raw };
        }
        let mut all_terms = Vec::new();
        let mut skipped = Vec::new();
        let mut selection = Vec::new();
        let mut riven_index = None;
        for card in &cards {
            if riven_index.is_none() && riven_slot.is_some_and(|slot| card.slot as i64 >= slot) {
                riven_index = Some(all_terms.len());
            }
            let raw = card.raw;
            let set = set_key(&raw["modSet"]);
            let pieces = if card.compatible { set_pieces.get(&set).and_then(Value::as_u64).unwrap_or(0) } else { 0 };
            let mut card_skipped = Vec::new();
            let mut current = expand(raw, card.rank, req, &stacks, &mut card_skipped);
            if card.compatible {
                if pieces >= 2 && shells.get(&set).is_none_or(|s| truth(&s["buffSet"])) {
                    let values = rows(&raw["modSetValues"]);
                    if !values.is_empty() {
                        let multiplier = 1.0 + num(&values[(pieces as usize - 2).min(values.len() - 1)]);
                        for effect in &mut current { effect.value *= multiplier; }
                    }
                }
                all_terms.extend(current.iter().cloned());
                skipped.extend(card_skipped);
            } else {
                let ancestry = raw["compatibility"].as_str().unwrap_or("None");
                skipped.push(format!("{}: compatibility {ancestry} does not match weapon", text(&raw["path"])));
            }
            if let Some(meta) = card.meta {
                let mut row = meta.clone();
                row["rank"] = json!(card.rank);
                if card.explicit_slot { row["slot"] = json!(card.slot); }
                if !set.is_empty() { row["setPieces"] = json!(pieces); }
                row["displayEffects"] = json!(current.iter().filter(|e| is_weapon_effect(&e.upgrade))
                    .map(Effect::display).collect::<Vec<_>>());
                let steady: Vec<_> = rows(&raw["conditionalEffects"]).iter().filter(|c| c["maxStacks"].is_i64()
                    && text(&c["conditionTag"]).to_ascii_lowercase().contains("killcondition"))
                    .map(|c| json!({"conditionTag": c["conditionTag"], "stacks": c["maxStacks"]})).collect();
                if !steady.is_empty() { row["steadyStacks"] = json!(steady); }
                selection.push(row);
                let path = text(&raw["path"]);
                if matches!(text(&data.weapon_mechanics_overrides["mods"][path]["status"]), "event-unmodeled" | "unsupported") {
                    notes.push(format!("weapon-mod-mechanic-unmodeled:{path}"));
                }
            }
        }
        for (key, count) in &set_pieces {
            let Some(shell) = shells.get(key) else { continue; };
            let count = num(count);
            if count == 0.0 { continue; }
            if !truth(&shell["buffSet"]) {
                all_terms.extend(expand(shell, 0, req, &stacks, &mut skipped).into_iter().map(|mut e| {
                    e.value *= count;
                    e
                }));
            }
            if truth(&shell["runtimeScripts"]) || (!truth(&shell["buffSet"]) &&
                rows(&shell["conditionalEffects"]).iter().any(|c| truth(&c["runtimeScripts"]))) {
                let source = text(&shell["path"]);
                all_terms.push(Effect::export(&json!({"upgradeType": "SET_SHELL_UNSUPPORTED"}), source));
                skipped.push(format!("{source}: unsupported[set-shell-runtime-script]"));
            }
        }
        skipped.extend(clamp_notes);
        let capacity = used + extra_drain;
        if used > limit { notes.push("loadout-over-capacity".into()); }
        let validation = json!({"status": "checked", "valid": capacity <= limit, "capacity": capacity,
            "capacityLimit": limit, "remaining": limit - capacity, "slotCount": slots.len(), "polaritySource": polarity_source,
            "slotPolarities": slots, "assignments": assignments, "compatibility": "checked-from-export-ancestry",
            "polarity": "checked", "polarityDetails": {"status": "checked", "source": polarity_source, "slots": slots}});
        if riven_slot.is_some() && riven_index.is_none() { riven_index = Some(all_terms.len()); }
        Ok(Self { cards, validation, base_capacity: used, selection, set_pieces, conditional_stacks: stacks, exilus,
            terms: all_terms, riven: riven_effects, riven_index, skipped, riven_skipped, notes })
    }

    pub fn effects(&self, evolution: &[Effect], arcanes: &[Effect], riven: bool) -> Vec<Effect> {
        let mut effects = self.terms.clone();
        if riven {
            if let Some(index) = self.riven_index { effects.splice(index..index, self.riven.iter().cloned()); }
        }
        effects.extend_from_slice(evolution);
        effects.extend_from_slice(arcanes);
        if riven && self.riven_index.is_none() { effects.extend_from_slice(&self.riven); }
        effects
    }


}

fn riven_terms(data: &EngineData, raw: &Value) -> SimResult<(Vec<Effect>, Vec<String>)> {
    let mut effects = Vec::new();
    let mut skipped = Vec::new();
    for (index, row) in rows(raw).iter().enumerate() {
        if !row.is_object() { return Err(format!("紫卡词条 {index} 必须是对象")); }
        let code = match &row["code"] { Value::Number(v) if num(&row["code"]) != 0.0 => v.to_string(),
            value => text(value).trim().to_ascii_uppercase() };
        let value = || row["value"].as_f64().filter(|v| v.is_finite()).ok_or_else(|| format!("紫卡词条 {index} 的数值无效"));
        if code.is_empty() {
            let pick = |camel, snake| row.get(camel).filter(|v| truth(v)).unwrap_or(&row[snake]);
            let upgrade = text(pick("upgradeType", "upgrade_type")).trim();
            if upgrade.is_empty() {
                skipped.push(format!("riven:{index}: unsupported[missing-upgrade-type]"));
                continue;
            }
            let source = row["sourcePath"].as_str().map(str::to_owned).unwrap_or_else(|| format!("riven:{index}"));
            effects.push(Effect::export(&json!({"upgradeType": upgrade, "operation": row["operation"].as_str().unwrap_or("STACKING_MULTIPLY"),
                "damageType": pick("damageType", "damage_type"), "symbolFilter": pick("symbolFilter", "symbol_filter"),
                "valuePerRank": value()?}), &source));
            continue;
        }
        let rule = &data.rules.riven["traits"][&code];
        let mut amount = value()?.abs() / if rule[1] == "flat" { 1.0 } else { 100.0 };
        if rule.is_null() {
            skipped.push(format!("riven:{code}: unsupported[unknown-code]"));
            continue;
        }
        if truth(&row["negative"]) { amount = -amount; }
        if matches!(code.as_str(), "Q" | "Z" | "AE") { amount = -amount; }
        let dtype = match code.as_str() {
            "4" => "DT_FIRE", "5" => "DT_FREEZE", "6" => "DT_POISON", "7" => "DT_ELECTRICITY",
            "8" => "DT_IMPACT", "9" => "DT_PUNCTURE", "A" => "DT_SLASH",
            "GA" => "DT_GAS", "CO" => "DT_CORROSIVE", "VI" => "DT_VIRAL", "RA" => "DT_RADIATION",
            "BL" => "DT_EXPLOSION", "MA" => "DT_MAGNETIC", _ => "",
        };
        let symbol = match code.as_str() { "C" => "Corpus", "G" => "Grineer", "I" => "Infestation",
            "FO" => "Orokin", "FT" => "Techrot", "FS" => "Scaldra", "E" => "CC_SLIDING", _ => "" };
        let operation = if matches!(code.as_str(), "4" | "5" | "6" | "7" | "B" | "N" | "P" | "Q" | "T" | "U" |
            "GA" | "CO" | "VI" | "RA" | "BL" | "MA" | "HR" | "PA") { "ADD" } else { "STACKING_MULTIPLY" };
        effects.push(Effect::export(&json!({"upgradeType": rule[4], "operation": operation, "damageType": (!dtype.is_empty()).then_some(dtype),
            "symbolFilter": symbol, "valuePerRank": amount, "validModifiers": if code == "HD" { json!(["PM_HEAVY_MELEE"]) } else { json!([]) }}),
            &format!("riven:{code}")));
    }
    Ok((effects, skipped))
}
