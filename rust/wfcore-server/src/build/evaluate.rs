use super::{arcane, cards, damage, events, loadout, melee, num, rows, text, timeline, weapon};
use super::{json, EngineData, Map, Request, SimResult, Target, Value};
use super::cards::{Card, Riven};
use super::weapon::{Effect, Evolution, Weapon};
use indexmap::IndexMap;
use std::collections::{HashMap, HashSet};
use wfcore_lib::{mods::ModdedFields, replay::Types, tables};

#[derive(Clone, Copy)]
pub(super) enum Metric<'a> {
    Events,
    Proxy,
    Panel(&'a str),
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

#[derive(Clone)]

pub(super) struct Evaluator<'a> {
    pub data: &'a EngineData,
    pub req: &'a Request<'a>,
    pub stance: &'a Value,
    pub schedule: Option<&'a melee::Schedule>,
    pub target: Target<'a>,
    pub cards: &'a [Card<'a>],
    pub riven: Option<&'a Riven>,
    pub fixed_exilus: Option<&'a Card<'a>>,
    pub arcanes: &'a arcane::Equipped<'a>,
    pub arcane_panel: &'a arcane::Snapshot,
    pub evolution: &'a Evolution,
    pub external: &'a [Effect],
    pub capacity_limit: i64,
    pub shells: HashMap<String, &'a Value>,
}

impl Evaluator<'_> {
    pub fn capacity(&self, cards: &[usize]) -> i64 {
        cards.iter().map(|i| self.cards[*i].cost()).sum::<i64>()
            + self.riven.map_or(0, |r| cards::matching_drain(18, &r.polarity))
    }

    pub fn can_equip(&self, selected: &[usize]) -> bool {
        selected.len() + usize::from(self.riven.is_some()) <= self.req.slot_count
            && self.capacity(selected) <= self.capacity_limit
            && selected.iter().enumerate().all(|(i, key)| {
                !selected[..i].iter().any(|other| self.cards[*key].family == self.cards[*other].family)
            })
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
        let weapon = Weapon::new(self.data, req);
        let (fields, heavy) = weapon.panel_fields(effects, count.unwrap_or(0) as f64, skipped)?;
        if let Some(count) = count { return Ok((fields, heavy, count)); }
        let mut types = Types::new();
        let core = weapon::core(&fields, &mut types, self.target.faction, self.target.bracket);
        let count = tables::expected_status_procs(&core, &Vec::new(), false).iter().filter(|(_, v)| *v > 0.0).count();
        if count == 0 { return Ok((fields, heavy, count)); }
        let (fields, heavy) = weapon.panel_fields(effects, count as f64, skipped)?;
        Ok((fields, heavy, count))
    }

    pub fn panel(&self, req: &Request<'_>, effects: &[Effect], skipped: &[String], count: Option<usize>) -> SimResult<Panel> {
        let (fields, heavy, count) = self.panel_fields(req, effects, skipped, count)?;
        let mut types = Types::new();
        let core = weapon::core(&fields, &mut types, self.target.faction, self.target.bracket);
        let wire = panel_wire(&fields, &core, &types);
        Ok(Panel { fields, heavy, wire, count })
    }

    pub fn alternate_panel(&self, req: &Request<'_>, effects: &[Effect], skipped: &[String]) -> SimResult<Panel> {
        self.panel(&Request { exported_mode_panel: true, ..req.clone() }, effects, skipped, Some(0))
    }

    pub fn score(&self, selected: &[usize], riven_slot: Option<usize>, special: Option<&Card<'_>>, metric: Metric<'_>,
        element: Option<&str>) -> SimResult<Option<f64>> {
        let req = self.req;
        let weapon = Weapon::new(self.data, req);
        let (effects, skipped) = self.terms(selected, riven_slot, special, true);
        let (fields, heavy, count) = self.panel_fields(req, &effects, &skipped, None)?;
        if element.is_some_and(|name| !fields.damage_types.0.iter().any(|(id, amount)| {
            *amount > 0.0 && wfcore_lib::mods::name(*id) == name
        })) { return Ok(None); }
        let stance_damage = self.stance["dpsMultiplier"].as_f64().unwrap_or(1.0);
        let stance_status = self.stance["statusRateMultiplier"].as_f64().unwrap_or(1.0);
        let score = match metric {
            Metric::Events => {
                let installed: Vec<_> = selected.iter().map(|i| &self.cards[*i]).chain(special)
                    .map(|card| loadout::Card::installed(card.raw, card.rank)).collect();
                let effects = events::Effects::prepare(req, &installed, self.arcanes, effects, self.evolution,
                    self.schedule.map(|s| s.route.as_str()), &self.data.mechanics["infinite-conditions"]["sources"],
                    rows(&weapon.mechanics("weapon-passives")["innate"]));
                num(&timeline::calculate(&weapon, self.evolution, &fields, req, &self.target, req.level,
                    req.steel, &req.enhancements, &effects, self.schedule, self.arcanes, stance_status, stance_damage, true)?)
            }
            Metric::Proxy => {
                let mut types = Types::new();
                let core = weapon::core(&fields, &mut types, self.target.faction, self.target.bracket);
                let defenses = self.target.defenses(req.level, req.steel, req.strip, req.elemental_strip, &req.enhancements);
                let hit = if req.body.get("hitLocation").is_some_and(|v| v == "head") { self.target.head }
                    else { super::super::target::HitLocation::BODY };
                let (_, steady) = damage::status_and_steady(&fields, &core, &types, &self.target, &defenses, hit,
                    &Vec::new(), stance_status, stance_damage);
                num(&steady["totalDps"])
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
        if !self.can_equip(selected) { return Ok(None); }
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
