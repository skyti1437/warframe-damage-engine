use super::{
    active_pool, named, num, rows, text, truth, ST_STACK_NAMES, SimResult,
    request::Request, target::Target, weapon::{self, Effect, Evolution, Weapon},
};
use serde_json::{json, Map, Value};
use std::cell::RefCell;
use wfcore_lib::{
    fx::FxHashMap, mods::ModdedFields, pymath::psum,
    replay::{self, BurstSpec, CycleSpec, ObjectId, ReplayError, ReplayProvider, ReplayResult,
        ResolveInput, SourceInput, StackKey, StackValue, Stats, StatsLinks, TrajectoryInput, Types, ViewContext},
};

struct Packet {
    fields: ModdedFields,
    radial: bool,
    victim_crit: bool,
    delayed_event: bool,
    head_only: bool,
    body_only: bool,
    persist_after_kill: bool,
    impact_delay: f64,
    charge_before_fire: bool,
    native_cooldown: f64,
    contact_shiver: bool,
    shiver_scale: f64,
    additional: Vec<ObjectId>,
    lingering: Option<(ObjectId, f64, f64)>,
}

impl Packet {
    fn new(fields: ModdedFields) -> Self {
        Self { fields, radial: false, victim_crit: false, delayed_event: false,
            head_only: false, body_only: false, persist_after_kill: false, impact_delay: 0.0,
            charge_before_fire: false, native_cooldown: 0.0, contact_shiver: false, shiver_scale: 1.0,
            additional: Vec::new(), lingering: None }
    }
}

type ViewKey = (usize, bool, Vec<u64>);
type ViewIds = (ObjectId, Option<ObjectId>);

#[derive(Default)]
struct Views {
    packets: Vec<Packet>,
    index: FxHashMap<ViewKey, ViewIds>,
}

impl Views {
    fn push(&mut self, packet: Packet) -> ObjectId {
        let id = self.packets.len();
        self.packets.push(packet);
        id
    }
}

struct Provider<'a> {
    weapon: &'a Weapon<'a>,
    evolution: &'a Evolution,
    views: RefCell<Views>,
}

