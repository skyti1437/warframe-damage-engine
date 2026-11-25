//! Single-trait trials against the optimizer's reference loadout.

use super::{cards::{Card, Riven, ELEMENTS}, evaluate::{Evaluation, Evaluator, Metric}, forma,
    json, loadout, num, request::{self, Scope}, riven_traits::{self, Trait}, rows, search::Search,
    text, Map, SimCatalog, SimResult, Value};
use crate::data::EngineData;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Mutex;

const HEAVY_CALIBER: &str = "/Lotus/Upgrades/Mods/Rifle/DualStat/CorruptedDamageRecoilRifle";
const SERRATION: &str = "/Lotus/Upgrades/Mods/Rifle/WeaponDamageAmountMod";

#[derive(Default)]
pub(crate) struct Cache(Mutex<VecDeque<(String, Value)>>);

pub(crate) fn run(data: &EngineData, catalog: &SimCatalog, fields: &Map<String, Value>, cache: &Cache) -> SimResult<Value> {
    let roll = match fields.get("roll") {
        None => "both",
        Some(Value::String(value)) if matches!(value.as_str(), "3+1-" | "2+1-" | "both") => value,
        _ => return Err("roll must be 3+1-, 2+1- or both".into()),
    };
    let mut body = request::normalize(fields)?;
    for key in ["roll", "liveChannel", "liveSeq", "progressToken", "_searchBudgetSeconds", "riven",
        "enhancements"] {
        body.remove(key);
    }
    if body.get("profile").is_some_and(|v| !v.is_null() && !v.as_str().is_some_and(|s|
        matches!(s.trim().to_ascii_lowercase().as_str(), "" | "ultra" | "fast" | "standard" | "deep"))) {
        return Err("profile must be one of ultra, fast, standard, deep".into());
    }
    if let Some(weapon) = body.get_mut("weapon") {
        if let Some(path) = weapon.as_str() { *weapon = json!(path.trim()); }
    }
    if let Some(location) = fields.get("benchmarkTarget").and_then(|v| v.get("hitLocation")).or_else(|| fields.get("hitLocation")) {
        if !matches!(location.as_str(), Some("body" | "head")) { return Err("hitLocation must be body or head".into()); }
    }
    body.remove("hitLocation");
    if body["enemyEnhancements"].as_object().is_some_and(Map::is_empty)
        && !fields.contains_key("enemyEnhancements") && !fields.contains_key("enhancements")
        && fields.get("benchmarkTarget").is_none_or(|value| value.get("enemyEnhancements").is_none() && value.get("enhancements").is_none()) {
        body.insert("enemyEnhancements".into(), json!({"eximus": false, "noOverguard": true}));
    }
    let auto_arcane = !body.contains_key("arcanes");
    let auto_exilus = !body.contains_key("exilus");
    if body.get("exilus").is_some_and(|value| value == &json!({"mode": "auto"})) {
        body.remove("exilus");
        body.insert("autoExilus".into(), json!(true));
    } else if body.get("exilus") == Some(&Value::Null) {
        body.remove("exilus");
    }
    if body.get("arcanes") == Some(&Value::Null) { body.insert("arcanes".into(), json!([])); }
    for (key, value) in [("autoArcane", json!(auto_arcane)), ("autoExilus", json!(auto_exilus)),
        ("autoIncarnon", json!(true)), ("fireMode", json!(0)), ("level", json!(170)),
        ("steelPath", json!(true)), ("windowSeconds", json!(30.0)), ("headshotChance", json!(0.0))] {
        body.entry(key.to_owned()).or_insert(value);
    }
    body.insert("metric".into(), json!("killsPerMinute"));
    body.insert("damageModel".into(), json!("target"));
    let key = serde_json::to_string(&canonical(&Value::Object(body.clone()))).map_err(|error| error.to_string())?;
    let cached = {
        let mut entries = cache.0.lock().unwrap();
        entries.iter().position(|(existing, _)| *existing == key).map(|index| {
            let entry = entries.remove(index).unwrap();
            let value = entry.1.clone();
            entries.push_back(entry);
            value
        })
    };
    let mut result = if let Some(value) = cached { value } else {
        let value = super::solve(data, catalog, &body, super::SolveMode::TraitMatrix)?;
        let mut entries = cache.0.lock().unwrap();
        if let Some(index) = entries.iter().position(|(existing, _)| *existing == key) { entries.remove(index); }
        entries.push_back((key, value.clone()));
        if entries.len() > 24 { entries.pop_front(); }
        value
    };
    if roll != "both" {
        let (kept, omitted) = if roll == "3+1-" { (31, 21) } else { (21, 31) };
        for sign in ["positive", "negative"] {
            for row in result["matrix"][sign].as_array_mut().unwrap() {
                row[format!("shown_{omitted}")] = Value::Null;
                row[format!("pct_{omitted}")] = Value::Null;
                if row.as_object_mut().unwrap().remove("elem_build_roll").and_then(|v| v.as_i64()).is_some_and(|v| v != kept) {
                    row["elem_build"] = Value::Null;
                }
            }
        }
    }
    Ok(result)
}

