//! 持续直伤及状态伤害，供配装搜索评估。

use crate::pymath::{pmax, pmin, psum, sqrt};
use crate::replay::Types;
use crate::tables::{self, Core, Location, TargetTables, Vector};

type D = u32;

/// 与配装无关的目标护甲、抗性与命中部位。
pub struct SustainedTarget {
    pub armour_base: f64,
    pub armour_strip: f64,
    pub aura_armour_reduction: f64,
    pub elemental_armour_strip: f64,
    pub overguard: f64,
    pub shield: f64,
    pub magnetic_stacks: i64,
    pub shielded: bool,
    pub faction_deltas: Vec<(String, f64)>,
    pub armour_bypass: Vec<String>,
    pub shield_bypass: Vec<String>,
    pub target_faction: String,
    pub faction_bracket: f64,
    pub body: (f64, f64, bool),
    pub located: (f64, f64, bool),
    pub head_eligible: bool,
    pub existing_magnetic: i64,
}

/// 对应 `_magnetic_defense_multiplier` 的磁力防御倍率。
fn magnetic_defense(stacks: i64) -> f64 {
    if stacks != 0 {
        pmin(2.0 + 0.25 * (stacks - 1) as f64, 4.25)
    } else {
        1.0
    }
}

/// 对应 `_armour_reduction` 的护甲减伤。
fn armour_reduction(armour: f64) -> f64 {
    if armour <= 0.0 {
        return 0.0;
    }
    let effective = pmin(armour, 2700.0);
    0.9 * sqrt(effective / 2700.0)
}

struct Context<'a> {
    target: &'a SustainedTarget,
    types: &'a Types,
}

impl Context<'_> {
    fn in_set(set: &[String], name: &str) -> bool {
        set.iter().any(|x| x == name)
    }

    /// 当前元素削甲对应的 `enemy_defenses(...)["armour"]`。
    fn armour(&self, elemental_strip: f64) -> f64 {
        let mut armour = self.target.armour_base;
        armour *= 1.0 - self.target.armour_strip;
        armour *= 1.0 - elemental_strip;
        armour *= 1.0 - self.target.aura_armour_reduction;
        armour
    }

    /// 计算每个伤害类型的倍率。优先处理超宏防护，否则用 `_target_damage_multiplier` 并应用护盾的磁力倍率。
    fn multiplier(&self, d: D, armour: f64) -> f64 {
        let name = self.types.name(d);
        let t = self.target;
        if t.overguard > 0.0 {
            let radiant = if name == "DT_RADIANT" || name == "DT_VOID" { 1.5 } else { 1.0 };
            return radiant * magnetic_defense(t.magnetic_stacks);
        }
        let delta = t.faction_deltas.iter().find(|x| x.0 == name).map(|x| x.1).unwrap_or(0.0);
        let mut value = pmax(0.0, 1.0 + delta);
        let hits_health = !t.shielded || Self::in_set(&t.shield_bypass, name);
        if hits_health && armour != 0.0 && !Self::in_set(&t.armour_bypass, name) {
            value *= 1.0 - armour_reduction(armour);
        }
        if t.shield > 0.0 && !Self::in_set(&t.shield_bypass, name) {
            value *= magnetic_defense(t.magnetic_stacks);
        }
        value
    }

    fn location(&self, s: &Core) -> (f64, f64, bool) {
        if s.radial_only { self.target.body } else { self.target.located }
    }

    /// `damage_against` 返回每种类型的伤害和单次射击伤害。
    fn damage_against(&self, s: &Core, armour: f64) -> (Vector, f64) {
        let (hm, hcm, _head) = self.location(s);
        let hcm = if s.ignore_headshot_bonus { 1.0 } else { hcm };
        let applied_hm = if s.ignore_headshot_bonus || s.area_direct { 1.0 } else { hm };
        let base = match s.dot_base_damage {
            Some(v) => v,
            None => s.total_damage(),
        };
        let quantized = tables::quantize(&s.damage_types, base);
        let mut per_type: Vector = quantized
            .iter()
            .map(|&(d, amount)| {
                let mut value = self.multiplier(d, armour);
                value *= amount;
                (d, value * s.falloff_multiplier)
            })
            .collect();
        let mut faction = s.faction_all;
        if !self.target.target_faction.is_empty() {
            faction *= s.faction_target;
        }
        faction *= self.target.faction_bracket;
        if faction != 1.0 {
            for e in per_type.iter_mut() {
                e.1 *= faction;
            }
        }
        if applied_hm != 1.0 {
            for e in per_type.iter_mut() {
                e.1 *= applied_hm;
            }
        }
        let raw = psum(per_type.iter().map(|e| e.1)) * tables::location_hit_factor(s, hm, hcm);
        (per_type, raw * s.multishot)
    }

    /// 计算此护甲下 `expected_status_damage` 的持续伤害及爆炸。
    fn status(&self, s: &Core, armour: f64, types: &[D; 6]) -> tables::Status {
        let (bhm, bhcm, bhead) = self.target.body;
        let (lhm, lhcm, lhead) = self.target.located;
        let tables = TargetTables {
            target_faction: !self.target.target_faction.is_empty(),
            damage: [Location { hit_multiplier: bhm, hit_critical_multiplier: bhcm, head: bhead },
                     Location { hit_multiplier: lhm, hit_critical_multiplier: lhcm, head: lhead }],
            dot: [Location { hit_multiplier: bhm, hit_critical_multiplier: bhcm, head: bhead },
                  Location { hit_multiplier: lhm, hit_critical_multiplier: lhcm, head: lhead }],
            dot_head_eligible: [false, self.target.head_eligible],
            dot_multiplier: types.iter().map(|&d| (d, self.multiplier(d, armour))).collect(),
            forced_procs: Vec::new(),
        };
        tables::status(s, 1, &tables)
    }
}

