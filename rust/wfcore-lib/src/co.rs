//! `damage.apply_mods` 的异况超量结算（`damage._co_damage`）。仅预期状态种数变化时，复用已有 `damage.CoSpec`，重新计算该效果组。
//!
//! 字典用向量保持插入顺序，删掉的键重新插入时放到末尾，以保留下游浮点求和顺序。
use crate::pymath::pmax;

pub type D = u32;

pub const STACKING_MULTIPLY: u8 = 1;
pub const MULTIPLY: u8 = 2;
pub const ADD: u8 = 3;
pub const ADD_BASE: u8 = 4;
pub const SET: u8 = 5;

pub fn op_code(op: &str) -> u8 {
    match op {
        "STACKING_MULTIPLY" => STACKING_MULTIPLY,
        "MULTIPLY" => MULTIPLY,
        "ADD" => ADD,
        "ADD_BASE" => ADD_BASE,
        "SET" | "SET_BASE" => SET,
        _ => 0,
    }
}

/// 在依次拼接的 `parts` 上执行 `damage._combine`。
pub fn combine(base: f64, parts: &[&[(u8, f64)]], base_value: Option<f64>) -> f64 {
    if parts.iter().all(|p| p.is_empty()) {
        return base;
    }
    let mut origin = base_value.unwrap_or(base);
    let mut additive = 0.0;
    let mut factors = 1.0;
    let mut flat = 0.0;
    let mut over: Option<f64> = None;
    for part in parts {
        for &(op, value) in part.iter() {
            match op {
                STACKING_MULTIPLY => additive += value,
                MULTIPLY => factors *= value,
                ADD => flat += value,
                ADD_BASE => origin += value,
                SET => over = Some(value),
                _ => {}
            }
        }
    }
    if let Some(v) = over {
        return v;
    }
    (origin * (1.0 + additive) * factors) + flat
}

/// 元素规则涉及的伤害类型 ID。
#[derive(Clone)]
pub struct Elements {
    pub physical: [D; 3],
    pub base: [D; 4],
    /// 元组是两个可互换的基础元素和一个复合元素。
    pub combos: Vec<(D, D, D)>,
    pub electricity: D,
    pub radiation: D,
}

impl Elements {
    pub(crate) fn physical(&self, d: D) -> bool {
        self.physical.contains(&d)
    }
    fn base(&self, d: D) -> bool {
        self.base.contains(&d)
    }
    fn combo(&self, a: D, b: D) -> Option<D> {
        self.combos.iter().find(|(x, y, _)| (*x == a && *y == b) || (*x == b && *y == a)).map(|c| c.2)
    }
}

/// 按插入顺序存放的伤害类型字典。
#[derive(Clone, Default)]
pub struct Dict(pub Vec<(D, f64)>);

impl Dict {
    pub(crate) fn get(&self, d: D) -> Option<f64> {
        self.0.iter().find(|e| e.0 == d).map(|e| e.1)
    }
    pub(crate) fn get_or(&self, d: D, default: f64) -> f64 {
        self.get(d).unwrap_or(default)
    }
    pub(crate) fn set(&mut self, d: D, v: f64) {
        match self.0.iter_mut().find(|e| e.0 == d) {
            Some(e) => e.1 = v,
            None => self.0.push((d, v)),
        }
    }
    pub(crate) fn pop(&mut self, d: D) -> Option<f64> {
        let i = self.0.iter().position(|e| e.0 == d)?;
        Some(self.0.remove(i).1)
    }
    pub(crate) fn contains(&self, d: D) -> bool {
        self.0.iter().any(|e| e.0 == d)
    }
    /// 保留数值非零的键值对。
    pub(crate) fn truthy(self) -> Dict {
        Dict(self.0.into_iter().filter(|e| e.1 != 0.0).collect())
    }
    pub(crate) fn scale(&mut self, f: f64) {
        for e in self.0.iter_mut() {
            e.1 *= f;
        }
    }
}

/// 单条元素效果的伤害类型和值，无指定类型或为 DT_ANY 时类型为 None。
pub type Element = (Option<D>, f64);

