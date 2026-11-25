use super::{context, json, num, round, rows, text, truth, Evaluator, HardnessNode, Input, Search, Value, CALIBRATION_VERSION};
use super::recommendation::Recommendation;
use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;

#[derive(Default)]
pub(crate) struct Cache {
    pub(super) solve_gate: Mutex<()>,
    pub(super) results: Mutex<Entries<Value>>,
    pub(super) statics: Mutex<Entries<Statics>>,
}

#[derive(Clone)]
pub(super) struct Statics {
    pub cards: Vec<usize>,
    pub baseline_score: f64,
    pub nodes: Vec<HardnessNode>,
    pub archetypes: Vec<Vec<usize>>,
    pub recommendations: HashMap<usize, Recommendation>,
}

pub(super) struct Entries<T>(VecDeque<(String, T)>);

impl<T> Default for Entries<T> {
    fn default() -> Self { Self(VecDeque::new()) }
}

impl<T: Clone> Entries<T> {
    pub fn get(&mut self, key: &str) -> Option<T> {
        let index = self.0.iter().position(|(stored, _)| stored == key)?;
        let entry = self.0.remove(index)?;
        let value = entry.1.clone();
        self.0.push_back(entry);
        Some(value)
    }

    pub fn insert(&mut self, key: String, value: T, limit: usize) {
        if let Some(index) = self.0.iter().position(|(stored, _)| stored == &key) { self.0.remove(index); }
        self.0.push_back((key, value));
        if self.0.len() > limit { self.0.pop_front(); }
    }
}

pub(super) fn key(eval: &Evaluator<'_>, search: &Search<'_>, input: &Input,
    profile: &str, metric: &str) -> Option<String> {
    if input.fields.get("arcanes").is_some_and(|v| !v.is_null())
        || input.fields.get("exilus").is_some_and(|v| !v.is_null())
        || input.fields.get("timelineSeconds").is_some_and(|v| !v.is_null())
        || eval.req.target["path"] == "__custom_enemy__"
        || !search.scope.required.is_empty() || !search.scope.forbidden.is_empty()
        || search.scope.min.is_some() || search.scope.max.is_some()
        || input.fields.get("specialSlotPolarities").is_some_and(|v| !v.is_null())
        || ["innateElement", "innateElementValue", "progenitorElement", "progenitorBonus"].iter()
            .any(|key| input.fields.get(*key).is_some_and(|v| !v.is_null() && v != "")) {
        return None;
    }
    let req = eval.req;
    let mut scenario = context::scenario(req, eval.stance);
    if scenario["combatState"]["gaugeChargingHeadshots"] == false {
        scenario["combatState"].as_object_mut()?.remove("gaugeChargingHeadshots");
    }
    let state = scenario["combatState"].as_object()?;
    let non_default = req.scenario.aiming || req.scenario.cycle || req.scenario.heavy || req.scenario.cloaked
        || req.scenario.spread || req.scenario.combo != 0 || req.scenario.policy != "emergent"
        || req.scenario.distance.is_some() || req.scenario.nearest.is_some()
        || num(&scenario["detonationDelaySeconds"]) != 0.0 || scenario["detonationBatchSize"] != 1
        || state.keys().any(|key| !key.starts_with("melee"));
    if non_default && !input.fields.get("_cacheScenario").is_some_and(truth) { return None; }
    if !req.choices.is_null() && !input.fields.get("_cacheIncarnon").is_some_and(truth) { return None; }
    let target = &input.requested_target;
    let get = |key: &str, default: Value| target.get(key).cloned().unwrap_or(default);
    let mut benchmark = json!({"enemy":target["enemy"], "level":target["level"], "steelPath":req.steel,
        "eximus":truth(&target["eximus"]), "archimedea":truth(&target["archimedea"]), "empowered":truth(&target["empowered"]),
        "armourStrip":get("armourStrip",json!(0.0)), "elementalArmourStrip":get("elementalArmourStrip",json!(0.0)),
        "headshotChance":get("headshotChance",json!(0.0)), "hitLocation":"body",
        "stanceMultiplier":eval.stance["dpsMultiplier"].as_f64().unwrap_or(1.0),
        "statusRateMultiplier":eval.stance["statusRateMultiplier"].as_f64().unwrap_or(1.0),
        "heavyAttack":req.scenario.heavy, "windowSeconds":get("windowSeconds",json!(30.0)),
        "enemyEnhancements":target["enemyEnhancements"], "playerCount":get("playerCount",json!(1)),
        "eventScoringVersion":"loadout-events-v1"});
    if non_default { benchmark["scenario"] = scenario; }
    if let Some(schedule) = eval.schedule { benchmark["melee"] = schedule.context(); }
    if !req.choices.is_null() { benchmark["incarnon"] = req.choices.clone(); }
    let mut path = text(&req.weapon["path"]).to_owned();
    let mut parts: Vec<_> = rows(&req.weapon["kitgunParts"]).iter().chain(rows(&req.weapon["zawParts"])).map(text).collect();
    if !parts.is_empty() {
        parts.sort_unstable();
        path.push('#');
        path.push_str(&parts.join("+"));
    }
    let mut key = String::new();
    context::canonical_json(&json!({"weapon":path, "fireMode":req.mode, "innateElements":rows(&req.weapon["innateElements"]),
        "elementTarget":input.fields.get("elementTarget").and_then(Value::as_str).filter(|s| !s.is_empty()).unwrap_or("-"),
        "profile":profile, "disposition":round(input.disposition,8), "capacityLimit":eval.capacity_limit,
        "metric":metric, "calibrationVersion":CALIBRATION_VERSION, "benchmark":benchmark}), &mut key);
    Some(key)
}
