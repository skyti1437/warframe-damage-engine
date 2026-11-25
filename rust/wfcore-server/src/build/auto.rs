use super::{arcane, cards, json, num, rows, text, truth, EngineData, Map, Request, SimCatalog, SimResult, Value};
use super::evaluate::{Evaluation, Evaluator};
use super::search::Search;
use super::weapon::Evolution;
use indexmap::IndexMap;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::time::Instant;

const EPSILON: f64 = 1e-9;

fn route_key(route: &Value) -> String {
    let mut tiers: Vec<_> = route.as_object().into_iter().flatten().collect();
    tiers.sort_by_key(|(tier, _)| *tier);
    let fields = tiers.into_iter().map(|(key, value)| format!("{}: {}", json!(key), value)).collect::<Vec<_>>();
    format!("{{{}}}", fields.join(", "))
}

fn routes(weapon: &Value) -> Vec<Value> {
    let mut choices = Vec::new();
    for tier in rows(&weapon["incarnon"]["tiers"]) {
        let branches = rows(&tier["branches"]);
        let n = tier["tier"].as_u64().unwrap_or(0);
        if !(2..=5).contains(&n) || (tier["selectable"] != true && branches.len() <= 1) { continue; }
        if choices.is_empty() { choices.push(json!({})); }
        choices = choices.into_iter().flat_map(|route| branches.iter().map(move |branch| {
            let mut route = route.clone();
            route[format!("tier{n}")] = branch["id"].as_str().map_or_else(|| branch["path"].clone(), |v| json!(v));
            route
        })).collect();
    }
    choices
}

fn resolve_routes(eval: &Evaluator<'_>) -> SimResult<Vec<Evolution>> {
    routes(eval.req.weapon).iter().map(|route| {
        let req = Request { choices: route, ..eval.req.clone() };
        Evolution::resolve(eval.data, &req)
    }).collect()
}

