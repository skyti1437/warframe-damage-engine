use super::{json, optimize, optimize_actual, rows, text, truth, Evaluation, Evaluator, Metric, Riven, Search, SimResult, Value};
use crate::simulate::{weapon::Evolution, Request};
use indexmap::IndexMap;
use std::collections::HashMap;
use std::time::Instant;

pub(super) struct Result {
    pub report: Value,
    pub adopted: Option<Adopted>,
}

pub(super) struct Adopted {
    pub evolution: Evolution,
    pub actual: Evaluation,
    pub baseline: Option<Evaluation>,
}

pub(super) fn screen(center: &Evaluator<'_>, integrated: &Evaluator<'_>, search: &Search<'_>,
    baseline: &Evaluation, actual: &Evaluation, riven: &Riven, metric: Metric<'_>, sensitive: bool)
    -> SimResult<Option<Result>> {
    let defaults = &center.evolution.choices;
    if !truth(defaults) { return Ok(None); }
    let mut tiers = Vec::new();
    for tier in rows(&center.req.weapon["incarnon"]["tiers"]) {
        let key = format!("tier{}", tier["tier"]);
        let Some(selected) = defaults[&key].as_str() else { continue; };
        let branches = rows(&tier["branches"]);
        let ids = |branch: &Value| branch["id"].as_str().or_else(|| branch["path"].as_str()).unwrap_or("").to_owned();
        if branches.len() < 2 || !branches.iter().any(|branch| ids(branch) == selected) { continue; }
        let options: Vec<_> = std::iter::once(selected.to_owned()).chain(branches.iter()
            .filter(|branch| ids(branch) != selected && !neutral(branch)).map(ids)).collect();
        if options.len() > 1 { tiers.push((key, options)); }
    }
    if tiers.is_empty() { return Ok(None); }
    let start = Instant::now();
    let fast_scope = crate::simulate::build::request::Scope { profile: "fast", ..search.scope.clone() };
    let fast = Search::new(center, &fast_scope)?;
    let solve = |choice: &Value, search: &Search<'_>, card: Option<&Riven>| -> SimResult<(Evolution, Evaluation)> {
        let req = Request { choices: choice, ..center.req.clone() };
        let evolution = Evolution::resolve(center.data, &req)?;
        let center = Evaluator { evolution: &evolution, riven: card, ..center.clone() };
        let integrated = Evaluator { evolution: &evolution, riven: card, ..integrated.clone() };
        let result = if card.is_some() {
            optimize_actual(&center, &integrated, search, baseline, metric, false, sensitive)?
        } else { optimize(&center, &integrated, search, metric, false, sensitive, false)? };
        Ok((evolution, result))
    };
    let mut screened = IndexMap::new();
    let mut probes = HashMap::new();
    let mut inert = Vec::new();
    let key = |choice: &Value| {
        let mut out = String::new();
        super::context::canonical_json(choice, &mut out);
        out
    };
    let default_key = key(defaults);
    screened.insert(default_key.clone(), (defaults.clone(), if search.scope.profile == "fast" {
        Some(actual.clone())
    } else { solve(defaults, &fast, Some(riven)).ok().map(|(_, value)| value) }));
    let mut current = defaults.clone();
    let mut current_key = default_key.clone();
    let score = |value: &Option<Evaluation>| value.as_ref().map_or(f64::NEG_INFINITY, |value| value.score);
    let mut current_score = score(&screened[&current_key].1);
    let mut probe = |choice: &Value, build: &Evaluation| -> SimResult<Option<f64>> {
        let key = (key(choice), build.cards.clone());
        if let Some(value) = probes.get(&key) { return Ok(*value); }
        let req = Request { choices: choice, ..center.req.clone() };
        let route = Evolution::resolve(center.data, &req)?;
        let eval = Evaluator { evolution: &route, riven: Some(riven), ..center.clone() };
        let value = eval.fixed(&build.cards, build.riven_slot, metric, None)?.map(|value| value.score);
        probes.insert(key, value);
        Ok(value)
    };
    for _ in 0..2 {
        let mut changed = false;
        for (tier, branches) in &tiers {
            for branch in branches {
                if current[tier] == *branch { continue; }
                let mut trial = current.clone();
                trial[tier] = json!(branch);
                let trial_key = key(&trial);
                if !screened.contains_key(&trial_key) {
                    if let Some(build) = &screened[&current_key].1 {
                        if let Some(before) = probe(&current, build)? {
                            if probe(&trial, build)?.is_some_and(|after| (after - before).abs() <= 1e-12 * before.abs().max(1.0)) {
                                inert.push(trial);
                                continue;
                            }
                        }
                    }
                    let evaluated = solve(&trial, &fast, Some(riven)).ok().map(|(_, value)| value);
                    screened.insert(trial_key.clone(), (trial.clone(), evaluated));
                }
                let value = score(&screened[&trial_key].1);
                if value > current_score * (1.0 + 1e-6) {
                    current_score = value;
                    current = trial;
                    current_key = trial_key;
                    changed = true;
                }
            }
        }
        if !changed { break; }
    }
    let mut adopted = None;
    if current_key != default_key {
        if let Some(mut picked) = screened[&current_key].1.clone() {
            if search.scope.profile != "fast" {
                if let Ok((_, value)) = solve(&current, search, Some(riven)) {
                    if value.score >= picked.score { picked = value; }
                }
            }
            if picked.score > actual.score * (1.0 + 1e-6) {
                let req = Request { choices: &current, ..center.req.clone() };
                let evolution = Evolution::resolve(center.data, &req)?;
                let baseline = solve(&current, search, None).ok().map(|(_, value)| value);
                adopted = Some(Adopted { evolution, actual:picked, baseline });
            }
        }
    }
    let chosen = adopted.as_ref().map_or(defaults, |selected| &selected.evolution.choices);
    let screened: Vec<_> = screened.values().map(|(route, value)| json!({"route":route,
        "score":value.as_ref().map(|v| v.score)})).collect();
    let report = json!({"defaultRoute":defaults, "chosenRoute":chosen, "adopted":adopted.is_some(),
        "screenProfile":"fast", "screened":screened, "inert":inert, "seconds":super::round(start.elapsed().as_secs_f64(), 2)});
    Ok(Some(Result { report, adopted }))
}

fn neutral(branch: &Value) -> bool {
    if truth(&branch["runtimeScripts"]) || truth(&branch["conditionalEffects"])
        || rows(&branch["sources"]).iter().any(|source| truth(&source["conditionalEffects"])) { return false; }
    rows(&branch["effects"]).iter().chain(rows(&branch["sources"]).iter().flat_map(|source| rows(&source["effects"])))
        .all(|effect| {
            let kind = text(&effect["upgradeType"]);
            matches!(kind, "NONE" | "WEAPON_RECOIL" | "WEAPON_ZOOM" | "WEAPON_AMMO_MAX" | "WEAPON_AUTO_RELOAD_RATE"
                | "WEAPON_ADD_CHARGE_COUNTER_MULTIPLIER" | "WEAPON_NOISE_REDUCTION")
                || kind.starts_with("AVATAR_") || kind.starts_with("SKILL_")
        })
}
