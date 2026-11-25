use super::{cards::Card, evaluate::{metrics, Evaluation, Evaluator}, forma, identity, json, num, rows, text, truth,
    projectiles, search::Search, weapon, Map, Request, SimResult, Value};
use std::collections::{BTreeSet, HashSet};

fn distinct(notes: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    notes.into_iter().filter(|note| seen.insert(note.clone())).collect()
}

pub(super) fn payload(eval: &Evaluator<'_>, search: &Search<'_>, best: &Evaluation, special: Option<&Card<'_>>,
    exilus: Value, report: Value, capacity_limit: i64, rejected: usize, weapon_meta: &Value,
    selected_target: Option<&Value>, catalog: &super::SimCatalog, analysis: bool) -> SimResult<Value> {
    let req = eval.req;
    let scenario = &req.scenario;
    let stance_damage = eval.stance["dpsMultiplier"].as_f64().unwrap_or(1.0);
    let stance_status = eval.stance["statusRateMultiplier"].as_f64().unwrap_or(1.0);
    let (effects, skipped) = eval.terms(&best.cards, best.riven_slot, special, true);
    let panel = eval.panel(req, &effects, &skipped, None)?;
    let (effects, skipped) = eval.terms(&best.cards, best.riven_slot, eval.fixed_exilus, true);
    let mut normal_req = req.clone();
    normal_req.scenario.heavy = false;
    let normal = eval.panel(&normal_req, &effects, &skipped, Some(panel.count))?;
    let mut heavy_req = req.clone();
    heavy_req.scenario.heavy = true;
    let heavy = eval.panel(&heavy_req, &effects, &skipped, Some(panel.count))?;
    let projectile_preview = projectiles::preview(&weapon::Weapon::new(eval.data, &normal_req), &normal_req, &effects, &skipped, panel.count)?;
    let mut support = req.weapon["heavyAttack"]["support"].clone();
    let mut build_notes: BTreeSet<_> = normal.fields.unsupported.iter().chain(&heavy.fields.unsupported)
        .chain(&panel.fields.unsupported).cloned().collect();
    build_notes.extend(rows(&projectile_preview["unsupported"]).iter().filter_map(Value::as_str).map(str::to_owned));
    if num(&projectile_preview["totals"]["averageHit"]["minimum"]) > 0.0 {
        build_notes.remove("projectile-component-damage-unapplied");
    }
    if search.scope.event {
        if let Some(schedule) = eval.schedule {
            build_notes.extend(schedule.notes.iter().cloned());
        }
        build_notes.extend(eval.evolution.replay_notes.iter().cloned());
    }
    if !support.is_null() {
        let reasons = rows(&support["reasons"]).iter().map(|v| text(v).to_owned()).chain(build_notes.iter().cloned()).collect();
        support["reasons"] = json!(distinct(reasons));
    }
    let mut unsupported = build_notes.clone();
    unsupported.extend(eval.evolution.notes.iter().cloned());
    if req.melee || (exilus["sourceKind"] == "tennokai" && exilus["catalogJoined"] != true) {
        unsupported.insert("tennokai-opportunities-not-modeled".into());
    }
    let count = eval.set_counts(&best.cards, None);
    let mods: Vec<_> = best.cards.iter().enumerate().map(|(slot, i)| {
        let card = &eval.cards[*i];
        let pieces = count.get(card.set.as_str()).copied().unwrap_or(0);
        let multiplier = eval.multiplier(card, pieces);
        let display: Vec<_> = card.effects.iter().filter(|effect| weapon::is_weapon_effect(&effect.upgrade)
            && effect.rejection(&normal_req, false).is_none()).map(|effect| {
            let mut effect = effect.clone();
            effect.value *= multiplier;
            if effect.upgrade == "WEAPON_DAMAGE_IF_VICTIM_PROC_ACTIVE" { effect.value *= panel.count as f64; }
            effect.display()
        }).collect();
        let steady: Vec<_> = rows(&card.raw["conditionalEffects"]).iter().filter(|row| row["maxStacks"].is_i64()
            && text(&row["conditionTag"]).to_ascii_lowercase().contains("killcondition"))
            .map(|row| json!({"conditionTag": row["conditionTag"], "stacks": row["maxStacks"]})).collect();
        let mut row = super::super::merge(identity(card.raw), json!({
            "rank":card.rank, "maxRank":card.raw["maxRank"], "compatibility":card.raw["compatibility"],
            "slot":slot + usize::from(best.riven_slot.is_some_and(|position| slot >= position)),
            "compatibilityTags":rows(&card.raw["compatibilityTags"]), "compatible":true, "displayEffects":display,
        }));
        if pieces > 0 { row["setPieces"] = json!(pieces); }
        if !steady.is_empty() { row["steadyStacks"] = json!(steady); }
        row
    }).collect();
    let forma = forma::summary(eval, best, &exilus);
    let heavy_metrics = metrics(&heavy.wire, &heavy.heavy);
    let mut build = if analysis { projectiles::metrics(&normal.wire, &normal.heavy, &projectile_preview) }
        else { metrics(&panel.wire, &panel.heavy) };
    build.as_object_mut().unwrap().extend(json!({
        "mods":mods, "includesRiven":eval.riven.is_some(), "victimStatusCount":panel.count,
        "heavyDamage":heavy_metrics["heavyDamage"], "heavyAttackDps":heavy_metrics["heavyDamage"],
        "heavyComponents":rows(&heavy.heavy["heavy_components"]), "heavySupport":support,
        "unsupported":build_notes, "damageTypes":panel.wire["damageTypes"], "archetype":"hybrid",
        "objectiveDps":best.score, "objectiveScore":best.score, "stance":eval.stance, "exilus":exilus, "forma":forma,
    }).as_object().unwrap().clone());
    if let Some(riven) = eval.riven {
        build["riven"] = riven.raw.clone();
        build["riven"]["slot"] = json!(best.riven_slot);
        build["rivenLines"] = json!(riven.lines);
    }
    if !projectile_preview.is_null() {
        if num(&projectile_preview["totals"]["averageHit"]["minimum"]) > 0.0 {
            build["damageBasis"] = json!("projectileEvents.minimum");
        }
        build["projectileEvents"] = projectile_preview;
    }
    if scenario.spread { build["hitProbability"] = panel.wire["hitProbability"].clone(); }

    let mut modded = Map::new();
    for key in ["damageTypes", "innateDamageTypes", "innateElementCandidates", "totalDamage", "averageHit",
        "sustainedDps", "burstDps", "critChance", "critMultiplier", "statusChance", "multishot", "fireRate", "magazine",
        "reloadTime", "firstShotDamageBonus", "firstShotHit", "statusConditionDamageMultiplier", "coAssumedCount"] {
        modded.insert(key.into(), panel.wire[key].clone());
    }
    let candidates: BTreeSet<_> = rows(&req.weapon["innateElementCandidates"]).iter().map(text).collect();
    modded.insert("innateElementCandidates".into(), json!(candidates));
    for (key, source) in [("spreadMultiplier","spread_multiplier"), ("recoilMultiplier","recoil_multiplier"), ("ammoMaxMultiplier","ammo_max_multiplier")] {
        modded.insert(key.into(), panel.wire[source].clone());
    }
    for key in ["sustainedDps", "burstDps", "firstShotHit"] { modded[key] = json!(num(&modded[key]) * stance_damage); }
    if req.melee {
        for (key, source) in [("initialCombo","initial_combo"), ("comboDuration","combo_duration"), ("heavyAttackEfficiency","heavy_attack_efficiency")] {
            modded.insert(key.into(), panel.wire[source].clone());
        }
        modded.insert("heavyDamage".into(), panel.heavy["heavy_damage"].clone());
        modded.insert("stanceMultiplier".into(), json!(stance_damage));
    }

    let mut state = scenario.state.cloned().unwrap_or_default();
    for key in ["frameArmor", "frameShield", "energyPool", "sprintMultiplier", "energySpent", "energySpendPerSecond",
        "selfShieldBreakAt", "selfShieldBreakInterval", "selfOverguardBreakAt", "selfOverguardBreakInterval",
        "selfShieldHitAt", "selfShieldHitInterval", "equipFromPrimaryAt", "equipFromPrimaryInterval",
        "abilityUseAt", "abilityUseInterval", "meleeTargetDistance"] {
        if let Some(value) = state.get_mut(key) { *value = json!(num(value)); }
    }
    if state.get("zymosChildOverlap").is_some_and(|v| v == "off") { state.remove("zymosChildOverlap"); }
    if !truth(state.get("meleeStance").unwrap_or(&Value::Null)) && truth(&eval.stance["path"]) {
        state.insert("meleeStance".into(), eval.stance["path"].clone());
    }
    let nested = |key| req.body.get("scenario").and_then(|v| v.get(key)).or_else(|| req.body.get(key)).unwrap_or(&Value::Null);
    let mut scenario_wire = json!({"aiming":scenario.aiming, "incarnonCycle":scenario.cycle, "heavyAttack":scenario.heavy,
        "triggerPolicy":scenario.policy, "distance":scenario.distance, "cloaked":scenario.cloaked,
        "nearestEnemyDistance":scenario.nearest, "comboTier":scenario.combo,
        "detonationDelaySeconds":num(nested("detonationDelaySeconds")),
        "detonationBatchSize":nested("detonationBatchSize").as_u64().unwrap_or(1),
        "combatState":state, "includeArcanes":search.scope.include_arcanes,
    });
    if scenario.spread { scenario_wire["modelSpread"] = json!(true); }
    let enhancements = &req.enhancements;
    let forced: Map<_, _> = req.forced.iter().map(|(key, value)| (key.clone(), json!(value))).collect();
    let hit_location = if req.body.get("hitLocation").is_some_and(|v| v == "head") && req.aim == 1.0 { "head" } else { "body" };
    let context = json!({"level":req.level, "armour_strip":req.strip, "elemental_armour_strip":req.elemental_strip,
        "steel_path":req.steel, "enemy_enhancements":enhancements, "hit_location":hit_location, "stanceMultiplier":stance_damage,
        "statusRateMultiplier":stance_status, "windowSeconds":req.window, "headshotChance":req.aim, "forcedProcs":forced});
    let mut special_slots = json!({"exilus":req.exilus_polarity});
    if req.melee { special_slots["stance"] = req.stance_polarity.clone(); }
    let mut skipped = notes(eval, search, capacity_limit, rejected, &report, best, catalog);
    skipped.extend(panel.fields.skipped.iter().cloned());
    let alternate = alternate_modes(eval, best)?;
    let target = if req.target["path"] == "__custom_enemy__" { &req.target }
        else { catalog.enemies.get(text(&req.target["path"])).or(selected_target).unwrap_or(&req.target) };
    let scope = search.scope;
    let incarnon_policy = weapon::Evolution::resolve(eval.data, req)?.policy;
    Ok(json!({
        "buildLabel":super::super::BUILD_LABEL, "schemaVersion":super::super::SCHEMA_VERSION,
        "weapon":identity(req.weapon), "weaponType":weapon_meta["weaponType"], "fireMode":req.mode,
        "metric":scope.metric, "scoringModel": if scope.event { "loadout-events-v1" } else { "panel-heavy" },
        "scoreMetric":if scope.event { "killsPerMinute" } else { scope.metric.as_str() },
        "evaluationScenario":scenario_wire, "evaluationContext":context, "evaluationTarget":identity(target),
        "buildArchetype":"hybrid", "archetypeComparison":null, "slotCount":req.slot_count, "elementTarget":scope.element,
        "innateElement":req.weapon["innateElements"][0], "innateElementCandidates":rows(&weapon_meta["innateElementCandidates"]),
        "incarnonChoices":eval.evolution.choices, "incarnonPolicy":incarnon_policy,
        "stance":eval.stance, "exilus":exilus, "optimizeWithArcanes":scope.include_arcanes,
        "autoLoadout":if truth(&report) { report } else { Value::Null }, "specialSlotPolarities":special_slots,
        "forma":forma, "capacity":best.capacity, "capacityLimit":capacity_limit, "build":build, "moddedStats":modded,
        "alternateModes":alternate, "unsupported":unsupported, "skipped":distinct(skipped), "profile":scope.profile,
        "limitations":catalog.build_options["limitations"], "exalted":null,
    }))
}

