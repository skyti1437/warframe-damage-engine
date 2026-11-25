//! 将导出的条件效果连接到内核事件，随实时层数重算属性。

use super::{arcane, arcane_status, loadout, num, rows, text, truth, Request};
use super::weapon::{condition_kind, Effect, Evolution, Trigger};
use serde_json::Value;
use wfcore_lib::replay::{SourceInput, Types};

pub(super) struct Effects {
    pub static_terms: Vec<Effect>,
    pub sources: Vec<Trigger>,
    pub tier_terms: Vec<(String, f64, Vec<Effect>)>,
    pub notes: Vec<String>,
    pub double_tap: Option<(f64, f64, f64)>,
}

pub(super) fn condition_text(kind: &str) -> &str {
    match kind {
        "on-reload-empty" => "弹匣打空后开始换弹时触发。",
        "on-reload-empty-complete" => "弹匣打空后完成换弹时触发。",
        "on-burst-hit" => "一轮连射全部命中时触发。",
        "on-headshot" => "命中头部弱点时触发。",
        "on-headshot-kill" => "头部弱点命中造成击杀时触发。",
        "on-kill" => "击杀时触发。",
        "on-primary-kill" => "主武器击杀时触发。",
        "on-melee-kill" => "近战击杀时触发。",
        "on-crit" => "暴击命中时触发。",
        "on-hit" => "命中时触发。",
        "on-reload" => "开始换弹时触发。",
        "on-reload-complete" => "完成换弹时触发。",
        "proc-DT_FIRE" => "施加火焰异常时触发。",
        "proc-DT_FREEZE" => "施加冰冻异常时触发。",
        "proc-DT_POISON" => "施加毒素异常时触发。",
        "proc-DT_ELECTRICITY" => "施加电击异常时触发。",
        "on-proc" => "施加异常状态时触发。",
        "on-noncrit-no-proc" => "伤害实例既未暴击、也未施加异常状态时触发。",
        "on-close-kill" => "击杀距离不超过 5 米的目标时触发。",
        "on-overguard-broken" => "目标超宏被击破时触发。",
        "on-self-shield-broken" => "玩家自身护盾被击破时触发。",
        "on-self-overguard-broken" => "玩家自身超宏被击破时触发。",
        "on-self-shield-hit" => "玩家自身护盾受到伤害时触发。",
        "on-fire" => "扣动扳机射击时触发。",
        "on-punchthrough" => "同次射击穿透并命中后续目标时触发。",
        "on-glaive-throw" => "战刃连续投掷时叠加（循环投掷时第二投起每投都是连续投掷），只加成投掷直击。",
        other => other,
    }
}

fn remove_effects(installed: &mut Vec<Effect>, terms: &[Effect]) {
    installed.retain(|e| !terms.iter().any(|c| c.source == e.source && c.upgrade == e.upgrade
        && c.operation == e.operation && c.dtype == e.dtype));
}

