use super::{merge, num, target::{overguard_multiplier, Defenses, HitLocation, Target}, ST_STACK_NAMES};
use serde_json::{json, Map, Value};
use std::collections::BTreeSet;
use wfcore_lib::{
    mods::{ModdedFields, Num}, pymath::psum, replay::{round_decimal, Types},
    sustained::{armour_reduction, magnetic_defense, sustained_snapshot, SustainedInput},
    tables::{self, Core, D, Vector, CINEMATIC, ELECTRICITY, EXPLOSION, GAS, SLASH},
};

pub(super) fn types_json(types: &Types, vector: &Vector) -> Value {
    Value::Object(vector.iter().map(|&(key, v)| (types.name(key).to_owned(), json!(v))).collect())
}

fn num_json(number: Num) -> Value {
    match number { Num::I(value) => json!(value), Num::F(value) => json!(value) }
}

pub(super) fn panel(fields: &ModdedFields, core: &Core, types: &Types) -> Value {
    let mut out = Map::new();
    macro_rules! fields {
        ($($name:ident),+ $(,)?) => { $(out.insert(stringify!($name).to_owned(), json!(fields.$name));)+ }
    }
    macro_rules! numbers {
        ($($name:ident),+ $(,)?) => { $(out.insert(stringify!($name).to_owned(), num_json(fields.$name));)+ }
    }
    macro_rules! maps {
        ($($name:ident),+ $(,)?) => { $(out.insert(stringify!($name).to_owned(), Value::Object(
            fields.$name.iter().map(|(key, value)| (key.to_string(), json!(value))).collect()));)+ }
    }
    fields!(crit_multiplier, status_chance, multishot, magazine, spread_multiplier, recoil_multiplier,
        ammo_max_multiplier, headshot_bonus_multiplier, proc_damage_multiplier, proc_duration_multiplier,
        status_condition_damage_multiplier, initial_combo, combo_duration, heavy_attack_efficiency,
        heavy_charge_speed, slam_damage_ratio, noncrit_damage_multiplier, unsupported, skipped, falloff_multiplier,
        area_direct, stack_native_fraction, stackable_hit_event, dot_base_damage, double_crit_chance,
        first_shot_damage_bonus, weakpoint_damage_bonus, bodyshot_crit_multiplier, ammo_cost, co_bucket,
        co_assumed_count, continuous, radial_only, explosion_radius_multiplier, incarnon_charge_multiplier,
        low_crit_threshold, low_crit_damage_base, low_crit_damage_bonus, ammo_pool_multishot_cost, ammo_pool_damage_bonus,
        victim_full_crit_damage, victim_low_health_multiplier, projectile_speed_multiplier, charge_rate_multiplier,
        charge_seconds, bow_draw_seconds, hit_probability, extra_hits, base_damage_bracket, frozen_damage_multiplier,
        reflection_explosion_chance);
    numbers!(crit_chance, fire_rate, reload_time, combo_extra_chance, combo_miss_chance, transform_time_scale,
        weakpoint_crit_chance_bonus, weakpoint_crit_chance_relative, base_crit_chance, victim_full_crit_chance,
        cold_stack_damage, throw_damage_bonus);
    maps!(proc_duration_types, proc_max_stacks, faction_damage_multipliers, independent_proc_chances,
        dot_elemental_multipliers, innate_damage_types, native_forced_procs);
    out.insert("damage_types".into(), types_json(types, &core.damage_types));
    out.insert("impact_proc_conversions".into(), Value::Object(fields.impact_proc_conversions.iter().map(|(key, values)| {
        (key.clone(), json!(values.iter().map(|(chance, threshold, multiplier)| json!({
            "chance": chance, "fireRateThreshold": threshold, "fireRateMultiplier": multiplier
        })).collect::<Vec<_>>()))
    }).collect()));
    out.insert("crit_proc_chances".into(), fields.crit_proc_chances.map_or_else(|| json!({}), |value| {
        json!({"DT_SLASH": num_json(value)})
    }));
    let damage = core.total_damage();
    let crit = tables::effective_hit_multiplier(core.crit_chance, core.crit_multiplier, core.noncrit_damage_multiplier,
        1.0, core.double_crit_chance);
    let panel_crit = tables::effective_hit_multiplier(core.crit_chance, core.crit_multiplier, core.noncrit_damage_multiplier, 1.0, 0.0);
    let average = damage * crit * core.falloff_multiplier;
    let interval = super::weapon::interval(fields);
    let burst_rate = if interval > 0.0 { 1.0 / interval } else { core.fire_rate };
    let burst = damage * core.multishot * burst_rate * core.falloff_multiplier;
    let sustained = raw_sustained_dps(fields, core);
    out.extend(json!({
        "ignore_headshot_bonus": core.ignore_headshot_bonus, "radial_victim_crit": false,
        "delayed_radial_hit_event": false, "heavy_damage": null, "heavy_components": [],
        "spread_min_deg": null, "spread_max_deg": null, "innate_element_candidates": [],
        "totalDamage": damage * core.multishot.max(1.0),
        "perProjectileDamage": damage, "averageHit": average * core.multishot.max(1.0),
        "burstDps": burst * panel_crit, "sustainedDps": sustained * panel_crit,
        "damageTypes": types_json(types, &core.damage_types), "innateDamageTypes": out["innate_damage_types"],
        "innateElementCandidates": [], "critChance": out["crit_chance"], "critMultiplier": core.crit_multiplier,
        "statusChance": core.status_chance, "fireRate": out["fire_rate"], "reloadTime": out["reload_time"],
        "falloffMultiplier": core.falloff_multiplier, "hitProbability": fields.hit_probability,
        "noncritDamageMultiplier": core.noncrit_damage_multiplier, "doubleCritChance": core.double_crit_chance,
        "firstShotDamageBonus": fields.first_shot_damage_bonus,
        "firstShotHit": average * (1.0 + fields.first_shot_damage_bonus),
        "statusConditionDamageMultiplier": fields.status_condition_damage_multiplier,
        "coAssumedCount": fields.co_assumed_count, "stanceMultiplier": 1.0,
    }).as_object().unwrap().clone());
    Value::Object(out)
}

