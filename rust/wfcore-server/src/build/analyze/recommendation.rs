use super::{json, num, optimize, percent, riven_traits, text, Evaluation, Evaluator, Input,
    Map, Metric, Search, SimResult, Value};
use crate::simulate::build::{evaluate::ReplayOptions, request::Scope, search::BeamMemo};
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet, VecDeque};
use indexmap::IndexMap;

const LOSS: f64 = 0.35;

#[derive(Clone, Default)]
pub(super) struct Recommendation {
    pub positive: Vec<Value>,
    pub tiers: Vec<Value>,
    pub negative: Vec<Value>,
    pub assessment: Vec<Value>,
    pub work: Map<String, Value>,
    pub memo_hits: usize,
}

pub(super) fn recommend(center: &Evaluator<'_>, integrated: &Evaluator<'_>, search: &Search<'_>,
    input: &Input, baseline: &Evaluation, kind: &str, metric: Metric<'_>, route_sensitive: bool)
    -> SimResult<(Recommendation, Vec<Vec<usize>>)> {
    let count = input.card.lines.iter().filter(|line| line["negative"] != true).count();
    let traits = riven_traits::traits(center.data, kind, center.req.weapon);
    let candidate = |rule: &riven_traits::Trait<'_>, scale: f64, negative| {
        rule.template_base.map(|base| {
            let value = base * input.disposition * (scale * 1.1);
            rule.line(value, 0.0, value, negative)
        })
    };
    let positive: Vec<_> = traits.iter().filter(|rule| rule.positive)
        .filter_map(|rule| candidate(rule, riven_traits::structure(2, false, false), false)).collect();
    let line = |code| positive.iter().find(|line| line["code"] == code);
    let memo = Fits::default();
    let score = |base, lines: &[Value]| memo.fit(center, base, lines, metric);
    let mut best_element = None;
    let mut element_score = f64::NEG_INFINITY;
    for code in ["4", "5", "6", "7"] {
        if let Some(line) = line(code) {
            let value = score(&baseline.cards, std::slice::from_ref(line))?;
            if value > element_score { best_element = Some(code); element_score = value; }
        }
    }
    let filler = if line("S").is_some() { "S" } else { "D" };
    let mut archetypes = Vec::new();
    for codes in [["0", "1", filler], ["2", best_element.unwrap_or("D"), filler]] {
        let mut seen = HashSet::new();
        let lines: Vec<_> = codes.into_iter().filter(|code| seen.insert(*code)).filter_map(line).cloned().collect();
        if lines.len() < 2 { continue; }
        let riven = riven_traits::card(center.data, lines, 8, "AP_ATTACK")?;
        let center = Evaluator { riven: Some(&riven), ..center.clone() };
        let integrated = Evaluator { riven: Some(&riven), ..integrated.clone() };
        if let Ok(value) = optimize(&center, &integrated, search, metric, false, route_sensitive, false) {
            archetypes.push(value.cards);
        }
    }
    let mut bases = vec![baseline.cards.clone()];
    for value in &archetypes {
        if !bases.contains(value) { bases.push(value.clone()); }
    }
    let fit_bases = |lines: &[Value]| -> SimResult<f64> {
        let mut best = f64::NEG_INFINITY;
        for base in &bases { best = best.max(score(base, lines)?); }
        Ok(best)
    };
    let is_fusion = |line: &Value| traits.iter().any(|rule| rule.fusion && line["code"] == rule.code);
    let mut ranked = Vec::new();
    for line in &positive {
        if traits.iter().any(|rule| rule.modeled && line["code"] == rule.code) {
            ranked.push((fit_bases(std::slice::from_ref(line))?, line));
        }
    }
    ranked.sort_by(|a, b| b.0.total_cmp(&a.0).then(text(&a.1["code"]).cmp(text(&b.1["code"]))));
    if ranked.len() < count { return Err("not enough supported Riven stats for this weapon".into()); }
    let mut shortlist: Vec<_> = ranked.iter().take(8).map(|entry| entry.1).collect();
    let ordinary = shortlist.iter().filter(|line| !is_fusion(line)).count();
    shortlist.extend(ranked.iter().skip(8).map(|entry| entry.1).filter(|line| !is_fusion(line))
        .take((count - 1).saturating_sub(ordinary)));
    let fast = search.scope.profile == "fast";
    let recommended: Vec<Value> = if fast {
        let mut fusion_used = false;
        let lines: Vec<_> = shortlist.into_iter().filter(|line| {
            !is_fusion(line) || !std::mem::replace(&mut fusion_used, true)
        }).take(count).cloned().collect();
        fit_bases(&lines)?;
        lines
    } else {
        let mut best = f64::NEG_INFINITY;
        let mut selected = Vec::new();
        for index in combinations(shortlist.len(), count) {
            let lines: Vec<_> = index.into_iter().map(|i| shortlist[i].clone()).collect();
            if lines.iter().filter(|line| is_fusion(line)).count() > 1 { continue; }
            let value = fit_bases(&lines)?;
            if value > best { best = value; selected = lines; }
        }
        fit_bases(&selected)?;
        selected
    };
    let mut marginal = Vec::new();
    if fast {
        let bare = fit_bases(&[])?;
        for &(value, line) in &ranked {
            let gain = percent(value, bare);
            if gain > 0.0 { marginal.push((gain, line)); }
        }
    } else {
        let mut anchors = HashMap::new();
        let mut trials = Vec::new();
        for &(_, line) in &ranked {
            let mut anchor: Vec<_> = recommended.iter().filter(|item| item["code"] != line["code"]).take(count - 1).cloned().collect();
            if anchor.len() < count - 1 { anchor = recommended[..count - 1].to_vec(); }
            let key: Vec<_> = anchor.iter().map(|line| text(&line["code"]).to_owned()).collect();
            if !anchors.contains_key(&key) { anchors.insert(key.clone(), fit_bases(&anchor)?); }
            anchor.push(line.clone());
            trials.push((line, key, anchor));
        }
        for (line, key, lines) in trials {
            let gain = percent(fit_bases(&lines)?, anchors[&key]);
            if gain > 0.0 { marginal.push((gain, line)); }
        }
    }
    marginal.sort_by(|a, b| b.0.total_cmp(&a.0).then(text(&a.1["code"]).cmp(text(&b.1["code"]))));
    let best_gain = marginal.first().map_or(1.0, |row| row.0);
    let tiers = [("T0", 80.0, f64::INFINITY), ("T1", 60.0, 80.0), ("T2", 40.0, 60.0), ("T3", 0.0, 40.0)]
        .into_iter().filter_map(|(tier, floor, ceiling)| {
            let stats: Vec<_> = marginal.iter().filter_map(|(gain, line)| {
                let score = gain / best_gain * 100.0;
                (floor <= score && score < ceiling).then(|| json!({"code":line["code"],"label":line["label"],"gain":gain,"score":score}))
            }).take(3).collect();
            (!stats.is_empty()).then(|| json!({"tier":tier,"stats":stats}))
        }).collect();
    let seed: Vec<_> = recommended.iter().filter_map(|line| traits.iter().find(|rule| line["code"] == rule.code))
        .filter_map(|rule| candidate(rule, riven_traits::structure(count, true, false), false)).collect();
    let negative: Vec<_> = traits.iter().filter(|rule| rule.negative)
        .filter_map(|rule| candidate(rule, riven_traits::structure(count, true, true), true).map(|line| (rule.modeled, line))).collect();
    let neutral = |line: &Value| riven_traits::neutral(center.req.melee, text(&line["code"]));
    let mut ranked_negative = Vec::new();
    for (modeled, line) in &negative {
        if (!modeled && !neutral(line)) || seed.iter().any(|item| item["code"] == line["code"]) { continue; }
        for roll in negative_rolls(line) {
            let mut trial = seed.clone();
            trial.push(roll.clone());
            ranked_negative.push((score(&baseline.cards, &trial)?, roll));
        }
    }
    ranked_negative.sort_by(|a, b| b.0.total_cmp(&a.0)
        .then((!neutral(&a.1)).cmp(&!neutral(&b.1))).then(text(&a.1["code"]).cmp(text(&b.1["code"]))));
    let mut seen = HashSet::new();
    let ranked: Vec<_> = ranked_negative.into_iter().map(|(_, line)| line).filter(|line| {
        seen.insert(text(&line["code"]).to_owned())
    }).collect();
    let recommended_negative = ranked.iter().take(3).cloned().collect();
    let lines: Vec<_> = ranked.iter().filter_map(|line| negative.iter()
        .find(|(_, top)| top["code"] == line["code"]).map(|(_, line)| line.clone())).collect();
    let assessment = assess(center, &baseline.cards, &seed, &lines,
        input.fields.get("_bakeNegativeClassificationOnly").is_some_and(super::truth))?;
    Ok((Recommendation { positive:recommended, tiers, negative:recommended_negative,
        assessment:assessment.rows, work:assessment.work, memo_hits:memo.hits.get() + assessment.memo_hits }, archetypes))
}