fn canonical(value: &Value) -> Value {
    match value {
        Value::Object(fields) => {
            let mut fields: Vec<_> = fields.iter().collect();
            fields.sort_by_key(|(key, _)| *key);
            Value::Object(fields.into_iter().map(|(key, value)| (key.clone(), canonical(value))).collect())
        }
        Value::Array(values) => Value::Array(values.iter().map(canonical).collect()),
        value => value.clone(),
    }
}

pub(super) fn calculate(eval: &Evaluator<'_>, best: &Evaluation, special: Option<&Card<'_>>,
    meta: &Value, catalog: &SimCatalog, result: Value) -> SimResult<Value> {
    let (build, cards, baseline) = reference(eval, best, special, catalog, &result)?;
    let kind = text(&meta["rivenType"]);
    let disposition = num(&meta["disposition"]);
    let traits = if disposition > 0.0 { riven_traits::traits(eval.data, kind, eval.req.weapon) } else { Vec::new() };
    let weak_request = super::Request { aim: 1.0, ..eval.req.clone() };
    let weak_eval = Evaluator { req: &weak_request, ..eval.clone() };
    let weak = eval.req.aim == 0.0 && traits.iter().any(|line| line.positive && matches!(line.code, "W" | "Y"));
    let weak_baseline = if weak { weak_eval.score(&cards, None, special, Metric::Events, None)? } else { None };
    let reserve = reserve_ammo(eval);
    let form = meta["fireModes"][eval.req.mode]["isIncarnonForm"] == true;
    let cycle = form && eval.req.scenario.cycle;
    let mut matrix = json!({
        "baseline_kpm": round(baseline, 4), "baseline_dps": build["full_dps"],
        "baseline_weakpoint_kpm": weak_baseline.map(|value| round(value, 4)),
        "reserve_ammo": reserve, "low_ammo": reserve.is_some_and(|n| n <= 30),
        "incarnon_form": form && !cycle, "incarnon_cycle": cycle
    });
    let special_cost = result["exilus"]["drain"].as_i64().unwrap_or(0);
    let elements = Evaluator { fixed_exilus: special,
        capacity_limit: 60 + eval.stance["capacityGain"].as_i64().unwrap_or(0) - special_cost, ..eval.clone() };
    let scope = Scope { profile: "fast", metric: "killsPerMinute".into(), event: true, element: Value::Null,
        required: Vec::new(), forbidden: Vec::new(), min: None, max: None, include_arcanes: true };
    let mut search = Search::new(&elements, &scope)?;
    let pool = element_pool(eval.cards, &cards);
    search.forbidden.extend((0..eval.cards.len()).filter(|index| !pool.contains(index)));
    let mut least_impact: Vec<_> = (0..cards.len()).collect();
    let ablation = |i: usize| build["without"][build["mods"][i]["slot"].to_string()].as_f64().unwrap_or(-1.0);
    least_impact.sort_by(|&a, &b| ablation(b).total_cmp(&ablation(a)));
    least_impact.truncate(2);
    let seeds = if cards.len() < eval.req.slot_count { vec![cards.clone()] } else {
        cards.iter().enumerate().filter(|(i, card)| least_impact.contains(i) || eval.cards[**card].elemental != 0)
            .map(|(i, _)| without(&cards, i)).collect()
    };
    let mut baselines = HashMap::new();
    for (negative, sign) in [(false, "positive"), (true, "negative")] {
        let mut table = Vec::new();
        for line in traits.iter().filter(|line| if negative { line.negative } else { line.positive }) {
            let weakpoint = line.modeled && !negative && weak && matches!(line.code, "W" | "Y");
            let reoptimized = line.modeled && !negative && matches!(line.code, "4" | "5" | "6" | "7");
            let reference = if weakpoint { weak_baseline.unwrap_or(0.0) } else { baseline };
            let evaluator = if weakpoint { &weak_eval } else { eval };
            let (stat_id, unit) = trait_display(line)?;
            let mut row = json!({"code": line.code, "stat_id": stat_id, "unit": unit,
                "spliced": line.fusion, "modeled": line.modeled, "neutral": negative && riven_traits::neutral(eval.req.melee, line.code),
                "weakpoint": weakpoint, "reoptimized": reoptimized, "elem_build": null});
            let base = if line.code == "PA" { 81.0 } else { line.template_base.unwrap_or(line.default_base) };
            let mut rivens = Vec::new();
            for (roll, count) in [("31", 3), ("21", 2)] {
                let value = base * disposition * (riven_traits::structure(count, true, negative) / 1.1 * 1.1);
                row[format!("shown_{roll}")] = json!(round(if negative { -value } else { value }, 2));
                let mut gain = None;
                if line.modeled {
                    let riven = riven_traits::card(eval.data, vec![line.line(value, 0.0, value, negative)], 8, "AP_ATTACK")?;
                    let trial = Evaluator { riven: Some(&riven), ..evaluator.clone() };
                    let mut score = trial.score(&cards, Some(cards.len()), special, Metric::Events, None)?.unwrap_or(0.0);
                    if reoptimized {
                        let trial = Evaluator { riven: Some(&riven), ..elements.clone() };
                        score = score.max(element_score(&trial, &search, &seeds)?);
                    }
                    if reference > 0.0 { gain = Some(round(percent(score, reference), 2)); }
                    rivens.push(riven);
                }
                row[format!("pct_{roll}")] = json!(gain);
            }
            if !negative && line.modeled {
                element_baselines(&elements, &scope, &mut baselines, &rivens, &mut row)?;
            }
            table.push(row);
        }
        matrix[sign] = json!(table);
    }
    let weapon: Map<_, _> = ["path", "nameEn", "nameZh", "weaponType", "fireModes"].into_iter()
        .map(|key| (key.into(), meta[key].clone())).collect();
    let mut target = result["evaluationTarget"].clone();
    target["enemyPath"] = target["path"].clone();
    for (key, source) in [("level", "level"), ("steelPath", "steel_path"), ("headshotChance", "headshotChance"),
        ("windowSeconds", "windowSeconds"), ("armourStrip", "armour_strip"),
        ("elementalArmourStrip", "elemental_armour_strip"), ("enemyEnhancements", "enemy_enhancements")] {
        target[key] = result["evaluationContext"][source].clone();
    }
    Ok(json!({"weapon": weapon, "fireMode": result["fireMode"], "evaluationTarget": target,
        "evaluationScenario": result["evaluationScenario"], "build": build, "matrix": matrix}))
}