/// 应用 `damage._apply_elements` 的元素加成。
pub fn apply_elements(scaled_base: &Dict, base_total: f64, pre: &[Element], post: &[Element], el: &Elements) -> Dict {
    let mut result = scaled_base.clone();
    let mut added = Dict::default();
    let mut order: Vec<D> = Vec::new();
    for &(dtype, value) in pre {
        let Some(dtype) = dtype else {
            for i in 0..result.0.len() {
                let key = result.0[i].0;
                result.0[i].1 += scaled_base.get_or(key, 0.0) * value;
            }
            continue;
        };
        if !added.contains(dtype) {
            order.push(dtype);
        }
        let bonus_base = if el.physical(dtype) { scaled_base.get_or(dtype, 0.0) } else { base_total };
        let prev = added.get_or(dtype, 0.0);
        added.set(dtype, prev + bonus_base * value);
    }
    let keys: Vec<D> = added.0.iter().map(|e| e.0).collect();
    for dtype in keys {
        if el.physical(dtype) {
            let amount = added.pop(dtype).unwrap();
            let v = pmax(0.0, result.get_or(dtype, 0.0) + amount);
            result.set(dtype, v);
            if let Some(i) = order.iter().position(|&d| d == dtype) {
                order.remove(i);
            }
        }
    }
    for &dtype in el.base.iter() {
        let amount = result.pop(dtype).unwrap_or(0.0);
        if amount == 0.0 {
            continue;
        }
        if !added.contains(dtype) {
            order.push(dtype);
        }
        let prev = added.get_or(dtype, 0.0);
        added.set(dtype, prev + amount);
    }
    coalesce(&added, &order, &mut result, el);
    for &(dtype, value) in post {
        match dtype {
            None => {
                for i in 0..result.0.len() {
                    let key = result.0[i].0;
                    result.0[i].1 += scaled_base.get_or(key, 0.0) * value;
                }
            }
            Some(dtype) => {
                let bonus_base = if el.physical(dtype) { scaled_base.get_or(dtype, 0.0) } else { base_total };
                let v = result.get_or(dtype, 0.0) + bonus_base * value;
                result.set(dtype, v);
            }
        }
    }
    result.truthy()
}

/// 按 `damage._coalesce` 的顺序合并元素。
fn coalesce(added: &Dict, order: &[D], result: &mut Dict, el: &Elements) {
    let pending: Vec<D> = order.iter().copied().filter(|&d| added.contains(d)).collect();
    let mut used = vec![false; pending.len()];
    for i in 0..pending.len() {
        let first = pending[i];
        if used[i] || !el.base(first) {
            continue;
        }
        for j in (i + 1)..pending.len() {
            if used[j] {
                continue;
            }
            let second = pending[j];
            if !el.base(second) {
                continue;
            }
            if let Some(compound) = el.combo(first, second) {
                let v = result.get_or(compound, 0.0) + added.get_or(first, 0.0) + added.get_or(second, 0.0);
                result.set(compound, v);
                used[i] = true;
                used[j] = true;
                break;
            }
        }
    }
    for (i, &dtype) in pending.iter().enumerate() {
        if !used[i] {
            let v = result.get_or(dtype, 0.0) + added.get_or(dtype, 0.0);
            result.set(dtype, v);
        }
    }
}

/// 按 `damage._apply_innate_elements` 处理固有元素，空类型用 None。
pub fn apply_innate(damage: Dict, base_total: f64, entries: &[(Option<D>, f64)], el: &Elements) -> Dict {
    let mut result = damage;
    for &(dtype, fraction) in entries {
        let amount = base_total * fraction;
        let Some(dtype) = dtype else { continue };
        if amount == 0.0 {
            continue;
        }
        if el.base(dtype) {
            let partner = el.base.iter().copied().find(|&c| {
                c != dtype && result.get_or(c, 0.0) != 0.0 && el.combo(dtype, c).is_some()
            });
            if let Some(partner) = partner {
                let partner_amount = result.pop(partner).unwrap();
                let compound = el.combo(dtype, partner).unwrap();
                let v = result.get_or(compound, 0.0) + partner_amount + amount;
                result.set(compound, v);
                continue;
            }
        }
        let v = result.get_or(dtype, 0.0) + amount;
        result.set(dtype, v);
    }
    result.truthy()
}