pub(super) fn raw_sustained_dps(fields: &ModdedFields, core: &Core) -> f64 {
    let damage = core.total_damage();
    let interval = super::weapon::interval(fields);
    let burst_rate = if interval > 0.0 { 1.0 / interval } else { core.fire_rate };
    let burst = damage * core.multishot * burst_rate * core.falloff_multiplier;
    let cost = if fields.ammo_cost == 0.0 && fields.bow_draw_seconds.is_some() { 1.0 } else { fields.ammo_cost };
    if fields.magazine == 0.0 || core.fire_rate == 0.0 || cost == 0.0 {
        burst
    } else {
        let shots = fields.magazine / cost;
        let cycle = shots * interval + fields.reload_time.f();
        damage * core.multishot * (shots + fields.first_shot_damage_bonus) * core.falloff_multiplier / cycle
    }
}

fn hit(core: &Core, loc: HitLocation) -> HitLocation {
    if core.radial_only { HitLocation::BODY } else { loc }
}

fn unsupported(defenses: &Defenses, loc: HitLocation) -> Vec<String> {
    let mut values: BTreeSet<_> = defenses.unsupported.iter().cloned().collect();
    values.extend(loc.unsupported.map(str::to_owned));
    values.into_iter().collect()
}

fn hit_factors(core: &Core, loc: HitLocation) -> (f64, f64) {
    let direct = if core.ignore_headshot_bonus || core.area_direct { 1.0 } else { loc.data.hit_multiplier };
    let crit = if core.ignore_headshot_bonus { 1.0 } else { loc.data.hit_critical_multiplier };
    (direct, crit)
}

fn faction(core: &Core, target: &Target<'_>) -> f64 {
    let base = if target.faction.is_empty() { core.faction_all } else { core.faction_all * core.faction_target };
    base * target.bracket
}