fn alternate_modes(eval: &Evaluator<'_>, best: &Evaluation) -> SimResult<Vec<Value>> {
    let req = eval.req;
    let (effects, skipped) = eval.terms(&best.cards, None, None, false);
    let mut rows_out = Vec::new();
    for (index, mode) in rows(&req.weapon["fireModes"]).iter().enumerate() {
        if index == req.mode { continue; }
        let alternate = Request { mode: index, ..req.clone() };
        let row = json!({"fireMode": index, "trigger":text(&mode["trigger"]), "isAlternateFire":truth(&mode["isAlternateFire"])});
        let info = match eval.alternate_panel(&alternate, &effects, &skipped) {
            Ok(panel) => {
                let exported = Request { exported_mode_panel: true, ..alternate.clone() };
                let preview = projectiles::preview(&weapon::Weapon::new(eval.data, &exported), &exported, &effects, &skipped, 0)?;
                let unsupported: BTreeSet<_> = panel.fields.unsupported.iter().map(String::as_str)
                    .chain(rows(&preview["unsupported"]).iter().filter_map(Value::as_str)).collect();
                json!({"available":true, "metrics":projectiles::metrics(&panel.wire, &panel.heavy, &preview),
                    "unsupported":unsupported, "skipped":distinct(panel.fields.skipped)})
            }
            Err(issue) => json!({"available":false, "issue":issue}),
        };
        rows_out.push(super::super::merge(row, info));
    }
    Ok(rows_out)
}

