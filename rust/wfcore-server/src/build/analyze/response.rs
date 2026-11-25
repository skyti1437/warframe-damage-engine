use super::{context, json, num, percent, riven_traits, rows, text, truth, Evaluation, Evaluator, HardnessNode,
    Input, Metric, Search, SimCatalog, SimResult, Value, CALIBRATION_VERSION, METRIC};
use super::recommendation::{self, Recommendation};
use crate::data::{BUILD_LABEL, SCHEMA_VERSION};
use crate::simulate::build::{forma, projectiles, response as build_response, weapon};
use std::cell::Cell;
use std::collections::{BTreeSet, HashSet};
use std::time::Instant;

pub(super) struct ResultContext<'a, 'b> {
    pub input: &'a Input<'a>,
    pub catalog: &'a SimCatalog,
    pub meta: &'a Value,
    pub exilus: &'a Value,
    pub search: &'a Search<'a>,
    pub integrated: &'a Evaluator<'b>,
    pub center: &'a Evaluator<'b>,
    pub baseline: &'a Evaluation,
    pub actual: &'a Evaluation,
    pub baseline_eval: &'a Evaluator<'b>,
    pub actual_eval: &'a Evaluator<'b>,
    pub route: Option<&'a Value>,
    pub metric: Metric<'a>,
    pub metric_name: &'a str,
    pub nodes: &'a [HardnessNode],
    pub work: &'a Cell<usize>,
    pub limited: &'a Cell<usize>,
    pub started: Instant,
    pub budget: Option<f64>,
    pub rejected: usize,
    pub unsupported: Vec<String>,
    pub recommendation: &'a Recommendation,
    pub cached_recommendation: bool,
}

pub(super) fn weapon_type<'a>(meta: &'a Value, weapon: &Value) -> &'a str {
    match text(&meta["weaponType"]) {
        "zaw" | "archmelee" => "melee",
        "kitgun" if weapon["slot"] == "secondary" => "pistol",
        "kitgun" => "rifle",
        "companion" => text(&meta["rivenType"]),
        kind => kind,
    }
}