pub(super) fn direct(fields: &ModdedFields, core: &Core, types: &Types,
                     target: &Target<'_>, defenses: &Defenses, loc: HitLocation) -> Value {
    let loc = hit(core, loc);
    let (direct, crit) = hit_factors(core, loc);
    let faction = faction(core, target);
    let per_type: Vector = tables::quantize(&core.damage_types, fields.dot_base_damage)
        .into_iter().map(|(d, amount)| {
            let multiplier = target.multiplier(defenses, types.name(d), defenses.armour);
            (d, (multiplier * amount * core.falloff_multiplier * faction) * direct)
        }).collect();
    let per_hit = psum(per_type.iter().map(|v| v.1)) * tables::location_hit_factor(core, loc.data.hit_multiplier, crit);
    let shot = per_hit * core.multishot;
    let [overguard, shield, health] = defenses.full;
    let magnetic = magnetic_defense(defenses.magnetic);
    merge(json!({
        "perType": types_json(types, &per_type), "perHit": per_hit, "perShot": shot,
        "firstShot": shot * (1.0 + fields.first_shot_damage_bonus), "dps": shot * core.fire_rate,
        "factionMultiplier": faction, "falloffMultiplier": core.falloff_multiplier,
        "health": health, "shield": shield, "overguard": overguard,
        "magneticDefenseMultiplier": magnetic, "overguardDamageMultiplier": magnetic,
        "armour": defenses.armour, "armourReduction": armour_reduction(defenses.armour),
        "unsupported": unsupported(defenses, loc),
    }), loc.metadata())
}

pub(super) fn kill_metrics(fields: &ModdedFields, core: &Core, types: &Types,
                           target: &Target<'_>, defenses: &Defenses, loc: HitLocation) -> Value {
    let loc = hit(core, loc);
    let (location, hcm) = hit_factors(core, loc);
    let crit = tables::effective_hit_multiplier(core.crit_chance, core.crit_multiplier,
        core.noncrit_damage_multiplier, hcm, core.double_crit_chance);
    let common = crit * core.falloff_multiplier * core.faction_target * core.multishot * location;
    let [overguard, shield, health] = defenses.full;
    let (mut guard_shot, mut shield_shot, mut bypass, mut after) = (0.0, 0.0, 0.0, 0.0);
    for &(d, amount) in &core.damage_types {
        let dtype = types.name(d);
        let raw = amount * target.resistance(dtype) * common;
        let post_armour = if target.armour_bypass.iter().any(|v| v == dtype) { raw }
            else { raw * (1.0 - armour_reduction(defenses.armour)) };
        if overguard > 0.0 {
            guard_shot += amount * common * overguard_multiplier(dtype, defenses.magnetic);
        }
        if target.shield_bypass.iter().any(|v| v == dtype) { bypass += post_armour; }
        else {
            after += post_armour;
            if shield > 0.0 { shield_shot += raw * magnetic_defense(defenses.magnetic); }
        }
    }
    let after = bypass + after;
    let unsupported = unsupported(defenses, loc);
    let supported = unsupported.is_empty();
    let mut pulls = f64::INFINITY;
    if supported {
        if health <= 0.0 { pulls = 0.0; }
        else if overguard == 0.0 || guard_shot > 0.0 {
            let guard = if overguard > 0.0 { overguard / guard_shot } else { 0.0 };
            pulls = if shield <= 0.0 { guard + health / after }
                else if shield_shot <= 0.0 { guard + health / bypass }
                else {
                    let to_break = shield / shield_shot;
                    let to_bypass = health / bypass;
                    if to_bypass <= to_break { guard + to_bypass }
                    else { guard + to_break + (health - bypass * to_break).max(0.0) / after }
                };
        }
    }
    let pulls = pulls.is_finite().then_some(pulls);
    let ammo = pulls.map(|value| {
        let effective = if fields.first_shot_damage_bonus > 0.0 { (value - fields.first_shot_damage_bonus).max(1e-9) } else { value };
        (effective - 1e-12).max(0.0).ceil() as u64
    });
    let burst = ammo.and_then(|ammo| if ammo == 0 { Some(0.0) }
        else if core.fire_rate > 0.0 { Some((ammo - 1) as f64 / core.fire_rate) } else { None });
    let sustained = burst.zip(ammo).map(|(burst, count)| burst +
        (count.saturating_sub(1) / fields.magazine.floor().max(1.0) as u64) as f64 * fields.reload_time.f().max(0.0));
    let effective = pulls.map(|v| v * (core.total_damage() * crit * core.falloff_multiplier
        * faction(core, target) * core.multishot * location));
    let mut assumptions = vec!["direct-hit-only", "final-enemy-level-is-explicit", "no-shield-regeneration-gating-or-overflow"];
    if defenses.elemental_strip != 0.0 { assumptions.push("elemental-armour-strip-is-explicit-snapshot"); }
    if loc.data.head && loc.unsupported.is_none() { assumptions.push("head-hit-proxy-multiplier-applied"); }
    merge(json!({"supported": supported, "defenses": defenses.raw,
        "directDamage": {"overguardPerTrigger": guard_shot, "shieldPerTrigger": shield_shot,
            "healthBypassPerTrigger": bypass, "healthAfterShieldPerTrigger": after},
        "effectiveHealth": effective, "continuousTriggerPulls": pulls, "ammoRequired": ammo,
        "burstTtkSeconds": burst, "sustainedTtkSeconds": sustained, "killableByDirectHits": pulls.is_some(),
        "assumptions": assumptions, "unsupported": unsupported,
    }), loc.metadata())
}