/// `sustained_status_damage` 中按叠层计算的易伤倍率。
fn vulnerability(count: f64) -> f64 {
    if count <= 0.0 {
        return 1.0;
    }
    if count < 1.0 {
        return 1.0 + count;
    }
    pmin(2.0 + 0.25 * (count - 1.0), 4.25)
}

/// 一次持续伤害计算所需的武器属性和速率。
pub struct SustainedInput<'a> {
    pub types: &'a Types,
    pub core: &'a Core,
    pub attacks_per_second: f64,
    pub status_rate_multiplier: f64,
    pub damage_rate_multiplier: f64,
    pub total: bool,
    pub magazine: f64,
    pub first_shot_bonus: f64,
}

pub fn sustained_status(target: &SustainedTarget, input: SustainedInput<'_>) -> f64 {
    let SustainedInput {
        types, core, attacks_per_second, status_rate_multiplier,
        damage_rate_multiplier, total, magazine, first_shot_bonus,
    } = input;
    let id = |n: &str| types.get(n).expect("内置伤害类型已注册");
    let (viral, corrosive, magnetic, cold, puncture, heat) = (
        id("DT_VIRAL"), id("DT_CORROSIVE"), id("DT_MAGNETIC"),
        id("DT_FREEZE"), id("DT_PUNCTURE"), id("DT_FIRE"),
    );
    let interactions = [
        id("DT_CINEMATIC"), id("DT_FIRE"), id("DT_POISON"),
        id("DT_ELECTRICITY"), id("DT_GAS"), id("DT_EXPLOSION"),
    ];
    let (slash, cinematic, gas, electricity) = (id("DT_SLASH"), id("DT_CINEMATIC"), id("DT_GAS"), id("DT_ELECTRICITY"));
    let ctx = Context { target, types };

    let rate = pmax(attacks_per_second, 0.0);
    let proc_rate = rate * pmax(status_rate_multiplier, 0.0);
    let mut damage_rate = rate * pmax(damage_rate_multiplier, 0.0);
    let magazine = pmax(magazine, 0.0);
    let first_shot_bonus = pmax(first_shot_bonus, 0.0);
    if magazine > 0.0 && first_shot_bonus > 0.0 && damage_rate > 0.0 {
        damage_rate *= 1.0 + first_shot_bonus / magazine;
    }
    // 用目标命中部位算出探测射击的预期异常数。
    let (_, _, head) = ctx.location(core);
    let procs = tables::expected_status_procs(core, &Vec::new(), head);
    let proc_of = |d: D| tables::get(&procs, d).unwrap_or(0.0);
    let stacks = |d: D, duration: f64, cap: f64| pmin(pmax(proc_of(d) * proc_rate * duration * core.duration_scale(d), 0.0), cap);
    let viral_count = stacks(viral, 6.0, 10.0);
    let corrosive_count = stacks(corrosive, 8.0, 10.0);
    let magnetic_count = stacks(magnetic, 6.0, 10.0);
    let cold_count = stacks(cold, 6.0, 10.0);
    let puncture_count = stacks(puncture, 10.0, core.stack_cap(puncture, 5.0));
    let heat_count = stacks(heat, 6.0, 1.0);

    let corrosive_strip = if corrosive_count < 1.0 {
        0.26 * corrosive_count
    } else {
        pmin(0.26 + 0.06 * (corrosive_count - 1.0), 0.8)
    };
    let heat_strip = 0.5 * heat_count;
    let elemental = target.elemental_armour_strip;
    let combined_strip = 1.0 - ((1.0 - elemental) * (1.0 - corrosive_strip) * (1.0 - heat_strip));
    let cold_proc_rate = proc_of(cold) * proc_rate;
    let cold_crit = if cold_count >= 10.0 && cold_proc_rate > 0.0 {
        let freeze_duration = 3.0 * core.duration_scale(cold);
        let rebuild_duration = 7.0 / cold_proc_rate;
        let uptime = if freeze_duration > 0.0 { freeze_duration / (freeze_duration + rebuild_duration) } else { 0.0 };
        0.35 * (1.0 - uptime) + uptime
    } else if cold_count < 1.0 {
        0.1 * cold_count
    } else {
        pmin(0.1 + 0.05 * (cold_count - 1.0), 0.5)
    };
    let puncture_crit = pmin(0.05 * puncture_count, 0.05 * core.stack_cap(puncture, 5.0));
    let mut adjusted = core.clone();
    adjusted.crit_chance = pmax(core.crit_chance + puncture_crit, 0.0);
    adjusted.crit_multiplier = core.crit_multiplier + cold_crit;

    let combined_armour = ctx.armour(combined_strip);
    let (per_type, per_shot) = ctx.damage_against(&adjusted, combined_armour);
    let viral_multiplier = vulnerability(viral_count);
    let combined_magnetic = pmin(target.existing_magnetic as f64 + magnetic_count, 10.0);
    let magnetic_multiplier = vulnerability(combined_magnetic) / vulnerability(target.existing_magnetic as f64);
    let pool_multiplier = |d: D| -> f64 {
        if target.overguard > 0.0 {
            return magnetic_multiplier;
        }
        if target.shield > 0.0 && !Context::in_set(&target.shield_bypass, types.name(d)) {
            return magnetic_multiplier;
        }
        viral_multiplier
    };
    let direct_total = psum(per_type.iter().map(|e| e.1));
    let weighted_direct = psum(per_type.iter().map(|&(d, v)| v * pool_multiplier(d)));
    let direct_dps = per_shot * (if direct_total > 0.0 { weighted_direct / direct_total } else { 1.0 }) * damage_rate;
    if !total {
        return direct_dps;
    }
    // 总 DPS 以直伤 DPS 为基础，再加独立的持续和异常伤害。异常对直伤的增幅已计入直伤 DPS。
    let status = ctx.status(&adjusted, combined_armour, &interactions);
    let mut dot = Vec::new();
    for (d, row) in status.rows.iter() {
        let expected_procs = pmax(tables::get(&status.procs, *d).unwrap_or(0.0), 0.0);
        let interaction = if *d == slash { cinematic } else { *d };
        let damage_multiplier = pool_multiplier(interaction);
        let per_second = (row.seed + row.accumulator) * row.landing;
        let value = if *d == gas || *d == electricity {
            let active = expected_procs * proc_rate * row.duration;
            let active = if *d == gas { pmin(active, 10.0) } else { active };
            let average_hit = if status_rate_multiplier != 0.0 {
                pmax(damage_rate_multiplier, 0.0) / pmax(status_rate_multiplier, 0.0)
            } else {
                0.0
            };
            // 稳态模型用平均活跃层数近似，只要均值为正就算一组。窗口回放会单独处理占用概率和最近一次成功命中。
            (row.seed * active + if active > 0.0 { row.accumulator } else { 0.0 }) * row.landing * average_hit
        } else {
            let count = tables::get(&status.procs, *d).unwrap_or(0.0);
            per_second * row.duration * count * damage_rate
        };
        dot.push(value * damage_multiplier);
    }
    let dot_dps = psum(dot);
    let blast = tables::get(&status.procs, interactions[5]).unwrap_or(0.0);
    let non_dot: Vec<f64> = status
        .non
        .iter()
        .map(|&(d, per_proc)| per_proc * blast * damage_rate * pool_multiplier(d))
        .collect();
    let proc_non_dot_dps = psum(non_dot);
    direct_dps + dot_dps + proc_non_dot_dps
}