pub(super) fn payload(context: ResultContext<'_, '_>) -> SimResult<Value> {
    let build_fields = context_fields(&context, context.exilus);
    let ResultContext { input, meta, exilus, search, integrated, center, baseline, actual,
        metric, metric_name, nodes, work, started, budget, rejected, recommendation, cached_recommendation,
        baseline_eval, actual_eval, route, limited, .. } = &context;
    let mut unsupported = context.unsupported.clone();
    let (metric, metric_name) = (*metric, *metric_name);
    let profile = search.scope.profile;
    let ultra = profile == "ultra";
    let fast = matches!(profile, "ultra" | "fast");
    let eval = *integrated;
    let req = eval.req;
    let capacity = eval.capacity_limit;
    let choices = &eval.evolution.choices;
    let requested_target = &input.requested_target;
    let target = context::evaluation_target(requested_target, req);
    let scenario = context::scenario(req, eval.stance);
    let calibration_status = if nodes.is_empty() { "pending" } else { "hardness-integrated" };
    let mut calibration = json!({"version": CALIBRATION_VERSION, "status": calibration_status,
        "requestedLevel": requested_target["level"], "evaluationLevel": target["level"],
        "iterations": [], "maxIterations": 5});
    if let Some(node) = nodes.get(1) {
        calibration["hardnessNodes"] = json!(nodes.iter().map(|node| json!({"ttkSeconds":node.seconds,
            "healthScale":node.scale, "weight":node.weight, "healthSpread":node.spread})).collect::<Vec<_>>());
        calibration["ttkRange"] = json!([1.0, 20.0]);
        calibration["ttkBasis"] = json!("no-riven-baseline-body-shot-ttk");
        calibration["aggregation"] = json!("geometric-mean-kpm");
        calibration["searchHealthScale"] = json!(node.scale);
    }
    let stance = if eval.stance["path"].is_string() { json!({
        "path":eval.stance["path"], "nameZh":eval.stance.get("nameZh").filter(|v| truth(v)).unwrap_or(&eval.stance["nameEn"]),
        "dpsMultiplier":num(&eval.stance["dpsMultiplier"]), "capacityGain":eval.stance["capacityGain"],
        "builtIn":eval.stance["builtIn"] == true
    }) } else { Value::Null };
    let heavy_route = &req.weapon["heavyAttack"];
    let throwable = req.melee && heavy_route["kind"] == "projectileRecall"
        && ["projectileDirect", "heavyRecall"].iter().all(|kind| rows(&heavy_route["components"]).iter().any(|part| part["kind"] == *kind));
    let mut hash_scenario = scenario.clone();
    hash_scenario["throwable"] = json!(throwable);
    hash_scenario["incarnonChoices"] = choices.clone();
    hash_scenario["arcanes"] = input.fields.get("arcanes").cloned().unwrap_or(Value::Null);
    let hash_stance = if stance.is_null() { Value::Null } else { json!({
        "path":stance["path"], "dpsMultiplier": num(&stance["dpsMultiplier"]),
        "statusRateMultiplier":num(&eval.stance["statusRateMultiplier"]), "capacityGain":stance["capacityGain"]
    }) };
    let hash_context = json!({"version":CALIBRATION_VERSION, "buildLabel":BUILD_LABEL,
        "weapon":req.weapon["path"], "fireMode":req.mode, "weaponType":weapon_type(meta, req.weapon),
        "innateElements":rows(&req.weapon["innateElements"]), "requestedTarget":requested_target,
        "evaluationTarget":target, "objectiveMetric":METRIC, "scoreMetric":metric_name,
        "profile":profile, "capacityLimit":capacity, "disposition":input.disposition, "scenario":hash_scenario,
        "stance":hash_stance, "targetCalibration":{"version":CALIBRATION_VERSION, "status":calibration_status,
            "requestedLevel":calibration["requestedLevel"], "evaluationLevel":calibration["evaluationLevel"]}});
    let context_hash = context::hash(&hash_context);
    let riven = Evaluator { riven: Some(&input.card), ..(*actual_eval).clone() };
    let ablation_eval = if ultra && !nodes.is_empty() { *center } else { eval };
    let with_ablation = Evaluator { riven: Some(&input.card), evolution:riven.evolution, ..ablation_eval.clone() };
    let ablation_score = if ultra && !nodes.is_empty() {
        let measured = with_ablation.fixed(&actual.cards, actual.riven_slot, metric, None)?;
        if let Some(value) = measured {
            unsupported.push("ultra-profile-contributions-center-node".into());
            value.score
        } else { actual.score }
    } else { actual.score };
    let mut details = Vec::new();
    let fatal: Vec<_> = recommendation.assessment.iter().filter(|row| row["fatal"] == true).collect();
    let fatal_above: serde_json::Map<_, _> = fatal.iter().filter(|row| row["fatalAbove"].is_number())
        .map(|row| (text(&row["label"]).to_owned(), row["fatalAbove"].clone())).collect();
    let mut submitted_fatal = Vec::new();
    for line in &input.card.lines {
        let code = text(&line["code"]);
        let without = riven_traits::card(eval.data, input.card.lines.iter().filter(|item| item["code"] != code)
            .cloned().collect(), input.rank, text(&input.card.polarity))?;
        let removed = Evaluator { riven: Some(&without), evolution:riven.evolution, ..ablation_eval.clone() };
        let removed_score = removed.fixed(&actual.cards, actual.riven_slot, metric, None)?.map(|value| value.score);
        let delta = removed_score.map(|score| percent(ablation_score, score));
        let reoptimized = if input.fields.get("includeReoptimizedContributions").is_some_and(truth) {
            let score = recommendation::reoptimized_ablation(actual_eval, &actual.cards, &without.lines, metric, input.rank, text(&input.card.polarity))?;
            Some(percent(actual.score, score))
        } else { None };
        let (condition, inactive) = match code {
            "W" | "Y" => (Some("弱点命中时"), (num(&target["headshotChance"]) == 0.0).then_some("当前评价靶弱点命中率为 0%")),
            "HR" => (Some("武器收起时自动装填"), Some("当前持续持枪，没有收起装填事件")),
            _ => (None, None),
        };
        let (value, lo, hi) = (num(&line["value"]), num(&line["low"]), num(&line["high"]));
        let quality = if hi <= lo { 0.0 } else {
            let fraction = (value - lo) / (hi - lo) * 100.0;
            (if line["negative"] == true { 100.0 - fraction } else { fraction }).clamp(0.0, 100.0)
        };
        let negative = line["negative"] == true;
        let fatal_line = negative && fatal.iter().any(|row| row["code"] == code
            && row["fatalAbove"].as_f64().is_none_or(|limit| value > limit + 1e-9));
        if fatal_line { submitted_fatal.push(line["label"].clone()); }
        let ranked = if negative { &recommendation.negative } else { &recommendation.positive };
        details.push(json!({"code":code, "label":line["label"], "value":value, "low":lo, "high":hi, "quality":quality,
            "delta":delta.or(reoptimized), "ablationDelta":delta, "reoptimizedDelta":reoptimized,
            "contributionMetric":metric_name, "condition":condition, "inactiveReason":inactive,
            "contributionStatus":if inactive.is_some() {"inactive"} else if delta.is_some() {"computed"} else {"unavailable"},
            "recommended":ranked.iter().any(|row| row["code"] == code), "fatalNegative":fatal_line,
            "explanation":"消融口径（delta/ablationDelta）：同一配卡、同一目标与指标下移除此词条后的变化；重优化口径（reoptimizedDelta）：移除词条并允许重选普通 MOD 后的变化。负值代表该词条拖低目标指标"}));
    }
    let baseline_payload = equipment(baseline_eval, baseline, &build_fields, false)?;
    let actual_payload = equipment(&riven, actual, &build_fields, true)?;
    let model_reasons = ["radial-or-field", "projectile-component", "event-mechanic", "runtime", "conditional",
        "not-applied", "not-evaluated", "unmodeled", "unverified", "unresolved", "idealized", "order-fixed"];
    let build_unsupported: Vec<_> = rows(&baseline_payload["unsupported"]).iter().chain(rows(&actual_payload["unsupported"]))
        .filter_map(Value::as_str).map(str::to_owned).collect();
    if build_unsupported.iter().any(|v| model_reasons.iter().any(|token| v.to_ascii_lowercase().contains(token))) {
        unsupported.push("analysis-model-partial".into());
    }
    if req.melee && heavy_route.is_object() && !truth(&heavy_route["components"]) {
        unsupported.extend(["heavy-attack-route-unavailable-using-fire-mode-fallback".into(),
            "heavy-attack-dps-uses-normal-fire-mode-timing-and-no-heavy-multiplier".into()]);
    }
    unsupported.extend(build_unsupported);
    let model_status = if unsupported.iter().any(|v| ["unmodeled", "unavailable", "unsupported", "partial",
        "not-applied", "not-evaluated", "unresolved", "unverified", "idealized", "order-fixed"]
        .iter().any(|token| v.to_ascii_lowercase().contains(token))) { "partial" } else { "modeled" };
    let format_number = build_response::general_number;
    let mut skipped = Vec::new();
    if !fast {
        skipped.push("评分与配装从一开始使用同靶窗口 DPS；KPM 为同一配装的独立战斗指标，无重复复评搜索".into());
    }
    skipped.push(format!("optimizer-assumption: 8 slots, {capacity} capacity, Catalyst and matching Forma"));
    skipped.push(if throwable {
        "optimizer-objective: expected heavy/throw damage; target resistances and status DOT excluded".into()
    } else if req.melee {
        "optimizer-objective: 面板普通攻击 DPS；派系歧视与敌方抗性不参与".into()
    } else {
        let steel = if req.steel { "钢铁之路" } else { "" };
        let health = if nodes.is_empty() { "（1–20s 区间）".into() } else {
            format!("（{}）", nodes.iter().map(|node| format!("{:.1}s", node.seconds)).collect::<Vec<_>>().join(" / "))
        };
        let level = target["level"].as_u64().map(|level| level.to_string()).unwrap_or("None".into());
        let heads = num(&target["headshotChance"]) * 100.0;
        let window = format_number(req.window, 6);
        format!("靶子：Lv{level} {steel}凶魂百夫长（去掉天生超宏，护甲 2700 参与）；弱点命中 {heads:.0}%，其余为身体命中；血量按本武器无紫卡基线的身体命中击杀时间取三个硬度节点{health}，每个节点 {window}s 窗口回放，KPM 取加权几何平均并计入血量展布；异常从零层叠起；同一武器的所有紫卡共用这组靶，增益可直接比较")
    });
    if truth(choices) || weapon::Weapon::new(eval.data, req).is_incarnon(req.mode) {
        let mode = if req.mode == 0 { "使用本体形态（模式 1）".into() } else { format!("使用灵化形态（模式 {}）", req.mode + 1) };
        let route_values = |choices: &Value| {
            let mut routes: Vec<_> = choices.as_object().unwrap().iter().collect();
            routes.sort_by_key(|(key, _)| *key);
            routes.into_iter().map(|(_, value)| text(value)).collect::<Vec<_>>().join(" / ")
        };
        let evolutions = if truth(choices) {
            let routes = route_values(choices);
            let source = if truth(req.choices) { "显式指定" } else { "无紫卡基准按武器默认策略选取" };
            format!("；进化 {routes}（{source}）")
        } else { String::new() };
        let screen = match route {
            Some(value) if value["adopted"] == true => {
                let chosen = route_values(&value["chosenRoute"]);
                let raised = if value["baselineRaised"] == true { "；新路线的无紫卡配装更强，基准与增益改按它计" } else { "" };
                format!("；当前紫卡配装改用进化 {chosen}（按这张紫卡逐阶筛选路线后胜出）{raised}")
            }
            Some(_) => "；已按这张紫卡逐阶筛选路线，默认路线最优".into(),
            None => String::new(),
        };
        skipped.push(format!("灵化路线：{mode}{evolutions}{screen}；灵化形态按常驻计算，“自身护盾 / 超宏击破”类进化效果按强制生效"));
    } else {
        skipped.push(format!("开火模式：使用模式 {}，其余模式仅展示", req.mode + 1));
    }
    if let Some(schedule) = eval.schedule { skipped.push(schedule.route_note(eval.stance)); }
    skipped.push("mod-family exclusivity checked by exported nameTag and weapon compatibility tags; account-specific conflicts are not exported".into());
    skipped.push(format!("Riven template values recovered from frozen build {}", eval.data.riven_templates["buildLabel"].as_str().unwrap_or(BUILD_LABEL)));
    skipped.push("recommended Riven stats use theoretical max-roll values from the active frozen build".into());
    skipped.push(match profile {
        "ultra" | "fast" => "optimizer-profile: 快速档（beam 12/32，无爬山；词条分层为粗排口径）",
        "deep" => "optimizer-profile: 深度档（beam 24/56，12 轮全邻域爬山）",
        _ => "optimizer-profile: 标准档（beam 24/40，3 轮爬山）",
    }.into());
    skipped.push(format!("optimizer-search: profile {}, {} candidate mods, {rejected} unsupported mods rejected",
        if fast { "fast" } else { profile }, eval.cards.len()));
    if !(search.scope.required.is_empty() && search.scope.forbidden.is_empty() && search.scope.min.is_none() && search.scope.max.is_none()) {
        let bounds = if search.scope.min.is_some() || search.scope.max.is_some() {
            format!(" · 数量范围 {}–{}", search.scope.min.unwrap_or(0), search.scope.max.unwrap_or(8))
        } else { String::new() };
        skipped.push(format!("optimizer-scope: 必选 {} 张 · 禁选 {} 张{bounds}", search.scope.required.len(), search.scope.forbidden.len()));
    }
    if let Some(element) = search.element() {
        skipped.push(format!("element-target: 偏好复合元素约束已生效（最终配装必须包含 {element}）"));
    }
    let time = nodes.iter().map(|node| format!("{}s", format_number(node.seconds, 6))).collect::<Vec<_>>().join("/");
    let scale = nodes.iter().map(|node| format_number(node.scale, 3)).collect::<Vec<_>>().join("/");
    let level = target["level"].as_u64().map(|level| level.to_string()).unwrap_or("None".into());
    skipped.push(format!("target: {calibration_status} · Lv{level} · ttk {time} · health x{scale}"));
    if !stance.is_null() {
        let name = text(&stance["nameZh"]);
        let multiplier = num(&stance["dpsMultiplier"]);
        let gain = num(&stance["capacityGain"]);
        skipped.push(format!("optimizer-stance: 近战分析默认使用普攻 DPS 倍率最高的架势 {name}（×{multiplier:.2}，容量 +{gain}），其余架势不参与"));
    }
    for (evaluator, value) in [(*baseline_eval, baseline), (&riven, actual)] {
        let (effects, notes) = evaluator.terms(&value.cards, value.riven_slot, evaluator.fixed_exilus, true);
        let panel = evaluator.panel(req, &effects, &notes, None)?;
        skipped.extend(panel.fields.skipped);
    }
    let timeline = input.timeline_seconds.map(|seconds| arcane_timeline(eval, seconds)).transpose()?;
    if timeline.is_some() {
        skipped.push("arcane timeline is an expected DPS envelope, not an enemy hit/status event replay".into());
    }
    let mut seen = HashSet::new();
    skipped.retain(|note| seen.insert(note.clone()));
    let status = if limited.get() == 0 { "heuristic-best-found" } else {
        skipped.push("搜索达到交互预算，返回已找到的合法配装；未证明全局最优".into());
        "heuristic-budget-limited"
    };
    let empty = serde_json::Map::new();
    let negative_work = if *cached_recommendation { &empty } else { &recommendation.work };
    let work = json!({"evaluations":work.get(), "memoHits":if *cached_recommendation { 0 } else { recommendation.memo_hits }, "limited":limited.get(),
        "negative":negative_work, "elapsedSeconds":started.elapsed().as_secs_f64(), "budgetSeconds":budget, "status":status});
    let fingerprint = serde_json::to_value(eval.data.health()).map_err(|error| error.to_string())?;
    let mut result = json!({
        "buildLabel":BUILD_LABEL, "schemaVersion":SCHEMA_VERSION, "weapon":super::super::identity(req.weapon),
        "fireMode":req.mode, "metric":METRIC, "scoreMetric":metric_name, "contextHash":context_hash,
        "evaluationScenario":scenario, "requestedTarget":requested_target, "evaluationTarget":target,
        "targetCalibration":calibration,
        "searchQuality":{"objectiveMetric":METRIC, "scoreMetric":metric_name,
            "fallbackReason":(!fast).then_some("continuous-ranking-from-start"), "targetCalibrated":false,
            "targetCalibrationStatus":calibration_status, "qualityCalibrated":!fast, "qualityCacheHit":false,
            "profile":profile, "bakeStaticsOnly":false},
    });
    result.as_object_mut().unwrap().extend(json!({
        "modelStatus":model_status, "targetStatus":calibration_status,
        "searchStatus":{"profile":profile, "metric":metric_name, "scope":"beam-and-hill-climb",
            "status":status, "work":work},
        "optimalityStatus":{"baseline":"heuristic", "recommended":"heuristic", "scope":"beam-and-hill-climb"},
        "scoringValues":{"baseline":baseline.score, "actual":actual.score}, "actualGain":percent(actual.score, baseline.score),
        "details":details, "recommendedPositive":recommendation.positive.iter().map(|u| &u["label"]).collect::<Vec<_>>(),
        "recommendedPositiveTiers":recommendation.tiers, "recommendedRoutes":null,
        "recommendedNegative":recommendation.negative.iter().map(|u| &u["label"]).collect::<Vec<_>>(),
        "fatalNegative":fatal.iter().map(|u| &u["label"]).collect::<Vec<_>>(),
        "fatalNegativeAbove":fatal_above, "submittedFatalNegative":submitted_fatal,
        "negativeAssessment":if *cached_recommendation { &[][..] } else { recommendation.assessment.as_slice() },
        "baseline":baseline_payload, "actual":actual_payload, "incarnonChoices":actual_eval.evolution.choices,
        "baselineIncarnonChoices":baseline_eval.evolution.choices, "incarnonRouteScreen":route, "incarnonPolicy":eval.evolution.policy,
        "arcaneStates":eval.arcane_panel.states, "stance":stance,
        "unsupported":unsupported.into_iter().collect::<BTreeSet<_>>(), "skipped":skipped, "profile":profile, "searchWork":work,
        "disposition":input.disposition, "engineFingerprint":fingerprint["engineFingerprint"], "engineCacheVersion":26,
        "innateElement":req.weapon["innateElements"][0], "innateElementCandidates":rows(&meta["innateElementCandidates"]),
        "responseCache":"miss",
    }).as_object().unwrap().clone());
    if input.fields.get("exilus").is_some_and(|v| !v.is_null()) { result["exilus"] = (*exilus).clone(); }
    if let Some(timeline) = timeline { result["timeline"] = timeline; }
    if let Some(mechanics) = eval.data.weapon_mechanics_overrides["weapons"].get(text(&req.weapon["path"])).filter(|v| v.is_object()) {
        result["weaponMechanics"] = mechanics.clone();
    }
    Ok(result)
}