pub(super) fn incarnon(eval: &Evaluator<'_>, search: &Search<'_>, mut best: Evaluation) -> SimResult<(Evaluation, Option<Evolution>, Value)> {
    let started = Instant::now();
    let mut routes = resolve_routes(eval)?;
    if routes.len() < 2 {
        return Ok((best, None, json!({"routes": routes.len(), "rows": [], "searches": 0,
            "seconds": 0.0, "defaultRoute": eval.evolution.choices, "defaultScore": null})));
    }
    let keys: Vec<_> = routes.iter().map(|route| route_key(&route.choices)).collect();
    let default = routes.iter().position(|route| route.choices == eval.evolution.choices)
        .ok_or("default Incarnon route is not selectable")?;
    let substitute = |i| Evaluator { evolution: &routes[i], ..eval.clone() };
    let screen = |i, selected: &[usize]| substitute(i).screen(selected, search.metric());
    let mut classes = IndexMap::<_, Vec<usize>>::new();
    let mut screens = Vec::new();
    for i in 0..routes.len() {
        let score = screen(i, &best.cards)?;
        screens.push(score);
        if let Some(value) = score {
            let bare = screen(i, &[])?;
            let round = |v: f64| (v * 1e9).round_ties_even() as i64;
            classes.entry((round(value), bare.map(round))).or_default().push(i);
        }
    }
    let mut rows = IndexMap::new();
    let mut members_of = HashMap::new();
    let mut representatives = Vec::new();
    for members in classes.into_values() {
        let i = if members.contains(&default) { default } else { members[0] };
        representatives.push(i);
        rows.insert(i, json!({"key": keys[i], "route": routes[i].choices, "screen": screens[i], "equivalent": members.len()}));
        members_of.insert(i, members);
    }
    representatives.sort_unstable();
    rows.entry(default).or_insert_with(|| json!({"key": keys[default], "route": eval.evolution.choices,
        "screen": screens[default], "equivalent": 1}));
    let branches: BTreeSet<_> = representatives.iter().flat_map(|i| routes[*i].choices.as_object().unwrap().iter()
        .map(|(tier, branch)| (tier, text(branch)))).collect();
    let sorted = |a: usize, b: usize| num_score(screens[b]).total_cmp(&num_score(screens[a])).then(a.cmp(&b));
    let mut picks = Vec::new();
    for (tier, branch) in branches {
        if let Some(i) = representatives.iter().copied().filter(|i| routes[*i].choices[tier] == branch)
            .min_by(|a, b| sorted(*a, *b)) {
            picks.push(i);
        }
    }
    let mut ranked = representatives.clone();
    ranked.sort_by(|a, b| sorted(*a, *b));
    let deep = search.scope.profile == "deep";
    picks.extend(ranked.into_iter().take(if deep { 6 } else { 3 }));
    let mut first = IndexMap::from([(default, best.clone())]);
    rows[&default]["fast"] = json!(best.score);
    let mut searches = 0;
    for i in picks {
        if first.contains_key(&i) { continue; }
        let found = search.run(&substitute(i), &best.cards, true)?;
        rows[&i]["fast"] = json!(found.score);
        first.insert(i, found);
        searches += 1;
    }
    for i in first.keys().copied().collect::<Vec<_>>() {
        let Some(members) = members_of.get(&i).filter(|members| members.len() > 1) else { continue; };
        let seed = first[&i].cards.clone();
        let representative = screen(i, &seed)?;
        for &member in members.iter().filter(|member| **member != i) {
            let value = screen(member, &seed)?;
            if !value.zip(representative).is_some_and(|(a, b)| (a * 1e9).round_ties_even() != (b * 1e9).round_ties_even()) { continue; }
            let found = search.run(&substitute(member), &best.cards, true)?;
            rows.insert(member, json!({"key":keys[member], "route":routes[member].choices,
                "screen":screens[member], "equivalent":1, "fast":found.score}));
            first.insert(member, found);
            representatives.push(member);
            searches += 1;
        }
    }
    let mut ranked: Vec<_> = first.keys().copied().collect();
    ranked.sort_by(|a, b| first[b].score.total_cmp(&first[a].score).then(a.cmp(b)));
    let threshold = first[&ranked[0]].score * if deep { 0.85 } else { 0.97 };
    let mut finalists: Vec<_> = ranked.into_iter().take(if deep { 4 } else { 2 })
        .filter(|i| first[i].score >= threshold).collect();
    if !finalists.contains(&default) { finalists.push(default); }
    let mut winner = default;
    for (i, value) in &first {
        if value.score > best.score + EPSILON { best = value.clone(); winner = *i; }
    }
    let mut searched = HashSet::new();
    for i in finalists {
        let found = search.run(&substitute(i), if i == default { &[] } else { &first[&i].cards }, false)?;
        rows[&i]["refined"] = json!(found.score);
        searched.insert(i);
        searches += 1;
        if found.score > best.score + EPSILON { best = found; winner = i; }
    }
    for _ in 0..2 {
        let mut challenger: Option<(usize, f64)> = None;
        for &i in &representatives {
            if searched.contains(&i) { continue; }
            if let Some(value) = screen(i, &best.cards)?.filter(|n| *n > best.score + EPSILON) {
                if challenger.is_none_or(|(j, prior)| value > prior || (value == prior && i < j)) {
                    challenger = Some((i, value));
                }
            }
        }
        let Some((i, _)) = challenger else { break; };
        let found = search.run(&substitute(i), &best.cards, false)?;
        rows[&i]["refined"] = json!(found.score);
        searches += 1;
        if found.score > best.score + EPSILON { best = found; winner = i; }
        searched.insert(i);
    }
    let default_score = rows[&default]["refined"].clone();
    let mut ranked: Vec<_> = rows.into_values().collect();
    sort_report(&mut ranked);
    ranked.truncate(12);
    let report = json!({"routes": routes.len(), "rows": ranked, "searches": searches,
        "seconds": (started.elapsed().as_secs_f64() * 100.0).round() / 100.0,
        "defaultRoute": eval.evolution.choices, "defaultScore": default_score});
    Ok((best, (winner != default).then(|| routes.swap_remove(winner)), report))
}

fn num_score(value: Option<f64>) -> f64 { value.unwrap_or(f64::NEG_INFINITY) }

fn sort_report(rows: &mut [Value]) {
    let score = |row: &Value| ["refined", "fast", "screen"].iter()
        .map(|key| num(&row[*key])).find(|value| *value != 0.0).unwrap_or(0.0);
    rows.sort_by(|a, b| score(b).total_cmp(&score(a)));
}

