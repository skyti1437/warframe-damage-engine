//! Portable Riven validation, hardness calibration and build analysis.

mod cache;
mod context;
mod incarnon;
mod recommendation;
mod response;

pub(crate) use cache::Cache;
use super::{cards::Riven, evaluate::{Evaluation, Evaluator, HardnessNode, Metric, ReplayOptions},
    json, num, request, riven_traits, rows, search::Search, text, truth, EngineData, Map, Request,
    SimCatalog, SimResult, Value};
use super::trait_matrix::{percent, round};
use std::cell::Cell;
use std::collections::HashSet;
use std::time::{Duration, Instant};

const METRIC: &str = "killsPerMinute";
const CALIBRATION_VERSION: &str = "hardness-integrated-v1";

pub(super) struct Input<'a> {
    requested_target: Value,
    disposition: f64,
    rank: u64,
    physical: bool,
    card: Riven,
    unsupported: Vec<String>,
    fields: Map<String, Value>,
    cache: &'a Cache,
    timeline_seconds: Option<f64>,
}

pub(crate) fn run(data: &EngineData, catalog: &SimCatalog, fields: &Map<String, Value>, cache: &Cache) -> SimResult<Value> {
    let legacy = fields.get("disposition").is_none_or(Value::is_null) && fields.get("sourceWeapon").is_some_and(truth);
    let path = fields.get(if legacy { "sourceWeapon" } else { "weapon" }).and_then(Value::as_str)
        .map(str::trim).filter(|v| !v.is_empty()).ok_or("weapon must be an exported weapon path")?;
    let meta = catalog.weapons.get(path).ok_or("weapon is not in the portable Riven catalog")?;
    let weapon = data.weapons.get(path).ok_or("weapon has no exported combat record")?;
    let assembled = fields.get("kitgunParts").is_some_and(|value| !value.is_null());
    if !assembled && (meta["available"] != true || meta["analysisAvailable"] != true) {
        return Err(meta["issue"].as_str().or(meta["analysisIssue"].as_str()).unwrap_or("weapon analysis is unavailable").to_owned());
    }
    let number = |value: Option<&Value>, label: &str| value.and_then(Value::as_f64)
        .filter(|value| *value > 0.0 && *value <= 2.0)
        .ok_or_else(|| format!("{label} must be finite and between 0 and 2"));
    let disposition = number(fields.get(if legacy { "sourceDisposition" } else { "disposition" }), "disposition")?;
    let source_disposition = fields.get("sourceDisposition").filter(|v| !v.is_null())
        .map_or(Ok(disposition), |value| number(Some(value), "source disposition"))?;
    let scale = disposition / source_disposition;
    if !scale.is_finite() { return Err("riven disposition scale must be finite".into()); }
    let physical = fields.contains_key("rivenCard");
    let (rank, polarity) = if let Some(value) = fields.get("rivenCard") {
        let rank = value["rank"].as_u64().filter(|n| *n <= 8).ok_or("rivenCard.rank must be an integer between 0 and 8")?;
        let polarity = value["polarity"].as_str().filter(|v| matches!(*v, "AP_ATTACK" | "AP_DEFENSE" | "AP_TACTIC"))
            .ok_or("rivenCard requires a valid fixed polarity")?;
        (rank, polarity)
    } else { (8, "AP_ATTACK") };
    let stats = fields.get("stats").and_then(Value::as_array).filter(|values| (2..=4).contains(&values.len()))
        .ok_or("portable Riven input requires 2 to 4 stat objects")?;
    let mut parsed = Vec::new();
    for stat in stats {
        let raw = stat.as_object().ok_or("portable Riven stats must be objects")?;
        let negative = raw.get("negative").map_or(Ok(false), |v| v.as_bool().ok_or("portable Riven stat negative flags must be boolean"))?;
        let code = match raw.get("code") {
            Some(Value::String(value)) => value.trim().to_ascii_uppercase(),
            Some(Value::Number(value)) if value.as_f64() != Some(0.0) => value.to_string(),
            _ => String::new(),
        };
        parsed.push((code, negative, stat));
    }
    let negatives = parsed.iter().filter(|(_, negative, _)| *negative).count();
    let positives = parsed.len() - negatives;
    if !matches!(positives, 2 | 3) || negatives > 1 {
        return Err("portable Riven input requires 2 or 3 positive stats and at most 1 negative stat".into());
    }
    let kind = text(&meta["rivenType"]);
    let traits = riven_traits::traits(data, kind, weapon);
    let mut seen = HashSet::new();
    let mut fusion = 0;
    let mut unsupported = Vec::new();
    let mut lines = Vec::new();
    for (code, negative, stat) in parsed {
        if !seen.insert(code.clone()) { return Err(format!("unsupported Riven stat(s): riven-stat-{code}-duplicate")); }
        let rule = traits.iter().find(|r| r.code == code).ok_or_else(|| {
            if rows(&data.rules.riven["fusionCodes"]).iter().any(|v| v == &code) {
                format!("融合词条 {code} 对 {} 类型不可用（该类型当前模板没有该融合词条）", text(&meta["weaponType"]))
            } else { format!("Riven stat {code} has no valid portable range") }
        })?;
        if rule.fusion {
            fusion += 1;
            if negative { return Err(format!("融合词条 {code} 不能作为负面词条（仅正面）")); }
            if fusion > 1 { return Err(format!("融合词条每卡限 1 条（当前 {fusion} 条）")); }
        }
        if !(if negative { rule.negative } else { rule.positive }) {
            return Err(format!("Riven stat {code} is not allowed for this weapon"));
        }
        let value = stat["value"].as_f64().filter(|value| *value >= 0.0)
            .ok_or_else(|| format!("Riven stat {code} value must be finite and non-negative"))?;
        let bounds = |base| -> SimResult<(f64, f64)> {
            let nominal = f64::abs(base * source_disposition * riven_traits::structure(positives, negatives != 0, negative))
                * ((rank + 1) as f64 / 9.0);
            let (low, high) = (nominal * 0.9, nominal * 1.1);
            if value < low - 0.11 || value > high + 0.11 || (value == 0.0 && (rank == 8 || low > 0.05)) {
                return Err(format!("Riven stat {code} value {value} is outside {low:.1}-{high:.1}"));
            }
            Ok((low, high))
        };
        let (mut low, mut high) = bounds(rule.default_base)?;
        if physical { (low, high) = bounds(rule.template_base.unwrap_or(rule.default_base))?; }
        if !rule.modeled { unsupported.push(format!("riven-stat-{code}-{}-not-evaluated", rule.upgrade)); }
        let mut line = rule.line(value * scale, low * scale, high * scale, negative);
        if let Some(label) = stat["label"].as_str().filter(|v| !v.is_empty()) { line["label"] = json!(label); }
        if let Some(unit) = stat["unit"].as_str().filter(|v| !v.is_empty()) { line["unit"] = json!(unit); }
        lines.push(line);
    }
    if lines.iter().any(|line| matches!(text(&line["code"]), "C" | "G" | "I")) {
        unsupported.push("faction-riven-stats-excluded-from-target-neutral-objective".into());
    }
    if fields.get("metric").filter(|v| !v.is_null() && **v != "").is_some() {
        unsupported.push(format!("requested-metric-{}-ignored-using-{METRIC}", text(&fields["metric"])));
    }
    if let Some(value) = fields.get("weaponType").filter(|v| !v.is_null() && **v != "") {
        let value = value.as_str().ok_or("weaponType must be a string")?;
        let weapon_type = match value.trim().to_ascii_lowercase().as_str() {
            "zaw" | "archmelee" | "arch-melee" | "spacemelee" | "space-melee" => "melee",
            "secondary" => "pistol", "primary" => "rifle", "archwing" | "arch-gun" => "archgun",
            "rifle" | "pistol" | "archgun" | "melee" | "shotgun" => value.trim(),
            "kitgun" if weapon["slot"] == "secondary" => "pistol",
            "kitgun" => "rifle",
            _ => kind,
        };
        if !weapon_type.eq_ignore_ascii_case(response::weapon_type(meta, weapon)) {
            return Err(format!("weaponType {weapon_type} does not match exported weapon type {}", response::weapon_type(meta, weapon)));
        }
    }
    let profile = fields.get("profile").filter(|v| !v.is_null()).map(|v| v.as_str().ok_or("profile must be a string"))
        .transpose()?.unwrap_or("").trim().to_ascii_lowercase();
    if !matches!(profile.as_str(), "" | "ultra" | "fast" | "standard" | "deep") {
        return Err("profile must be one of ultra, fast, standard, deep".into());
    }
    let (target, mut body) = context::benchmark(fields, data, catalog)?;
    body.insert("weapon".into(), json!(path));
    if body.get("fireMode").is_none_or(Value::is_null) {
        let count = rows(&weapon["fireModes"]).len();
        body.insert("fireMode".into(), json!(if !assembled && weapon["isIncarnon"] == true && count > 1 { count - 1 } else { 0 }));
    }
    body.insert("slotCount".into(), json!(8));
    body.insert("metric".into(), json!(METRIC));
    if body.get("arcanes").is_some_and(Value::is_null) { body.remove("arcanes"); }
    for key in ["riven", "externalBuffs", "pinnedMods", "excludedMods", "forcedProcs"] { body.remove(key); }
    if let Some(raw) = fields.get("incarnonChoices").filter(|v| !v.is_null()) {
        for (key, choice) in raw.as_object().ok_or("incarnonChoices must be an object")? {
            let key = key.trim().to_ascii_lowercase();
            if !matches!(key.strip_prefix("tier").unwrap_or(&key), "2" | "3" | "4" | "5")
                || choice.as_str().is_none_or(|v| v.trim().is_empty()) {
                return Err("incarnon choices require a selectable tier and a non-empty string".into());
            }
        }
    }
    let combat_state = body.get("combatState").cloned().unwrap_or_else(|| json!({}));
    if let Some(state) = body.get_mut("scenario").and_then(Value::as_object_mut) {
        if let Some(route) = fields.get("meleeRoute").filter(|v| truth(v)) {
            let combat = state.entry("combatState").or_insert_with(|| combat_state.clone());
            combat.as_object_mut().ok_or("combatState must be an object")?.insert("meleeRoute".into(), route.clone());
        }
        if let Some(combo) = fields.get("meleeCombo").filter(|v| truth(v)) {
            let combat = state.entry("combatState").or_insert(combat_state);
            combat.as_object_mut().ok_or("combatState must be an object")?.insert("meleeCombo".into(), combo.clone());
        }
    }
    let timeline_seconds = fields.get("timelineSeconds").filter(|value| !value.is_null()).map(|value| {
        value.as_f64().or_else(|| value.as_str().and_then(|s| s.parse().ok()))
            .filter(|value| *value > 0.0 && *value <= 300.0)
            .ok_or("timeline seconds must be greater than zero and at most 300")
    }).transpose()?.or_else(|| rows(fields.get("arcanes").unwrap_or(&Value::Null)).iter()
        .any(|arcane| arcane.get("triggerRate").is_some_and(|v| !v.is_null())).then_some(10.0));
    if timeline_seconds.is_some() && response::weapon_type(meta, weapon) == "melee" && weapon["heavyAttack"].is_object() {
        return Err("timeline seconds are not supported for single heavy/throw damage".into());
    }
    let card = riven_traits::card(data, lines, rank, polarity)?;
    let mut request_key = fields.clone();
    for key in ["progressToken", "_searchBudgetSeconds"] { request_key.remove(key); }
    let mut key = String::new();
    context::canonical_json(&Value::Object(request_key), &mut key);
    let _gate = cache.solve_gate.lock().unwrap();
    if !fields.get("_bakeStaticsOnly").is_some_and(truth) {
        if let Some(mut result) = cache.results.lock().unwrap().get(&key) {
            result["responseCache"] = json!("exact-request-hit");
            return Ok(result);
        }
    }
    let input = Input { requested_target: target, disposition, rank, physical, card, unsupported, fields: fields.clone(), cache, timeline_seconds };
    let result = super::solve(data, catalog, &body, super::SolveMode::Analyze(&input))?;
    if !fields.get("_bakeStaticsOnly").is_some_and(truth) && result["searchWork"]["limited"] == 0 {
        cache.results.lock().unwrap().insert(key, result.clone(), 32);
    }
    Ok(result)
}

