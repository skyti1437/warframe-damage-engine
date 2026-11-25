//! Automatic loadout search over the same panel and replay reducers as manual simulation.

mod auto;
mod buffs;
mod cards;
mod evaluate;
mod forma;
mod modular;
mod projectiles;
mod request;
mod response;
mod search;

use super::{arcane, damage, events, identity, loadout, melee, num, rows, text, timeline, truth, weapon,
    EngineData, Map, Request, SimCatalog, SimResult, Target, Value, BENCHMARK_TARGET, json};
use evaluate::Evaluator;
use search::Search;
use std::collections::HashMap;
use weapon::Evolution;

pub(crate) fn run(data: &EngineData, catalog: &SimCatalog, fields: &Map<String, Value>) -> SimResult<Value> {
    let mut body = request::normalize(fields)?;
    let raw_weapon = catalog.weapons.get(body.get("weapon").and_then(Value::as_str).unwrap_or("").trim())
        .ok_or("weapon must be an exported weapon path")?;
    let weapon_type = text(&raw_weapon["weaponType"]);
    let scope = request::Scope::parse(&body, catalog, matches!(weapon_type, "melee" | "zaw"))?;
    if catalog.enemies.get(body.get("enemy").and_then(Value::as_str).unwrap_or("")).is_none() {
        return Err("enemy must be an exported enemy path".into());
    }
    if raw_weapon["exalted"].is_object() {
        return Err("exalted weapon builds require a linked Warframe configuration calculator".into());
    }
    let base = data.weapons.get(text(&raw_weapon["path"])).ok_or("weapon has no exported combat record")?;
    let parts = body.remove("kitgunParts").unwrap_or(Value::Null);
    let assembled = modular::assemble(data, base, &parts)?;
    if assembled.is_none() && raw_weapon["buildAvailable"] != true {
        return Err(raw_weapon["buildIssue"].as_str().unwrap_or("weapon build is unavailable").into());
    }
    if assembled.is_some() { body.insert("fireMode".into(), json!(0)); }
    if scope.metric == "heavyDamage" {
        if let Some(value) = body["scenario"].get("heavyAttack").or_else(|| body.get("heavyAttack")) {
            if !value.is_boolean() { return Err("heavyAttack must be a boolean".into()); }
        }
        body["scenario"]["heavyAttack"] = json!(true);
    }
    if body.get("damageModel").is_some_and(|v| text(v).eq_ignore_ascii_case("panel")) {
        body.insert("damageModel".into(), json!("target"));
    }
    let mut resolved = assembled.unwrap_or_else(|| base.clone());
    let innate = modular::innate(&body, &resolved)?;
    body.remove("innateElement");
    if !innate.is_null() { resolved["innateElements"] = json!([innate]); }
    let external = body.remove("externalBuffs").unwrap_or(Value::Null);
    let req = Request::parse_weapon(data, catalog, &body, Some(&resolved))?;
    let availability = &raw_weapon["fireModes"][req.mode]["metricAvailability"][&scope.metric];
    if parts.is_null() && availability["available"] != true {
        return Err(availability["issue"].as_str().unwrap_or("fire mode metric is unavailable").into());
    }
    let stance = melee::stance(data, &catalog.stances, &req)?;
    let schedule = melee::Schedule::resolve(data, &req, &stance)?;
    let selected_target = catalog.enemies.get(text(&req.target["path"]));
    let mut req = req;
    let buffs = buffs::Buffs::resolve(data, &external, req.weapon, req.melee)?;
    for (key, value) in [("auraArmourReduction", buffs.armour_reduction), ("auraShieldReduction", buffs.shield_reduction)] {
        if value > 0.0 { req.enhancements[key] = json!(value); }
    }
    if !truth(&req.target["health"]) {
        req.target = data.enemies.get(BENCHMARK_TARGET).ok_or("missing benchmark enemy")?.clone();
        req.level = 170;
        req.steel = true;
        req.enhancements = json!({"eximus": false, "noOverguard": true});
        req.strip = 0.0;
        req.elemental_strip = 0.0;
    }
    let target = Target::new(&req.target, data);
    let capacity_limit = 60 + stance["capacityGain"].as_i64().unwrap_or(0);

    let mut fixed = Value::Null;
    let mut fixed_exilus = None;
    let exilus = body.get("exilus").unwrap_or(&Value::Null);
    if !exilus.is_null() {
        let values = exilus.as_object().ok_or("exilus must be an object")?;
        match values.get("mode").map_or("fixed", text) {
            "auto" if values.len() == 1 => (),
            "fixed" => {
                let row = catalog.utility.get(loadout::query(exilus)).ok_or("exilus must use a supported utility MOD path")?;
                if !cards::utility_fits(data, &req, row) { return Err("exilus MOD is incompatible with this weapon".into()); }
                let rank = loadout::ranked(&exilus["rank"], cards::utility_rank(row), "exilus.rank")?;
                fixed = cards::utility_at_rank(row, rank, &req.exilus_polarity);
                fixed_exilus = cards::utility(data, &req, row, rank);
            }
            _ => return Err("exilus must select a fixed MOD or specify only mode=auto".into()),
        }
    }
    let riven = cards::Riven::parse(data, body.get("riven").unwrap_or(&Value::Null))?;
    let (cards, rejected) = cards::candidates(data, catalog, &req, weapon_type,
        schedule.as_ref().map(|s| s.route.as_str()), fixed_exilus.as_ref());
    let shells: HashMap<_, _> = data.mods.values().filter_map(|raw| {
        let key = loadout::set_key(&raw["modSet"]);
        (!key.is_empty() && !raw["setPieces"].is_null() && (raw["selectable"] == false
            || (!truth(&raw["selectable"]) && !truth(&raw["compatibility"])))).then_some((key, raw))
    }).collect();
    let evolution = Evolution::resolve(data, &req)?;
    let auto_route = body.get("autoIncarnon").is_some_and(|v| v == true) && truth(&evolution.choices);
    let auto_arcane = body.get("autoArcane").is_some_and(|v| v == true);
    let arcane_rows = auto::arcane_rows(data, catalog, &req, scope.include_arcanes, auto_arcane);
    let arcanes = auto::resolve_arcanes(data, catalog, &req, &arcane_rows)?;
    let arcane_panel = arcanes.snapshot(&req, None, None)?;

    let evaluator = Evaluator { data, req: &req, target, stance: &stance, schedule: schedule.as_ref(),
        cards: &cards, riven: riven.as_ref(), fixed_exilus: fixed_exilus.as_ref(), arcanes: &arcanes,
        arcane_panel: &arcane_panel, evolution: &evolution, external: &buffs.effects,
        capacity_limit: capacity_limit - fixed["drain"].as_i64().unwrap_or(0), shells };
    let search = Search::new(&evaluator, &scope)?;
    let mut best = search.run(&evaluator, &[], auto_route)?;
    let mut report = Map::new();
    let auto_started = std::time::Instant::now();
    let mut route = None;
    if auto_route {
        let (selected, chosen, info) = auto::incarnon(&evaluator, &search, best)?;
        best = selected;
        route = chosen;
        report.insert("incarnon".into(), info);
    }
    let mut equipment = arcane_rows.clone();
    if auto_arcane {
        let current = Evaluator { evolution: route.as_ref().unwrap_or(&evolution), ..evaluator.clone() };
        let (selected, chosen, info) = auto::arcanes(&current, catalog, &search, best, &arcane_rows)?;
        best = selected;
        equipment = chosen;
        report.insert("arcane".into(), info);

        if auto_route {
            let arcanes = auto::resolve_arcanes(data, catalog, &req, &equipment)?;
            let panel = arcanes.snapshot(&req, None, None)?;
            let current = Evaluator { arcanes: &arcanes, arcane_panel: &panel, ..current };
            let rescreened = if equipment.len() > arcane_rows.len() {
                auto::rescreen_route(&current, &search, &best)?
            } else { None };
            if let Some((selected, _)) = &rescreened { best = selected.clone(); }
            let current = Evaluator { evolution: rescreened.as_ref().map(|(_, route)| route).unwrap_or(current.evolution), ..current };
            let (coupled, info) = auto::coupled(&current, catalog, &search, &best, &arcane_rows, &equipment[arcane_rows.len()..])?;
            report.insert("coupled".into(), info);
            if let Some((selected, chosen_route, picked)) = coupled {
                best = selected;
                route = Some(chosen_route);
                equipment = arcane_rows.iter().cloned().chain(picked).collect();
                report.get_mut("arcane").unwrap()["chosen"] = json!(auto::named_arcanes(catalog, &equipment[arcane_rows.len()..]));
            } else if let Some((_, chosen_route)) = rescreened {
                route = Some(chosen_route);
            }
        }
    }
    if !report.is_empty() {
        report.insert("seconds".into(), json!((auto_started.elapsed().as_secs_f64() * 100.0).round() / 100.0));
    }
    let arcanes = auto::resolve_arcanes(data, catalog, &req, &equipment)?;
    let arcane_panel = arcanes.snapshot(&req, None, None)?;
    let evaluator = Evaluator { evolution: route.as_ref().unwrap_or(&evolution), arcanes: &arcanes,
        arcane_panel: &arcane_panel, ..evaluator };
    best = search.refine(&evaluator, best)?;
    if (best.cards.is_empty() && scope.min != Some(0)) || !best.score.is_finite() || best.score < 0.0 {
        return Err(format!("optimizer returned no usable MOD build for fire mode {} metric {}", req.mode, scope.metric));
    }
    let mut special = fixed_exilus.as_ref();
    let auto_special;
    let exilus = if !fixed.is_null() {
        best.capacity += fixed["drain"].as_i64().unwrap_or(0);
        fixed
    } else if exilus["mode"] == "auto" || body.get("autoExilus").is_none_or(|v| v == true) {
        let (selected, row, card) = auto::exilus(&evaluator, catalog, &search, best, capacity_limit)?;
        best = selected;
        auto_special = card;
        special = auto_special.as_ref();
        row
    } else {
        Value::Null
    };
    for key in ["arcane", "coupled"] {
        if let Some(choice) = report.get_mut(key) { choice["score"] = json!(best.score); }
    }
    response::payload(&evaluator, &search, &best, special, exilus, Value::Object(report),
        capacity_limit, rejected, raw_weapon, selected_target, catalog)
}