impl Provider<'_> {
    fn view(&self, index: usize, last_round: bool, stacks: &[f64]) -> SimResult<ViewIds> {
        let key = (index, last_round, stacks.iter().map(|v| v.to_bits()).collect());
        let mut views = self.views.borrow_mut();
        if let Some(&hit) = views.index.get(&key) { return Ok(hit); }
        let mut effects = self.evolution.effects.clone();
        for (source, &stack) in self.evolution.sources.iter().zip(stacks) {
            if stack > 0.0 {
                effects.extend(source.effects.iter().cloned().map(|mut effect| { effect.value *= stack; effect }));
            }
        }
        if last_round && self.evolution.last_multishot != 0.0 && !self.weapon.is_incarnon(index) {
            effects.push(Effect::last_multishot(self.evolution.last_multishot));
        }
        let profile = self.weapon.profile(index);
        let mode = &self.weapon.modes[index];
        let mut packet = Packet::new(self.weapon.reduce(index, mode, &profile, &effects, false, 0, false)?);
        packet.charge_before_fire = truth(&profile["charge"]["beforeFire"]);
        packet.native_cooldown = num(&mode["nativeFireCooldownSeconds"]);
        let mut radials = Vec::new();
        if !truth(&profile["pureRadial"]) {
            for part in rows(&profile["radials"]).iter().chain(profile.get("radial").filter(|p| p.is_object())) {
                if num(&part["armingDistance"]) > self.weapon.req.scenario.distance.unwrap_or(f64::INFINITY) {
                    continue;
                }
                let part = self.part(index, mode, &profile, &effects, part, PartKind::Radial)?;
                radials.push(views.push(part));
            }
        }
        if let Some(part) = profile.get("lingering").filter(|v| v.is_object()) {
            let field = self.part(index, mode, &profile, &effects, part, PartKind::Field)?;
            packet.lingering = Some((views.push(field), num(&part["tick_rate"]), num(&part["duration_seconds"])));
        }
        // 档案中的末端爆炸与变形主弹各自结算，但使用同一份暴击和触发面板基值。
        let cycle = self.weapon.incarnon;
        if self.weapon.is_incarnon(index) && radials.is_empty()
            && matches!(text(&cycle["referenceId"]), "burston_incarnon" | "burston_prime_incarnon") {
            radials.push(views.push(self.part(index, mode, &profile, &effects, &cycle["radial"], PartKind::Terminal)?));
        }
        let radial = radials.first().copied();
        if let Some(id) = radial { views.packets[id].additional.extend(radials.into_iter().skip(1)); }
        let result = (views.push(packet), radial);
        views.index.insert(key, result);
        Ok(result)
    }

    fn part(&self, index: usize, source: &Value, profile: &Value, effects: &[Effect],
            part: &Value, kind: PartKind) -> SimResult<Packet> {
        let mut mode = source.clone();
        let damage = &mut mode["damage"];
        damage["types"] = Value::Object(named(&part["damage"]).into_iter()
            .map(|(name, amount)| (damage_type(&name).to_owned(), json!(amount))).collect());
        damage["forcedProcs"] = part["forcedProcs"].as_array().map_or_else(|| json!([]), |v| json!(v));
        if kind != PartKind::Terminal {
            for (field, key) in [("critChance", "crit_chance"), ("critMultiplier", "crit_multiplier")] {
                if !part[key].is_null() { damage[field] = part[key].clone(); }
            }
            if let Some(chance) = part["status_chance"].as_f64() {
                damage["procChance"] = json!(chance * source["multishot"].as_f64().unwrap_or(1.0));
            }
            if rows(&part["forced_procs"]).contains(&json!("electricity")) {
                damage["forcedProcs"].as_array_mut().unwrap().push(json!("PT_ELECTROCUTION"));
            }
            if rows(&part["forced_procs"]).contains(&json!("cold")) {
                damage["forcedProcs"].as_array_mut().unwrap().push(json!("PT_CHILLED"));
            }
            mode["falloffRange"] = json!([0, 0]);
            mode["falloffMinDamage"] = json!(1);
        }
        let effects: Vec<_> = effects.iter().filter(|effect| {
            !(kind == PartKind::Field && effect.upgrade == "WEAPON_DAMAGE_AMOUNT" && effect.operation == "ADD_BASE")
        }).cloned().collect();
        let mut fields = self.weapon.reduce(index, &mode, profile, &effects, false, 0, true)?;
        if part["takes_multishot"] == false || kind == PartKind::Terminal { fields.multishot = 1.0; }
        if let Some(count) = part["count"].as_f64().filter(|v| *v != 0.0) { fields.multishot *= count; }
        fields.weakpoint_damage_bonus = 0.0;
        fields.weakpoint_crit_chance_bonus = wfcore_lib::mods::Num::F(0.0);
        fields.weakpoint_crit_chance_relative = wfcore_lib::mods::Num::F(0.0);
        fields.headshot_bonus_multiplier = 1.0;
        let mut packet = Packet::new(fields);
        packet.radial = true;
        packet.victim_crit = truth(&part["takes_victim_crit"]);
        packet.head_only = truth(&part["headOnly"]);
        packet.body_only = truth(&part["bodyOnly"]);
        packet.persist_after_kill = truth(&part["persistAfterKill"]);
        packet.impact_delay = num(&part["impact_delay_seconds"]);
        packet.contact_shiver = truth(&part["takes_victim_stack_damage"]);
        packet.shiver_scale = part["coProfile"]["baseFraction"].as_f64().unwrap_or(1.0);
        packet.delayed_event = self.audited(part);
        Ok(packet)
    }

    fn audited(&self, part: &Value) -> bool {
        let audit = &self.weapon.data.mechanics["damage-event-producers"];
        let producers = &audit["delayedRadialProducers"][text(&part["nativeSource"])];
        let delay = part["native_event_delay_seconds"].as_f64().or_else(|| part["impact_delay_seconds"].as_f64());
        !truth(&part["headOnly"]) && !truth(&part["bodyOnly"]) && rows(producers).iter().any(|row| {
            let range = rows(&row["nativeDelayRange"]);
            row["nativeField"] == part["nativeField"] && row["nativeRoute"] == part["nativeRoute"]
                && truth(&row["genericHit"]) && truth(&row["genericCriticalHit"])
                && range.len() == 2 && delay.is_some_and(|v| (num(&range[0])..=num(&range[1])).contains(&v))
        })
    }
}

