use super::{num, SimResult};
use super::evaluate::{Evaluation, Evaluator, Metric};
use super::request::Scope;
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use indexmap::IndexMap;

#[derive(Default)]
pub(super) struct BeamMemo {
    scores: RefCell<IndexMap<String, Option<Evaluation>>>,
    pub hits: Cell<usize>,
}

pub(super) struct Search<'a> {
    pub scope: &'a Scope,
    pub required: Vec<usize>,
    pub forbidden: HashSet<usize>,
    pub faction_excluded: usize,
}

impl<'a> Search<'a> {
    pub fn new(evaluator: &Evaluator<'_>, scope: &'a Scope) -> SimResult<Self> {
        let exclude_faction = evaluator.req.body.get("excludeFactionMods").is_some_and(|v| v == true);
        let mut required = Vec::new();
        let mut forbidden = HashSet::new();
        let mut faction_excluded = 0;
        for (i, card) in evaluator.cards.iter().enumerate() {
            if card.reserved || scope.required.iter().any(|path| path == card.path) {
                required.push(i);
            } else if scope.forbidden.iter().any(|path| path == card.path) {
                forbidden.insert(i);
            } else if exclude_faction && super::rows(&card.raw["effects"]).iter().any(|e| e["upgradeType"] == "GAMEPLAY_FACTION_DAMAGE") {
                forbidden.insert(i);
                faction_excluded += 1;
            }
        }
        for path in &scope.required {
            if !required.iter().any(|i| evaluator.cards[*i].path == path) {
                return Err(format!("requiredMods are not in this weapon's candidate pool: {path}"));
            }
        }
        if required.iter().any(|i| scope.forbidden.iter().any(|path| path == evaluator.cards[*i].path)) {
            return Err("a required MOD cannot also be forbidden".into());
        }
        Ok(Self { scope, required, forbidden, faction_excluded })
    }