fn negative_rolls(line: &Value) -> Vec<Value> {
    let top = num(&line["value"]);
    let low = top * 0.9 / 1.1;
    let mut values = vec![top];
    if line["code"] == "3" {
        values.extend([low, (low + top) / 2.0]);
        if low < 99.9 && top > 99.9 { values.extend([(low + 99.9) / 2.0, 99.9]); }
    }
    values.sort_by(f64::total_cmp);
    values.dedup();
    values.into_iter().map(|value| roll(line, value)).collect()
}

fn roll(line: &Value, value: f64) -> Value {
    let mut line = line.clone();
    line["value"] = json!(value);
    line
}

fn combinations(size: usize, count: usize) -> Vec<Vec<usize>> {
    fn append(out: &mut Vec<Vec<usize>>, current: &mut Vec<usize>, start: usize, size: usize, count: usize) {
        if count == 0 { out.push(current.clone()); return; }
        for index in start..=size - count {
            current.push(index);
            append(out, current, index + 1, size, count - 1);
            current.pop();
        }
    }
    let mut out = Vec::new();
    if count <= size { append(&mut out, &mut Vec::new(), 0, size, count); }
    out
}

pub(super) fn reoptimized_ablation(eval: &Evaluator<'_>, cards: &[usize], lines: &[Value], metric: Metric<'_>,
    rank: u64, polarity: &str) -> SimResult<f64> {
    let riven = riven_traits::card(eval.data, lines.to_vec(), rank, polarity)?;
    best_fixed(&Evaluator { riven: Some(&riven), ..eval.clone() }, cards, metric, true)
}