pub(super) fn source(record: &Value, condition: &Value, index: usize, kind: &str, mut effects: Vec<Effect>,
          arcane: Option<&arcane::Arcane<'_>>, req: &Request<'_>, route: Option<&str>, infinite: &Value) -> Trigger {
    let path = condition["sourcePath"].as_str().unwrap_or(text(&record["path"]));
    let native = &condition["nativeEvent"];
    let cap = loadout::cap(condition) as f64;
    let state = |key| req.scenario.state.and_then(|s| s.get(key)).unwrap_or(&Value::Null);
    let self_break = matches!(kind, "on-self-shield-broken" | "on-self-overguard-broken") ||
        (truth(&native["actorOnly"]) && matches!(kind, "on-shield-broken" | "on-overguard-broken"));
    let force_break = self_break && state("selfShieldBreakForced") != false;
    let fixed = force_break || arcane.is_some_and(|card| card.stacks.is_some() || card.uptime.is_some()) ||
        (req.scenario.policy == "assumed_max" && !truth(&condition["energyGate"]) && !truth(&condition["toggleOn"])
            && !kind.starts_with("on-self-") && !matches!(kind, "on-equip-from-primary" | "on-warframe-ability" |
                "on-three-enemy-punchthrough" | "on-onos-damage-condition"));
    let mut initial = if fixed { arcane.and_then(|card| card.stacks).map_or(cap, |v| (v as f64).min(cap)) *
        arcane.and_then(|card| card.uptime).unwrap_or(1.0) } else { 0.0 };
    let energy = num(&condition["energyGate"]);
    if !fixed {
        if let Some(key) = condition["initialStateKey"].as_str() { initial = num(state(key)).min(cap); }
        if energy > 0.0 { initial = (num(state("energySpent")) / energy).floor().min(cap); }
    }
    let hold = truth(&condition["hold"]) || truth(&condition["isInfinite"]) ||
        rows(infinite).iter().any(|v| v == path) || infinite.get(path).is_some();
    let duration = if fixed || hold || kind == "on-glaive-throw" { 0.0 } else { num(&condition["duration"]) };
    let mut chance = if fixed { 0.0 } else { condition.get("chance").map_or_else(
        || record["trigger"]["chance"].as_f64().unwrap_or(1.0), num) };
    if truth(&condition["meleeRoutes"]) && !route.is_some_and(|r| rows(&condition["meleeRoutes"]).iter().any(|v| v == r)) {
        chance = 0.0;
        initial = 0.0;
    }
    if kind == "on-glaive-throw" {
        for effect in &mut effects {
            if effect.upgrade == "WEAPON_DAMAGE_AMOUNT" { effect.set_upgrade("RUNTIME_THROW_DAMAGE"); }
        }
    }
    let no_refresh = truth(&condition["noRefresh"]) && !fixed;
    let decay = if condition["stackMode"] == "SM_DECAY" { "lose_one_and_reset" }
        else if !fixed && (condition["stackMode"] == "per-stack" || no_refresh) { "per-stack" }
        else if cap == 1.0 && duration > 0.0 && !truth(&condition["toggleOn"]) && !truth(&condition["energyGate"])
            && !truth(&condition["initialStateKey"]) && !matches!(kind, "on-consecutive-headshot" | "on-burst-hit") { "union" }
        else { "reset" };
    let reset_on = if native.is_object() {
        ["on-reload", "on-transform", "on-holster", "on-body-hit"].into_iter().filter(|key|
            rows(&condition["resetOn"]).iter().chain(rows(&native["resetOn"])).any(|v| v == *key)).map(str::to_owned).collect()
    } else { rows(&condition["resetOn"]).iter().filter_map(Value::as_str).map(str::to_owned).collect() };
    let id = format!("{path}:{index}:0");
    let label = [&record["nameZh"], &record["nameEn"]].into_iter().find_map(|v| v.as_str().filter(|s| !s.is_empty()))
        .map_or_else(|| record["id"].as_str().map_or_else(|| id.clone(), |name| format!("灵化 {name}")), str::to_owned);
    let required_count = condition["requiredCount"].as_f64().or_else(|| native["requiredCount"].as_f64()).unwrap_or(1.0);
    let required_time = condition["requiredTime"].as_f64().or_else(|| native["requiredTime"].as_f64());
    let free_ammo = !fixed && effects.iter().any(|e| e.upgrade == "WEAPON_AMMO_CONSUME_RATE"
        && matches!(e.operation.as_str(), "SET" | "SET_BASE") && e.value <= 0.0);
    Trigger { id, label, kind: kind.into(), condition_text: condition["conditionText"].as_str().unwrap_or_else(|| condition_text(kind)).into(),
        effects, cap, duration, chance, fixed, hold, victim_proc_type: native["victimProcType"].as_str().map(str::to_owned),
        replay: SourceInput { initial_stacks: initial, stacks: initial, no_refresh, reset_on, free_ammo,
            required_count, required_time, actor_only: truth(&native["actorOnly"]), decay: decay.into(),
            energy_threshold: energy,
            expires: if !fixed && initial > 0.0 && energy > 0.0 { num(&condition["duration"]) } else { f64::INFINITY },
            minimum_consecutive: condition["minimumConsecutive"].as_i64().unwrap_or(0),
            duration_per_tier: num(&condition["durationPerTier"]), charges: num(&condition["charges"]),
            toggle_on: condition["toggleOn"].as_str().map(str::to_owned),
            consume_on: condition["consumeOn"].as_str().map(str::to_owned),
            lock_on: condition["lockOn"].as_str().map(str::to_owned),
            ..SourceInput::default() } }
}