fn reference(eval: &Evaluator<'_>, best: &Evaluation, special: Option<&Card<'_>>, catalog: &SimCatalog,
    result: &Value) -> SimResult<(Value, Vec<usize>, f64)> {
    let plan = forma::plan(eval, best, &result["exilus"], true, false);
    if plan["fits"] != true { return Err("reference build does not fit its capacity".into()); }
    let mut layout: Vec<_> = rows(&plan["placement"]).iter().enumerate().filter_map(|(slot, row)| {
        row["origin"].as_u64().map(|origin| (slot, row["polarity"].clone(), row["drain"].as_i64().unwrap(),
            best.cards[origin as usize]))
    }).collect();
    let mut cards: Vec<_> = layout.iter().map(|row| row.3).collect();
    let replay = |cards: &[usize]| eval.window(cards, None, special);
    let mut window = replay(&cards)?;
    let mut used = plan["drain"].as_i64().unwrap();
    let mut replaced = None;
    if let Some(serration) = eval.cards.iter().position(|card| card.path == SERRATION && card.rank == 10) {
        if !cards.contains(&serration) {
            if let Some(i) = cards.iter().position(|&index| eval.cards[index].path == HEAVY_CALIBER && eval.cards[index].rank == 10) {
                let (slot, polarity, drain, index) = &mut layout[i];
                let card = &eval.cards[serration];
                let alternative = loadout::adjust_drain(card.drain, &card.raw["polarity"], polarity).0;
                if alternative < *drain {
                    cards[i] = serration;
                    let trial = replay(&cards)?;
                    let (before, after) = (num(&window["windowDps"]), num(&trial["windowDps"]));
                    if (before - after).abs() <= before.abs().max(after.abs()) * 1e-12 {
                        used += alternative - *drain;
                        *drain = alternative;
                        *index = serration;
                        replaced = Some(*slot);
                        window = trial;
                    } else {
                        cards[i] = *index;
                    }
                }
            }
        }
    }
    let mut mods = Vec::new();
    let mut ablations = Map::new();
    let mut unmodeled = Vec::new();
    for (i, (slot, slot_polarity, drain, index)) in layout.into_iter().enumerate() {
        let card = &eval.cards[index];
        let meta = &catalog.mods[card.path];
        mods.push(json!({"path":card.path, "nameEn": meta["nameEn"], "nameZh": meta["nameZh"],
            "slot": slot, "rank":card.rank, "polarity":card.raw["polarity"], "slotPolarity":slot_polarity, "drain":drain}));
        if matches!(text(&eval.data.weapon_mechanics_overrides["mods"][card.path]["status"]), "event-unmodeled" | "unsupported") {
            unmodeled.push(slot);
        } else {
            let dropped = replay(&without(&cards, i))?;
            ablations.insert(slot.to_string(), json!(round(num(&dropped["windowDps"]), 2)));
        }
    }
    let arcanes: Vec<_> = eval.arcanes.cards.iter().map(|card| {
        let row = catalog.arcanes.get(text(&card.record["path"])).unwrap_or(card.record);
        let mut item = json!({"path":card.record["path"], "rank":card.rank,
            "nameEn":row["nameEn"], "nameZh":row["nameZh"]});
        if let Some(stacks) = card.stacks { item["stacks"] = json!(stacks); }
        if let Some(uptime) = card.uptime { item["uptime"] = json!(uptime); }
        item
    }).collect();
    let kpm = num(&window["killsPerMinute"]);
    Ok((json!({"mods":mods, "arcanes":arcanes, "exilus":result["exilus"], "stance":result["stance"],
        "incarnonChoices":result["incarnonChoices"], "innateElement":result["innateElement"],
        "full_dps":round(num(&window["windowDps"]), 2), "kpm":round(kpm, 4), "without":ablations,
        "unmodeledSlots":unmodeled, "serrationSlot":replaced,
        "capacity":{"used":used, "limit":plan["limit"], "forma":plan["total"], "valid":true}}), cards, kpm))
}

