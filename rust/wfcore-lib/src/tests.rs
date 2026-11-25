use std::convert::Infallible;
use std::sync::Arc;

use crate::{heat, mods, proc, pymath, replay, spatial, sustained, tables};

#[test]
fn compensated_sum_and_signed_zero() {
    assert_eq!(pymath::psum([1e16, 1.0, -1e16]).to_bits(), 1.0_f64.to_bits());
    assert_eq!(pymath::psum([-0.0]).to_bits(), 0.0_f64.to_bits());
    assert_eq!(pymath::pmin(-0.0, 0.0).to_bits(), (-0.0_f64).to_bits());
    assert_eq!(pymath::pow(f64::NAN, 0.0).to_bits(), 1.0_f64.to_bits());
    assert!(pymath::psum([f64::INFINITY, f64::NEG_INFINITY]).is_nan());
}

#[test]
fn heat_ramps_and_expires() {
    let mut chains = heat::HeatChains::new();
    chains.add(0.0, 0.0, 1.0, 12.0, 6.0, 1.0);
    assert_eq!(chains.count().to_bits(), 1.0_f64.to_bits());
    assert_eq!(chains.seed().to_bits(), 12.0_f64.to_bits());
    assert_eq!(chains.strip(0.5).to_bits(), 0.15_f64.to_bits());
    assert_eq!(chains.strip(2.0).to_bits(), 0.5_f64.to_bits());
    chains.expire(6.25);
    assert_eq!(chains.alive().to_bits(), 0.0_f64.to_bits());
    assert_eq!(chains.seed().to_bits(), 0.0_f64.to_bits());
    assert_eq!(chains.strip(9.25).to_bits(), 0.3_f64.to_bits());
}

#[test]
fn capped_pool_and_presence_expire_on_the_same_boundary() {
    let mut pool = proc::CappedPool::new(2.0);
    assert!(pool.add(1.0, 0.5, 0.5));
    assert!(pool.add(2.0, 0.0, 1.0));
    assert_eq!(pool.distribution(), &[0.0, 0.5, 0.5]);
    pool.expire(1.0);
    assert_eq!(pool.stats(), (1.0, 1.5));
    pool.expire(1.25);
    assert_eq!(pool.distribution(), &[0.0, 1.0, 0.0]);

    let mut presence = proc::StatusPresence::new();
    presence.add(1.0, 0.0);
    presence.add(2.0, 0.5);
    assert_eq!(presence.probability(1.0).to_bits(), 1.0_f64.to_bits());
    assert_eq!(presence.probability(1.25).to_bits(), 0.5_f64.to_bits());
}

fn damage_effect(index: usize, upgrade: &str, value: f64) -> mods::ClassifiedEffect {
    let row = mods::EffectInput {
        operation: "STACKING_MULTIPLY".into(),
        value,
        upgrade: upgrade.into(),
        damage_type: None,
        symbol: None,
        source: None,
        runtime_kind: None,
        runtime_damage_type: None,
        runtime_chance: None,
        runtime_threshold: None,
        runtime_multiplier: 1.0,
    };
    mods::ClassifiedEffect {
        index,
        row: Arc::new(row.into()),
        status: 0,
        message: None,
        unsupported: None,
    }
}

#[test]
fn mods_preserve_scalar_types_and_effect_indexes() {
    let impact = mods::intern("DT_IMPACT");
    let prep = mods::ModsPrep {
        base: vec![(impact, 16.0)],
        innate_multishot: 1.0,
        base_magazine: 10.0,
        crit_multiplier: mods::Num::I(2),
        fire_rate: mods::Num::I(2),
        ammo_cost: 1.0,
        co_base_fraction: 1.0,
        ..Default::default()
    };
    let input = mods::ModsInput {
        effects: vec![
            damage_effect(4, "WEAPON_DAMAGE_AMOUNT", 0.5),
            damage_effect(8, "WEAPON_DAMAGE_IF_VICTIM_PROC_ACTIVE", 0.25),
        ],
        skipped: vec!["示例".into(), "示例".into()],
        victim_status_count: 2.0,
        need_spec: true,
    };
    let result = mods::apply_mods(&prep, input, std::iter::empty::<Result<_, Infallible>>()).unwrap();
    let fields = result.fields;
    assert_eq!(fields.damage_types.0, vec![(impact, 32.0)]);
    assert!(matches!(fields.fire_rate, mods::Num::I(2)));
    assert_eq!(fields.status_condition_damage_multiplier.to_bits(), 1.5_f64.to_bits());
    assert_eq!(fields.co_bucket, vec![8]);
    assert_eq!(fields.skipped, vec!["示例"]);
    assert!(matches!(result.spec.unwrap().scale_effects.as_slice(), [mods::Scale::Installed(4)]));
}

fn impact_core() -> tables::Core {
    tables::Core {
        damage_types: vec![(tables::IMPACT, 20.0)],
        crit_multiplier: 2.0,
        noncrit_damage_multiplier: 1.0,
        falloff_multiplier: 1.0,
        faction_all: 1.0,
        faction_target: 1.0,
        faction_bracket: 1.0,
        multishot: 1.0,
        headshot_bonus_multiplier: 1.0,
        bodyshot_crit_multiplier: 1.0,
        proc_duration_multiplier: 1.0,
        proc_damage_multiplier: 1.0,
        fire_rate: 1.0,
        ..Default::default()
    }
}