fn arcane_timeline(eval: &Evaluator<'_>, seconds: f64) -> SimResult<Value> {
    let mut points = Vec::with_capacity(41);
    let step = seconds / 40.0;
    for index in 0..=40 {
        let time = if index == 40 { seconds } else { f64::from(index) * step };
        let snapshot = eval.arcanes.snapshot(eval.req, None, Some(time))?;
        let states: Vec<_> = snapshot.states.iter().map(|state| json!({"query":state["query"],
            "expectedStacks":state["expectedStacks"], "uptime":state["expectedUptime"]})).collect();
        // The envelope's KPM reducer has no enemy pools. Only the expected Arcane states change.
        points.push(json!({"time":time, "baselineDps":0.0, "actualDps":0.0, "optimalDps":0.0,
            "baselineDamage":0.0, "actualDamage":0.0, "optimalDamage":0.0, "arcanes":states}));
    }
    Ok(json!({"model":"expected-objective-dps-envelope", "durationSeconds":seconds, "points":points,
        "assumptions":["trigger events follow a Poisson process at each supplied triggerRate",
            "Arcane stack expiry uses a capped duration-window approximation", "selected mods stay fixed at the steady-state optimum",
            "individual shots, reload gaps, target defenses and status ticks are not scheduled"]}))
}