fn without(cards: &[usize], drop: usize) -> Vec<usize> {
    cards[..drop].iter().chain(&cards[drop + 1..]).copied().collect()
}

fn element_pool(candidates: &[Card<'_>], reference: &[usize]) -> HashSet<usize> {
    let families: HashSet<_> = reference.iter().map(|&i| candidates[i].family.as_str()).collect();
    let mut pool: HashSet<_> = candidates.iter().enumerate()
        .filter(|(_, card)| card.reserved || families.contains(card.family.as_str())).map(|(i, _)| i).collect();
    for element in ELEMENTS {
        let mut ranked: Vec<_> = candidates.iter().enumerate().filter_map(|(index, card)| {
            let amount: f64 = card.effects.iter().filter(|effect| effect.dtype.as_deref() == Some(element))
                .map(|effect| effect.value).sum();
            (amount > 0.0).then_some((index, amount, card.drain, card.path))
        }).collect();
        ranked.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.2.cmp(&b.2)).then(a.3.cmp(b.3)));
        pool.extend(ranked.into_iter().take(2).map(|entry| entry.0));
    }
    pool
}

fn element_score(eval: &Evaluator<'_>, search: &Search<'_>, seeds: &[Vec<usize>]) -> SimResult<f64> {
    let mut seed: Option<Evaluation> = None;
    for cards in seeds {
        if let Some(trial) = eval.evaluate(cards, Metric::Events, None, eval.fixed_exilus)? {
            if seed.as_ref().is_none_or(|current| trial.score > current.score) { seed = Some(trial); }
        }
    }
    let result = search.element_trial(eval, seed.as_ref().map_or(&[], |row| &row.cards), 1, 8)?;
    Ok(seed.map_or(0.0, |row| row.score).max(result.map_or(0.0, |row| row.score)))
}

