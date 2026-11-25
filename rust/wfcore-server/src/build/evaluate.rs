use super::{arcane, cards, damage, events, loadout, melee, num, rows, text, timeline, weapon};
use super::{json, EngineData, Map, Request, SimResult, Target, Value};
use super::cards::{Card, Riven};
use super::super::target::Defenses;
use super::weapon::{Effect, Evolution, Weapon};
use indexmap::IndexMap;
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};
use wfcore_lib::{fx::FxHashMap, mods::ModdedFields, replay::Types, tables};
use wfcore_lib::sustained::{sustained_status, SustainedInput};

#[derive(Clone, Copy)]
pub(super) enum Metric<'a> {
    Events,
    WindowDps,
    Proxy,
    Panel(&'a str),
}

#[derive(Clone, Copy)]
pub(super) struct HardnessNode {
    pub seconds: f64,
    pub scale: f64,
    pub spread: f64,
    pub weight: f64,
}

#[derive(Clone, Copy)]
pub(super) struct ReplayOptions<'a> {
    pub scale: Option<f64>,
    pub nodes: &'a [HardnessNode],
    pub branches: usize,
    pub evaluations: Option<&'a Cell<usize>>,
    pub deadline: Option<Instant>,
    pub limited: Option<&'a Cell<usize>>,
    pub memo: Option<&'a super::search::BeamMemo>,
}

impl Default for ReplayOptions<'_> {
    fn default() -> Self {
        Self { scale: None, nodes: &[], branches: 16, evaluations: None, deadline: None, limited: None, memo: None }
    }
}

impl ReplayOptions<'_> {
    pub fn phase_deadline(self, profile: &str) -> Option<Instant> {
        let seconds = match profile { "ultra" => 5, "fast" => 8, "deep" => 45, _ => 15 };
        self.deadline.map(|deadline| deadline.min(Instant::now() + Duration::from_secs(seconds)))
    }

    pub fn expired(self, deadline: Option<Instant>) -> bool {
        if !deadline.is_some_and(|deadline| Instant::now() >= deadline) { return false; }
        if let Some(limited) = self.limited { limited.set(limited.get() + 1); }
        true
    }
}

#[derive(Clone)]
pub(super) struct Evaluation {
    pub cards: Vec<usize>,
    pub riven_slot: Option<usize>,
    pub score: f64,
    pub capacity: i64,
}

pub(super) struct Panel {
    pub fields: ModdedFields,
    pub heavy: Value,
    pub wire: Value,
    pub count: usize,
}

type LayoutKey = ([usize; 8], Option<usize>);

#[derive(Default)]
pub(super) struct EventScores(RefCell<FxHashMap<LayoutKey, f64>>);

#[derive(Clone)]
pub(super) struct Evaluator<'a> {
    pub data: &'a EngineData,
    pub req: &'a Request<'a>,
    pub stance: &'a Value,
    pub schedule: Option<&'a melee::Schedule>,
    pub target: &'a Target<'a>,
    pub replay_defenses: &'a Defenses,
    pub proxy_defenses: &'a Defenses,
    pub weapon: &'a Weapon<'a>,
    pub infinite_conditions: &'a Value,
    pub cards: &'a [Card<'a>],
    pub riven: Option<&'a Riven>,
    pub fixed_exilus: Option<&'a Card<'a>>,
    pub arcanes: &'a arcane::Equipped<'a>,
    pub arcane_panel: &'a arcane::Snapshot,
    pub evolution: &'a Evolution,
    pub external: &'a [Effect],
    pub capacity_limit: i64,
    pub shells: &'a HashMap<String, &'a Value>,
    pub replay_options: ReplayOptions<'a>,
    pub event_scores: Option<&'a EventScores>,
}