pub(super) fn exilus<'a>(eval: &Evaluator<'a>, catalog: &SimCatalog, search: &Search<'_>,
    mut best: Evaluation, capacity: i64) -> SimResult<(Evaluation, Value, Option<cards::Card<'a>>)> {
    let mut choice = None;
    let mut ranking = (best.score + EPSILON, false, 0, 0, "");
    for row in catalog.utility.values().filter(|row| cards::utility_fits(eval.data, eval.req, row)) {
        let max = cards::utility_rank(row);
        for rank in [max, 0].into_iter().take(1 + usize::from(max > 0)) {
            let wire = cards::utility_at_rank(row, rank, &eval.req.exilus_polarity);
            let cost = wire["drain"].as_i64().unwrap_or(0);
            if best.capacity + cost > capacity { continue; }
            let card = cards::utility(eval.data, eval.req, row, rank);
            if card.as_ref().is_some_and(|exilus| best.cards.iter().any(|i| eval.cards[*i].family == exilus.family)) { continue; }
            let Some(score) = eval.score(&best.cards, best.riven_slot, card.as_ref(), search.metric(), None)? else { continue; };
            if score <= best.score + EPSILON { continue; }
            let key = (score, wire["polarityRelation"] == "matched", -cost, -(rank as i64), text(&row["path"]));
            if key > ranking { ranking = key; choice = Some((wire, card, score, cost)); }
        }
    }
    if let Some((wire, card, score, cost)) = choice {
        best.score = score;
        best.capacity += cost;
        Ok((best, wire, card))
    } else {
        Ok((best, Value::Null, None))
    }
}

pub(super) fn resolve_arcanes<'a>(data: &'a EngineData, catalog: &SimCatalog, req: &Request<'_>,
    equipment: &[Value]) -> SimResult<arcane::Equipped<'a>> {
    let mut body = Map::new();
    body.insert("arcanes".into(), json!(equipment));
    arcane::Equipped::resolve(data, catalog, &Request { body: &body, ..req.clone() })
}

pub(super) fn arcane_rows(data: &EngineData, catalog: &SimCatalog, req: &Request<'_>, include: bool, automatic: bool) -> Vec<Value> {
    if !include { return Vec::new(); }
    let mut seen = HashSet::new();
    let (mut weapon, mut frame) = (0, 0);
    let limit = if req.weapon["slot"] == "arch_gun" { 2 } else { 1 };
    rows(req.body.get("arcanes").unwrap_or(&Value::Null)).iter().take(4).filter_map(|row| {
        let path = text(&row["path"]).trim();
        let arcane = catalog.arcanes.get(path)?;
        let is_frame = arcane["slotKind"] == "warframe";
        let (count, limit) = if is_frame { (&mut frame, 2) } else { (&mut weapon, limit) };
        if *count >= limit || !seen.insert(path) { return None; }
        *count += 1;
        if automatic && !is_frame { return None; }
        if !data.arcanes.get(path).is_some_and(|raw| arcane::applies(raw, req, is_frame)) { return None; }
        let max = arcane["maxRank"].as_u64().unwrap_or(0);
        let mut value = json!({"path":path, "rank":row["rank"].as_u64().unwrap_or(max).min(max)});
        for key in ["stacks", "uptime"] {
            if !row[key].is_null() { value[key] = row[key].clone(); }
        }
        Some(value)
    }).collect()
}