pub(super) fn calculate(eval: &Evaluator<'_>, search: &Search<'_>, input: &Input<'_>, catalog: &SimCatalog,
    meta: &Value, exilus: Value, rejected: usize) -> SimResult<Value> {
    let started = Instant::now();
    let profile = search.scope.profile;
    let ultra = profile == "ultra";
    let fast = matches!(profile, "ultra" | "fast");
    let budget = input.fields.get("_searchBudgetSeconds").filter(|v| !v.is_null()).map(|value| {
        value.as_f64().filter(|v| *v > 0.0).ok_or("_searchBudgetSeconds must be a finite positive number")
            .map(|v| v.min(match profile { "ultra" => 35.0, "fast" => 45.0, "deep" => 240.0, _ => 90.0 }))
    }).transpose()?;
    let work = Cell::new(0);
    let limited = Cell::new(0);
    let deadline = budget.map(|seconds| started + Duration::from_secs_f64(seconds));
    let eval = Evaluator { replay_options: ReplayOptions { branches: if fast { 4 } else { 16 },
        evaluations: Some(&work), deadline, limited:Some(&limited), ..Default::default() }, ..eval.clone() };
    let metric = if fast { Metric::Events } else { Metric::WindowDps };
    let metric_name = if fast { METRIC } else { "windowedDps" };
    let statics_profile = if ultra {
        input.fields.get("_staticsProfile").and_then(Value::as_str)
            .filter(|v| matches!(*v, "standard" | "deep")).unwrap_or("fast")
    } else { profile };
    let statics_metric = if statics_profile == "fast" { METRIC } else { "windowedDps" };
    let signature = cache::key(&eval, search, input, statics_profile, statics_metric);
    let cached = signature.as_ref().and_then(|key| input.cache.statics.lock().unwrap().get(key));
    let cached_nodes = cached.as_ref().map_or(&[][..], |entry| &entry.nodes);
    let count = input.card.lines.iter().filter(|line| line["negative"] != true).count();
    let route_sensitive = eval.schedule.is_some_and(|schedule| eval.req.scenario.heavy || schedule.route != "combo");
    let mut unsupported = input.unsupported.clone();
    unsupported.extend(eval.evolution.notes.iter().cloned());
    unsupported.extend(eval.arcane_panel.notes.iter().cloned());
    let cached_eval = Evaluator { replay_options: ReplayOptions { nodes: cached_nodes, ..eval.replay_options }, ..eval.clone() };
    let reused = cached.as_ref().map(|entry| {
        if metric_name == statics_metric && !cached_nodes.is_empty() {
            Ok(Some(Evaluation { cards: entry.cards.clone(), riven_slot: None,
                score: entry.baseline_score, capacity: eval.capacity(&entry.cards) }))
        } else { cached_eval.fixed(&entry.cards, None, metric, None) }
    }).transpose()?.flatten();
    let fresh = reused.is_none();
    if ultra && fresh { unsupported.push("ultra-profile-baseline-approximate".into()); }
    let mut baseline = if let Some(value) = reused { value } else {
        optimize(&eval, &eval, search, metric, ultra, route_sensitive, false)?
    };
    let mut nodes = if cached_nodes.is_empty() { hardness(&eval, &baseline)? } else { cached_nodes.to_vec() };
    if cached_nodes.is_empty() && !ultra && nodes.get(1).is_some_and(|node| node.scale > 2.0 || node.scale < 0.5) {
        let center = Evaluator { replay_options: ReplayOptions { scale: Some(nodes[1].scale), ..eval.replay_options }, ..eval.clone() };
        baseline = optimize(&center, &eval, search, metric, ultra, route_sensitive, false)?;
        let adjusted = hardness(&eval, &baseline)?;
        if !adjusted.is_empty() { nodes = adjusted; }
    }
    if nodes.is_empty() { unsupported.push("hardness-nodes-unavailable-single-target".into()); }
    let center_scale = nodes.get(1).map(|node| node.scale);
    let center = Evaluator { replay_options: ReplayOptions { scale: center_scale, ..eval.replay_options }, ..eval.clone() };
    let integrated = Evaluator { replay_options: ReplayOptions { nodes: &nodes, ..eval.replay_options }, ..eval.clone() };
    if !nodes.is_empty() && cached_nodes.is_empty() {
        baseline = integrated.fixed(&baseline.cards, None, metric, None)?.unwrap_or(baseline);
    }
    let searched_baseline = baseline.clone();
    let card_center = Evaluator { riven: Some(&input.card), ..center.clone() };
    let card_integrated = Evaluator { riven: Some(&input.card), ..integrated.clone() };
    let mut actual = optimize_actual(&card_center, &card_integrated, search, &baseline, metric, ultra, route_sensitive)?;
    if ultra {
        let cached_seeds = cached.as_ref().map_or(&[][..], |entry| &entry.archetypes);
        let warm = warm_starts(&card_center, search, &baseline, &actual, cached_seeds, metric)?;
        let witnesses: Vec<_> = baseline.cards.iter().chain(&actual.cards)
            .chain(cached_seeds.iter().flatten()).copied().collect();
        for start in warm.into_iter().take(2) {
            let value = warm_refine(&card_center, search, start, metric, &witnesses)?;
            let value = card_integrated.fixed(&value.cards, value.riven_slot, metric, None)?.unwrap_or(value);
            if value.score > actual.score { actual = value; }
        }
        if fresh && search.scope.min.is_none() && search.scope.max.is_none() && search.scope.required.is_empty() && search.scope.forbidden.is_empty() {
            if let Some(value) = reverse_floor(&center, &integrated, &actual, metric)? {
                if value.score > baseline.score * (1.0 + 1e-9) {
                    baseline = value;
                    unsupported.push("baseline-raised-by-reverse-floor".into());
                }
            }
        }
    }
    let mut route = if !ultra && eval.req.choices.is_null() && input.timeline_seconds.is_none() {
        incarnon::screen(&center, &integrated, search, &searched_baseline, &actual, &input.card, metric, route_sensitive)?
    } else { None };
    let mut changed_baseline = false;
    if let Some(route) = &mut route {
        if let Some(adopted) = &route.adopted {
            actual = adopted.actual.clone();
            unsupported.extend(adopted.evolution.notes.iter().cloned());
        }
        let candidate = route.adopted.as_ref().and_then(|selected| selected.baseline.as_ref());
        route.report["baselineScores"] = json!({"defaultRoute":baseline.score, "chosenRoute":candidate.map(|v| v.score)});
        if let Some(value) = candidate.filter(|value| value.score > baseline.score * (1.0 + 1e-9)) {
            baseline = value.clone();
            changed_baseline = true;
        }
        route.report["baselineRaised"] = json!(changed_baseline);
    }
    let selected = route.as_ref().and_then(|route| route.adopted.as_ref()).map(|route| &route.evolution);
    let actual_eval = Evaluator { evolution: selected.unwrap_or(integrated.evolution), ..integrated.clone() };
    let baseline_eval = if changed_baseline { &actual_eval } else { &integrated };
    if eval.req.body.get("exilus").is_some_and(|value| value["mode"] == "auto") {
        let scout = Evaluator { evolution:actual_eval.evolution, riven:Some(&input.card), ..center.clone() };
        let selected = super::auto::exilus(&scout, catalog, search, actual.clone(), eval.capacity_limit)?.1;
        if !selected.is_null() {
            let mut body = eval.req.body.clone();
            body.insert("exilus".into(), json!({"path":selected["path"], "rank":selected["rank"]}));
            return super::solve(eval.data, catalog, &body, super::SolveMode::Analyze(input));
        }
    }
    let existing = cached.as_ref().and_then(|value| value.recommendations.get(&count));
    let cached_recommendation = existing.is_some();
    let recommendation = if let Some(value) = existing {
        value.clone()
    } else if ultra {
        unsupported.extend(["ultra-profile-recommendation-not-cached".into(), "ultra-profile-negative-ranking-not-cached".into()]);
        recommendation::Recommendation::default()
    } else {
        let (value, archetypes) = recommendation::recommend(&center, &integrated, search, input, &searched_baseline,
            text(&meta["rivenType"]), metric, route_sensitive)?;
        if value.negative.is_empty() { unsupported.push("no-objective-neutral-supported-negative-stat".into()); }
        if let Some(key) = signature.filter(|_| limited.get() == 0) {
            let mut cache = input.cache.statics.lock().unwrap();
            let mut entry = cache.get(&key).unwrap_or(cache::Statics {
                cards:searched_baseline.cards.clone(), baseline_score:searched_baseline.score, nodes:nodes.clone(), archetypes,
                recommendations:Default::default(),
            });
            entry.recommendations.insert(count, value.clone());
            cache.insert(key, entry, 64);
        }
        value
    };
    let result = response::payload(response::ResultContext {
        input, catalog, meta, exilus: &exilus, search, integrated: &integrated, center: &center,
        baseline: &baseline, actual: &actual, metric, metric_name, nodes: &nodes, work: &work,
        started, budget, limited: &limited, rejected, unsupported, recommendation: &recommendation, cached_recommendation,
        baseline_eval, actual_eval: &actual_eval, route: route.as_ref().map(|value| &value.report),
    })?;
    Ok(result)
}

