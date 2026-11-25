use super::{
    active_pool, arcane, arcane_status::{self, StatusArcane}, named, num, rows, text, truth, ST_STACK_NAMES, SimResult,
    events::Effects, melee::Schedule, request::Request, target::Target, weapon::{self, Effect, Evolution, Weapon},
};
use serde_json::{json, Map, Value};
use std::cell::RefCell;
use wfcore_lib::{
    fx::FxHashMap, mods::ModdedFields, pymath::psum,
    replay::{self, BurstSpec, CycleSpec, ObjectId, ReplayError, ReplayProvider, ReplayResult,
        ResolveInput, StackKey, StackValue, Stats, StatsLinks, TrajectoryInput, Types, ViewContext},
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

type ViewKey = (usize, bool, bool, bool, u64, Vec<u64>);
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
    effects: &'a Effects,
    schedule: Option<&'a Schedule>,
    duplicate: Option<(f64, f64)>,
    views: RefCell<Views>,
}

impl Provider<'_> {
    fn view(&self, index: usize, last_round: bool, stacks: &[f64], heavy: bool, no_co: bool, live: f64) -> SimResult<ViewIds> {
        let key = (index, last_round, heavy, no_co, live.to_bits(), stacks.iter().map(|v| v.to_bits()).collect());
        let mut views = self.views.borrow_mut();
        if let Some(&hit) = views.index.get(&key) { return Ok(hit); }
        let mut effects = self.effects.static_terms.clone();
        for (source, &stack) in self.effects.sources.iter().map(|e| &e.effects)
            .chain(self.effects.tier_terms.iter().map(|(_, _, terms)| terms)).zip(stacks) {
            if stack > 0.0 {
                effects.extend(source.iter().cloned().map(|mut effect| { effect.value = if effect.operation == "MULTIPLY" { 1.0 + (effect.value - 1.0) * stack } else { effect.value * stack }; effect }));
            }
        }
        if last_round && self.evolution.last_multishot != 0.0 && !self.weapon.is_incarnon(index) {
            effects.push(Effect::last_multishot(self.evolution.last_multishot));
        }
        if no_co { effects.retain(|effect| effect.upgrade != "WEAPON_DAMAGE_IF_VICTIM_PROC_ACTIVE"); }
        let ceiling = live.ceil();
        if live != ceiling {
            for effect in &mut effects {
                if matches!(effect.upgrade.as_str(), "WEAPON_DAMAGE_IF_VICTIM_PROC_ACTIVE" | "WEAPON_CRIT_CHANCE_IF_VICTIM_PROC_ACTIVE") {
                    effect.value = effect.value * live / ceiling;
                }
            }
        }
        let profile = self.weapon.profile(index);
        let mode = &self.weapon.modes[index];
        let mut packet = Packet::new(self.weapon.reduce(index, mode, &profile, &effects, false, ceiling, false, heavy, &[])?);
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
        let mut fields = self.weapon.reduce(index, &mode, profile, &effects, false, 0.0, true, false, &[])?;
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
        let index = if input.form == "base" || self.schedule.is_some() { 0 } else { self.weapon.req.mode };
        let mut stacks = vec![0.0; self.effects.sources.len() + self.effects.tier_terms.len()];
        let mut heavy = false;
        let mut no_co = false;
        for (key, value) in input.stacks {
            match (key, value) {
                (StackKey::Object(id), StackValue::Float(value)) => {
                    if let Some(slot) = stacks.get_mut(id) { *slot = value; }
                }
                (StackKey::Named("__heavy"), StackValue::Flag(v)) => heavy = v,
                (StackKey::Named("__noCO"), StackValue::Flag(v)) => no_co = v,
                _ => {}
            }
        }
        self.view(index, input.last_round, &stacks, heavy, no_co, input.live_types.unwrap_or(0.0))
            .map_err(ReplayError::Unsupported)
    }

    fn supports_co_variants(&self) -> bool { false }

    fn melee_starting_tier(&self) -> u64 { self.weapon.req.scenario.combo }

    fn hit_spec(&self, id: i64, types: &mut Types) -> ReplayResult<replay::HitSpec> {
        let mut hit = self.schedule.expect("命中必有近战路线").hit(id, types);
        if hit.can_crit { hit.duplicate = self.duplicate; }
        Ok(hit)
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
                         req: &Request<'_>, target: &Target<'_>, level: u64, steel: bool, enhancements: &Value,
                         effects: &Effects, schedule: Option<&Schedule>, arcanes: &arcane::Equipped<'_>,
                         status_rate: f64, damage_rate: f64) -> SimResult<Value> {
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
    let deltas: Vec<_> = target.deltas.iter().map(|(name, v)| (types.id(name), *v)).collect();
    let armour_bypass: Vec<_> = target.armour_bypass.iter().map(|name| types.id(name)).collect();
    let shield_bypass: Vec<_> = target.shield_bypass.iter().map(|name| types.id(name)).collect();
    let forced_procs: Vec<_> = req.forced.iter().map(|(name, v)| (types.id(name), *v)).collect();
    let sources = effects.input(&mut types);
    let initial_stacks: Vec<_> = sources.iter().map(|s| s.initial_stacks).collect();
    let (mut encumber, mut cryogenic, mut empowered, mut duplicate, mut fortifier) = (None, None, None, None, None);
    for card in &arcanes.cards {
        let uptime = if card.stacks == Some(0) { 0.0 } else { card.uptime.unwrap_or(1.0) };
        match arcane_status::status(card.record, card.rank) {
            Some(StatusArcane::Encumber(chance)) if uptime != 0.0 => encumber = Some((chance, uptime)),
            Some(StatusArcane::Cryogenic(stacks, radius)) => cryogenic = Some((stacks, uptime, radius)),
            Some(StatusArcane::Empowered(damage)) => empowered = Some((damage, uptime)),
            Some(StatusArcane::Duplicate(chance)) => duplicate = Some((chance, uptime)),
            Some(StatusArcane::Fortifier(multiplier, steal, cap)) => fortifier = Some((multiplier, steal, cap, uptime)),
            _ => {}
        }
    }
    let enervate_card = arcanes.cards.iter().rev().find(|c| arcane_status::enervate(c.record, c.rank));
    let enervate = enervate_card.map(|c| replay::EnervateInput { reset_after: c.rank as i64 + 1,
        min_interval: 1.0 / 30.0, fixed_stacks: c.stacks.map(|v| v as f64), uptime: c.uptime.unwrap_or(1.0),
        ignore_selected: arcane_status::ignore_selected(weapon.path, req.mode) });
    let provider = Provider { weapon, evolution, effects, schedule, duplicate, views: RefCell::new(Views::default()) };
    let (initial, _) = provider.view(if schedule.is_some() { 0 } else { req.mode }, false, &initial_stacks, false, false, 0.0)?;
    let tier_keys: Vec<_> = effects.tier_terms.iter().enumerate()
        .map(|(i, (key, cap, _))| (effects.sources.len() + i, key.clone(), *cap)).collect();
    let has_co = |e: &Effect| matches!(e.upgrade.as_str(),
        "WEAPON_DAMAGE_IF_VICTIM_PROC_ACTIVE" | "WEAPON_CRIT_CHANCE_IF_VICTIM_PROC_ACTIVE");
    let live_types_enabled = effects.static_terms.iter().chain(effects.sources.iter().flat_map(|s| &s.effects)).any(has_co);
    let melee_rate = schedule.map(|_| provider.views.borrow().packets[initial].fields.fire_rate.f());
    let battery = &profile["battery"];
    let shell = &profile;
    let co = &weapon.reference["coProfiles"][weapon.path];
    let status_pool: Vec<_> = if encumber.is_some() || empowered.is_some() {
        arcane_status::STATUS_POOL.into_iter().map(|name| types.id(name)).collect()
    } else { Vec::new() };
    let state = |key: &str| req.scenario.state.and_then(|s| s.get(key)).unwrap_or(&Value::Null);
    let actor_events: Vec<_> = [("selfShieldBreak", "on-shield-broken"), ("selfOverguardBreak", "on-overguard-broken"),
        ("selfShieldHit", "on-shield-hit"), ("equipFromPrimary", "on-equip-from-primary"),
        ("abilityUse", "on-warframe-ability")].into_iter().filter_map(|(key, kind)| {
            state(&format!("{key}At")).as_f64().map(|time| (time, num(state(&format!("{key}Interval"))), kind.to_owned()))
        }).collect();
    let track_kills = sources.iter().any(|source| !source.actor_only && source.per_event != 0.0
        && source.kind.as_deref() == Some("on-headshot-kill"));
    let feedback = track_kills || sources.iter().any(|source| !source.actor_only && source.per_event != 0.0 && source.free_ammo);
    let run = |weak_draw| {
        let input = TrajectoryInput {
            window: req.window, rate: melee_rate.unwrap_or_else(|| weapon::attack_rate(fields)), forced_procs: forced_procs.clone(),
            faction_deltas: deltas.clone(), armour_bypass: armour_bypass.clone(), shield_bypass: shield_bypass.clone(), distance: req.scenario.distance,
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
            head: req.aim, status_rate_multiplier: if schedule.is_some() { 1.0 } else { status_rate },
            damage_rate_multiplier: if schedule.is_some() { 1.0 } else { damage_rate }, full, ehp,
            melee: schedule.map(|schedule| schedule.spec(tier_keys.clone())), live_types_enabled, weak_draw,
            armour_base: defenses.armour, elemental_armour_strip: req.elemental_strip, extra_scale: 1.0,
            weapon_slot: text(&req.weapon["slot"]).into(), lane_stratum: 0.5, selected_incarnon: is_incarnon && cycle.is_none(),
            reload_after_every_shot: !is_incarnon && truth(weapon.mode_profile("reload-after-shot", req.mode)),
            last_burst: evolution.last_burst,
            infinite_ammo: req.scenario.state.and_then(|state| state.get("infiniteAmmo")) == Some(&Value::Bool(true)),
            puncture_cap: 5.0 + fields.proc_max_stacks.iter().find(|(dtype, _)| dtype == "DT_PUNCTURE").map_or(0.0, |v| v.1),
            blocks_radiation: target.record["path"] == "/Lotus/Types/Enemies/Grineer/Disruption/Avatars/DemoDevourerAvatar",
            encumber, cryogenic: cryogenic.map(|(stacks, uptime, _)| (stacks, uptime)), empowered, fortifier, enervate: enervate.clone(),
            status_pool: status_pool.clone(), actor_events: actor_events.clone(), energy_rate: num(state("energySpendPerSecond")),
            sources: sources.clone(), types: types.clone(), initial,
            ..TrajectoryInput::default()
        };
        let output = replay::trajectory_sampled(input, &provider).map_err(|error| error.to_string())?;
        Ok::<_, String>(output)
    };
    let serialize = |output: replay::TrajectoryOutput| {
        let details = output.details.expect("展示回放必须有事件统计");
        let gauge_limit = weapon.incarnon["hits"].as_i64();
        let mut points = Vec::with_capacity(output.samples.len());
        for point in &output.samples {
            let mut stacks: Map<String, Value> = ST_STACK_NAMES.into_iter().zip(point.stacks)
                .map(|(name, value)| (name.to_owned(), json!(value))).collect();
            if point.frozen || point.stacks[4] == 0.0 { stacks.insert("cold".into(), json!(point.stacks[4] as i64)); }
            let mut trigger_stacks: Map<String, Value> = effects.sources.iter().zip(&point.trigger_stacks)
                .map(|(source, value)| (source.id.clone(), json!(value))).collect();
            let gauge = gauge_limit.filter(|&v| v as f64 == point.gauge).map_or_else(|| json!(point.gauge), |v| json!(v));
            if let Some((card, sample)) = enervate_card.zip(point.enervate.as_ref()) {
                trigger_stacks.insert(text(&card.record["path"]).to_owned(), json!(sample.stacks));
            }
            let mut wire = json!({"time": point.time, "pool": active_pool(point.pools).0, "overguard": point.pools[0],
                "shield": point.pools[1], "health": point.pools[2], "stacks": stacks, "armour": point.armour,
                "heatArmourStrip": point.heat_strip, "punctureArmourStrip": point.puncture_strip,
                "heatSeedPerTick": point.heat_seed, "punctureCritChance": point.puncture_crit, "coldCritDamage": point.cold_crit,
                "frozen": point.frozen, "totalArmourStrip": point.total_strip, "generation": point.generation,
                "form": point.form, "ammo": point.ammo, "gauge": gauge, "fieldDurationReady": point.field_duration_ready,
                "shots": point.shots, "directDps": point.direct_dps, "dotDps": point.dot_dps, "stepDamage": point.step_damage,
                "syndicatePoints": point.affinity, "triggerStacks": trigger_stacks,
            });
            if let Some(sample) = &point.enervate { wire["enervate"] = enervate_payload(sample); }
            points.push(wire);
        }
        let mut series: Vec<_> = effects.sources.iter().enumerate().map(|(index, source)| {
            let series: Vec<_> = output.samples.iter().map(|p| p.trigger_stacks[index]).collect();
            let len = series.len() as f64;
            json!({"id": source.id, "label": source.label, "kind": source.kind, "maxStacks": source.cap, "series": series,
                "conditionText": source.condition_text, "resetOn": source.replay.reset_on, "simulationMode": if source.fixed { "fixed" } else { "events" },
                "avgStacks": psum(series.iter().copied()) / len, "uptime": series.iter().filter(|v| **v > 0.0).count() as f64 / len})
        }).collect();
        if let Some((card, sample)) = enervate_card.zip(details.enervate_sample.as_ref()) {
            let samples: Vec<_> = output.samples.iter().filter_map(|s| s.enervate.as_ref()).collect();
            let average = |read: fn(&replay::EnervateSample) -> f64| {
                if samples.is_empty() { json!(0) } else { json!(psum(samples.iter().map(|s| read(s))) / samples.len() as f64) }
            };
            let mut summary = super::merge(enervate_payload(sample), json!({"id": card.record["path"],
                "label": card.record["nameZh"].as_str().filter(|s| !s.is_empty()).unwrap_or("Secondary Enervate"), "rank": card.rank,
                "resetAfter": card.rank + 1, "flatPerStack": 0.1, "minInterval": 1.0 / 30.0, "uptime": average(|s| s.uptime),
                "ignoreSelected": arcane_status::ignore_selected(weapon.path, req.mode),
                "kind": "enervate", "maxStacks": null, "duration": null, "series": samples.iter().map(|s| s.stacks).collect::<Vec<_>>(),
                "avgStacks": average(|s| s.stacks),
                "mode": if card.stacks.is_some() { "explicit-fixed" } else { "joint-probability-events" },
                "assumptions": arcane_status::ENERVATE_ASSUMPTIONS}));
            if let Some(stacks) = card.stacks { summary["fixedStacks"] = json!(stacks); }
            series.push(summary);
        }
        let mut unsupported = effects.notes.clone();
        if let Some(s) = schedule { unsupported.extend(s.notes.iter().cloned()); }
        if truth(&profile["childSpawnAudit"]) {
            unsupported.push("zymos-child-overlap-off-until-explicit-geometry-policy".into());
            unsupported.push("zymos-child-external-source-context-defaults".into());
        }
        if req.scenario.cycle && is_incarnon && cycle.is_none() { unsupported.push("incarnon-cycle-data-unavailable".into()); }
        let mut limitations = vec![
            "expected-proc-counts-not-Monte-Carlo", "single-target", "non-heat-stacking-procs-expected",
            "single-target-syndicate-no-player-buffs", "electric-gas-shared-expected-clock;random-first-proc-phase-not-branched",
            "electric-gas-latest-expired-landing-retained-unverified", "electric-gas-mixed-origin-kill-credit-oldest-live-source",
            "magnetic-break-ordinary-electric-source-context", "electric-gas-adjacent-count-arrival",
            "gas-mixed-duration-cumulative-newer-rank", "spread-hit-loss-not-modeled",
        ];
        if enervate_card.is_some() { limitations.extend(arcane_status::ENERVATE_ASSUMPTIONS); }
        if encumber.is_some() { limitations.extend(["encumber-poisson-status-presence", "encumber-single-target-expected-procs"]); }
        if cryogenic.is_some() { limitations.push("cryogenic-single-target-expected-procs-no-neighbor-burst"); }
        if empowered.is_some() { limitations.extend(["empowered-grouped-expected-instances", "empowered-derived-script-procs-excluded"]); }
        if fortifier.is_some() { limitations.extend(["fortifier-raw-multiply-r5-x8-reference-x9-conflict", "fortifier-player-overguard-no-incoming-damage"]); }
        let score = output.kills as f64 + if ehp != 0.0 { 1.0 - psum(output.pools) / ehp } else { 0.0 };
        let total = psum(output.totals);
        let named = |values: replay::NamedDamage| -> Value {
            Value::Object(values.into_iter().map(|(k, v)| (k, json!(v))).collect())
        };
        let stacks: Map<String, Value> = ST_STACK_NAMES.into_iter().zip(output.averaged_stacks)
            .map(|(name, value)| (name.to_owned(), json!(value))).collect();
        let enemy = target.record;
        json!({"shots": output.attacks, "transforms": details.transforms, "reloads": details.reloads,
            "syndicateEvents": details.syndicate_events.into_iter().map(|(time, name, damage)|
                json!({"time": time, "name": name, "damage": damage})).collect::<Vec<_>>(),
            "syndicateWindow": output.totals[3], "limitations": limitations, "unsupported": unsupported,
            "model": "event-expected-v3", "windowSeconds": req.window, "killsPerMinute": score * 60.0 / req.window,
            "windowDps": total / req.window, "directWindow": output.totals[0], "dotWindow": output.totals[1],
            "dotByType": named(details.dot_by_type), "procCounts": named(details.proc_counts), "hitProbability": fields.hit_probability,
            "totalWindowDamage": total, "killsInWindow": score, "targetTotalEhp": ehp, "points": points, "stacks": stacks,
            "triggerSeries": series, "fieldDuration": null, "sniperCombo": null, "melee": schedule.map(|s| s.payload(&details.melee_events)), "manualDetonation": null,
            "chargeVolley": null, "delayedAttackEvents": details.delayed_attacks, "projectileEvents": [],
            "projectileLifecycle": {}, "onosSequence": null, "reloadShellEvents": details.reload_shell_events.into_iter()
                .map(|(time, rounds, source, form)| json!({"time": time, "rounds": rounds, "source": source, "form": form})).collect::<Vec<_>>(),
            "cryogenic": cryogenic.map(|(stacks, _, radius)| json!({"coldStacksPerPuncture": stacks as u64, "radiusMeters": radius,
                "expectedColdApplications": details.cryogenic_expected, "neighborTargetsModeled": false})),
            "empowered": empowered.map(|(damage, _)| json!({"damagePerStatus": damage as u64, "expectedInstances": details.empowered.0,
                "windowDamage": output.totals[4], "damageByType": named(details.empowered.1),
                "sourcePolicy": "weapon-status-only-no-script-link-dot-recursion"})), "triggerPolicy": req.scenario.policy,
            "context": {"enemyPath": enemy["path"], "enemyName": if truth(&enemy["nameZh"]) { &enemy["nameZh"] } else { &enemy["nameEn"] },
                "level": level, "steelPath": steel, "enhancements": enhancements},
        })
    };
    if !feedback {
        return run(None).map(serialize);
    }
    let golden = (5.0_f64.sqrt() - 1.0) / 2.0;
    let probe = run(Some((true, 0.0, golden, track_kills)))?;
    if probe.weak_uncertainty < 0.05 { return Ok(serialize(probe)); }
    let branches = (0..16).map(|i| run(Some((false, (i as f64 + 0.5) / 16.0, golden, true))).map(&serialize))
        .collect::<SimResult<Vec<_>>>()?;
    Ok(merge_branches(&branches))
}