/// 已将伤害类型转换为 ID 的 `damage.CoSpec`。
pub struct CoSpec {
    pub victim_status_count: f64,
    pub damage_scale_effects: Vec<(u8, f64)>,
    pub ordinary_scale: f64,
    pub compression_mult: f64,
    pub gunco_independent: bool,
    pub gunco_fraction: Option<f64>,
    pub victim_damage: f64,
    pub co_fraction: f64,
    pub melee: bool,
    pub melee_damage_effects: Vec<(u8, f64)>,
    pub slam_effects: Vec<(u8, f64)>,
    pub base: Dict,
    pub base_total: f64,
    pub elemental_base_total: f64,
    pub pre: Vec<Element>,
    pub post: Vec<Element>,
    pub innate: Vec<(Option<D>, f64)>,
    pub pool_factor: Option<f64>,
    pub copy_factor: Option<f64>,
    /// 电击加成、特殊护盾、穿过次数与衰减。
    pub orb: Option<(f64, bool, f64, f64)>,
    pub final_damage: Option<(f64, Dict)>,
    pub frozen_factor: Option<f64>,
}

/// 供回放读取的 `_co_damage` 结果。
pub struct CoDamage {
    pub damage_types: Dict,
    /// 结算近战加成后的最终伤害倍率。
    pub scale: f64,
    pub base_bracket: f64,
    pub victim_low_health_multiplier: f64,
    pub slam_damage_ratio: f64,
}

/// `damage._co_damage(spec, co_effects)`。效果组是已按状态种数缩放的（操作，值）。
pub fn co_damage(spec: &CoSpec, co: &[(u8, f64)], el: &Elements) -> CoDamage {
    let status: Vec<(u8, f64)> = co.iter().map(|&(op, v)| (op, v * spec.victim_status_count)).collect();
    let mut damage_scale = combine(1.0, &[&spec.damage_scale_effects, &status], None);
    if let Some(fraction) = spec.gunco_fraction {
        let ordinary_scale = spec.ordinary_scale;
        let raw_co = combine(1.0, &[&status], None) - 1.0;
        damage_scale = if spec.gunco_independent {
            ordinary_scale * (1.0 + raw_co * fraction)
        } else {
            ordinary_scale + raw_co * fraction
        };
    }
    damage_scale += spec.ordinary_scale * (spec.compression_mult - 1.0);
    damage_scale = pmax(0.0, damage_scale);
    let base_bracket = damage_scale;
    let mut victim_low = 1.0;
    let (victim_damage, co_fraction) = (spec.victim_damage, spec.co_fraction);
    if victim_damage != 0.0 {
        if spec.gunco_independent {
            let co_current = combine(1.0, &[&status], None);
            victim_low = 1.0 + victim_damage * co_fraction / pmax(co_current, 1e-9);
        } else {
            victim_low = 1.0 + victim_damage * co_fraction / pmax(damage_scale, 1e-9);
        }
    }
    let mut slam_damage_ratio = 1.0;
    if spec.melee {
        let pre_melee = damage_scale;
        damage_scale = pmax(0.0, combine(damage_scale, &[&spec.melee_damage_effects], Some(damage_scale)));
        if !spec.slam_effects.is_empty() && damage_scale != 0.0 {
            slam_damage_ratio =
                pmax(0.0, combine(pre_melee, &[&spec.melee_damage_effects, &spec.slam_effects], Some(pre_melee))) / damage_scale;
        }
    }
    let mut scaled = spec.base.clone();
    for e in scaled.0.iter_mut() {
        e.1 *= damage_scale;
    }
    let mut damage = apply_elements(&scaled, spec.elemental_base_total * damage_scale, &spec.pre, &spec.post, el);
    if !spec.innate.is_empty() {
        damage = apply_innate(damage, spec.base_total * damage_scale, &spec.innate, el);
    }
    if let Some(f) = spec.pool_factor {
        damage.scale(f);
    }
    if let Some(f) = spec.copy_factor {
        damage.scale(f);
    }
    if let Some((electric_bonus, special, passes, atten)) = spec.orb {
        let ebt = spec.elemental_base_total;
        let v = damage.get_or(el.electricity, 0.0) + ebt * damage_scale * electric_bonus;
        damage.set(el.electricity, v);
        if special {
            let v = damage.get_or(el.radiation, 0.0) + ebt * damage_scale * 2.0 * passes;
            damage.set(el.radiation, v);
        }
        damage.scale(atten);
    }
    if let Some((final_damage, typed)) = &spec.final_damage {
        for e in damage.0.iter_mut() {
            e.1 = e.1 * final_damage * typed.get_or(e.0, 1.0);
        }
    }
    if let Some(f) = spec.frozen_factor {
        damage.scale(f);
    }
    for entry in &mut damage.0 {
        entry.1 = pmax(0.0, entry.1);
    }
    CoDamage { damage_types: damage, scale: damage_scale, base_bracket, victim_low_health_multiplier: victim_low, slam_damage_ratio }
}