fn get(vector: &Vector, dtype: D) -> f64 {
    vector.iter().find(|&&(key, _)| key == dtype).map_or(0.0, |v| v.1)
}

struct Status {
    response: Value,
    procs: Vector,
    dots: Vec<Dot>,
    blast: f64,
}

struct Dot {
    dtype: D,
    expected: f64,
    count: f64,
    duration: f64,
    seed: f64,
    landing: f64,
    coefficient: f64,
}

fn status(core: &Core, types: &Types, target: &Target<'_>, defenses: &Defenses,
          loc: HitLocation, forced: &Vector) -> Status {
    let loc = hit(core, loc);
    let table = target.tables(types, Some(defenses), forced.clone());
    let mut status = tables::status_details(core, usize::from(loc.data.head), &table);
    if target.record["path"] == "/Lotus/Types/Enemies/Grineer/Disruption/Avatars/DemoDevourerAvatar" {
        status.procs.retain(|(d, _)| types.name(*d) != "DT_RADIATION");
    }
    let procs = status.procs;
    let expected_procs = types_json(types, &procs);
    let mut rows = Map::new();
    let mut dots = Vec::new();
    let mut dot_total = 0.0;
    for ((d, row), seed) in status.rows.into_iter().zip(status.seeds) {
        let dtype = types.name(d);
        let count = get(&procs, d);
        let interaction = if d == SLASH { CINEMATIC } else { d };
        let target_multiplier = target.multiplier(defenses, types.name(interaction), defenses.armour);
        let elemental = core.dot_elemental_multipliers.iter().find(|&&(key, _)| key == d)
            .map_or(1.0, |v| v.1).max(0.0);
        let radial = d == ELECTRICITY || d == GAS;
        let landing_multiplier = if row.landing_head { core.headshot_bonus_multiplier } else { 1.0 };
        let radial_scale = if radial { elemental * faction(core, target).max(0.0) * target_multiplier } else { 1.0 };
        let per_second = if radial { seed.radial_per_second } else { row.seed + row.accumulator };
        let per_proc = per_second * row.duration;
        let expected = if radial {
            let instances = if core.continuous { 1.0 } else { core.multishot.max(0.0) };
            let extra = if instances > 0.0 { get(forced, d) / instances } else { 0.0 };
            let probability = 1.0 - tables::no_status_probability(core, d, if loc.data.head { 1.0 } else { 0.0 }, core.multishot, extra);
            if count == probability { per_second * probability * row.duration }
            else { ((row.seed * count + row.accumulator * probability) * radial_scale * landing_multiplier) * row.duration }
        } else { per_proc * count };
        dot_total += expected;
        let mut details = json!({"expectedProcs": count, "coefficient": row.coefficient, "durationSeconds": row.duration,
            "elementalMultiplier": elemental, "critMultiplier": seed.crit, "critConditionedShare": seed.crit_share,
            "seedTiers": seed.tiers.into_iter().map(|(weight, ps)| json!({"weight": weight, "perSecond": ps * radial_scale})).collect::<Vec<_>>(),
            "targetMultiplier": target_multiplier, "perSecond": per_second, "landingMultiplier": landing_multiplier,
            "landingHead": row.landing_head, "landingWeakEligible": row.landing_weak_eligible,
            "seedPerSecondPerProc": row.seed * radial_scale, "accumulatorPerSecondPerAttack": row.accumulator * radial_scale,
            "accumulatorSharedGroup": row.shared, "perProc": per_proc, "expectedDamage": expected});
        if radial {
            details["radialSeed"] = json!(row.seed);
            details["radialLanding"] = json!(row.landing);
        }
        rows.insert(dtype.to_owned(), details);
        dots.push(Dot { dtype: d, expected, count, duration: row.duration, seed: row.seed, landing: row.landing, coefficient: row.coefficient });
    }
    let diagnostics = unsupported(defenses, loc);
    let dot = merge(json!({"perType": rows, "total": dot_total, "expectedProcs": expected_procs,
        "durationSeconds": 6.0 * core.proc_duration_multiplier.max(0.0),
        "falloffMultiplier": core.falloff_multiplier, "factionMultiplier": faction(core, target),
        "armour": defenses.armour, "armourReduction": armour_reduction(defenses.armour),
        "elementalArmourStrip": defenses.elemental_strip, "overguard": defenses.full[0], "unsupported": diagnostics,
    }), loc.metadata());
    let mut non = json!({});
    let mut blast = 0.0;
    let mut aoe = 0.0;
    if let Some(&(_, per_proc)) = status.non.first() {
        let (_, hcm) = hit_factors(core, loc);
        let count = get(&procs, EXPLOSION);
        blast = per_proc * count;
        aoe = blast * (3.0 / 0.3);
        non["DT_EXPLOSION"] = json!({
            "expectedProcs": count, "coefficient": 0.3, "delaySeconds": 1.5,
            "baseDamage": core.dot_base_damage.unwrap_or_else(|| core.total_damage()).max(0.0),
            "elementalMultiplier": 1.0, "critMultiplier": tables::effective_hit_multiplier(core.crit_chance,
                core.crit_multiplier, core.noncrit_damage_multiplier, hcm, core.double_crit_chance),
            "statusDamageMultiplier": core.proc_damage_multiplier.max(0.0), "factionMultiplier": faction(core, target),
            "hitMultiplier": loc.data.hit_multiplier,
            "targetMultiplier": target.multiplier(defenses, "DT_EXPLOSION", defenses.armour),
            "perProc": per_proc, "expectedDamage": blast, "aoeMultiplier": 3.0, "aoePotential": aoe,
        });
    }
    let response = json!({"dot": dot, "nonDot": {"perType": non, "total": blast, "aoePotential": aoe},
        "dotTotal": dot_total, "nonDotTotal": blast, "total": dot_total + blast,
        "expectedProcs": expected_procs, "aoePotential": aoe, "unsupported": diagnostics});
    Status { response, procs, dots, blast }
}