type FitKey = (Vec<usize>, String, bool);

#[derive(Default)]
struct Fits {
    scores: RefCell<IndexMap<FitKey, f64>>,
    hits: Cell<usize>,
}

impl Fits {
    fn fit(&self, eval: &Evaluator<'_>, cards: &[usize], lines: &[Value], metric: Metric<'_>) -> SimResult<f64> {
        let key = (cards.to_vec(), serde_json::to_string(lines).map_err(|error| error.to_string())?,
            matches!(metric, Metric::WindowDps));
        {
            let mut cache = self.scores.borrow_mut();
            if let Some(value) = cache.shift_remove(&key) {
                cache.insert(key, value);
                self.hits.set(self.hits.get() + 1);
                return Ok(value);
            }
        }
        let riven = riven_traits::card(eval.data, lines.to_vec(), 8, "AP_ATTACK")?;
        let value = best_fixed(&Evaluator { riven: Some(&riven), ..eval.clone() }, cards, metric, false)?;
        let mut cache = self.scores.borrow_mut();
        cache.insert(key, value);
        if cache.len() > 256 { cache.shift_remove_index(0); }
        Ok(value)
    }
}

fn best_fixed(eval: &Evaluator<'_>, base: &[usize], metric: Metric<'_>, preserve: bool) -> SimResult<f64> {
    let (required, optional): (Vec<_>, Vec<_>) = base.iter().copied().partition(|&index| eval.cards[index].reserved);
    let mut best = if preserve { eval.evaluate(base, metric, None, eval.fixed_exilus)?.map(|value| value.score) } else { None };
    let limit = eval.req.slot_count.saturating_sub(1 + required.len()).min(optional.len());
    for keep in (0..=limit).rev() {
        let mut score: Option<f64> = None;
        for indexes in combinations(optional.len(), keep) {
            let cards: Vec<_> = required.iter().copied().chain(indexes.into_iter().map(|i| optional[i])).collect();
            if let Some(value) = eval.evaluate(&cards, metric, None, eval.fixed_exilus)? {
                score = Some(score.map_or(value.score, |before| before.max(value.score)));
            }
        }
        if let Some(score) = score {
            best = Some(best.map_or(score, |value| value.max(score)));
            break;
        }
    }
    best.ok_or_else(|| "no capacity-valid build can equip the Riven".into())
}