fn context_fields<'a>(context: &'a ResultContext<'a, '_>, exilus: &'a Value) -> BuildFields<'a> {
    BuildFields { input: context.input, search: context.search, exilus, metric_name: context.metric_name,
        rejected: context.rejected, meta:context.meta, catalog:context.catalog }
}

struct BuildFields<'a> {
    input: &'a Input<'a>,
    search: &'a Search<'a>,
    exilus: &'a Value,
    metric_name: &'a str,
    rejected: usize,
    meta: &'a Value,
    catalog: &'a SimCatalog,
}

fn equipment(eval: &Evaluator<'_>, best: &Evaluation, fields: &BuildFields<'_>, is_actual: bool) -> SimResult<Value> {
    let BuildFields { input, search, exilus, metric_name, rejected, meta, catalog } = fields;
    let mut details = build_response::payload(eval, search, best, eval.fixed_exilus, (*exilus).clone(), Value::Null,
        eval.capacity_limit, *rejected, meta, None, catalog, true)?;
    let mut build = details.as_object_mut().unwrap().remove("build").unwrap();
    for key in ["damageTypes", "archetype", "objectiveDps", "stance", "exilus"] { build.as_object_mut().unwrap().remove(key); }
    let placement = !exilus.is_null() || (input.physical && is_actual);
    let plan = forma::plan(eval, best, exilus, placement, true);
    if !exilus.is_null() {
        build["exilus"] = (*exilus).clone();
        build["capacity"] = plan["drain"].clone();
        build["capacityLimit"] = plan["limit"].clone();
    }
    build["forma"] = plan;
    let score = if *metric_name == METRIC { best.score }
        else { eval.score(&best.cards, best.riven_slot, eval.fixed_exilus, Metric::Events, None)?.unwrap_or(0.0) };
    build["objectiveScore"] = json!(score);
    build["objectiveMetric"] = json!(METRIC);
    build["optimizationMetric"] = json!(*metric_name);
    build["scoringMetric"] = json!(*metric_name);
    build["scoringScore"] = json!(best.score);
    if truth(&eval.evolution.choices) { build["incarnonChoices"] = eval.evolution.choices.clone(); }
    let multiplier = eval.stance["dpsMultiplier"].as_f64().unwrap_or(1.0);
    if multiplier != 1.0 {
        for key in ["attackDps", "burstDps", "sustainedDps"] { build[key] = json!(num(&build[key]) * multiplier); }
    }
    if is_actual {
        if input.physical {
            build["riven"]["rank"] = json!(input.rank);
            build["riven"]["drain"] = json!(input.card.drain());
        }
        let modes = alternate(eval, best)?;
        if !modes.is_empty() { build["alternateModes"] = json!(modes); }
    }
    Ok(build)
}