pub(super) fn status_and_steady(fields: &ModdedFields, core: &Core, types: &Types, target: &Target<'_>,
                               defenses: &Defenses, loc: HitLocation, forced: &Vector, status_rate_multiplier: f64, damage_rate_multiplier: f64) -> (Value, Value) {
    let loc = hit(core, loc);
    let original = status(core, types, target, defenses, loc, forced);
    let rate = super::weapon::attack_rate(fields);
    let snapshot = sustained_snapshot(&target.sustained(defenses, loc), SustainedInput {
        types, core, attacks_per_second: rate, status_rate_multiplier, damage_rate_multiplier,
        total: false, magazine: fields.magazine, first_shot_bonus: fields.first_shot_damage_bonus,
    }, forced);
    let scaled = Defenses {
        raw: Value::Null, full: defenses.full, armour: (defenses.armour_base * (1.0 - defenses.armour_strip)) * (1.0 - snapshot.elemental_strip),
        armour_base: defenses.armour_base, armour_strip: defenses.armour_strip, elemental_strip: snapshot.elemental_strip,
        magnetic: defenses.magnetic, shielded: defenses.shielded, unsupported: defenses.unsupported.clone(),
    };
    let adjusted = status(&snapshot.adjusted, types, target, &scaled, loc, forced);
    let pool_multiplier = |dtype| {
        if defenses.full[0] > 0.0 || (defenses.full[1] > 0.0 &&
            !target.shield_bypass.iter().any(|name| name == types.name(dtype))) {
            snapshot.magnetic_multiplier
        } else { snapshot.viral_multiplier }
    };
    let mut dot_by_type = Map::new();
    let mut dot_dps = 0.0;
    for row in adjusted.dots {
        let value = if matches!(row.dtype, GAS | ELECTRICITY) {
            let active = row.count * rate * status_rate_multiplier * row.duration;
            let active = if row.dtype == GAS { active.min(10.0) } else { active };
            (row.seed * active + if active > 0.0 { row.coefficient } else { 0.0 }) * row.landing * (damage_rate_multiplier / status_rate_multiplier)
        } else { row.expected * snapshot.damage_rate };
        let value = value * pool_multiplier(if row.dtype == SLASH { CINEMATIC } else { row.dtype });
        dot_by_type.insert(types.name(row.dtype).to_owned(), json!(value));
        dot_dps += value;
    }
    let blast_dps = adjusted.blast * snapshot.damage_rate * pool_multiplier(EXPLOSION);
    let non_dot = if get(&adjusted.procs, EXPLOSION) > 0.0 { json!({"DT_EXPLOSION": blast_dps}) } else { json!({}) };
    let direct_bonus = (snapshot.direct_dps - snapshot.baseline_direct_dps).max(0.0);
    let non_dot_dps = blast_dps + direct_bonus;
    let (pool, amount) = super::active_pool(defenses.full);
    let mut ramp = snapshot.ramp;
    if num(&adjusted.response["dotTotal"]) > 0.0 {
        ramp = ramp.max(6.0 * [tables::SLASH, tables::FIRE, tables::POISON, ELECTRICITY, GAS]
            .into_iter().filter(|&d| get(&original.procs, d) != 0.0)
            .map(|d| core.duration_scale(d)).fold(core.proc_duration_multiplier.max(0.0), f64::max));
    }
    if adjusted.blast != 0.0 { ramp = ramp.max(1.5); }
    let steady = json!({"model": "steady-status-rate", "attacksPerSecond": rate, "statusEventsPerSecond": rate * status_rate_multiplier,
        "damageRateMultiplier": damage_rate_multiplier, "statusRateMultiplier": status_rate_multiplier,
        "steadyStateSeconds": ramp, "stacks": ST_STACK_NAMES.into_iter().zip(snapshot.stacks).collect::<MapFromStacks>(),
        "activePool": pool, "vulnerabilityMultiplier": if pool == "health" { snapshot.viral_multiplier } else { snapshot.magnetic_multiplier },
        "viralDamageMultiplier": snapshot.viral_multiplier, "magneticDamageMultiplier": snapshot.magnetic_multiplier,
        "corrosiveArmourStrip": snapshot.corrosive_strip, "heatArmourStrip": snapshot.heat_strip,
        "elementalArmourStrip": snapshot.elemental_strip, "coldCriticalDamageBonus": snapshot.cold_crit,
        "coldFreezeUptime": snapshot.cold_uptime, "punctureCriticalChanceBonus": snapshot.puncture_crit,
        "directDps": snapshot.direct_dps, "baselineDirectDps": snapshot.baseline_direct_dps, "statusDirectBonusDps": direct_bonus,
        "dotDps": dot_dps, "dotByType": dot_by_type, "nonDotDps": non_dot_dps,
        "procNonDotDps": if get(&adjusted.procs, EXPLOSION) > 0.0 { json!(blast_dps) } else { json!(0) },
        "nonDotByType": non_dot, "statusDps": dot_dps + non_dot_dps, "totalDps": snapshot.direct_dps + dot_dps + blast_dps,
        "magneticBreakPotential": if pool == "health" { 0.0 } else { amount * 0.03 * snapshot.combined_magnetic },
        "perAttack": adjusted.response, "unsupported": unsupported(defenses, loc),
    });
    (original.response, steady)
}

