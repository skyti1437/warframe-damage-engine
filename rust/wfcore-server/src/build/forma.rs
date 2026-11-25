use super::{cards, json, loadout, rows, text, truth, Value};
use super::evaluate::{Evaluation, Evaluator};
use super::super::request::polarity;
use indexmap::IndexMap;

struct Item {
    polarity: Value,
    drain: i64,
    elemental: bool,
}

#[derive(Clone, Copy)]
struct Plan {
    drain: i64,
    formed: u8,
    placement: [usize; 8],
}

impl Default for Plan {
    fn default() -> Self {
        Self { drain: 0, formed: 0, placement: [usize::MAX; 8] }
    }
}

fn insert(layer: &mut IndexMap<(usize, u8, usize, usize), Plan>,
    key: (usize, u8, usize, usize), plan: Plan) {
    let incumbent = layer.entry(key).or_insert(plan);
    if plan.drain < incumbent.drain { *incumbent = plan; }
}

pub(super) fn summary(eval: &Evaluator<'_>, best: &Evaluation, exilus: &Value) -> Value {
    plan(eval, best, exilus, false, false)
}

pub(super) fn plan(eval: &Evaluator<'_>, best: &Evaluation, exilus: &Value, placement: bool, fixed_exilus: bool) -> Value {
    let mut items: Vec<_> = best.cards.iter().map(|i| {
        let card = &eval.cards[*i];
        Item { polarity: polarity(&card.raw["polarity"]), drain: card.drain, elemental: card.elemental != 0 }
    }).collect();
    if let Some((riven, slot)) = eval.riven.zip(best.riven_slot) {
        items.insert(slot, Item { polarity: polarity(&riven.polarity), drain: riven.drain(), elemental: cards::elemental(&riven.effects) != 0 });
    }
    let chain: Vec<_> = (0..items.len()).filter(|i| items[*i].elemental).collect();
    let free: Vec<_> = (0..items.len()).filter(|i| !items[*i].elemental).collect();
    let full = ((1u16 << free.len()) - 1) as u8;
    let mut layer = IndexMap::from([((0, 0, 0, 0), Plan::default())]);
    for slot in 0..8 {
        let native = rows(&eval.req.weapon["polarities"]).get(slot).unwrap_or(&Value::Null);
        let mut next = IndexMap::new();
        for ((element, mask, formas, moves), plan) in layer {
            let remaining = chain.len() - element + (full ^ mask).count_ones() as usize;
            if remaining < 8 - slot { insert(&mut next, (element, mask, formas, moves), plan); }
            for (index, element, mask) in chain.get(element).into_iter().map(|i| (*i, element + 1, mask))
                .chain(free.iter().enumerate().filter(|(bit, _)| mask & (1 << bit) == 0)
                    .map(|(bit, i)| (*i, element, mask | (1 << bit)))) {
                if (placement || fixed_exilus) && chain.len() - element + (full ^ mask).count_ones() as usize > 7 - slot {
                    continue;
                }
                let item = &items[index];
                let used = loadout::adjust_drain(item.drain, &item.polarity, native).0;
                let matched = loadout::adjust_drain(item.drain, &item.polarity, &item.polarity).0;
                let moved = moves + usize::from(index != slot);
                let mut placed = plan;
                placed.placement[slot] = index;
                placed.drain += used;
                insert(&mut next, (element, mask, formas, moved), placed);
                if truth(&item.polarity) && used != matched {
                    placed.formed |= 1 << slot;
                    placed.drain += matched - used;
                    insert(&mut next, (element, mask, formas + 1, moved), placed);
                }
            }
        }
        layer = next;
    }
    let stance = eval.stance;
    let reference = placement || fixed_exilus;
    let gain = if reference && (stance["builtIn"] == true || stance["path"].is_null()) { 0 }
        else if reference { stance["nativeCapacityGain"].as_i64().or_else(|| stance["capacityGain"].as_i64()).unwrap_or(0) }
        else { stance["capacityGain"].as_i64().unwrap_or(0) };
    let matched_gain = stance["matchedCapacityGain"].as_i64().unwrap_or(gain);
    let stance_options = usize::from(stance["polarityRelation"] != "matched" && matched_gain > gain && stance["builtIn"] != true);
    let exilus_raw = exilus["rawDrain"].as_i64().unwrap_or(0);
    let exilus_current = loadout::adjust_drain(exilus_raw, &exilus["polarity"], &exilus["slotPolarity"]).0;
    let exilus_matched = loadout::adjust_drain(exilus_raw, &exilus["polarity"], &exilus["polarity"]).0;
    let exilus_options = usize::from(!fixed_exilus && exilus_matched < exilus_current);
    let mut best: Option<(_, Plan, usize, usize, bool)> = None;
    for ((element, mask, formas, moves), plan) in layer {
        if element != chain.len() || mask != full { continue; }
        for stance_count in 0..=stance_options {
            for exilus_count in 0..=exilus_options {
                let drain = plan.drain + if stance_count == 1 { gain - matched_gain } else { 0 }
                    + if exilus_count == 1 { exilus_matched } else { exilus_current };
                let fits = drain <= 60 + gain;
                let rank = (!fits, if fits { 0 } else { drain }, formas + stance_count + exilus_count, moves, drain);
                if best.as_ref().is_none_or(|(prior, ..)| rank < *prior) {
                    best = Some((rank, plan, stance_count, exilus_count, fits));
                }
            }
        }
    }
    let (rank, plan, stance, exilus, fits) = best.expect("Forma plan always has a native placement");
    let mut regular = 0;
    let mut umbra = 0;
    let mut slots = serde_json::Map::new();
    for slot in 0..8 {
        if plan.formed & (1 << slot) == 0 { continue; }
        let polarity = &items[plan.placement[slot]].polarity;
        if matches!(text(polarity), "AP_UMBRA" | "AP_ULTIMATE") { umbra += 1; } else { regular += 1; }
        slots.insert(slot.to_string(), polarity.clone());
    }
    let mut summary = json!({"regular": regular, "umbra": umbra, "stance": stance, "exilus": exilus,
        "total": regular + umbra + stance + exilus, "fits": fits, "slots": slots});
    if placement {
        let placements: Vec<_> = plan.placement.into_iter().enumerate().map(|(slot, origin)| {
            let Some(item) = items.get(origin) else { return Value::Null; };
            let native = rows(&eval.req.weapon["polarities"]).get(slot).map(polarity).unwrap_or(Value::Null);
            let slot_polarity = if plan.formed & (1 << slot) != 0 { &item.polarity } else { &native };
            json!({"origin": origin, "polarity": slot_polarity,
                "drain": loadout::adjust_drain(item.drain, &item.polarity, slot_polarity).0})
        }).collect();
        let delta = if stance == 1 { matched_gain - gain } else { 0 };
        summary["placement"] = json!(placements);
        summary["drain"] = json!(rank.4 + delta);
        summary["limit"] = json!(60 + gain + delta);
    }
    summary
}