fn alternate(eval: &Evaluator<'_>, best: &Evaluation) -> SimResult<Vec<Value>> {
    if eval.req.melee { return Ok(Vec::new()); }
    let mut result = Vec::new();
    for (index, mode) in rows(&eval.req.weapon["fireModes"]).iter().enumerate() {
        if index == eval.req.mode || !truth(&mode["trigger"]) { continue; }
        let mut req = super::Request { mode:index, exported_mode_panel:true, ..eval.req.clone() };
        req.scenario.aiming = false;
        req.scenario.heavy = false;
        req.scenario.cycle = false;
        req.scenario.combo = 0;
        req.scenario.state = None;
        req.scenario.distance = None;
        req.scenario.spread = false;
        let (effects, skipped) = eval.terms(&best.cards, best.riven_slot, eval.fixed_exilus, true);
        let weapon = weapon::Weapon::new(eval.data, &req);
        let panel = eval.alternate_panel(&req, &effects, &skipped)?;
        let preview = projectiles::preview(&weapon, &req, &effects, &skipped, 0)?;
        let metrics = projectiles::metrics(&panel.wire, &panel.heavy, &preview);
        let mut row = json!({"fireMode":index, "trigger":mode["trigger"], "isAlternateFire":truth(&mode["isAlternateFire"]),
            "isIncarnonForm":weapon.is_incarnon(index), "shotDamage":metrics["shotDamage"], "burstDps":metrics["burstDps"],
            "sustainedDps":metrics["sustainedDps"], "attackDps":metrics["attackDps"]});
        if !preview.is_null() {
            row["projectileEvents"] = preview.clone();
            if truth(&preview["totals"]["averageHit"]["minimum"]) { row["damageBasis"] = json!("projectileEvents.minimum"); }
        }
        let unsupported: BTreeSet<_> = panel.fields.unsupported.iter().map(String::as_str).collect();
        if !unsupported.is_empty() { row["unsupported"] = json!(unsupported); }
        result.push(row);
    }
    Ok(result)
}