type Element = (&'static str, &'static str);
const HEAT: Element = ("heat", "DT_FIRE");
const COLD: Element = ("cold", "DT_FREEZE");
const TOXIN: Element = ("toxin", "DT_POISON");
const ELECTRIC: Element = ("electric", "DT_ELECTRICITY");
const GAS: Element = ("gas", "DT_GAS");
const CORROSIVE: Element = ("corrosive", "DT_CORROSIVE");
const VIRAL: Element = ("viral", "DT_VIRAL");
const RADIATION: Element = ("radiation", "DT_RADIATION");
const BLAST: Element = ("blast", "DT_EXPLOSION");
const MAGNETIC: Element = ("magnetic", "DT_MAGNETIC");

fn element_baselines(eval: &Evaluator<'_>, scope: &Scope, baselines: &mut HashMap<&'static str, Option<Evaluation>>,
    rivens: &[Riven], row: &mut Value) -> SimResult<()> {
    let (compound, choices): (bool, &[Element]) = match text(&row["code"]) {
        "4" => (false, &[HEAT, BLAST, GAS, RADIATION]),
        "5" => (false, &[VIRAL, BLAST, MAGNETIC, COLD]),
        "6" => (false, &[VIRAL, CORROSIVE, GAS, TOXIN]),
        "7" => (false, &[CORROSIVE, MAGNETIC, RADIATION, ELECTRIC]),
        "GA" => (true, &[GAS]), "CO" => (true, &[CORROSIVE]), "VI" => (true, &[VIRAL]),
        "RA" => (true, &[RADIATION]), "BL" => (true, &[BLAST]), "MA" => (true, &[MAGNETIC]),
        _ => return Ok(()),
    };
    let prior = [row["pct_31"].as_f64(), row["pct_21"].as_f64()];
    if !prior.iter().flatten().any(|&gain| gain < -0.05)
        && (compound || prior.iter().flatten().any(|&gain| gain > 0.0)) { return Ok(()); }
    let mut candidates = Vec::new();
    for &(label, damage_type) in choices {
        if !baselines.contains_key(damage_type) {
            let element_scope = Scope { element: json!({"damageType": damage_type}), ..scope.clone() };
            let search = Search::new(eval, &element_scope)?;
            let found = search.element_baseline(eval)?;
            baselines.insert(damage_type, found.filter(|trial| trial.score > 0.0));
        }
        if let Some(trial) = &baselines[damage_type] { candidates.push((label, damage_type, trial.clone())); }
    }
    candidates.sort_by(|a, b| b.2.score.total_cmp(&a.2.score));
    for (label, damage_type, baseline) in candidates {
        let mut gains = Vec::new();
        let panel = damage_types(eval, &baseline.cards, None)?;
        for riven in rivens {
            let trial = Evaluator { riven: Some(riven), ..eval.clone() };
            let mut best: Option<f64> = None;
            for slot in 0..=baseline.cards.len() {
                let damage = damage_types(&trial, &baseline.cards, Some(slot))?;
                if damage.len() != panel.len() || !panel.keys().all(|kind| damage.contains_key(kind))
                    || num(&damage[damage_type]) <= num(&panel[damage_type]) { continue; }
                if let Some(score) = trial.score(&baseline.cards, Some(slot), eval.fixed_exilus, Metric::Events, Some(damage_type))? {
                    best = Some(best.map_or(score, |value| value.max(score)));
                }
            }
            gains.push(best.map(|value| percent(value, baseline.score)));
        }
        if gains.iter().all(Option::is_none) { continue; }
        let mut accepted = Vec::new();
        for ((roll, previous), gain) in [31, 21].into_iter().zip(prior).zip(gains) {
            if compound && previous.is_none_or(|value| value >= -0.05) { continue; }
            if let Some(value) = gain.filter(|&value| value >= -0.05) {
                row[format!("pct_{roll}")] = json!(round(value, 2));
                accepted.push(roll);
            }
        }
        if !accepted.is_empty() {
            row["elem_build"] = json!(label);
            if accepted.len() == 1 { row["elem_build_roll"] = json!(accepted[0]); }
        }
        break;
    }
    Ok(())
}