fn merge_branches(branches: &[Value]) -> Value {
    fn mean(values: impl Iterator<Item = f64>, count: usize) -> f64 { psum(values) / count as f64 }
    fn merge(values: &[&Value], fallback: &Value) -> Value {
        let first = values[0];
        match first {
            Value::Number(_) if values.iter().all(|v| v.is_number()) =>
                json!(mean(values.iter().map(|v| num(v)), values.len())),
            Value::Object(first) if values.iter().all(|v| v.as_object().is_some_and(|m| {
                m.len() == first.len() && first.keys().all(|key| m.contains_key(key))
            })) => Value::Object(first.keys().map(|key| {
                let column: Vec<_> = values.iter().map(|v| &v[key]).collect();
                (key.clone(), merge(&column, &fallback[key]))
            }).collect()),
            Value::Array(first) if values.iter().all(|v| v.as_array().is_some_and(|a| a.len() == first.len())) =>
                Value::Array((0..first.len()).map(|index| {
                    let column: Vec<_> = values.iter().map(|v| &v[index]).collect();
                    merge(&column, &fallback[index])
                }).collect()),
            _ => fallback.clone(),
        }
    }
    let score = |v: &Value| num(&v["killsPerMinute"]);
    let center = mean(branches.iter().map(score), branches.len());
    let representative = branches.iter().min_by(|a, b| (score(a) - center).abs().total_cmp(&(score(b) - center).abs())).unwrap();
    let mut result = merge(&branches.iter().collect::<Vec<_>>(), representative);
    for key in ["windowSeconds", "hitProbability", "context"] {
        result[key] = representative[key].clone();
    }
    let mut unsupported = Vec::new();
    for item in branches.iter().flat_map(|v| rows(&v["unsupported"])) {
        if !unsupported.contains(item) { unsupported.push(item.clone()); }
    }
    result["unsupported"] = json!(unsupported);
    result["limitations"].as_array_mut().unwrap().push(json!("weakpoint-kill-feedback-stratified-branches"));
    result
}



fn enervate_payload(sample: &replay::EnervateSample) -> Value {
    json!({"stacks": sample.stacks, "bigCrits": sample.big_crits, "resets": sample.resets,
        "acceptedHits": if sample.fractional_hits { json!(sample.hits) } else { json!(sample.hits as u64) }, "probabilityMass": sample.mass, "states": sample.states})
}