impl Evaluator<'_> {
    pub fn window(&self, selected: &[usize], riven_slot: Option<usize>, special: Option<&Card<'_>>) -> SimResult<Value> {
        let (effects, skipped) = self.terms(selected, riven_slot, special, true);
        let (fields, _, _) = self.panel_fields(self.req, &effects, &skipped, None)?;
        self.replay(selected, special, effects, &fields, timeline::Output::Timeline).map(timeline::ReplayOutput::details)
    }

    pub fn life_curve(&self, best: &Evaluation, seconds: f64) -> SimResult<Value> {
        let req = Request { aim: 0.0, window: seconds,
            ..self.req.clone() };
        let eval = Evaluator { req: &req, replay_options: ReplayOptions {
            scale: Some(1e9), nodes: &[], ..self.replay_options
        }, ..self.clone() };
        let (effects, skipped) = eval.terms(&best.cards, best.riven_slot, eval.fixed_exilus, true);
        let (fields, _, _) = eval.panel_fields(&req, &effects, &skipped, None)?;
        eval.replay(&best.cards, eval.fixed_exilus, effects, &fields, timeline::Output::LifeCurve).map(timeline::ReplayOutput::details)
    }

    fn replay(&self, selected: &[usize], special: Option<&Card<'_>>, terms: Vec<Effect>,
        fields: &ModdedFields, output: timeline::Output) -> SimResult<timeline::ReplayOutput> {
        let req = self.req;
        let weapon = self.weapon.with_request(req);
        let installed: Vec<_> = selected.iter().map(|i| &self.cards[*i]).chain(special)
            .map(|card| loadout::Card::installed(card.raw, card.rank)).collect();
        let effects = events::Effects::prepare(req, &installed, self.arcanes, terms, self.evolution,
            self.schedule.map(|s| s.route.as_str()), self.infinite_conditions, weapon.innate);
        let replay = |scale| {
            timeline::replay_window(&weapon, self.evolution, fields, req, self.target, req.level,
                req.steel, &req.enhancements, self.replay_defenses, scale, &effects, self.schedule, self.arcanes,
                self.stance["statusRateMultiplier"].as_f64().unwrap_or(1.0),
                self.stance["dpsMultiplier"].as_f64().unwrap_or(1.0), output, self.replay_options.branches)
        };
        if self.replay_options.nodes.is_empty() { return replay(self.replay_options.scale); }
        let mut weights = 0.0;
        let mut total = 0.0;
        let mut any = false;
        for node in self.replay_options.nodes {
            let low = replay(Some(node.scale * (1.0 - node.spread)))?.score();
            let value = if node.spread > 0.0 {
                (low + replay(Some(node.scale * (1.0 + node.spread)))?.score()) / 2.0
            } else { low };
            any |= value > 0.0;
            total += node.weight * value.max(1e-12).ln();
            weights += node.weight;
        }
        Ok(timeline::ReplayOutput::from_score(if any { (total / weights).exp() } else { 0.0 }))
    }

    pub fn capacity(&self, cards: &[usize]) -> i64 {
        cards.iter().map(|i| self.cards[*i].cost()).sum::<i64>()
            + self.riven.map_or(0, |r| cards::matching_drain(r.drain(), &r.polarity))
    }

    pub fn can_equip(&self, selected: &[usize]) -> bool {
        self.fits(selected) && selected.iter().enumerate().all(|(i, key)| {
                !selected[..i].iter().any(|other| self.cards[*key].family == self.cards[*other].family)
            })
    }

    fn fits(&self, selected: &[usize]) -> bool {
        selected.len() + usize::from(self.riven.is_some()) <= self.req.slot_count
            && self.capacity(selected) <= self.capacity_limit
    }

    pub fn set_counts<'a>(&'a self, selected: &[usize], special: Option<&'a Card<'_>>) -> IndexMap<&'a str, usize> {
        let mut counts = IndexMap::new();
        for card in selected.iter().map(|i| &self.cards[*i]).chain(special) {
            if !card.set.is_empty() { *counts.entry(card.set.as_str()).or_insert(0) += 1; }
        }
        counts
    }

    pub fn multiplier(&self, card: &Card<'_>, pieces: usize) -> f64 {
        let values = rows(&card.raw["modSetValues"]);
        if pieces < 2 || values.is_empty() { 1.0 } else { 1.0 + num(&values[(pieces - 2).min(values.len() - 1)]) }
    }

    pub fn terms(&self, selected: &[usize], riven_slot: Option<usize>, special: Option<&Card<'_>>,
        arcane: bool) -> (Vec<Effect>, Vec<String>) {
        let counts = self.set_counts(selected, special);
        let mut effects = Vec::new();
        let mut skipped = Vec::new();
        for (slot, card) in selected.iter().map(|i| Some(&self.cards[*i])).chain([None]).enumerate() {
            if riven_slot == Some(slot) {
                if let Some(riven) = self.riven {
                    effects.extend_from_slice(&riven.effects);
                    skipped.extend_from_slice(&riven.skipped);
                }
            }
            if let Some(card) = card.or(special) {
                let scale = self.multiplier(card, counts.get(card.set.as_str()).copied().unwrap_or(0));
                effects.extend(card.effects.iter().cloned().map(|mut e| { e.value *= scale; e }));
                skipped.extend_from_slice(&card.skipped);
            }
        }
        effects.extend_from_slice(&self.evolution.effects);
        if arcane {
            effects.extend_from_slice(&self.arcane_panel.effects);
            skipped.extend_from_slice(&self.arcane_panel.skipped);
            effects.extend_from_slice(self.external);
        }
        for (set, pieces) in counts {
            let Some(shell) = self.shells.get(set) else { continue; };
            let pieces = pieces.min(shell["setPieces"].as_u64().unwrap_or(u64::MAX) as usize);
            if shell["buffSet"] != true {
                effects.extend(loadout::expand(shell, 0, self.req, &Map::new(), &mut skipped)
                    .into_iter().map(|mut effect| { effect.value *= pieces as f64; effect }));
            } else if super::truth(&shell["runtimeScripts"]) {
                skipped.push(format!("{}: unsupported[set-shell-runtime-script]", text(&shell["path"])));
            }
        }
        (effects, skipped)
    }

    fn panel_fields(&self, req: &Request<'_>, effects: &[Effect], skipped: &[String],
        count: Option<usize>) -> SimResult<(ModdedFields, Value, usize)> {
        let weapon = if req.exported_mode_panel { Weapon::new(self.data, req) } else { self.weapon.with_request(req) };
        self.reduce_panel(&weapon, effects, skipped, count, true)
    }

    fn reduce_panel(&self, weapon: &Weapon<'_>, effects: &[Effect], skipped: &[String],
        count: Option<usize>, metadata: bool) -> SimResult<(ModdedFields, Value, usize)> {
        let reduce = |count| weapon.panel_for_score(effects, count, skipped, metadata);
        let (fields, heavy) = reduce(count.unwrap_or(0) as f64)?;
        if let Some(count) = count { return Ok((fields, heavy, count)); }
        if !metadata && !weapon.uses_status_count(effects) { return Ok((fields, heavy, 0)); }
        let mut types = Types::new();
        let core = weapon::core(&fields, &mut types, self.target.faction, self.target.bracket);
        let count = tables::expected_status_procs(&core, &Vec::new(), false).iter().filter(|(_, v)| *v > 0.0).count();
        if count == 0 { return Ok((fields, heavy, count)); }
        let (fields, heavy) = reduce(count as f64)?;
        Ok((fields, heavy, count))
    }

    pub fn panel(&self, req: &Request<'_>, effects: &[Effect], skipped: &[String], count: Option<usize>) -> SimResult<Panel> {
        let (fields, heavy, count) = self.panel_fields(req, effects, skipped, count)?;
        let mut types = Types::new();
        let core = weapon::core(&fields, &mut types, self.target.faction, self.target.bracket);
        let wire = panel_wire(&fields, &core, &types);
        Ok(Panel { fields, heavy, wire, count })
    }

    pub fn has_element(&self, selected: &[usize], riven_slot: Option<usize>, element: &str) -> SimResult<bool> {
        let (effects, skipped) = self.terms(selected, riven_slot, self.fixed_exilus, true);
        let (fields, _, _) = self.reduce_panel(self.weapon, &effects, &skipped, None, false)?;
        let amount = damage_amount(&fields, element);
        Ok(amount.is_finite() && amount > 0.0)
    }

    pub fn alternate_panel(&self, req: &Request<'_>, effects: &[Effect], skipped: &[String]) -> SimResult<Panel> {
        self.panel(&Request { exported_mode_panel: true, ..req.clone() }, effects, skipped, Some(0))
    }

    pub fn score(&self, selected: &[usize], riven_slot: Option<usize>, special: Option<&Card<'_>>, metric: Metric<'_>,
        element: Option<&str>) -> SimResult<Option<f64>> {
        // A matrix search installs this cache only for a fixed request, Riven
        // and exilus. The ordered layout still controls elemental fusion.
        if let Some(cache) = self.event_scores.filter(|_| {
            matches!(metric, Metric::Events) && element.is_none()
                && special.map(std::ptr::from_ref) == self.fixed_exilus.map(std::ptr::from_ref)
        }) {
            let mut layout = [usize::MAX; 8];
            layout[..selected.len()].copy_from_slice(selected);
            let key = (layout, riven_slot);
            if let Some(&score) = cache.0.borrow().get(&key) { return Ok(Some(score)); }
            let score = self.score_uncached(selected, riven_slot, special, metric, element)?;
            if let Some(score) = score {
                let mut scores = cache.0.borrow_mut();
                if scores.len() < 8192 { scores.insert(key, score); }
            }
            return Ok(score);
        }
        self.score_uncached(selected, riven_slot, special, metric, element)
    }

    fn score_uncached(&self, selected: &[usize], riven_slot: Option<usize>, special: Option<&Card<'_>>,
        metric: Metric<'_>, element: Option<&str>) -> SimResult<Option<f64>> {
        let req = self.req;
        let weapon = self.weapon.with_request(req);
        let (effects, skipped) = self.terms(selected, riven_slot, special, true);
        let (fields, heavy, count) = self.reduce_panel(&weapon, &effects, &skipped, None, matches!(metric, Metric::Panel(_)))?;
        if let Some(name) = element {
            let amount = damage_amount(&fields, name);
            if amount.is_nan() || amount <= 0.0 { return Ok(None); }
        }
        let stance_damage = self.stance["dpsMultiplier"].as_f64().unwrap_or(1.0);
        let stance_status = self.stance["statusRateMultiplier"].as_f64().unwrap_or(1.0);
        let score = match metric {
            Metric::Events | Metric::WindowDps => self.replay(selected, special, effects, &fields,
                if matches!(metric, Metric::Events) { timeline::Output::Kpm } else { timeline::Output::Dps })?.score(),
            Metric::Proxy => {
                let mut types = Types::new();
                let core = weapon::core(&fields, &mut types, self.target.faction, self.target.bracket);
                let defenses = self.proxy_defenses;
                let hit = if req.body.get("hitLocation").is_some_and(|v| v == "head") { self.target.head }
                    else { super::super::target::HitLocation::BODY };
                if self.replay_options.evaluations.is_some() {
                    let mut target = self.target.sustained(defenses, hit);
                    target.shielded = super::truth(&self.target.record["shield"]);
                    sustained_status(&target, SustainedInput { types: &types, core: &core,
                        attacks_per_second: weapon::attack_rate(&fields), status_rate_multiplier: stance_status,
                        damage_rate_multiplier: stance_damage, total: true,
                        magazine: fields.magazine, first_shot_bonus: fields.first_shot_damage_bonus })
                } else {
                    let (_, steady) = damage::status_and_steady(&fields, &core, &types, self.target, defenses, hit,
                        &Vec::new(), stance_status, stance_damage);
                    num(&steady["totalDps"])
                }
            }
            Metric::Panel(key) => {
                let mut types = Types::new();
                let core = weapon::core(&fields, &mut types, self.target.faction, self.target.bracket);
                let wire = panel_wire(&fields, &core, &types);
                let projectiles = super::projectiles::preview(&weapon, req, &effects, &skipped, count)?;
                num(&super::projectiles::metrics(&wire, &heavy, &projectiles)[key])
            }
        };
        Ok(Some(score))
    }

    pub fn evaluate(&self, selected: &[usize], metric: Metric<'_>, element: Option<&str>,
        special: Option<&Card<'_>>) -> SimResult<Option<Evaluation>> {
        self.evaluate_layouts(selected, metric, element, special, true)
    }

    pub fn rank(&self, selected: &[usize], metric: Metric<'_>) -> SimResult<Option<Evaluation>> {
        self.evaluate_layouts(selected, metric, None, self.fixed_exilus, self.replay_options.evaluations.is_none())
    }

    fn evaluate_layouts(&self, selected: &[usize], metric: Metric<'_>, element: Option<&str>,
        special: Option<&Card<'_>>, check_families: bool) -> SimResult<Option<Evaluation>> {
        if let Some(count) = self.replay_options.evaluations { count.set(count.get() + 1); }
        if !(if check_families { self.can_equip(selected) } else { self.fits(selected) }) { return Ok(None); }
        let mut best: Option<Evaluation> = None;
        let riven_element = self.riven.map_or(0, |r| cards::elemental(&r.effects));
        let slots: Vec<_> = selected.iter().enumerate().filter_map(|(i, card)|
            (self.cards[*card].elemental != 0).then_some(i)).collect();
        let mut positions = Vec::new();
        if riven_element != 0 { positions.extend(slots.iter().copied().map(Some)); }
        positions.push(self.riven.map(|_| selected.len()));
        let mut seen = HashSet::new();
        let elements = |order: &[usize], position| {
            let mut terms = Vec::with_capacity(4);
            for (slot, card) in order.iter().map(|i| Some(*i)).chain([None]).enumerate() {
                if position == Some(slot) {
                    if let Some(riven) = self.riven { append_elements(&riven.effects, &mut terms); }
                }
                if let Some(card) = card { append_elements(&self.cards[card].effects, &mut terms); }
            }
            (terms.iter().take(2).fold(0u8, |a, b| a | *b), terms.iter().skip(2).fold(0u8, |a, b| a | *b))
        };
        for position in positions {
            let mut try_order = |order: &[usize]| -> SimResult<()> {
                if !seen.insert(elements(order, position)) { return Ok(()); }
                if let Some(score) = self.score(order, position, special, metric, element)? {
                    if best.as_ref().is_none_or(|value| score > value.score) {
                        best = Some(Evaluation { cards: order.to_vec(), riven_slot: position, score, capacity: self.capacity(order) });
                    }
                }
                Ok(())
            };
            try_order(selected)?;
            if slots.len() >= if position.is_some() { 2 } else { 3 } && elements(selected, position).1 != 0 {
                let mut permutation: Vec<_> = (0..slots.len()).collect();
                let mut order = selected.to_vec();
                while next_permutation(&mut permutation) {
                    for (slot, index) in slots.iter().zip(&permutation) { order[*slot] = selected[slots[*index]]; }
                    try_order(&order)?;
                }
            }
        }
        Ok(best)
    }

    pub fn screen(&self, selected: &[usize], metric: Metric<'_>) -> SimResult<Option<f64>> {
        self.evaluate(selected, metric, None, self.fixed_exilus).map(|value| value.map(|v| v.score))
    }

    pub fn fixed(&self, selected: &[usize], slot: Option<usize>, metric: Metric<'_>,
        element: Option<&str>) -> SimResult<Option<Evaluation>> {
        if let Some(count) = self.replay_options.evaluations { count.set(count.get() + 1); }
        if !self.can_equip(selected) { return Ok(None); }
        Ok(self.score(selected, slot, self.fixed_exilus, metric, element)?.map(|score| Evaluation {
            cards: selected.to_vec(), riven_slot: slot, score, capacity: self.capacity(selected)
        }))
    }
}