fn optimize_actual(card_center: &Evaluator<'_>, card_integrated: &Evaluator<'_>, search: &Search<'_>,
    baseline: &Evaluation, metric: Metric<'_>, ultra: bool, route_sensitive: bool) -> SimResult<Evaluation> {
    let mut actual = optimize(card_center, card_integrated, search, metric, ultra, route_sensitive, false)?;
    let floors = subsets(&baseline.cards, &card_center, search);
    let mut best_floor: Option<Evaluation> = None;
    for cards in &floors {
        let start = if ultra && search.element().is_none() {
            card_center.fixed(cards, Some(cards.len()), metric, None)?
        } else { card_center.evaluate(cards, metric, search.element(), card_center.fixed_exilus)? };
        if let Some(mut value) = start {
            if ultra {
                value = card_center.evaluate(&value.cards, metric, search.element(), card_center.fixed_exilus)?.unwrap_or(value);
            }
            if !card_integrated.replay_options.nodes.is_empty() {
                value = card_integrated.fixed(&value.cards, value.riven_slot, metric, search.element())?.unwrap_or(value);
            }
            if best_floor.as_ref().is_none_or(|best| value.score > best.score) { best_floor = Some(value); }
        }
    }
    if let Some(floor) = best_floor.filter(|floor| floor.score > actual.score) { actual = floor; }
    Ok(actual)
}