#[derive(Clone, Copy, PartialEq)]
enum PartKind { Radial, Field, Terminal }

fn damage_type(value: &str) -> &str {
    match value {
        "impact" => "DT_IMPACT", "puncture" => "DT_PUNCTURE", "slash" => "DT_SLASH",
        "heat" => "DT_FIRE", "cold" => "DT_FREEZE", "electric" | "electricity" => "DT_ELECTRICITY",
        "toxin" => "DT_POISON", "blast" => "DT_EXPLOSION", "gas" => "DT_GAS",
        "radiation" => "DT_RADIATION", "viral" => "DT_VIRAL", "magnetic" => "DT_MAGNETIC",
        "corrosive" => "DT_CORROSIVE", "void" => "DT_VOID", other => other,
    }
}

impl ReplayProvider for Provider<'_> {
    fn resolve(&self, input: ResolveInput) -> ReplayResult<ViewIds> {
        let index = if input.form == "base" { 0 } else { self.weapon.req.mode };
        let mut stacks = vec![0.0; self.evolution.sources.len()];
        for (key, value) in input.stacks {
            if let (StackKey::Object(id), StackValue::Float(value)) = (key, value) {
                if let Some(slot) = stacks.get_mut(id) { *slot = value; }
            }
        }
        self.view(index, input.last_round, &stacks).map_err(ReplayError::Unsupported)
    }

    fn stats(&self, object: ObjectId, ctx: &mut ViewContext<'_>) -> ReplayResult<Stats> {
        let views = self.views.borrow();
        let packet = &views.packets[object];
        let mut core = weapon::core(&packet.fields, ctx.types, ctx.target_faction, ctx.faction_bracket);
        if packet.radial { core.radial_only = true; }
        let mut stats = weapon::replay_stats(&packet.fields, core);
        stats.obj = object;
        stats.radial_victim_crit = packet.victim_crit;
        stats.delayed_radial_hit_event = packet.delayed_event;
        stats.head_only = packet.head_only;
        stats.body_only = packet.body_only;
        stats.persist_after_kill = packet.persist_after_kill;
        stats.impact_delay = packet.impact_delay;
        stats.charge_before_fire = packet.charge_before_fire;
        stats.native_fire_cooldown = packet.native_cooldown;
        stats.contact_shiver = packet.contact_shiver;
        stats.contact_shiver_scale = packet.shiver_scale;
        stats.charge_pool_cost = 1.0;
        Ok(stats)
    }

    fn stats_links(&self, object: ObjectId) -> ReplayResult<StatsLinks> {
        let views = self.views.borrow();
        let packet = &views.packets[object];
        Ok(StatsLinks { additional: packet.additional.clone(), lingering: packet.lingering, ..StatsLinks::default() })
    }
}

fn burst(weapon: &Weapon<'_>, index: usize, profile: &Value) -> Option<BurstSpec> {
    let native = weapon.mode_profile("burst-profiles", index);
    let record = if profile["burst"].is_object() { &profile["burst"] } else { native };
    if record.is_object() {
        return Some((truth(&record["remainingClip"]), record["count"].as_i64().unwrap_or(0),
            num(&record["delay_seconds"]), truth(&record["ignoreFireRate"])));
    }
    let mode = &weapon.modes[index];
    if mode["trigger"] == "/Lotus/Language/Menu/Loadout_TriggerSemiAutoFireOnUp" && num(&mode["fireRate"]) > 0.0 {
        Some((false, 2, 1.0 / num(&mode["fireRate"]), false))
    } else { None }
}