pub(super) fn arcanes(eval: &Evaluator<'_>, catalog: &SimCatalog, search: &Search<'_>,
    mut best: Evaluation, frame: &[Value]) -> SimResult<(Evaluation, Vec<Value>, Value)> {
    let base = best.score;
    let mut picked = Vec::<Value>::new();
    let mut slots = Vec::new();
    let deep = search.scope.profile == "deep";
    let limit = if eval.req.weapon["slot"] == "arch_gun" { 2 } else { 1 };
    for _ in 0..limit {
        let mut choices = Vec::new();
        let mut rows = Vec::new();
        for row in arcane_options(eval, catalog) {
            let path = text(&row["path"]).to_owned();
            if picked.iter().any(|row| row["path"] == path) { continue; }
            let mut trial: Vec<_> = frame.iter().chain(&picked).cloned().collect();
            trial.push(row.clone());
            let Ok(arcanes) = resolve_arcanes(eval.data, catalog, eval.req, &trial) else { continue; };
            let panel = arcanes.snapshot(eval.req, None, None)?;
            let candidate = Evaluator { arcanes: &arcanes, arcane_panel: &panel, ..eval.clone() };
            let screen = candidate.screen(&best.cards, search.metric())?;
            rows.push(json!({"key":path, "screen":screen}));
            choices.push((row, arcanes, panel, screen));
        }

        let mut ranked: Vec<_> = (0..choices.len()).filter(|i| choices[*i].3.is_some_and(|v| deep || v > best.score + EPSILON)).collect();
        ranked.sort_by(|a, b| num_score(choices[*b].3).total_cmp(&num_score(choices[*a].3)));
        ranked.truncate(if deep { 6 } else { 2 });
        let seed = best.cards.clone();
        let mut winner = None;
        let mut searched = HashSet::new();
        for i in ranked {
            let (_, arcanes, panel, _) = &choices[i];
            let candidate = Evaluator { arcanes, arcane_panel: panel, ..eval.clone() };
            let found = search.run(&candidate, &seed, false)?;
            rows[i]["refined"] = json!(found.score);
            searched.insert(i);
            if found.score > best.score + EPSILON { best = found; winner = Some(i); }
        }
        for _ in 0..2 {
            if winner.is_none() { break; }
            let mut challenger = None;
            let mut highest = best.score + EPSILON;
            for (i, (_, arcanes, panel, _)) in choices.iter().enumerate().filter(|(i, _)| !searched.contains(i)) {
                let candidate = Evaluator { arcanes, arcane_panel: panel, ..eval.clone() };
                if let Some(score) = candidate.screen(&best.cards, search.metric())?.filter(|n| *n > highest) {
                    challenger = Some(i);
                    highest = score;
                }
            }
            let Some(i) = challenger else { break; };
            let (_, arcanes, panel, _) = &choices[i];
            let candidate = Evaluator { arcanes, arcane_panel: panel, ..eval.clone() };
            let found = search.run(&candidate, &best.cards, false)?;
            rows[i]["refined"] = json!(found.score);
            searched.insert(i);
            if found.score > best.score + EPSILON { best = found; winner = Some(i); }
        }
        sort_report(&mut rows);
        rows.truncate(12);
        for row in &mut rows {
            row["nameZh"] = catalog.arcanes[text(&row["key"])]["nameZh"].clone();
        }
        slots.push(json!({"candidates":choices.len(), "searches":searched.len(), "rows":rows}));
        let Some(winner) = winner else { break; };
        picked.push(choices[winner].0.clone());
    }
    let chosen = named_arcanes(catalog, &picked);
    let report = json!({"chosen": chosen, "slots": slots, "baseScore": base, "score": best.score});
    Ok((best, frame.iter().cloned().chain(picked).collect(), report))
}

fn arcane_options(eval: &Evaluator<'_>, catalog: &SimCatalog) -> Vec<Value> {
    catalog.arcanes.values().filter_map(|meta| {
        if meta["slotKind"] == "warframe" || truth(&meta["scoreExcluded"]) { return None; }
        let path = text(&meta["path"]);
        let mut row = json!({"path": path, "rank": meta["maxRank"]});
        if path == "/Lotus/Upgrades/CosmeticEnhancers/Offensive/CorrosiveMeleePerAbilityCast" {
            row["stacks"] = json!(4);
            row["uptime"] = json!(1.0);
        }
        resolve_arcanes(eval.data, catalog, eval.req, std::slice::from_ref(&row)).ok()?;
        Some(row)
    }).collect()
}

pub(super) fn named_arcanes(catalog: &SimCatalog, picked: &[Value]) -> Vec<Value> {
    picked.iter().map(|row| {
        let mut row = row.clone();
        row["nameZh"] = catalog.arcanes[text(&row["path"])]["nameZh"].clone();
        row
    }).collect()
}

pub(super) fn rescreen_route(eval: &Evaluator<'_>, search: &Search<'_>, best: &Evaluation)
    -> SimResult<Option<(Evaluation, Evolution)>> {
    let mut winner = None;
    let mut high = best.score + EPSILON;
    for route in resolve_routes(eval)? {
        let candidate = Evaluator { evolution: &route, ..eval.clone() };
        if let Some(score) = candidate.screen(&best.cards, search.metric())?.filter(|s| *s > high) {
            winner = Some(route);
            high = score;
        }
    }
    let Some(route) = winner else { return Ok(None); };
    let candidate = Evaluator { evolution: &route, ..eval.clone() };
    let found = search.run(&candidate, &best.cards, false)?;
    Ok((found.score > best.score).then_some((found, route)))
}