fn optimize(center: &Evaluator<'_>, integrated: &Evaluator<'_>, search: &Search<'_>, metric: Metric<'_>,
    ultra: bool, route_sensitive: bool, provisional: bool) -> SimResult<Evaluation> {
    let candidates = search.analysis_candidates(center, metric, ultra, route_sensitive, provisional)?;
    let mut best: Option<Evaluation> = None;
    let mut seen = HashSet::new();
    for row in candidates {
        if !seen.insert((row.cards.clone(), row.riven_slot)) { continue; }
        let row = if integrated.replay_options.nodes.is_empty() { row } else {
            integrated.fixed(&row.cards, row.riven_slot, metric, None)?.unwrap_or(row)
        };
        if best.as_ref().is_none_or(|value| row.score > value.score) { best = Some(row); }
    }
    best.ok_or_else(|| "no finalists to score".into())
}

fn hardness(eval: &Evaluator<'_>, baseline: &Evaluation) -> SimResult<Vec<HardnessNode>> {
    let log_max = 20.0_f64.ln();
    let nodes: Vec<_> = [(-0.6_f64.sqrt(), 5.0 / 18.0), (0.0, 8.0 / 18.0), (0.6_f64.sqrt(), 5.0 / 18.0)]
        .into_iter().map(|(x, weight)| ((log_max / 2.0 + x * (log_max / 2.0)).exp(), weight)).collect();
    let curve = eval.life_curve(baseline, nodes[2].0 + 2.0)?;
    let health = num(&curve["targetTotalEhp"]) / 1e9;
    let mut events: Vec<_> = rows(&curve["curve"]).iter().map(|row| (num(&row[0]), num(&row[1]), num(&row[2]))).collect();
    events.sort_by(|a, b| a.0.total_cmp(&b.0));
    let Some(first) = events.first().map(|row| row.0).filter(|_| health > 0.0) else { return Ok(Vec::new()); };
    let result: Vec<_> = nodes.into_iter().map(|(seconds, weight)| {
        let end = events.partition_point(|row| row.0 - first <= seconds + 1e-9);
        let damage = wfcore_lib::pymath::psum(events[..end].iter().map(|row| row.1));
        let variance = wfcore_lib::pymath::psum(events[..end].iter().map(|row| row.2));
        HardnessNode { seconds: round(seconds, 3), weight, scale: damage / health,
            spread: if damage > 0.0 { (variance.sqrt() / damage).clamp(0.0, 0.5) } else { 0.0 } }
    }).collect();
    Ok(if result.iter().all(|node| node.scale.is_finite() && node.scale > 0.0) { result } else { Vec::new() })
}