#[test]
fn sustained_scales_the_first_shot_over_the_magazine() {
    let target = sustained::SustainedTarget {
        armour_base: 0.0,
        armour_strip: 0.0,
        aura_armour_reduction: 0.0,
        elemental_armour_strip: 0.0,
        overguard: 0.0,
        shield: 0.0,
        magnetic_stacks: 0,
        shielded: false,
        faction_deltas: Vec::new(),
        armour_bypass: Vec::new(),
        shield_bypass: Vec::new(),
        target_faction: String::new(),
        faction_bracket: 1.0,
        body: (1.0, 1.0, false),
        located: (1.0, 1.0, false),
        head_eligible: false,
        existing_magnetic: 0,
    };
    for total in [false, true] {
        let dps = sustained::sustained_status(&target, sustained::SustainedInput {
            types: &replay::Types::new(),
            core: &impact_core(),
            attacks_per_second: 2.0,
            status_rate_multiplier: 1.0,
            damage_rate_multiplier: 1.0,
            total,
            magazine: 2.0,
            first_shot_bonus: 0.25,
        });
        assert_eq!(dps.to_bits(), 45.0_f64.to_bits());
    }
}

#[test]
fn spatial_ties_and_disabled_reverse_edges() {
    let bvh = spatial::SpatialIndex::new(
        vec![[0.0, 0.0, 1.0], [1.0, 0.0, 1.0], [0.0, 1.0, 1.0]],
        vec![[0, 1, 2], [0, 2, 1]],
    ).unwrap();
    let origin = [0.25, 0.25, 0.0];
    let direction = [0.0, 0.0, 2.0];
    assert_eq!(bvh.raycast(origin, direction, 2.0, 1e-5), Ok(Some((1.0, 0))));
    assert_eq!(bvh.all_hits(origin, direction, 2.0, 1e-5, 2), Ok(vec![(1.0, 0), (1.0, 1)]));

    let graph = spatial::PathGraph::new(3, vec![(0, 1, 0.5, 10), (1, 2, 0.5, 11), (0, 2, 2.0, 12)]).unwrap();
    assert_eq!(graph.search(0, false, vec![]), Ok((vec![0.0, 0.5, 1.0], vec![-1, 0, 1], vec![-1, 0, 1])));
    assert_eq!(graph.search(2, true, vec![11]), Ok((vec![2.0, f64::INFINITY, 0.0], vec![2, -1, -1], vec![2, -1, -1])));
}

struct FixedWeapon;

impl replay::ReplayProvider for FixedWeapon {
    fn resolve(&self, _input: replay::ResolveInput) -> replay::ReplayResult<(replay::ObjectId, Option<replay::ObjectId>)> {
        Ok((0, None))
    }

    fn stats(&self, object: replay::ObjectId, _context: &mut replay::ViewContext<'_>)
        -> replay::ReplayResult<replay::Stats> {
        assert_eq!(object, 0);
        let core = impact_core();
        Ok(replay::Stats {
            damage_types: core.damage_types.clone(),
            core,
            magazine: 100.0,
            fire_rate: 1.0,
            ammo_cost: 1.0,
            multishot: 1.0,
            crit_multiplier: 2.0,
            bodyshot_crit_multiplier: 1.0,
            victim_low_health_multiplier: 1.0,
            ..Default::default()
        })
    }
}

fn replay_input() -> replay::TrajectoryInput {
    let location = tables::Location { hit_multiplier: 1.0, hit_critical_multiplier: 1.0, head: false };
    replay::TrajectoryInput {
        window: 2.5,
        rate: 1.0,
        status_rate_multiplier: 1.0,
        damage_rate_multiplier: 1.0,
        full: [0.0, 0.0, 200.0],
        ehp: 200.0,
        extra_scale: 1.0,
        faction_bracket: 1.0,
        puncture_cap: 5.0,
        tables: tables::TargetTables { damage: [location; 2], dot: [location; 2], ..Default::default() },
        ..Default::default()
    }
}

#[test]
fn replay_and_bake_emit_the_same_three_direct_hits() {
    let out = replay::trajectory(replay_input(), &FixedWeapon).unwrap();
    assert_eq!(out.attacks, 3);
    assert_eq!(out.kills, 0);
    assert_eq!(out.pools, [0.0, 0.0, 140.0]);
    assert_eq!(out.totals, [60.0, 0.0, 0.0, 0.0, 0.0]);

    let stream = replay::direct_bake_stream(replay::DirectBakeStreamInput { trajectory: replay_input() }, &FixedWeapon).unwrap();
    assert_eq!(stream, (vec![20.0, 20.0, 20.0], 200.0, 2.5));

    let mut input = replay_input();
    input.head = 1.0;
    assert!(matches!(
        replay::direct_bake_stream(replay::DirectBakeStreamInput { trajectory: input }, &FixedWeapon),
        Err(replay::ReplayError::Unsupported(_))
    ));
}

#[test]
fn replay_health_batch_isolates_targets_and_checks_input() {
    let rows = replay::trajectory_health_batch(replay_input(), &FixedWeapon,
        vec![([0.0, 0.0, 200.0], 200.0), ([0.0, 0.0, 400.0], 400.0)]).unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].damage.to_bits(), 60.0_f64.to_bits());
    assert_eq!(rows[1].damage.to_bits(), 60.0_f64.to_bits());
    assert!(rows[0].kpm > rows[1].kpm);
    assert!(matches!(
        replay::trajectory_health_batch(replay_input(), &FixedWeapon, vec![]),
        Err(replay::ReplayError::Invalid(replay::InvalidReplay::Value(_)))
    ));
}