#[derive(Default)]
struct Assessment {
    rows: Vec<Value>,
    work: Map<String, Value>,
    memo_hits: usize,
}

struct NegativeSearch<'a, 'b> {
    eval: &'a Evaluator<'b>,
    baseline: &'a [usize],
    seed: &'a [Value],
    memo: &'a Fits,
    witnesses: VecDeque<Vec<usize>>,
    work: Map<String, Value>,
}

impl NegativeSearch<'_, '_> {
    fn count(&mut self, name: &str) {
        self.work.insert(name.into(), json!(self.work.get(name).and_then(Value::as_u64).unwrap_or(0) + 1));
    }

    fn lines(&self, line: Option<&Value>) -> Vec<Value> {
        self.seed.iter().chain(line).cloned().collect()
    }

    fn fixed(&self, line: Option<&Value>) -> SimResult<f64> {
        self.memo.fit(self.eval, self.baseline, &self.lines(line), Metric::WindowDps)
    }

    fn witness(&self, line: &Value, reference: f64) -> SimResult<(f64, bool)> {
        let riven = riven_traits::card(self.eval.data, self.lines(Some(line)), 8, "AP_ATTACK")?;
        let eval = Evaluator { riven: Some(&riven), ..self.eval.clone() };
        let mut score = 0.0_f64;
        for cards in &self.witnesses {
            if let Some(value) = eval.evaluate(cards, Metric::WindowDps, None, eval.fixed_exilus)? {
                score = score.max(value.score);
                if score / reference - 1.0 >= -LOSS { return Ok((score, true)); }
            }
        }
        Ok((score, false))
    }

    fn search(&mut self, line: Option<&Value>, proof: Option<f64>) -> SimResult<(f64, bool)> {
        let riven = riven_traits::card(self.eval.data, self.lines(line), 8, "AP_ATTACK")?;
        let eval = Evaluator { riven: Some(&riven), ..self.eval.clone() };
        let scope = |profile| Scope { profile, event:true, metric:"killsPerMinute".into(), element:Value::Null,
            required:Vec::new(), forbidden:Vec::new(), min:None, max:None, include_arcanes:true };
        let metric = Metric::WindowDps;
        let fast = scope("fast");
        let standard = scope("standard");
        let seeded = Search::new(&eval, &fast)?;
        let standard = Search::new(&eval, &standard)?;
        let safe = |value: &Evaluation| proof.is_some_and(|reference| value.score / reference - 1.0 >= -LOSS);
        self.count("fastSearches");
        let seed = seeded.fixed_seed(&eval, self.baseline, metric)?;
        if safe(&seed) {
            self.count("fastProofs");
            return Ok((self.remember(seed), false));
        }
        if proof.is_some() {
            self.count("proxyScreens");
            let screened = standard.proxy_seeds(&eval, seed.clone(), metric)?;
            if safe(&screened) {
                self.count("proxyProofs");
                return Ok((self.remember(screened), false));
            }
        }
        self.count("standardSearches");
        let fresh = standard.primary_beam(&eval, metric, 2)?;
        let best = if fresh.score > seed.score { fresh } else { seed };
        if safe(&best) {
            self.count("standardProofs");
            return Ok((self.remember(best), false));
        }
        self.count("proxySearches");
        let best = standard.proxy_seeds(&eval, best, metric)?;
        Ok((self.remember(best), true))
    }

    fn remember(&mut self, best: Evaluation) -> f64 {
        if !best.cards.is_empty() {
            if let Some(index) = self.witnesses.iter().position(|cards| *cards == best.cards) { self.witnesses.remove(index); }
            self.witnesses.push_back(best.cards);
            if self.witnesses.len() > 8 { self.witnesses.pop_front(); }
        }
        best.score
    }