fn notes(eval: &Evaluator<'_>, search: &Search<'_>, capacity: i64, rejected: usize, report: &Value,
    best: &Evaluation, catalog: &super::SimCatalog) -> Vec<String> {
    let scope = search.scope;
    let (label, detail) = match scope.profile {
        "ultra" => ("超快", "配装搜索同快速档"),
        "fast" => ("快速", "beam 12/32，无爬山"),
        "deep" => ("深度", "beam 24/56，12 轮全邻域爬山"),
        _ => ("标准", "beam 24/40，3 轮爬山"),
    };
    let slot_count = eval.req.slot_count;
    let window = general_seconds(eval.req.window);
    let mut notes = vec![
        format!("optimizer-assumption: {slot_count} ordinary slots, {capacity} capacity after stance, Catalyst and matching ordinary-slot Forma"),
        format!("optimizer-objective: 当前目标/爆头率/瞄准/灵化循环的 {window} 秒事件 KPM；未选择有效目标时回退标准靶"),
        format!("optimizer-profile: {label}档（{detail}）；元素小生境 beam + 开放/病毒/腐蚀面板代理种子"),
        format!("optimizer-search: profile {}, {} candidate mods, {rejected} unsupported mods rejected", scope.profile, eval.cards.len()),
    ];
    let names = |paths: &[String]| paths.iter().map(|path| catalog.mod_at(path).and_then(|row| row["nameZh"].as_str()).unwrap_or(path))
        .collect::<Vec<_>>().join("、");
    let mut details = Vec::new();
    if search.faction_excluded != 0 { details.push(format!("屏蔽派系歧视卡 {} 张", search.faction_excluded)); }
    if !scope.required.is_empty() { details.push(format!("必选 {}", names(&scope.required))); }
    if !scope.forbidden.is_empty() { details.push(format!("禁选 {}", names(&scope.forbidden))); }
    if scope.min.is_some() || scope.max.is_some() {
        details.push(format!("数量 {}–{}", scope.min.unwrap_or(0), scope.max.unwrap_or(slot_count)));
    }
    if !details.is_empty() { notes.push(format!("optimizer-scope: {}", details.join(" · "))); }
    if scope.include_arcanes {
        if !eval.arcanes.cards.is_empty() || !eval.external.is_empty() {
            notes.push("optimizer-arcanes: 已配置赋能按当前触发策略参与每个候选的同一事件回放评分".into());
        }
        notes.push(format!("optimizer-trigger-policy: {}；显式层数/覆盖率按手动设置", eval.req.scenario.policy));
    } else {
        notes.push("optimizer-arcanes: 求解已排除赋能；已装备赋能仍参与手动回放，两者分数不可直接比较".into());
    }
    notes.extend(eval.arcane_panel.notes.iter().cloned());
    if let Some(riven) = eval.riven { notes.extend(riven.notes.iter().cloned()); }
    if report["incarnon"].is_object() {
        let route = eval.evolution.choices.as_object().into_iter().flatten().map(|(_, v)| text(v)).collect::<Vec<_>>().join("/");
        let info = &report["incarnon"];
        let searches = info["searches"].as_u64().unwrap_or(0);
        let work = if searches == 0 { "，按当前配装精确比较".to_owned() }
            else { format!("，{searches} 次配装搜索，{:.1} 秒", num(&info["seconds"])) };
        let default = num(&info["defaultScore"]);
        let gain = if default == 0.0 { String::new() } else { format!("，比原路线 {:+.1}%", 100.0 * (best.score / default - 1.0)) };
        notes.push(format!("optimizer-auto-incarnon: 自动选择灵化路线 {route}（{} 条路线筛选{work}{gain}）", info["routes"]));
    }
    if report["arcane"].is_object() {
        let info = &report["arcane"];
        let names = rows(&info["chosen"]).iter().map(|row| row["nameZh"].as_str().unwrap_or(text(&row["path"]))).collect::<Vec<_>>();
        let names = if names.is_empty() { "不装武器赋能".into() } else { names.join("、") };
        let base = num(&info["baseScore"]);
        let gain = if base == 0.0 { String::new() } else { format!("，比不装武器赋能 {:+.1}%", 100.0 * (best.score / base - 1.0)) };
        notes.push(format!("optimizer-auto-arcane: 自动选择武器赋能：{names}（按当前触发策略评估{gain}）"));
    }
    if let Some(kind) = scope.element["damageType"].as_str() {
        notes.push(format!("optimizer-element-target: final build must contain {kind}"));
    }
    notes
}

// Python's general format uses six significant digits and drops trailing zeros.
fn general_seconds(value: f64) -> String { general_number(value, 6) }

pub(super) fn general_number(value: f64, significant: usize) -> String {
    let decimals = significant - 1;
    let rounded = format!("{value:.decimals$e}");
    let (mantissa, exponent) = rounded.split_once('e').expect("scientific notation has an exponent");
    let exponent: i32 = exponent.parse().expect("valid exponent");
    let trim = |mut output: String| {
        if output.contains('.') { output.truncate(output.trim_end_matches('0').trim_end_matches('.').len()); }
        output
    };
    if (-4..significant as i32).contains(&exponent) {
        trim(format!("{value:.places$}", places = (decimals as i32 - exponent) as usize))
    } else {
        format!("{}e{exponent:+03}", trim(mantissa.to_owned()))
    }
}