fn floor(profile: &Value, co: &Value) -> f64 {
    let source = if truth(&profile["source"]) { text(&profile["source"]) } else { text(&co["source"]) };
    let name = source.rsplit('/').next().unwrap_or("").trim_end_matches(".yaml");
    match name {
        "amprex" | "cycron" | "glaxion_vandal" | "kuva_nukor" | "nukor" | "tenet_cycron" |
        "quanta" | "quanta_vandal" | "tenet_quanta" => 0.30,
        "convectrix" => 0.60, "convectrix_alt" => 0.80, "embolist" => 0.45,
        "flux_rifle" | "spectra" | "spectra_vandal" => 0.25,
        "ignis" | "ignis_wraith" => 0.35, "phage" => 0.70,
        "phantasma" | "phantasma_prime" => 0.15, _ => 0.20,
    }
}

pub(super) fn calculate(weapon: &Weapon<'_>, evolution: &Evolution, fields: &ModdedFields,
                         req: &Request<'_>, target: &Target<'_>, level: u64, steel: bool, enhancements: &Value) -> SimResult<Value> {
    let defenses = target.defenses(level, steel, req.strip, 0.0, enhancements);
    let full = defenses.full;
    let ehp = psum(full);
    let profile = weapon.profile(req.mode);
    let base_profile = weapon.profile(0);
    let is_incarnon = weapon.is_incarnon(req.mode);
    let cycle = if req.scenario.cycle && is_incarnon && truth(weapon.incarnon) {
        let v = weapon.incarnon;
        Some(CycleSpec {
            hits: num(&v["hits"]), enter: num(&v["enterSeconds"]), exit: num(&v["exitSeconds"]), pool: num(&v["pool"]),
            charge_on: match text(&v["chargeOn"]) {
                "kills" => 2, "direct_hits" => 1, _ if req.scenario.gauge_heads => 1, _ => 0,
            },
            kill_charged: truth(&v["killCharged"]), charge_before_fire: truth(&v["chargeBeforeFire"]),
            damage_scale: v["damageScale"].as_f64().unwrap_or(1.0), source_mode_charge: truth(&v["sourceModeKillChargeTrue"]),
        })
    } else { None };
    let selected_burst = burst(weapon, req.mode, &profile);
    let base_burst = burst(weapon, if cycle.is_some() { 0 } else { req.mode },
        if cycle.is_some() { &base_profile } else { &profile });
    let base_burst = base_burst.or_else(|| {
        let v = &weapon.incarnon["burst"];
        (cycle.is_some() && v.is_object()).then(|| (false, v["count"].as_i64().unwrap_or(0), num(&v["delay_seconds"]), false))
    });
    let base_index = if cycle.is_some() { 0 } else { req.mode };
    let mut types = Types::new();
    let deltas = target.deltas.iter().map(|(name, v)| (types.id(name), *v)).collect();
    let armour_bypass = target.armour_bypass.iter().map(|name| types.id(name)).collect();
    let shield_bypass = target.shield_bypass.iter().map(|name| types.id(name)).collect();
    let forced_procs = req.forced.iter().map(|(name, v)| (types.id(name), *v)).collect();
    let mut initial_stacks = Vec::new();
    let sources = evolution.sources.iter().enumerate().map(|(id, source)| {
        let stacks = if source.fixed { source.cap } else { 0.0 };
        initial_stacks.push(stacks);
        SourceInput { id, kind: Some(source.kind.to_owned()), stacks, expires: f64::INFINITY,
            max_stacks: source.cap, per_event: if source.fixed { 0.0 } else { source.chance },
            duration: if source.fixed { 0.0 } else { source.duration }, decay: "reset".into(),
            hold: source.hold, initial_stacks: stacks, required_count: 1.0, ..SourceInput::default() }
    }).collect();
    let provider = Provider { weapon, evolution, views: RefCell::new(Views::default()) };
    let (initial, _) = provider.view(req.mode, false, &initial_stacks)?;
    let battery = &profile["battery"];
    let shell = &profile;
    let co = &weapon.reference["coProfiles"][weapon.path];
    let input = TrajectoryInput {
        window: req.window, rate: weapon::attack_rate(fields), forced_procs,
        faction_deltas: deltas, armour_bypass, shield_bypass, distance: req.scenario.distance,
        beam_floor: Some(floor(&profile, &co[req.mode.to_string()])),
        beam_floor_base: Some(floor(&base_profile, &co["0"])),
        burst: selected_burst, burst_rate: Some(num(&weapon.modes[req.mode]["fireRate"])),
        burst_base: base_burst, burst_rate_base: Some(num(&weapon.modes[base_index]["fireRate"])),
        burst_incarnon: selected_burst, burst_rate_incarnon: Some(num(&weapon.modes[req.mode]["fireRate"])),
        target_faction: target.faction.to_owned(), faction_bracket: target.bracket,
        tables: target.tables(&types, None, Vec::new()),
        battery: battery["regenPerSecond"].as_f64().map(|regen| (num(&battery["delayEmptySeconds"]), regen, num(&battery["delayPartialSeconds"]))),
        shell: shell["reloadPerShell"].as_f64().map(|v| (num(&shell["reloadStart"]), v, num(&shell["reloadEnd"]))),
        shell_mode_switch: truth(&shell["modeSwitchLoadsShell"]), cycle,
        head: req.aim, status_rate_multiplier: 1.0, damage_rate_multiplier: 1.0, full, ehp,
        armour_base: defenses.armour, elemental_armour_strip: req.elemental_strip, extra_scale: 1.0,
        weapon_slot: text(&req.weapon["slot"]).into(), lane_stratum: 0.5, selected_incarnon: is_incarnon && cycle.is_none(),
        reload_after_every_shot: !is_incarnon && truth(weapon.mode_profile("reload-after-shot", req.mode)),
        last_burst: evolution.last_burst,
        infinite_ammo: req.scenario.state.and_then(|state| state.get("infiniteAmmo")) == Some(&Value::Bool(true)),
        puncture_cap: 5.0 + fields.proc_max_stacks.iter().find(|(dtype, _)| dtype == "DT_PUNCTURE").map_or(0.0, |v| v.1),
        blocks_radiation: target.record["path"] == "/Lotus/Types/Enemies/Grineer/Disruption/Avatars/DemoDevourerAvatar",
        sources, types, initial, ..TrajectoryInput::default()
    };
    let output = replay::trajectory_sampled(input, &provider).map_err(|error| error.to_string())?;
    let details = output.details.expect("展示回放必须有事件统计");
    let gauge_limit = weapon.incarnon["hits"].as_i64();
    let mut points = Vec::with_capacity(output.samples.len());
    for point in &output.samples {
        let mut stacks: Map<String, Value> = ST_STACK_NAMES.into_iter().zip(point.stacks)
            .map(|(name, value)| (name.to_owned(), json!(value))).collect();
        if point.frozen || point.stacks[4] == 0.0 { stacks.insert("cold".into(), json!(point.stacks[4] as i64)); }
        let trigger_stacks: Map<String, Value> = evolution.sources.iter().zip(&point.trigger_stacks)
            .map(|(source, value)| (source.id.clone(), json!(value))).collect();
        let gauge = gauge_limit.filter(|&v| v as f64 == point.gauge).map_or_else(|| json!(point.gauge), |v| json!(v));
        points.push(json!({"time": point.time, "pool": active_pool(point.pools).0, "overguard": point.pools[0],
            "shield": point.pools[1], "health": point.pools[2], "stacks": stacks, "armour": point.armour,
            "heatArmourStrip": point.heat_strip, "punctureArmourStrip": point.puncture_strip,
            "heatSeedPerTick": point.heat_seed, "punctureCritChance": point.puncture_crit, "coldCritDamage": point.cold_crit,
            "frozen": point.frozen, "totalArmourStrip": point.total_strip, "generation": point.generation,
            "form": point.form, "ammo": point.ammo, "gauge": gauge, "fieldDurationReady": point.field_duration_ready,
            "shots": point.shots, "directDps": point.direct_dps, "dotDps": point.dot_dps, "stepDamage": point.step_damage,
            "syndicatePoints": point.affinity, "triggerStacks": trigger_stacks,
        }));
    }
    let series: Vec<_> = evolution.sources.iter().enumerate().map(|(index, source)| {
        let series: Vec<_> = output.samples.iter().map(|p| p.trigger_stacks[index]).collect();
        let len = series.len() as f64;
        let condition = match source.kind {
            "on-reload-empty" => "弹匣打空后开始换弹时触发。",
            "on-reload-empty-complete" => "弹匣打空后完成换弹时触发。",
            "on-reload" => "开始换弹时触发。",
            "on-reload-complete" => "完成换弹时触发。",
            other => other,
        };
        json!({"id": source.id, "label": source.label, "kind": source.kind, "maxStacks": source.cap, "series": series,
            "conditionText": condition, "resetOn": [], "simulationMode": if source.fixed { "fixed" } else { "events" },
            "avgStacks": psum(series.iter().copied()) / len, "uptime": series.iter().filter(|v| **v > 0.0).count() as f64 / len})
    }).collect();
    let mut unsupported = Vec::new();
    if truth(&profile["childSpawnAudit"]) {
        unsupported.push("zymos-child-overlap-off-until-explicit-geometry-policy");
        unsupported.push("zymos-child-external-source-context-defaults");
    }
    if req.scenario.cycle && is_incarnon && cycle.is_none() { unsupported.push("incarnon-cycle-data-unavailable"); }
    let limitations = [
        "expected-proc-counts-not-Monte-Carlo", "single-target", "non-heat-stacking-procs-expected",
        "single-target-syndicate-no-player-buffs", "electric-gas-shared-expected-clock;random-first-proc-phase-not-branched",
        "electric-gas-latest-expired-landing-retained-unverified", "electric-gas-mixed-origin-kill-credit-oldest-live-source",
        "magnetic-break-ordinary-electric-source-context", "electric-gas-adjacent-count-arrival",
        "gas-mixed-duration-cumulative-newer-rank", "spread-hit-loss-not-modeled",
    ];
    let score = output.kills as f64 + if ehp != 0.0 { 1.0 - psum(output.pools) / ehp } else { 0.0 };
    let total = psum(output.totals);
    let named = |values: replay::NamedDamage| -> Value {
        Value::Object(values.into_iter().map(|(k, v)| (k, json!(v))).collect())
    };
    let stacks: Map<String, Value> = ST_STACK_NAMES.into_iter().zip(output.averaged_stacks)
        .map(|(name, value)| (name.to_owned(), json!(value))).collect();
    let enemy = target.record;
    Ok(json!({"shots": output.attacks, "transforms": details.transforms, "reloads": details.reloads,
        "syndicateEvents": details.syndicate_events.into_iter().map(|(time, name, damage)|
            json!({"time": time, "name": name, "damage": damage})).collect::<Vec<_>>(),
        "syndicateWindow": output.totals[3], "limitations": limitations, "unsupported": unsupported,
        "model": "event-expected-v3", "windowSeconds": req.window, "killsPerMinute": score * 60.0 / req.window,
        "windowDps": total / req.window, "directWindow": output.totals[0], "dotWindow": output.totals[1],
        "dotByType": named(details.dot_by_type), "procCounts": named(details.proc_counts), "hitProbability": fields.hit_probability,
        "totalWindowDamage": total, "killsInWindow": score, "targetTotalEhp": ehp, "points": points, "stacks": stacks,
        "triggerSeries": series, "fieldDuration": null, "sniperCombo": null, "melee": null, "manualDetonation": null,
        "chargeVolley": null, "delayedAttackEvents": details.delayed_attacks, "projectileEvents": [],
        "projectileLifecycle": {}, "onosSequence": null, "reloadShellEvents": details.reload_shell_events.into_iter()
            .map(|(time, rounds, source, form)| json!({"time": time, "rounds": rounds, "source": source, "form": form})).collect::<Vec<_>>(),
        "cryogenic": null, "empowered": null, "triggerPolicy": req.scenario.policy,
        "context": {"enemyPath": enemy["path"], "enemyName": if truth(&enemy["nameZh"]) { &enemy["nameZh"] } else { &enemy["nameEn"] },
            "level": level, "steelPath": steel, "enhancements": enhancements},
    }))
}