fn neighbour_routes(eval: &Evaluator<'_>) -> SimResult<Vec<Evolution>> {
    let mut result = Vec::new();
    let baseline = &eval.evolution.choices;
    for tier in rows(&eval.req.weapon["incarnon"]["tiers"]) {
        let key = format!("tier{}", tier["tier"]);
        if baseline[&key].is_null() || rows(&tier["branches"]).len() <= 1 { continue; }
        for branch in rows(&tier["branches"]) {
            let id = if truth(&branch["id"]) { &branch["id"] } else { &branch["path"] };
            if baseline[&key] == *id { continue; }
            let neutral = !["runtimeScripts", "conditionalEffects"].iter().any(|key| truth(&branch[*key]))
                && !rows(&branch["sources"]).iter().any(|row| truth(&row["conditionalEffects"]))
                && rows(&branch["effects"]).iter().all(|row| {
                    let kind = text(&row["upgradeType"]);
                    kind.starts_with("AVATAR_") || kind.starts_with("SKILL_") || matches!(kind,
                        "NONE" | "WEAPON_RECOIL" | "WEAPON_ZOOM" | "WEAPON_AMMO_MAX" | "WEAPON_AUTO_RELOAD_RATE"
                        | "WEAPON_ADD_CHARGE_COUNTER_MULTIPLIER" | "WEAPON_NOISE_REDUCTION")
                });
            if neutral { continue; }
            let mut choice = baseline.clone();
            choice[&key] = id.clone();
            let req = Request { choices: &choice, ..eval.req.clone() };
            result.push(Evolution::resolve(eval.data, &req)?);
        }
    }
    Ok(result)
}

pub(super) fn coupled(eval: &Evaluator<'_>, catalog: &SimCatalog, search: &Search<'_>, best: &Evaluation,
    frame: &[Value], equipped: &[Value]) -> SimResult<(Option<(Evaluation, Evolution, Vec<Value>)>, Value)> {
    let mut best = best.clone();
    let mut picked = equipped.to_vec();
    let mut selected_route = None;
    let candidates = arcane_options(eval, catalog);
    let mut candidate_count = 0;
    let mut searches = 0;
    let base_score = best.score;
    let limit = if eval.req.weapon["slot"] == "arch_gun" { 2 } else { 1 };
    for _ in 0..if search.scope.profile == "fast" { 1 } else { 2 } {
        let eval = Evaluator { evolution: selected_route.as_ref().unwrap_or(eval.evolution), ..eval.clone() };
        let routes = neighbour_routes(&eval)?;
        if routes.is_empty() { break; }
        let mut variants: Vec<_> = Vec::new();
        for slot in 0..limit.min(picked.len() + 1) {
            for row in std::iter::once(None).chain(candidates.iter().map(Some)) {
                let mut trial = picked.clone();
                if let Some(row) = row {
                    if trial.iter().enumerate().any(|(i, part)| i != slot && part["path"] == row["path"]) { continue; }
                    if slot == trial.len() { trial.push(row.clone()); }
                    else if trial[slot] == *row { continue; }
                    else { trial[slot] = row.clone(); }
                } else if slot < trial.len() {
                    trial.remove(slot);
                } else { continue; }
                if variants.iter().any(|(existing, _, _)| *existing == trial) { continue; }
                let equipment: Vec<_> = frame.iter().chain(&trial).cloned().collect();
                let Ok(arcanes) = resolve_arcanes(eval.data, catalog, eval.req, &equipment) else { continue; };
                let panel = arcanes.snapshot(eval.req, None, None)?;
                variants.push((trial, arcanes, panel));
            }
        }
        candidate_count += routes.len() * variants.len();
        let mut ranked = Vec::new();
        for (i, route) in routes.iter().enumerate() {
            for (j, (_, arcanes, panel)) in variants.iter().enumerate() {
                let candidate = Evaluator { evolution: route, arcanes, arcane_panel: panel, ..eval.clone() };
                if let Some(score) = candidate.screen(&best.cards, search.metric())?.filter(|score| *score > best.score + EPSILON) {
                    ranked.push((i, j, score));
                }
            }
        }
        ranked.sort_by(|a, b| b.2.total_cmp(&a.2));
        let seed = best.cards.clone();
        let mut chosen = None;
        for (i, j, _) in ranked.into_iter().take(if search.scope.profile == "deep" { 3 } else { 2 }) {
            let (_, arcanes, panel) = &variants[j];
            let candidate = Evaluator { evolution: &routes[i], arcanes, arcane_panel: panel, ..eval.clone() };
            let found = search.run(&candidate, &seed, false)?;
            searches += 1;
            if found.score > best.score + EPSILON {
                best = found;
                chosen = Some((i, j));
            }
        }
        let Some((i, j)) = chosen else { break; };
        picked = variants.swap_remove(j).0;
        selected_route = routes.into_iter().nth(i);
    }
    let report = json!({"candidates":candidate_count, "searches":searches, "baseScore":base_score,
        "adopted":selected_route.is_some(), "score":best.score});
    Ok((selected_route.map(|route| (best, route, picked)), report))
}