    fn fatal_at(&mut self, line: &Value, delta: f64, reference: f64, searched: f64) -> SimResult<bool> {
        if delta >= -LOSS { return Ok(false); }
        let (witness, safe) = self.witness(line, searched)?;
        if safe { self.count("witnessProofs"); return Ok(false); }
        let (optimized, _) = self.search(Some(line), Some(searched))?;
        Ok(optimized.max(witness).max((1.0 + delta) * reference) / searched - 1.0 < -LOSS)
    }

    fn threshold(&mut self, line: &Value, reference: f64, searched: f64) -> SimResult<Option<f64>> {
        let mut high = num(&line["value"]);
        let mut low = high * 0.9 / 1.1;
        let initial = roll(line, low);
        let delta = self.fixed(Some(&initial))? / reference - 1.0;
        if self.fatal_at(&initial, delta, reference, searched)? { return Ok(None); }
        let round_visible = |value: f64| (value * 10.0 + 1e-9).floor() / 10.0;
        if delta >= -LOSS {
            let mut bad = high;
            while bad - low > 0.1 {
                let mid = (low + bad) / 2.0;
                if self.fixed(Some(&roll(line, mid)))? / reference - 1.0 >= -LOSS { low = mid; } else { bad = mid; }
            }
            let rolled = roll(line, bad);
            let delta = self.fixed(Some(&rolled))? / reference - 1.0;
            if self.fatal_at(&rolled, delta, reference, searched)? { return Ok(Some(round_visible(low))); }
            low = bad;
        }
        while high - low > 0.1 {
            let mid = (low + high) / 2.0;
            let rolled = roll(line, mid);
            let delta = self.fixed(Some(&rolled))? / reference - 1.0;
            if self.fatal_at(&rolled, delta, reference, searched)? { high = mid; } else { low = mid; }
        }
        Ok(Some(round_visible(low)))
    }
}

fn assess(eval: &Evaluator<'_>, baseline: &[usize], seed: &[Value], lines: &[Value], classification: bool) -> SimResult<Assessment> {
    let searches = BeamMemo::default();
    let fixed_checks = Fits::default();
    let eval = Evaluator { replay_options:ReplayOptions { memo:Some(&searches), ..eval.replay_options }, ..eval.clone() };
    let mut session = NegativeSearch { eval:&eval, baseline, seed, memo:&fixed_checks, witnesses:VecDeque::new(), work:Map::new() };
    let reference = session.fixed(None)?;
    if reference == 0.0 { return Ok(Assessment::default()); }
    let mut rows = Vec::new();
    let mut suspects = Vec::new();
    for line in lines {
        let delta = session.fixed(Some(line))? / reference - 1.0;
        let neutral = riven_traits::neutral(eval.req.melee, text(&line["code"]));
        if !neutral && delta < -LOSS { suspects.push((rows.len(), line, delta)); }
        rows.push(json!({"code":line["code"], "label":line["label"], "value":line["value"], "neutral":neutral,
            "fixedDelta":delta, "reoptimizedDelta":null, "fatal":false, "fatalAbove":null}));
    }
    if !suspects.is_empty() {
        let searched = session.search(None, None)?.0.max(reference);
        for &(index, line, delta) in &suspects {
            let proof = if classification { session.witness(line, searched)? } else { (0.0, false) };
            let (score, complete) = if proof.1 {
                session.count("classificationWitnessProofs");
                (proof.0, false)
            } else { session.search(Some(line), classification.then_some(searched))? };
            let gain = score.max((1.0 + delta) * reference) / searched - 1.0;
            rows[index]["reoptimizedDelta"] = json!(gain);
            if !complete { rows[index]["reoptimizedDeltaIsLowerBound"] = json!(true); }
            rows[index]["fatal"] = json!(gain < -LOSS);
        }
        for (index, line, _) in suspects {
            if rows[index]["fatal"] == true {
                rows[index]["fatalAbove"] = json!(session.threshold(line, reference, searched)?);
            }
        }
    }
    let delta = |row: &Value| row["reoptimizedDelta"].as_f64().unwrap_or(num(&row["fixedDelta"]));
    rows.sort_by(|a, b| delta(a).total_cmp(&delta(b)));
    Ok(Assessment { rows, work:session.work, memo_hits:searches.hits.get() + fixed_checks.hits.get() })
}