type MapFromStacks = std::collections::BTreeMap<&'static str, f64>;

pub(super) fn kill_timeline(result: &Value, stance: &Value) -> Option<Value> {
    let damage = &result["targetDamage"];
    let stats = &result["moddedStats"];
    let steady = &result["statusSustained"];
    let (per_shot, fire_rate, health) = (num(&damage["perShot"]), num(&stats["fireRate"]), num(&damage["health"]));
    if per_shot <= 0.0 || fire_rate <= 0.0 || health <= 0.0 { return None; }
    let (shield, overguard) = (num(&damage["shield"]), num(&damage["overguard"]));
    let hp = health + shield + overguard;
    let (magazine, reload) = (num(&stats["magazine"]), num(&stats["reloadTime"]));
    let has_cycle = magazine > 0.0 && reload > 0.0;
    let combo = &stance["bestCombo"];
    let duration = num(&combo["duration"]);
    let combo_multiplier = num(&combo["totalMultiplier"]);
    let (interval, event_damage, label) = if duration > 0.0 && combo_multiplier > 0.0 {
        (duration / fire_rate, per_shot * combo_multiplier, "combo")
    } else { (1.0 / fire_rate, per_shot, "attack") };
    let rate = if has_cycle { magazine / (magazine * interval + reload) } else { 1.0 / interval };
    let multiplier = (num(&steady["directDps"]) / (event_damage * rate)).max(0.0);
    let (dot, blast, ramp) = (num(&steady["dotDps"]), num(&steady["procNonDotDps"]), num(&steady["steadyStateSeconds"]).max(0.0));
    let magnetic = num(&steady["magneticBreakPotential"]).max(0.0);
    let threshold = if steady["activePool"] == "overguard" { health + shield } else { health };
    let mut schedule = vec![0.0];
    let mut cursor = 0.0;
    while cursor <= 30.0 + interval && schedule.len() < 4000 {
        cursor += interval;
        schedule.push(cursor);
        if has_cycle && (schedule.len() - 1) % (magazine as usize).max(1) == 0 { cursor += reload; }
    }
    let step = (interval / 5.0).max(0.005);
    let round = round_decimal;
    let mut points = vec![json!({"time": 0.0, "remaining": round(hp, 3), "shots": 0, "cumulative": 0.0})];
    let (mut remaining, mut shots, mut elapsed) = (hp, 0, -step);
    let mut killed = None;
    let mut magnetic_applied = false;
    while elapsed < 30.0 {
        elapsed = round(elapsed + step, 6);
        if elapsed <= 0.0 { continue; }
        let progress = if ramp > 0.0 { (elapsed / ramp).min(1.0) } else { 1.0 };
        let blast_progress = if elapsed < 1.5 { 0.0 }
            else if ramp > 1.5 { ((elapsed - 1.5) / (ramp - 1.5).max(step)).min(1.0) }
            else { 1.0 };
        let mut dealt = (dot * progress + blast * blast_progress) * step;
        let weighted = 1.0 + (multiplier - 1.0) * progress;
        while killed.is_none() && schedule.get(shots).is_some_and(|v| *v <= elapsed) {
            dealt += event_damage * weighted;
            shots += 1;
        }
        if killed.is_none() {
            remaining -= dealt;
            if !magnetic_applied && magnetic > 0.0 && steady["activePool"] != "health" && remaining <= threshold {
                remaining -= magnetic;
                magnetic_applied = true;
            }
            if remaining <= 0.0 { killed = Some(elapsed); remaining = 0.0; }
        }
        points.push(json!({"time": round(elapsed, 4), "remaining": round(remaining.max(0.0), 3),
            "shots": shots, "cumulative": round(hp - remaining.max(0.0), 3)}));
        if killed.is_some_and(|t| elapsed - t >= interval.max(step * 4.0)) { break; }
    }
    Some(json!({"model": "attack-or-combo-with-reload-and-status-ramp",
        "perShot": round(per_shot, 3), "eventDamage": round(event_damage, 3), "eventLabel": label,
        "fireRate": round(fire_rate, 3), "statusDps": round(dot + blast, 3),
        "steadyDirectMultiplier": round(multiplier, 6), "statusRampSeconds": round(ramp, 4),
        "magneticBreakDamage": round(magnetic, 3), "magneticBreakApplied": magnetic_applied,
        "targetHp": round(hp, 3), "health": round(health, 3), "shield": round(shield, 3), "overguard": round(overguard, 3),
        "magazine": has_cycle.then_some(magazine), "reloadTime": has_cycle.then_some(round(reload, 3)),
        "killTime": killed.map(|t| round(t, 4)), "shotsToKill": killed.map(|_| shots),
        "step": round(step, 4), "points": points,
    }))
}