fn damage_types(eval: &Evaluator<'_>, cards: &[usize], riven_slot: Option<usize>) -> SimResult<Map<String, Value>> {
    let (effects, skipped) = eval.terms(cards, riven_slot, eval.fixed_exilus, true);
    let panel = eval.panel(eval.req, &effects, &skipped, None)?;
    Ok(panel.wire["damageTypes"].as_object().into_iter().flatten().filter(|(_, value)| num(value) > 0.0)
        .map(|(key, value)| (key.clone(), value.clone())).collect())
}

fn reserve_ammo(eval: &Evaluator<'_>) -> Option<u64> {
    let weapon = eval.req.weapon;
    let path = text(&weapon["path"]);
    if eval.req.melee || super::weapon::Weapon::new(eval.data, eval.req).mechanics("battery-profiles").is_object() {
        return None;
    }
    ["reserveAmmo", "reserve", "ammoCapacity", "maxAmmo", "AmmoMax"].into_iter()
        .find_map(|key| weapon[key].as_u64().filter(|&n| n > 0)).or(match path {
            "/Lotus/Weapons/Corpus/Bow/Longbow/CrpBow" => Some(6),
            "/Lotus/Weapons/Grineer/Bows/GrnBow/GrnBowWeapon" => Some(5),
            "/Lotus/Weapons/Grineer/KuvaLich/LongGuns/Zarr/KuvaZarr" => Some(5),
            "/Lotus/Weapons/Grineer/KuvaLich/LongGuns/Ogris/KuvaOgris" => Some(7),
            "/Lotus/Weapons/Grineer/KuvaLich/LongGuns/Tonkor/KuvaTonkor" => Some(11),
            "/Lotus/Weapons/Infested/Bow/InfCernosBow/InfCernos" => Some(72),
            "/Lotus/Weapons/Tenno/Bows/PrimeDerelictCernos/DerelictCernos" => Some(7),
            "/Lotus/Weapons/Corpus/LongGuns/CrpBriefcaseLauncher/CrpBriefcaseLauncher" => Some(16),
            _ => None,
        })
}

fn trait_display(line: &Trait<'_>) -> SimResult<(&'static str, &'static str)> {
    let label = match line.code {
        "0" => "crit_chance", "1" => "crit_damage", "2" => "status_chance", "3" => "status_duration",
        "4" => "heat_damage", "5" => "cold_damage", "6" => "toxin_damage", "7" => "electric_damage",
        "8" => "impact_damage", "9" => "puncture_damage", "A" => "slash_damage", "B" => "initial_combo",
        "C" => "damage_vs_corpus", "D" | "K" => "melee_damage", "E" => "slide_crit", "F" => "reload_speed",
        "G" => "damage_vs_grineer", "H" => "zoom", "I" => "damage_vs_infested", "J" => "attack_speed",
        "L" => "magazine_capacity", "M" => "ammo_max", "N" => "combo_duration", "O" => "extra_combo_count",
        "P" => "punch_through", "Q" => "combo_gain_chance", "R" => "fire_rate", "S" => "multishot", "T" => "range",
        "U" => "heavy_attack_efficiency", "V" => "projectile_speed", "W" => "weakpoint_damage", "X" => "finisher_damage",
        "Y" => "weakpoint_crit_chance", "Z" => "recoil", "SD" => "status_damage", "AE" => "ammo_efficiency",
        "HR" => "reload_holstered", "FO" => "damage_vs_orokin", "FT" => "damage_vs_techrot", "FS" => "damage_vs_scaldra",
        "HD" => "melee_heavy_attack_damage", "HC" => "melee_heavy_attack_charge", "SL" => "melee_slam_damage",
        "PA" => "melee_parry_angle", "GA" => "gas_damage", "CO" => "corrosive_damage", "VI" => "viral_damage",
        "RA" => "radiation_damage", "BL" => "blast_damage", "MA" => "magnetic_damage",
        code => return Err(format!("no public trait identifier for {code}")),
    };
    let unit = match (line.unit, line.code) {
        ("percent", _) => "%", ("x", _) => "x", (_, "P" | "T") => "m", (_, "N") => "s",
        (_, "PA") => "°", _ => "",
    };
    Ok((label, unit))
}

pub(super) fn round(value: f64, digits: usize) -> f64 {
    format!("{value:.digits$}").parse().unwrap_or(value)
}

pub(super) fn percent(value: f64, baseline: f64) -> f64 {
    if baseline > 0.0 { (value / baseline - 1.0) * 100.0 } else { 0.0 }
}