fn subsets(cards: &[usize], eval: &Evaluator<'_>, search: &Search<'_>) -> Vec<Vec<usize>> {
    let mut options = Vec::new();
    let mut max = 0;
    for mask in (0usize..1 << cards.len()).rev() {
        let count = mask.count_ones() as usize;
        if count < max { continue; }
        let kept: Vec<_> = cards.iter().enumerate().filter(|(index, _)| mask & (1 << (cards.len() - 1 - index)) != 0)
            .map(|(_, card)| *card).collect();
        if !search.required.iter().all(|i| kept.contains(i)) || !search.in_bounds(eval, &kept) { continue; }
        if count > max { max = count; options.clear(); }
        options.push(kept);
    }
    options
}

fn warm_starts(eval: &Evaluator<'_>, search: &Search<'_>, baseline: &Evaluation, actual: &Evaluation,
    cached: &[Vec<usize>], metric: Metric<'_>) -> SimResult<Vec<Evaluation>> {
    let layers: Vec<_> = [&baseline.cards, &actual.cards].into_iter().chain(cached)
        .map(|seed| subsets(seed, eval, search)).collect();
    let mut seen = HashSet::new();
    let mut values = Vec::new();
    for index in 0..layers.iter().map(Vec::len).max().unwrap_or(0) {
        for cards in layers.iter().filter_map(|layer| layer.get(index)) {
            if seen.len() >= 64 { break; }
            if !seen.insert(cards) { continue; }
            if let Some(value) = eval.evaluate(cards, metric, search.element(), eval.fixed_exilus)? { values.push(value); }
        }
    }
    values.sort_by(|a, b| b.score.total_cmp(&a.score));
    Ok(values)
}