impl Effects {
    pub fn prepare(req: &Request<'_>, cards: &[loadout::Card<'_>], arcanes: &arcane::Equipped<'_>,
                   effects: Vec<Effect>, evolution: &Evolution, route: Option<&str>, infinite: &Value, innate: &[Value]) -> Self {
        let double_tap = cards.iter().find_map(|card| super::mod_runtime::double_tap(card.raw, card.rank));
        let mut result = Self { static_terms: effects, sources: Vec::new(), tier_terms: Vec::new(), notes: Vec::new(), double_tap };
        let mut innate_start = None;
        for (record, rank, arcane) in cards.iter().map(|c| (c.raw, c.rank, None))
                        .chain(arcanes.cards.iter().map(|c| (c.record, c.rank, Some(c))))
            .chain(innate.iter().filter(|row| !truth(&row["fireModes"]) || rows(&row["fireModes"]).iter().any(|v| v == req.mode))
                .map(|row| (row, 0, None))) {
            if innate.iter().any(|source| std::ptr::eq(source, record)) && innate_start.is_none() {
                innate_start = Some((result.sources.len(), result.notes.len()));
            }
            if arcane::runtime_effect(record, rank) || arcane_status::status(record, rank).is_some() || arcane_status::compression(record, rank).is_some() { continue; }
            if arcane_status::enervate(record, rank) {
                result.static_terms.retain(|e| e.source != text(&record["path"]));
                if req.scenario.policy == "assumed_max" && arcane.is_some_and(|a| a.stacks.is_none()) {
                    result.notes.push("enervate-has-no-maximum-using-events".into());
                }
                continue;
            }
            let crux = arcane_status::crux(record);
            let path = text(&record["path"]);
            for (index, condition) in crux.as_ref().map(std::slice::from_ref)
                .unwrap_or_else(|| rows(&record["conditionalEffects"])).iter().enumerate() {
                let from = condition["sourcePath"].as_str().unwrap_or(path);
                if truth(&condition["runtimeScripts"]) {
                    result.notes.push(format!("{from}:runtime-condition-unmodeled"));
                    continue;
                }
                let mut terms = loadout::terms(&condition["effects"], from, rank, 1.0);
                if terms.is_empty() { continue; }
                if !truth(&condition["conditions"]) && !truth(&condition["conditionTag"]) &&
                    condition_kind(condition).is_none() { continue; }
                if route.is_some() && loadout::combo_gate(condition) {
                    let cap = condition["maxStacks"].as_u64().unwrap_or(12);
                    let key = format!("__comboTier:{cap}");
                    remove_effects(&mut result.static_terms, &terms);
                    if let Some((_, _, group)) = result.tier_terms.iter_mut().find(|(k, _, _)| k == &key) {
                        group.append(&mut terms);
                    } else {
                        result.tier_terms.push((key, cap as f64, terms));
                    }
                    continue;
                }
                if let Some(kind) = condition_kind(condition) {
                    remove_effects(&mut result.static_terms, &terms);
                    if req.scenario.aiming || !terms.iter().any(|e| e.required.iter().any(|m| m == "PM_AIM")) {
                        result.sources.push(source(record, condition, index, kind, terms, arcane, req, route, infinite));
                    }
                } else if arcane.is_some_and(|a| a.stacks.is_some() || a.uptime.is_some()) {
                    result.notes.push(format!("{from}:event-condition-fixed-by-user"));
                } else {
                    remove_effects(&mut result.static_terms, &terms);
                    result.notes.push(format!("{from}:event-condition-unmodeled"));
                }
            }
        }
        let (source_at, notes_at) = innate_start.unwrap_or((result.sources.len(), result.notes.len()));
        result.sources.splice(source_at..source_at, evolution.sources.iter().cloned());
        result.notes.splice(notes_at..notes_at, evolution.replay_notes.iter().cloned());
        result
    }

    pub fn input(&self, types: &mut Types) -> Vec<SourceInput> {
        self.sources.iter().enumerate().map(|(id, source)| {
            let mut input = source.replay.clone();
            input.id = id;
            input.kind = Some(source.kind.clone());

            input.max_stacks = source.cap;
            input.duration = if source.fixed { 0.0 } else { source.duration };
            input.per_event = if source.fixed { 0.0 } else { source.chance };
            input.hold = source.hold;
            if input.decay.is_empty() {
                input.decay = "reset".into();
                input.stacks = if source.fixed { source.cap } else { 0.0 };
                input.initial_stacks = input.stacks;
                input.required_count = 1.0;
            }
            if let Some(name) = &source.victim_proc_type { input.victim_proc_type = Some(types.id(name)); }
            input
        }).collect()
    }
}