    pub fn metric(&self) -> Metric<'_> {
        if self.scope.event { Metric::Events } else { Metric::Panel(&self.scope.metric) }
    }

    pub fn element(&self) -> Option<&str> { self.scope.element["damageType"].as_str() }

    pub fn element_trial(&self, eval: &Evaluator<'_>, seed: &[usize], width: usize,
        shortlist: usize) -> SimResult<Option<Evaluation>> {
        let settings = Beam { width, shortlist, niche: 0, profile: "fast",
            metric: Metric::Events, element: self.element() };
        self.beam(eval, &self.required, seed, &settings)
    }

    pub fn element_baseline(&self, eval: &Evaluator<'_>) -> SimResult<Option<Evaluation>> {
        let settings = Beam { width: 12, shortlist: 32, niche: 0, profile: "fast",
            metric: Metric::Events, element: self.element() };
        if let Some(found) = self.beam(eval, &self.required, &[], &settings)? { return Ok(Some(found)); }
        match self.element_fallback(eval, &[], &settings) {
            Err(message) if message.starts_with("no capacity-valid build satisfies damage type ") => Ok(None),
            result => result.map(Some),
        }
    }

    pub fn in_bounds(&self, eval: &Evaluator<'_>, selected: &[usize]) -> bool {
        let total = selected.len() + usize::from(eval.riven.is_some());
        self.scope.min.is_none_or(|n| total >= n)
            && self.scope.max.is_none_or(|n| total <= n)
            && eval.can_equip(selected)
    }

    pub fn analysis_candidates(&self, eval: &Evaluator<'_>, metric: Metric<'_>, ultra: bool,
        route_sensitive: bool, provisional: bool) -> SimResult<Vec<Evaluation>> {
        let profile = if ultra || provisional { "fast" } else { self.scope.profile };
        let (width, shortlist) = match profile {
            "fast" => (12, 32), "standard" => (24, 40), _ => (24, 56)
        };
        let mut candidates = Vec::new();
        if !ultra || route_sensitive {
            let settings = Beam { width, shortlist, niche: 0, profile, metric, element: self.element() };
            let best = self.beam(eval, &self.required, &[], &settings)?
                .map_or_else(|| self.element_fallback(eval, &[], &settings), Ok)?;
            candidates.push(best);
            if provisional || self.element().is_some() { return Ok(candidates); }
        }
        let targets = [None, Some("DT_VIRAL"), Some("DT_CORROSIVE")];
        for element in targets.into_iter().filter(|element| self.element().is_none_or(|target| Some(target) == *element)) {
            let mut required = self.required.clone();
            let parts = match element {
                Some("DT_VIRAL") => ["DT_FREEZE", "DT_POISON"].as_slice(),
                Some("DT_CORROSIVE") => ["DT_ELECTRICITY", "DT_POISON"].as_slice(),
                _ => &[],
            };
            for part in parts {
                if let Some(card) = self.element_seed(eval, &required, part, Preference::Proxy) { required.push(card); }
            }
            required.sort_unstable();
            let settings = Beam { width: 4, shortlist: 24, niche: 0, profile, metric: Metric::Proxy, element };
            if let Ok(Some(found)) = self.beam(eval, &required, &[], &settings) {
                let evaluation = if ultra && eval.riven.is_some() {
                    eval.fixed(&found.cards, Some(found.cards.len()), metric, None)?
                } else {
                    eval.evaluate(&found.cards, metric, None, eval.fixed_exilus)?
                };
                if let Some(value) = evaluation { candidates.push(value); }
            }
        }
        if candidates.is_empty() && self.element().is_some() {
            let settings = Beam { width: 4, shortlist: 24, niche: 0, profile, metric: Metric::Proxy, element: None };
            if let Some(found) = self.beam(eval, &self.required, &[], &settings)? {
                if let Some(value) = eval.fixed(&found.cards, found.riven_slot, metric, None)? { candidates.push(value); }
            }
        }
        if ultra && eval.riven.is_some() {
            for value in &mut candidates {
                if let Some(rescored) = eval.evaluate(&value.cards, metric, None, eval.fixed_exilus)? { *value = rescored; }
            }
        }
        if candidates.is_empty() { return Err("no capacity-valid build found".into()); }
        Ok(candidates)
    }

    pub fn run(&self, eval: &Evaluator<'_>, seed: &[usize], scout: bool) -> SimResult<Evaluation> {
        self.run_metric(eval, seed, scout, self.metric())
    }

    pub fn run_metric(&self, eval: &Evaluator<'_>, seed: &[usize], scout: bool, metric: Metric<'_>) -> SimResult<Evaluation> {
        let profile = if scout || self.scope.profile == "ultra" { "fast" } else { self.scope.profile };
        let (width, shortlist) = match (scout, profile) {
            (true, _) => (4, 16),
            (_, "fast") => (12, 32),
            (_, "standard") => (24, 40),
            _ => (24, 56),
        };
        let settings = Beam { width, shortlist, niche: 2, profile, metric, element: self.element() };
        let first = self.beam(eval, &self.required, seed, &settings)?;
        let best = match first {
            Some(value) => value,
            None => self.element_fallback(eval, seed, &settings)?,
        };
        if !self.scope.event { return Ok(best); }
        self.proxy_seeds_profile(eval, best, metric, profile)
    }

    pub fn primary_beam(&self, eval: &Evaluator<'_>, metric: Metric<'_>, niches: usize) -> SimResult<Evaluation> {
        let profile = self.scope.profile;
        let settings = Beam { width: 24, shortlist: if profile == "deep" { 56 } else { 40 },
            niche: niches, profile, metric, element: self.element() };
        self.beam(eval, &self.required, &[], &settings)?.ok_or_else(|| "no capacity-valid build found".into())
    }

    pub fn proxy_seeds(&self, eval: &Evaluator<'_>, best: Evaluation, metric: Metric<'_>) -> SimResult<Evaluation> {
        let profile = if self.scope.profile == "ultra" { "fast" } else { self.scope.profile };
        self.proxy_seeds_profile(eval, best, metric, profile)
    }

    fn proxy_seeds_profile(&self, eval: &Evaluator<'_>, mut best: Evaluation, metric: Metric<'_>, profile: &str) -> SimResult<Evaluation> {
        let targets = match self.element() {
            Some(element) => vec![Some(element)],
            None => vec![None, Some("DT_VIRAL"), Some("DT_CORROSIVE")],
        };
        for element in targets {
            let mut required = self.required.clone();
            let parts = match element {
                Some("DT_VIRAL") => ["DT_FREEZE", "DT_POISON"].as_slice(),
                Some("DT_CORROSIVE") => ["DT_ELECTRICITY", "DT_POISON"].as_slice(),
                _ => &[],
            };
            for part in parts {
                if let Some(card) = self.element_seed(eval, &required, part, Preference::Proxy) {
                    required.push(card);
                }
            }
            required.sort_unstable();
            let proxy = Beam { width: 4, shortlist: 24, niche: 0, profile, metric: Metric::Proxy, element };
            let Some(found) = self.beam(eval, &required, &[], &proxy).unwrap_or(None) else { continue; };
            if let Some(scored) = eval.evaluate(&found.cards, metric, self.element(), eval.fixed_exilus)? {
                if scored.score > best.score + 1e-9 { best = scored; }
            }
        }
        Ok(best)
    }

    pub fn fixed_seed(&self, eval: &Evaluator<'_>, seed: &[usize], metric: Metric<'_>) -> SimResult<Evaluation> {
        let settings = Beam { width: 12, shortlist: 32, niche: 0, profile: "fast", metric, element: None };
        self.beam(eval, &self.required, seed, &settings)?.ok_or_else(|| "no capacity-valid build found".into())
    }

    fn element_seed(&self, eval: &Evaluator<'_>, required: &[usize], part: &str, prefer: Preference) -> Option<usize> {
        let mut options: Vec<_> = eval.cards.iter().enumerate().filter_map(|(i, card)| {
            if self.forbidden.contains(&i) || required.contains(&i) || !card.effects.iter().any(|e| {
                e.dtype.as_deref() == Some(part) && (prefer == Preference::Proxy || e.upgrade == "WEAPON_PERCENT_BASE_DAMAGE_ADDED")
            }) { return None; }
            let value = card.effects.iter().filter(|e| e.dtype.as_deref() == Some(part)).map(|e| e.value).sum::<f64>();
            Some((i, value, card.drain))
        }).collect();
        options.sort_by(|a, b| match prefer {
            Preference::Proxy => b.1.total_cmp(&a.1).then(b.0.cmp(&a.0)),
            Preference::Strength => b.1.total_cmp(&a.1).then(a.2.cmp(&b.2)).then(b.0.cmp(&a.0)),
            Preference::Drain => a.2.cmp(&b.2).then(b.1.total_cmp(&a.1)).then(a.0.cmp(&b.0)),
        });
        options.first().map(|v| v.0)
    }

    fn element_fallback(&self, eval: &Evaluator<'_>, seed: &[usize], settings: &Beam<'_>) -> SimResult<Evaluation> {
        let element = settings.element.ok_or("optimizer found no capacity-valid build")?;
        let parts = match element {
            "DT_EXPLOSION" => ["DT_FIRE", "DT_FREEZE"].as_slice(),
            "DT_RADIATION" => ["DT_FIRE", "DT_ELECTRICITY"].as_slice(),
            "DT_GAS" => ["DT_FIRE", "DT_POISON"].as_slice(),
            "DT_MAGNETIC" => ["DT_FREEZE", "DT_ELECTRICITY"].as_slice(),
            "DT_VIRAL" => ["DT_FREEZE", "DT_POISON"].as_slice(),
            "DT_CORROSIVE" => ["DT_ELECTRICITY", "DT_POISON"].as_slice(),
            _ => std::slice::from_ref(&element),
        };
        for preference in [Preference::Strength, Preference::Drain] {
            let mut required = self.required.clone();
            for part in parts {
                if let Some(card) = self.element_seed(eval, &required, part, preference) {
                    required.push(card);
                } else { break; }
            }
            if required.len() == self.required.len() + parts.len() {
                required.sort_unstable();
                if let Ok(Some(found)) = self.beam(eval, &required, seed, settings) { return Ok(found); }
            }
        }
        Err(format!("no capacity-valid build satisfies damage type {element}"))
    }

    fn beam(&self, eval: &Evaluator<'_>, required: &[usize], seed: &[usize], settings: &Beam<'_>) -> SimResult<Option<Evaluation>> {
        let kind = match settings.metric { Metric::Events => "killsPerMinute", Metric::WindowDps => "windowedDps",
            Metric::Proxy => "statusSustainedDps", Metric::Panel(key) => key };
        if let Some(memo) = eval.replay_options.memo {
            let key = super::json!([kind, settings.profile, settings.width, settings.shortlist, settings.niche,
                settings.element, required, seed, eval.riven.map(|riven| &riven.lines)]).to_string();
            {
                let mut cache = memo.scores.borrow_mut();
                if let Some(value) = cache.shift_remove(&key) {
                    cache.insert(key, value.clone());
                    memo.hits.set(memo.hits.get() + 1);
                    return Ok(value);
                }
            }
            let value = self.beam_inner(eval, required, seed, settings)?;
            let mut cache = memo.scores.borrow_mut();
            cache.insert(key, value.clone());
            if cache.len() > 256 { cache.shift_remove_index(0); }
            Ok(value)
        } else {
            self.beam_inner(eval, required, seed, settings)
        }
    }

    fn beam_inner(&self, eval: &Evaluator<'_>, required: &[usize], seed: &[usize], settings: &Beam<'_>) -> SimResult<Option<Evaluation>> {
        let deadline = eval.replay_options.phase_deadline(self.scope.profile);
        let riven_count = usize::from(eval.riven.is_some());
        let limit = self.scope.max.unwrap_or(eval.req.slot_count).min(eval.req.slot_count);
        let base_fill = required.len() + riven_count;
        if base_fill > limit || !eval.can_equip(required) {
            return Err("required MODs do not fit the selected slots or capacity".into());
        }
        let slots = limit - base_fill;
        let score = |cards: &[usize]| eval.evaluate(cards, settings.metric, None, eval.fixed_exilus);
        let eligible = |value: &Evaluation| -> SimResult<bool> {
            if let Some(element) = settings.element {
                let (effects, skipped) = eval.terms(&value.cards, value.riven_slot, eval.fixed_exilus, true);
                let panel = eval.panel(eval.req, &effects, &skipped, None)?;
                return Ok(num(&panel.wire["damageTypes"][element]) > 0.0);
            }
            Ok(true)
        };
        let mut best = score(required)?;
        if let Some(value) = &best {
            if !eligible(value)? { best = None; }
        }
        if slots == 0 {
            if best.as_ref().is_some_and(|v| !self.in_bounds(eval, &v.cards)) { return Ok(None); }
            return Ok(best);
        }

        let mut ranked = Vec::new();
        for (i, card) in eval.cards.iter().enumerate() {
            if self.forbidden.contains(&i) || required.contains(&i) { continue; }
            if let Some(duplicate) = card.raw["optimizerDuplicateOf"].as_str() {
                if eval.cards.iter().enumerate().any(|(j, other)| other.path == duplicate && !self.forbidden.contains(&j)) {
                    continue;
                }
            }
            let mut selected = required.to_vec();
            selected.push(i);
            if let Some(value) = eval.rank(&selected, settings.metric)? { ranked.push((i, value.score)); }
        }
        ranked.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
        let mut families = HashSet::new();
        let mut pool: Vec<_> = ranked.iter().map(|(i, _)| *i).filter(|i| {
            families.insert(eval.cards[*i].family.as_str())
        }).take(settings.shortlist).collect();
        for bit in [1, 2, 4, 8] {
            let mut have = pool.iter().filter(|i| eval.cards[**i].elemental & bit != 0).count();
            for &(i, _) in &ranked {
                if have >= settings.niche { break; }
                let card = &eval.cards[i];
                if card.elemental & bit != 0 && families.insert(&card.family) {
                    pool.push(i);
                    have += 1;
                }
            }
        }

        let mut greedy = required.to_vec();
        for &card in pool.iter().take(slots) {
            greedy.push(card);
            if !self.in_bounds(eval, &greedy) { greedy.pop(); break; }
        }
        if greedy.len() > required.len() {
            if let Some(value) = score(&greedy)? { improve(&mut best, value, &eligible, 0.0)?; }
        }
        let mut seeded = required.to_vec();
        for &card in seed {
            if !self.forbidden.contains(&card) && !required.contains(&card) && seeded.len() < limit - riven_count {
                seeded.push(card);
                if !eval.can_equip(&seeded) { seeded.pop(); }
            }
        }
        if seeded.len() > required.len() && self.in_bounds(eval, &seeded) {
            if let Some(value) = score(&seeded)? { improve(&mut best, value, &eligible, 0.0)?; }
        }

        let mut beam = vec![Vec::new()];
        for _ in 0..slots {
            if eval.replay_options.expired(deadline) { break; }
            let mut cache = HashMap::new();
            let mut scored = Vec::new();
            'layer: for selection in &beam {
                for &card in &pool {
                    if selection.contains(&card) { continue; }
                    let mut candidate = selection.clone();
                    candidate.push(card);
                    let mut key = candidate.clone();
                    key.sort_unstable();
                    let value = match cache.get(&key) {
                        Some(value) => *value,
                        None => {
                            if eval.replay_options.expired(deadline) { break 'layer; }
                            let cards: Vec<_> = required.iter().chain(&candidate).copied().collect();
                            let value = score(&cards)?.map(|v| v.score);
                            cache.insert(key, value);
                            value
                        }
                    };
                    if let Some(value) = value { scored.push((candidate, value)); }
                }
            }
            scored.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
            if scored.is_empty() { break; }
            if settings.element.is_none() {
                if best.as_ref().is_none_or(|v| scored[0].1 > v.score) {
                    let cards: Vec<_> = required.iter().chain(&scored[0].0).copied().collect();
                    if let Some(value) = score(&cards)? { best = Some(value); }
                }
            } else {
                let mut exact = Vec::new();
                for (selected, _) in survivors(eval, scored, settings.width + 1, settings.niche) {
                    let cards: Vec<_> = required.iter().chain(&selected).copied().collect();
                    if let Some(value) = score(&cards)? {
                        exact.push((selected, value));
                    }
                }
                exact.sort_by(|a, b| b.1.score.total_cmp(&a.1.score).then(a.0.cmp(&b.0)));
                scored = Vec::with_capacity(exact.len());
                for (selected, value) in exact {
                    scored.push((selected, value.score));
                    improve(&mut best, value, &eligible, 0.0)?;
                }
            }
            beam = survivors(eval, scored, settings.width, settings.niche).into_iter().map(|(row, _)| row).collect();
        }

        let Some(mut best) = best else { return Ok(None); };
        while best.cards.len() + riven_count < self.scope.min.unwrap_or(0).min(eval.req.slot_count) {
            let mut next = None;
            for &card in &pool {
                let mut cards = best.cards.clone();
                cards.push(card);
                if let Some(value) = score(&cards)? {
                    if eligible(&value)? { next = Some(value); break; }
                }
            }
            let Some(value) = next else { break; };
            best = value;
        }

        let rounds = match settings.profile { "standard" => 3, "deep" => 12, _ => 0 };
        let mut seen = HashSet::from([best.cards.clone()]);
        for _ in 0..rounds {
            if eval.replay_options.expired(deadline) { break; }
            let base = best.cards.clone();
            let optional: Vec<_> = base.iter().enumerate().filter(|(_, i)| !required.contains(i)).collect();
            let mut improved = false;
            let mut chosen_swap = None;
            let mut last_swap = None;
            for &(slot, _) in &optional {
                for &card in &pool {
                    if base.contains(&card) { continue; }
                    let mut cards = base.clone();
                    cards[slot] = card;
                    if !self.in_bounds(eval, &cards) || !seen.insert(cards.clone()) { continue; }
                    if let Some(value) = score(&cards)? {
                        if value.score > best.score + 1e-9 && eligible(&value)? {
                            best = value;
                            improved = true;
                            chosen_swap = Some(cards.clone());
                        }
                        last_swap = Some(cards);
                    }
                }
            }
            if eval.replay_options.evaluations.is_some() {
                if let Some(cards) = chosen_swap.filter(|cards| Some(cards) != last_swap.as_ref()) {
                    // A committed swap carries its own evaluated order after the rest of the round has run.
                    if let Some(value) = score(&cards)? { best = value; }
                }
            }
            if best.cards.len() + riven_count < limit {
                for &card in &pool {
                    let mut cards = best.cards.clone();
                    cards.push(card);
                    if !self.in_bounds(eval, &cards) || !seen.insert(cards.clone()) { continue; }
                    if let Some(value) = score(&cards)? {
                        if value.score > best.score + 1e-9 && eligible(&value)? { best = value; improved = true; break; }
                    }
                }
            }
            for (_, old) in &optional {
                let cards: Vec<_> = best.cards.iter().copied().filter(|card| card != *old).collect();
                if cards.is_empty() || !self.in_bounds(eval, &cards) || !seen.insert(cards.clone()) { continue; }
                if let Some(value) = score(&cards)? {
                    if value.score > best.score + 1e-9 && eligible(&value)? { best = value; improved = true; }
                }
            }
            if settings.profile == "deep" {
                let mut pairs = Vec::new();
                'pairs: for (left, &(_, a)) in optional.iter().enumerate() {
                    for &(_, b) in &optional[left + 1..] {
                        let kept: Vec<_> = base.iter().copied().filter(|i| i != a && i != b).collect();
                        for &x in &pool {
                            if base.contains(&x) { continue; }
                            for &y in &pool {
                                if base.contains(&y) || x == y { continue; }
                                let mut cards = kept.clone();
                                cards.extend([x, y]);
                                if !self.in_bounds(eval, &cards) || !seen.insert(cards.clone()) { continue; }
                                if let Some(value) = score(&cards)? {
                                    if eligible(&value)? {
                                        pairs.push((cards, value));
                                        if pairs.len() == 192 { break 'pairs; }
                                    }
                                }
                            }
                        }
                    }
                }
                pairs.sort_by(|a, b| b.1.score.total_cmp(&a.1.score).then(a.0.cmp(&b.0)));
                if let Some((_, value)) = pairs.into_iter().next() {
                    if value.score > best.score + 1e-9 { best = value; improved = true; }
                }
            }
            if !improved { break; }
        }

        if self.scope.min.is_none() && self.scope.max.is_none() {
            for &card in &pool {
                if best.cards.len() + riven_count >= limit { break; }
                if best.cards.iter().any(|&index| eval.cards[index].family == eval.cards[card].family) {
                    continue;
                }
                let mut cards = best.cards.clone();
                cards.push(card);
                if let Some(value) = score(&cards)? {
                    if value.score >= best.score && eligible(&value)? { best = value; }
                }
            }
        }
        Ok(self.in_bounds(eval, &best.cards).then_some(best))
    }

    pub fn refine(&self, eval: &Evaluator<'_>, best: Evaluation) -> SimResult<Evaluation> {
        if !self.scope.event { return Ok(best); }
        self.refine_scored(eval, best, self.metric())
    }

    pub fn refine_scored(&self, eval: &Evaluator<'_>, mut best: Evaluation, metric: Metric<'_>) -> SimResult<Evaluation> {
        let fast = matches!(self.scope.profile, "fast" | "ultra");
        let rounds = if fast { 1 } else if self.scope.profile == "deep" { 4 } else { 2 };
        let mut seen = HashSet::from([best.cards.clone()]);
        for _ in 0..rounds {
            let base = best.cards.clone();
            let old_score = best.score;
            let mut alternatives = vec![Vec::new(); base.len()];
            let mut visit = |cards: Vec<usize>| -> SimResult<Option<f64>> {
                if !self.in_bounds(eval, &cards) || !seen.insert(cards.clone()) { return Ok(None); }
                let value = eval.evaluate(&cards, metric, self.element(), eval.fixed_exilus)?;
                Ok(value.map(|value| {
                    let score = value.score;
                    if score > best.score + 1e-9 { best = value; }
                    score
                }))
            };
            for (slot, old) in base.iter().enumerate() {
                if self.required.contains(old) { continue; }
                for (i, card) in eval.cards.iter().enumerate() {
                    if self.forbidden.contains(&i) || base.contains(&i) ||
                        (fast && card.family != eval.cards[*old].family) { continue; }
                    let mut cards = base.clone();
                    cards[slot] = i;
                    if let Some(score) = visit(cards)? { alternatives[slot].push((i, score)); }
                }
                if !fast {
                    let mut cards = base.clone();
                    cards.remove(slot);
                    visit(cards)?;
                }
            }
            if !fast && base.len() + usize::from(eval.riven.is_some()) < eval.req.slot_count {
                for i in 0..eval.cards.len() {
                    if self.forbidden.contains(&i) || base.contains(&i) { continue; }
                    let mut cards = base.clone();
                    cards.push(i);
                    visit(cards)?;
                }
            }
            if best.score == old_score && self.scope.profile == "deep" {
                for row in &mut alternatives {
                    row.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
                    row.truncate(4);
                }
                let mut pairs = Vec::new();
                for (left, a) in alternatives.iter().enumerate() {
                    for (right, b) in alternatives.iter().enumerate().skip(left + 1) {
                        for &(x, sa) in a {
                            for &(y, sb) in b {
                                let mut cards = base.clone();
                                cards[left] = x;
                                cards[right] = y;
                                if self.in_bounds(eval, &cards) { pairs.push((cards, sa + sb)); }
                            }
                        }
                    }
                }
                pairs.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
                for (cards, _) in pairs.into_iter().take(256) {
                    if seen.insert(cards.clone()) {
                        if let Some(value) = eval.evaluate(&cards, metric, self.element(), eval.fixed_exilus)? {
                            if value.score > best.score + 1e-9 { best = value; }
                        }
                    }
                }
            }
            if best.score == old_score { break; }
        }
        Ok(best)
    }
}

#[derive(PartialEq, Clone, Copy)]
enum Preference { Proxy, Strength, Drain }

struct Beam<'a> {
    width: usize,
    shortlist: usize,
    niche: usize,
    profile: &'a str,
    metric: Metric<'a>,
    element: Option<&'a str>,
}

fn improve(best: &mut Option<Evaluation>, value: Evaluation, eligible: &impl Fn(&Evaluation) -> SimResult<bool>,
    margin: f64) -> SimResult<()> {
    if best.as_ref().is_none_or(|before| value.score > before.score + margin) && eligible(&value)? { *best = Some(value); }
    Ok(())
}

fn survivors(eval: &Evaluator<'_>, scored: Vec<(Vec<usize>, f64)>, width: usize, niches: usize)
    -> Vec<(Vec<usize>, f64)> {
    let mut counts = [0; 16];
    scored.into_iter().enumerate().filter_map(|(index, row)| {
        let mask = row.0.iter().fold(0, |mask, card| mask | eval.cards[*card].elemental as usize);
        if index >= width && counts[mask] >= niches { return None; }
        counts[mask] += 1;
        Some(row)
    }).collect()
}