fn warm_refine(eval: &Evaluator<'_>, search: &Search<'_>, mut best: Evaluation,
    metric: Metric<'_>, witnesses: &[usize]) -> SimResult<Evaluation> {
    let witness: HashSet<_> = witnesses.iter().copied().collect();
    let families: HashSet<_> = witness.iter().map(|&index| &eval.cards[index].family).collect();
    let mut pool: Vec<_> = (0..eval.cards.len()).filter(|index| !search.forbidden.contains(index)).collect();
    pool.sort_by_key(|index| (!witness.contains(index), !families.contains(&eval.cards[*index].family), *index));
    let mut seen = HashSet::from([best.cards.clone()]);
    let mut count = 0;
    let deadline = eval.replay_options.phase_deadline("ultra");
    for _ in 0..2 {
        let base = best.cards.clone();
        let before = best.score;
        let mut buckets = Vec::new();
        for (slot, &old) in base.iter().enumerate() {
            if search.required.contains(&old) { continue; }
            let mut drop = base.clone();
            drop.remove(slot);
            let mut proposals = vec![drop];
            for &index in &pool {
                if base.contains(&index) || base.iter().enumerate().any(|(i, &card)| {
                    i != slot && eval.cards[card].family == eval.cards[index].family
                }) { continue; }
                let mut proposal = base.clone();
                proposal[slot] = index;
                proposals.push(proposal);
            }
            buckets.push(proposals);
        }
        if base.len() + usize::from(eval.riven.is_some()) < eval.req.slot_count {
            buckets.push(pool.iter().filter(|index| !base.contains(index)
                && !base.iter().any(|card| eval.cards[*card].family == eval.cards[**index].family))
                .map(|index| base.iter().copied().chain([*index]).collect()).collect());
        }
        for index in 0..buckets.iter().map(Vec::len).max().unwrap_or(0) {
            for proposal in buckets.iter().filter_map(|bucket| bucket.get(index)) {
                if count >= 64 || eval.replay_options.expired(deadline) { return Ok(best); }
                if !search.in_bounds(eval, proposal) || !seen.insert(proposal.clone()) { continue; }
                count += 1;
                if let Some(value) = eval.evaluate(proposal, metric, search.element(), eval.fixed_exilus)? {
                    if value.score > best.score + 1e-9 { best = value; }
                }
            }
        }
        if best.score == before { break; }
    }
    Ok(best)
}

fn reverse_floor(center: &Evaluator<'_>, integrated: &Evaluator<'_>, actual: &Evaluation,
    metric: Metric<'_>) -> SimResult<Option<Evaluation>> {
    let mut panel = Vec::new();
    for (index, card) in center.cards.iter().enumerate() {
        if actual.cards.iter().any(|&i| center.cards[i].family == card.family) { continue; }
        let cards: Vec<_> = actual.cards.iter().copied().chain([index]).collect();
        if let Some(value) = center.fixed(&cards, None, Metric::Proxy, None)? {
            panel.push((value.score, index, cards));
        }
    }
    panel.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)));
    let mut best: Option<Evaluation> = None;
    for cards in std::iter::once(actual.cards.as_slice()).chain(panel.iter().take(6).map(|value| value.2.as_slice())) {
        if let Some(value) = center.fixed(cards, None, metric, None)? {
            if best.as_ref().is_none_or(|prior| value.score > prior.score) { best = Some(value); }
        }
    }
    if let Some(value) = &best { return integrated.fixed(&value.cards, None, metric, None); }
    Ok(None)
}