fn damage_amount(fields: &ModdedFields, name: &str) -> f64 {
    fields.damage_types.0.iter().find_map(|(id, amount)| {
        (wfcore_lib::mods::name(*id) == name).then_some(*amount)
    }).unwrap_or(0.0)
}

fn append_elements(effects: &[Effect], elements: &mut Vec<u8>) {
    for effect in effects.iter().filter(|effect| effect.value > 0.0) {
        if let Some(index) = cards::ELEMENTS.iter().position(|key| effect.dtype.as_deref() == Some(key)) {
            let bit = 1 << index;
            if !elements.contains(&bit) { elements.push(bit); }
        }
    }
}

fn next_permutation(values: &mut [usize]) -> bool {
    let Some(index) = values.windows(2).rposition(|row| row[0] < row[1]) else { return false; };
    let after = values.iter().rposition(|v| *v > values[index]).unwrap();
    values.swap(index, after);
    values[index + 1..].reverse();
    true
}

pub(super) fn metrics(panel: &Value, heavy: &Value) -> Value {
    let average = num(&panel["averageHit"]);
    let attack = average * num(&panel["fireRate"]);
    let heavy = heavy["heavy_damage"].as_f64().unwrap_or(attack);
    json!({"shotDamage": average * num(&panel["multishot"]), "burstDps": panel["burstDps"],
        "sustainedDps": panel["sustainedDps"], "attackDps": attack * num(&panel["multishot"]),
        "heavyDamage": heavy, "heavyAttackDps": heavy})
}

pub(super) fn panel_wire(fields: &ModdedFields, core: &tables::Core, types: &Types) -> Value {
    let mut wire = damage::panel(fields, core, types);
    let damage = core.total_damage();
    let crit = tables::effective_hit_multiplier(core.crit_chance, core.crit_multiplier,
        core.noncrit_damage_multiplier, 1.0, core.double_crit_chance);
    wire["totalDamage"] = json!(damage);
    wire["averageHit"] = json!(damage * crit * core.falloff_multiplier);
    wire
}
