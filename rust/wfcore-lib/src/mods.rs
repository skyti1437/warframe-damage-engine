//! 配装效果分组、属性结算与异况超量计算。

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

use crate::co::{self, Dict, Element, D};
use crate::fx::FxHashMap;
use crate::pymath::{pmax, pmin, pow, psum};

/// 保留参考实现中的整数和浮点数类型。
#[derive(Clone, Copy, Debug)]
pub enum Num {
    I(i64),
    F(f64),
}

impl Default for Num {
    fn default() -> Self {
        Self::I(0)
    }
}

impl Num {
    fn f(self) -> f64 {
        match self {
            Num::I(i) => i as f64,
            Num::F(f) => f,
        }
    }
    fn truthy(self) -> bool {
        match self {
            Num::I(i) => i != 0,
            Num::F(f) => f != 0.0,
        }
    }
    fn add(self, other: Num) -> Num {
        match (self, other) {
            (Num::I(a), Num::I(b)) => Num::I(a + b),
            (a, b) => Num::F(a.f() + b.f()),
        }
    }
    fn sub(self, other: Num) -> Num {
        match (self, other) {
            (Num::I(a), Num::I(b)) => Num::I(a - b),
            (a, b) => Num::F(a.f() - b.f()),
        }
    }
}

/// `sum(values)` 对空序列返回整数零。
fn pysum(values: impl IntoIterator<Item = f64>) -> Num {
    let values: Vec<f64> = values.into_iter().collect();
    if values.is_empty() {
        Num::I(0)
    } else {
        Num::F(psum(values))
    }
}

/// 对应 `quantize_base_crit_damage` 的基础暴伤量化。
fn quantize(cd: f64) -> f64 {
    let scaled = (cd as f32).clamp(0.0, 32.0) * 127.96875;
    ((scaled + 0.5).trunc() * (32.0_f32 / 4095.0_f32)) as f64
}

/// 对（操作，值）组成的效果组应用 `damage._combine`。
fn combine(base: f64, parts: &[&[(u8, f64)]]) -> f64 {
    co::combine(base, parts, None)
}

/// 无效果时，`damage._combine` 保留基础值的数值类型。
fn combine_num(base: Num, parts: &[&[(u8, f64)]]) -> Num {
    if parts.iter().all(|p| p.is_empty()) {
        base
    } else {
        Num::F(co::combine(base.f(), parts, None))
    }
}

struct Names {
    names: Vec<String>,
    index: FxHashMap<String, D>,
}

static NAMES: Mutex<Option<Names>> = Mutex::new(None);

fn with_names<R>(f: impl FnOnce(&mut Names) -> R) -> R {
    let mut guard = NAMES.lock().unwrap();
    let names = guard.get_or_insert_with(|| Names { names: Vec::new(), index: FxHashMap::default() });
    f(names)
}

pub fn intern(name: &str) -> D {
    with_names(|n| {
        if let Some(&d) = n.index.get(name) {
            return d;
        }
        let d = n.names.len() as D;
        n.names.push(name.to_string());
        n.index.insert(name.to_string(), d);
        d
    })
}

pub fn name(d: D) -> String {
    with_names(|n| n.names[d as usize].clone())
}

fn elements() -> co::Elements {
    let id = intern;
    let (fire, cold, electricity, toxin) = (id("DT_FIRE"), id("DT_FREEZE"), id("DT_ELECTRICITY"), id("DT_POISON"));
    co::Elements {
        physical: [id("DT_IMPACT"), id("DT_PUNCTURE"), id("DT_SLASH")],
        base: [fire, cold, electricity, toxin],
        combos: vec![
            (fire, cold, id("DT_EXPLOSION")),
            (fire, electricity, id("DT_RADIATION")),
            (fire, toxin, id("DT_GAS")),
            (cold, electricity, id("DT_MAGNETIC")),
            (cold, toxin, id("DT_VIRAL")),
            (electricity, toxin, id("DT_CORROSIVE")),
        ],
        electricity,
        radiation: id("DT_RADIATION"),
    }
}

const BUCKETS: &[&str] = &[
    "GAMEPLAY_FACTION_DAMAGE", "RUNTIME_IMPACT_PROC_CONVERSION", "RUNTIME_INDEPENDENT_PROC", "WEAPON_AMMO_CONSUME_RATE",
    "WEAPON_AMMO_MAX", "WEAPON_DAMAGE_AMOUNT", "WEAPON_DAMAGE_IF_VICTIM_PROC_ACTIVE", "WEAPON_DAMAGE_WEAKPOINT",
    "WEAPON_DOUBLE_CRIT_CHANCE", "WEAPON_HEADSHOT_MULTIPLIER", "WEAPON_INIT_DAMAGE_MOD",
    "WEAPON_MELEE_COMBO_DURATION_BONUS", "WEAPON_MELEE_COMBO_GAIN_EXTRA_CHANCE", "WEAPON_MELEE_COMBO_GAIN_MISS_CHANCE",
    "WEAPON_MELEE_COMBO_INITIAL_BONUS", "WEAPON_MELEE_COMBO_USAGE_EFFICIENCY", "WEAPON_MELEE_DAMAGE",
    "WEAPON_MELEE_HEAVY_CHARGE_SPEED", "WEAPON_MELEE_SLAM_DAMAGE_BONUS", "WEAPON_NOCRIT_DAMAGE",
    "WEAPON_PERCENT_BASE_DAMAGE_ADDED", "WEAPON_PERCENT_BASE_DAMAGE_ADDED_POST_COALESCE", "WEAPON_PROC_DAMAGE",
    "WEAPON_PROC_TIME", "WEAPON_RECOIL", "WEAPON_SLASH_PROC_ON_CRIT_CHANCE", "WEAPON_SPREAD", "RUNTIME_COLD_STACK_DAMAGE",
    "RUNTIME_EXTRA_HIT", "RUNTIME_FINAL_CRIT_DAMAGE", "RUNTIME_FINAL_DAMAGE", "RUNTIME_FROZEN_DAMAGE",
    "RUNTIME_INCARNON_STAT_CONVERSION", "RUNTIME_LOCK_FIRE_RATE", "RUNTIME_LOCK_MULTISHOT", "RUNTIME_NEXT_SHOT_DAMAGE",
    "RUNTIME_PRIMARY_COMPRESSION", "WEAPON_ADD_CHARGE_COUNTER_MULTIPLIER", "WEAPON_AMMO_POOL_MULTISHOT_DAMAGE",
    "WEAPON_CHARGE_RATE", "WEAPON_CLIP_MAX", "WEAPON_CRIT_CHANCE", "WEAPON_CRIT_CHANCE_BODY_PART",
    "WEAPON_CRIT_CHANCE_WEAKPOINT", "WEAPON_CRIT_DAMAGE", "WEAPON_CRIT_UNDER_PROC_THRESHOLD_BONUS",
    "WEAPON_CRIT_UNDER_PROC_THRESHOLD_MAX", "WEAPON_EXPLOSION_RADIUS", "WEAPON_FIRE_ITERATIONS", "WEAPON_FIRE_RATE",
    "WEAPON_LOW_CRIT_DAMAGE", "WEAPON_LOW_CRIT_VALUE", "WEAPON_NOCRIT_CHANCE", "WEAPON_PROC_CHANCE",
    "WEAPON_PROJECTILE_SPEED", "WEAPON_RELOAD_SPEED", "WEAPON_RELOAD_TIME", "WEAPON_VICTIM_CRIT_CHANCE",
    "WEAPON_VICTIM_CRIT_DAMAGE", "WEAPON_VICTIM_DAMAGE_AMOUNT", "WEAPON_CRIT_CHANCE_IF_VICTIM_PROC_ACTIVE",
    "RUNTIME_TYPED_PROC_TIME", "RUNTIME_TYPED_PROC_MAX_STACK", "WEAPON_REFLECTION_EXPLOSION_CHANCE",
    "RUNTIME_THROW_DAMAGE",
];
const OTHER: usize = BUCKETS.len();

fn bucket_of(name: &str) -> usize {
    BUCKETS.iter().position(|b| *b == name).unwrap_or(OTHER)
}

macro_rules! b {
    ($name:literal) => {{
        const fn eq(a: &str, b: &str) -> bool {
            let (a, b) = (a.as_bytes(), b.as_bytes());
            if a.len() != b.len() {
                return false;
            }
            let mut i = 0;
            while i < a.len() {
                if a[i] != b[i] {
                    return false;
                }
                i += 1;
            }
            true
        }
        const fn find(name: &str) -> usize {
            let mut i = 0;
            while i < BUCKETS.len() {
                if eq(BUCKETS[i], name) {
                    return i;
                }
                i += 1;
            }
            panic!("unknown bucket")
        }
        const I: usize = find($name);
        I
    }};
}

// 与 co 模块共享操作码，并保留按操作名称区分的 SET_BASE。
const OP_ADD_BASE: u8 = co::ADD_BASE;
const OP_ADD: u8 = co::ADD;
const OP_SM: u8 = co::STACKING_MULTIPLY;

pub struct Row {
    bucket: usize,
    op: u8,
    set_base: bool,
    value: f64,
    damage_type: Option<String>,
    dtype: Option<D>,
    symbol: Option<String>,
    source: Option<String>,
    upgrade: String,
    zoom: bool,
    runtime_kind: Option<String>,
    runtime_damage_type: Option<String>,
    runtime_chance: Option<f64>,
    runtime_threshold: Option<f64>,
    runtime_multiplier: f64,
}

pub struct EffectInput {
    pub operation: String,
    pub value: f64,
    pub damage_type: Option<String>,
    pub symbol: Option<String>,
    pub source: Option<String>,
    pub upgrade: String,
    pub runtime_kind: Option<String>,
    pub runtime_damage_type: Option<String>,
    pub runtime_chance: Option<f64>,
    pub runtime_threshold: Option<f64>,
    pub runtime_multiplier: f64,
}

impl From<EffectInput> for Row {
    fn from(input: EffectInput) -> Self {
        Self {
            bucket: bucket_of(&input.upgrade),
            op: co::op_code(&input.operation),
            set_base: input.operation == "SET_BASE",
            dtype: nonempty(&input.damage_type).map(intern),
            zoom: input.source.as_deref().unwrap_or("").starts_with("zoom:"),
            value: input.value,
            damage_type: input.damage_type,
            symbol: input.symbol,
            source: input.source,
            upgrade: input.upgrade,
            runtime_kind: input.runtime_kind,
            runtime_damage_type: input.runtime_damage_type,
            runtime_chance: input.runtime_chance,
            runtime_threshold: input.runtime_threshold,
            runtime_multiplier: input.runtime_multiplier,
        }
    }
}

/// 分类结果及原始效果在输入序列中的位置。
pub struct ClassifiedEffect {
    pub index: usize,
    pub row: Arc<Row>,
    pub status: u8,
    pub message: Option<String>,
    pub unsupported: Option<String>,
}

fn nonempty(s: &Option<String>) -> Option<&str> {
    s.as_deref().filter(|x| !x.is_empty())
}

#[derive(Default)]
pub struct ModsPrep {
    pub melee: bool,
    pub base: Vec<(D, f64)>,
    pub compression: Option<(f64, bool, Option<f64>, f64, bool)>,
    pub aiming: bool,
    pub gunco_present: bool,
    pub gunco_base_fraction: Option<f64>,
    pub gunco_exclude: Vec<String>,
    pub gunco_independent: bool,
    pub direct_source_stack_damage: bool,
    pub co_base_fraction: f64,
    pub innate: Vec<(String, f64)>,
    pub notes: Vec<(String, String)>,
    pub speed_falloff: bool,
    pub falloff: Option<(f64, f64, f64)>,
    pub distance: Option<f64>,
    pub innate_multishot: f64,
    pub native_ignore_fire_iterations: bool,
    pub per_pellet_proc: f64,
    pub incarnon_form: bool,
    pub incarnon_pool: Option<f64>,
    pub incarnon_cycle_seconds: Option<f64>,
    pub incarnon_cycle: bool,
    pub base_magazine: f64,
    pub reload_every_shot: bool,
    pub reload_base: Num,
    pub shell: Option<(f64, f64, f64)>,
    /// 电池武器的空仓延迟、再生、空仓动画和非空仓延迟。
    pub battery: Option<(Num, f64, Option<f64>, f64)>,
    /// 触发 Gotva Prime 效果的概率及触发后设定的暴击率。
    pub super_crit: Option<(f64, f64)>,
    /// 触发 Sagek Prime 效果的概率及触发后设定的触发率。
    pub super_status: Option<(f64, f64)>,
    pub crit_chance: Num,
    pub crit_multiplier: Num,
    pub fire_rate: Num,
    pub charge_scales: bool,
    pub charge_seconds: f64,
    pub bow_draw: Option<f64>,
    pub continuous: bool,
    pub copy: Option<(f64, Option<Num>, Option<f64>, Option<f64>)>,
    pub orb_passes: i64,
    pub orb_stacks: i64,
    pub orb_special: bool,
    pub radial_only: bool,
    pub ignore_headshot_bonus: Option<bool>,
    pub forced: Vec<(String, f64)>,
    pub ammo_cost: f64,
}

/// `damage.py` 的 RELOAD_NEVER_SECONDS 表示装填无法结束。
const RELOAD_NEVER_SECONDS: f64 = 1e9;
/// `damage.py` 的 AMMO_EFFICIENCY_CAP 限制扳机的总弹药效率。
const AMMO_EFFICIENCY_CAP: f64 = 0.75;

struct Applied<'a> {
    rows: Vec<&'a Row>,
    indexes: Vec<usize>,
    buckets: Vec<Vec<usize>>,
}

impl<'a> Applied<'a> {
    fn idx(&self, b: usize) -> &[usize] {
        &self.buckets[b]
    }
    fn ops(&self, b: usize) -> Vec<(u8, f64)> {
        self.buckets[b].iter().map(|&i| (self.rows[i].op, self.rows[i].value)).collect()
    }
    fn ops_where(&self, b: usize, keep: impl Fn(&Row) -> bool) -> Vec<(u8, f64)> {
        self.buckets[b].iter().filter(|&&i| keep(self.rows[i])).map(|&i| (self.rows[i].op, self.rows[i].value)).collect()
    }
    fn values(&self, b: usize) -> Vec<f64> {
        self.buckets[b].iter().map(|&i| self.rows[i].value).collect()
    }
    fn empty(&self, b: usize) -> bool {
        self.buckets[b].is_empty()
    }
}

pub enum Scale {
    Installed(usize),
    Synthetic(&'static str, &'static str, f64, Option<&'static str>),
}

fn element_of(row: &Row) -> Element {
    match nonempty(&row.damage_type) {
        None | Some("DT_ANY") => (None, row.value),
        Some(_) => (row.dtype, row.value),
    }
}

/// 已提取的效果、跳过原因以及目标的异况种数。
pub struct ModsInput {
    pub effects: Vec<ClassifiedEffect>,
    pub skipped: Vec<String>,
    pub victim_status_count: f64,
    pub need_spec: bool,
}

pub struct ModsOutput {
    pub fields: ModdedFields,
    pub spec: Option<ModsSpecOutput>,
}

pub struct ModdedFields {
    pub damage_types: Dict,
    pub crit_chance: Num,
    pub crit_multiplier: f64,
    pub status_chance: f64,
    pub multishot: f64,
    pub fire_rate: Num,
    pub magazine: f64,
    pub reload_time: Num,
    pub spread_multiplier: f64,
    pub recoil_multiplier: f64,
    pub ammo_max_multiplier: f64,
    pub headshot_bonus_multiplier: f64,
    pub proc_damage_multiplier: f64,
    pub proc_duration_multiplier: f64,
    pub proc_duration_types: Vec<(String, f64)>,
    pub proc_max_stacks: Vec<(String, f64)>,
    pub status_condition_damage_multiplier: f64,
    pub initial_combo: f64,
    pub combo_duration: f64,
    pub heavy_attack_efficiency: f64,
    pub combo_extra_chance: Num,
    pub combo_miss_chance: Num,
    pub heavy_charge_speed: f64,
    pub slam_damage_ratio: f64,
    pub faction_damage_multipliers: Vec<(String, f64)>,
    pub noncrit_damage_multiplier: f64,
    pub unsupported: Vec<String>,
    pub skipped: Vec<String>,
    pub falloff_multiplier: f64,
    pub transform_time_scale: Num,
    pub area_direct: bool,
    pub stack_native_fraction: f64,
    pub stackable_hit_event: bool,
    pub dot_base_damage: f64,
    pub dot_elemental_multipliers: Vec<(&'static str, f64)>,
    pub impact_proc_conversions: Vec<(String, Vec<(f64, f64, f64)>)>,
    pub independent_proc_chances: Vec<(String, f64)>,
    pub innate_damage_types: Vec<(String, f64)>,
    pub double_crit_chance: f64,
    pub first_shot_damage_bonus: f64,
    pub weakpoint_damage_bonus: f64,
    pub weakpoint_crit_chance_bonus: Num,
    pub weakpoint_crit_chance_relative: Num,
    pub bodyshot_crit_multiplier: f64,
    pub base_crit_chance: Num,
    pub ammo_cost: f64,
    pub co_bucket: Vec<usize>,
    pub co_assumed_count: f64,
    pub continuous: bool,
    pub radial_only: bool,
    pub ignore_headshot_bonus: Option<bool>,
    pub explosion_radius_multiplier: f64,
    pub native_forced_procs: Vec<(String, f64)>,
    pub incarnon_charge_multiplier: f64,
    pub low_crit_threshold: Option<f64>,
    pub low_crit_damage_base: Option<f64>,
    pub low_crit_damage_bonus: f64,
    pub ammo_pool_multishot_cost: f64,
    pub ammo_pool_damage_bonus: f64,
    pub victim_full_crit_chance: Num,
    pub victim_full_crit_damage: f64,
    pub victim_low_health_multiplier: f64,
    pub projectile_speed_multiplier: f64,
    pub charge_rate_multiplier: f64,
    pub charge_seconds: f64,
    pub bow_draw_seconds: Option<f64>,
    pub hit_probability: f64,
    pub crit_proc_chances: Option<Num>,
    pub extra_hits: Vec<(String, f64, bool)>,
    pub cold_stack_damage: Num,
    pub base_damage_bracket: f64,
    pub frozen_damage_multiplier: f64,
    pub reflection_explosion_chance: f64,
    pub throw_damage_bonus: Num,
}

/// 计算后的异况超量参数，效果位置用于调用方关联原始记录。
pub struct ModsSpecOutput {
    pub scale_effects: Vec<Scale>,
    pub ordinary_scale: f64,
    pub compression_multiplier: f64,
    pub gunco_independent: bool,
    pub gunco_fraction: Option<f64>,
    pub victim_damage: f64,
    pub co_fraction: f64,
    pub melee: bool,
    pub melee_effects: Vec<usize>,
    pub slam_effects: Vec<usize>,
    pub base_scaled: Option<(Dict, f64)>,
    pub elemental_base_total: f64,
    pub pre: Vec<usize>,
    pub post: Vec<usize>,
    pub innate: Vec<(String, f64)>,
    pub pool_factor: Option<f64>,
    pub copy_factor: Option<f64>,
    pub orb: Option<(f64, bool, i64, f64)>,
    pub final_damage: Option<(Num, Vec<(String, f64)>)>,
    pub frozen_factor: Option<f64>,
}

/// 附加瞄准效果按需读取，以保留原有分类顺序。
pub fn apply_mods<E>(
    prep: &ModsPrep,
    input: ModsInput,
    zoom: impl IntoIterator<Item = Result<ClassifiedEffect, E>>,
) -> Result<ModsOutput, E> {
    let ModsInput { effects: mut entries, skipped, victim_status_count: vsc, need_spec } = input;
    let el = elements();
    if !entries.iter().any(|e| e.row.zoom) {
        for effect in zoom {
            entries.push(effect?);
        }
    }
    let mut skipped = skipped;
    let mut unsupported: BTreeSet<String> = BTreeSet::new();
    let mut ap = Applied { rows: Vec::new(), indexes: Vec::new(), buckets: vec![Vec::new(); OTHER + 1] };
    for entry in entries.iter() {
        match entry.status {
            0 => {
                ap.buckets[entry.row.bucket].push(ap.rows.len());
                ap.rows.push(&entry.row);
                ap.indexes.push(entry.index);
            }
            1 => {
                if let Some(u) = &entry.unsupported {
                    unsupported.insert(u.clone());
                }
                skipped.push(entry.message.clone().unwrap_or_default());
            }
            3 => skipped.push(entry.message.clone().unwrap_or_default()),
            _ => {}
        }
    }
    let rows = &ap.rows;

    // 处理运行时异常转换。
    let mut impact: Vec<(String, Vec<(f64, f64, f64)>)> = Vec::new();
    for &i in ap.idx(b!("RUNTIME_IMPACT_PROC_CONVERSION")) {
        let r = rows[i];
        let dtype = nonempty(&r.runtime_damage_type).or(nonempty(&r.damage_type));
        match (dtype, r.runtime_chance) {
            (Some(dtype), Some(chance)) => {
                let entry = (chance, r.runtime_threshold.unwrap_or(f64::INFINITY), r.runtime_multiplier);
                match impact.iter_mut().find(|x| x.0 == dtype) {
                    Some(x) => x.1.push(entry),
                    None => impact.push((dtype.to_string(), vec![entry])),
                }
            }
            _ => {
                skipped.push(format!("{}: unsupported[runtime-parameter]", r.source.as_deref().unwrap_or("?")));
                unsupported.insert(r.upgrade.clone());
            }
        }
    }
    let mut independent: Vec<(String, f64)> = Vec::new();
    for &i in ap.idx(b!("RUNTIME_INDEPENDENT_PROC")) {
        let r = rows[i];
        let dtype = nonempty(&r.runtime_damage_type).or(nonempty(&r.damage_type));
        match (dtype, r.runtime_chance) {
            (Some(dtype), Some(chance)) => match independent.iter_mut().find(|x| x.0 == dtype) {
                Some(x) => x.1 += chance,
                None => independent.push((dtype.to_string(), 0.0 + chance)),
            },
            _ => {
                skipped.push(format!("{}: unsupported[runtime-parameter]", r.source.as_deref().unwrap_or("?")));
                unsupported.insert(r.upgrade.clone());
            }
        }
    }
    let mut groups: Vec<(String, Vec<(u8, f64)>)> = Vec::new();
    for &i in ap.idx(b!("GAMEPLAY_FACTION_DAMAGE")) {
        let r = rows[i];
        let key = r.symbol.clone().unwrap_or_default();
        match groups.iter_mut().find(|g| g.0 == key) {
            Some(g) => g.1.push((r.op, r.value)),
            None => groups.push((key, vec![(r.op, r.value)])),
        }
    }
    let all_factions = match groups.iter().position(|g| g.0.is_empty()) {
        Some(p) => {
            let g = groups.remove(p);
            combine(1.0, &[&g.1])
        }
        None => 1.0,
    };
    let mut faction: Vec<(String, f64)> = groups.iter().map(|g| (g.0.clone(), combine(1.0, &[&g.1]))).collect();
    if all_factions != 1.0 {
        faction.push((String::new(), all_factions));
    }

    let co_idx: Vec<usize> = ap.idx(b!("WEAPON_DAMAGE_IF_VICTIM_PROC_ACTIVE")).to_vec();
    let mut damage_flat_base = 0.0;
    let mut scale_effects: Vec<Scale> = Vec::new();
    let mut melee_idx: Vec<usize> = ap.idx(b!("WEAPON_MELEE_DAMAGE")).to_vec();
    if prep.melee {
        damage_flat_base += pysum(melee_idx.iter().filter(|&&i| rows[i].op == OP_ADD_BASE).map(|&i| rows[i].value)).f();
        melee_idx.retain(|&i| rows[i].op != OP_ADD_BASE);
    }
    for &i in ap.idx(b!("WEAPON_DAMAGE_AMOUNT")) {
        if rows[i].op == OP_ADD_BASE {
            damage_flat_base += rows[i].value;
        } else {
            scale_effects.push(Scale::Installed(i));
        }
    }
    let cold_rate = pysum(ap.values(b!("RUNTIME_COLD_STACK_DAMAGE")));
    let cold_forced = ap.idx(b!("RUNTIME_COLD_STACK_DAMAGE")).iter()
        .map(|&i| rows[i].runtime_chance.unwrap_or(0.0)).fold(None, |acc: Option<f64>, x| Some(match acc { None => x, Some(a) => pmax(a, x) }))
        .unwrap_or(0.0);
    if cold_rate.truthy() && cold_forced != 0.0 && !prep.direct_source_stack_damage {
        scale_effects.push(Scale::Synthetic("WEAPON_DAMAGE_AMOUNT", "STACKING_MULTIPLY",
                                            cold_rate.f() * pmin(cold_forced, 10.0), Some("runtime:cold-stack-damage")));
    }
    let mut compression_mult = 1.0 + pysum(ap.values(b!("RUNTIME_NEXT_SHOT_DAMAGE"))).f();
    if prep.direct_source_stack_damage && cold_rate.truthy() && cold_forced != 0.0 {
        compression_mult += cold_rate.f() * pmin(cold_forced, 10.0);
    }
    let mut compression_ammo = 0.0;
    if !ap.empty(b!("RUNTIME_PRIMARY_COMPRESSION")) && prep.aiming {
        match prep.compression {
            Some((radius0, takes_mods, reads, effectiveness, adds)) => {
                let mut radius = radius0;
                if takes_mods {
                    radius = combine(radius, &[&ap.ops(b!("WEAPON_EXPLOSION_RADIUS"))]);
                }
                let considered = reads.unwrap_or(radius * effectiveness);
                let lost = pmax(considered, 0.0) * 0.8;
                let bonus = pysum(ap.values(b!("RUNTIME_PRIMARY_COMPRESSION"))).f() * lost;
                compression_ammo = pysum(ap.idx(b!("RUNTIME_PRIMARY_COMPRESSION")).iter()
                    .map(|&i| rows[i].runtime_chance.filter(|c| *c != 0.0).unwrap_or(0.0))).f() * lost;
                if adds {
                    scale_effects.push(Scale::Synthetic("WEAPON_DAMAGE_AMOUNT", "ADD", bonus, None));
                } else {
                    compression_mult += bonus;
                }
            }
            None => {
                unsupported.insert("primary-compression-weapon-profile-unverified".to_string());
            }
        }
    }
    let scale_ops: Vec<(u8, f64)> = scale_effects.iter().map(|s| match s {
        Scale::Installed(i) => (rows[*i].op, rows[*i].value),
        Scale::Synthetic(_, op, v, _) => (co::op_code(op), *v),
    }).collect();
    let ordinary_scale = combine(1.0, &[&scale_ops]);
    // CoSpec 保留此处的值，随后弹池和复制倍率各自进入异况超量结算。
    let spec_compression_mult = compression_mult;
    let raw_base_total = psum(prep.base.iter().map(|e| e.1));
    let mut gunco_fraction = None;
    if prep.gunco_present && !co_idx.is_empty() {
        let mut fraction = prep.gunco_base_fraction.unwrap_or(0.0);
        let excluded = pysum(ap.idx(b!("WEAPON_DAMAGE_AMOUNT")).iter()
            .filter(|&&i| rows[i].op == OP_ADD_BASE && rows[i].source.as_ref().map_or(false, |s| prep.gunco_exclude.contains(s)))
            .map(|&i| rows[i].value)).f();
        let total = raw_base_total + damage_flat_base;
        fraction *= if total != 0.0 { pmax(total - excluded, 0.0) / total } else { 0.0 };
        gunco_fraction = Some(fraction);
    }
    let victim_damage = combine(1.0, &[&ap.ops(b!("WEAPON_VICTIM_DAMAGE_AMOUNT"))]) - 1.0;
    let co_fraction = prep.co_base_fraction * raw_base_total / pmax(raw_base_total + damage_flat_base, 1e-9);
    let mut base: Vec<(D, f64)> = prep.base.clone();
    let mut base_scaled = false;
    if damage_flat_base != 0.0 && raw_base_total > 0.0 {
        let scale_up = 1.0 + damage_flat_base / raw_base_total;
        for e in base.iter_mut() {
            e.1 *= scale_up;
        }
        base_scaled = true;
    }
    let base_total = if base_scaled { psum(base.iter().map(|e| e.1)) } else { raw_base_total };
    let innate: Vec<(Option<D>, f64)> = prep.innate.iter().map(|(d, v)| (Some(intern(d)), *v)).collect();
    let elemental_base_total = base_total * (1.0 + pysum(prep.innate.iter().map(|e| e.1)).f());
    for (code, line) in prep.notes.iter() {
        unsupported.insert(code.clone());
        skipped.push(line.clone());
    }
    let pre_idx: Vec<usize> = ap.idx(b!("WEAPON_PERCENT_BASE_DAMAGE_ADDED")).to_vec();
    let post_idx: Vec<usize> = ap.idx(b!("WEAPON_PERCENT_BASE_DAMAGE_ADDED_POST_COALESCE")).to_vec();
    let speed = pmax(0.0, combine(1.0, &[&ap.ops(b!("WEAPON_PROJECTILE_SPEED"))]));
    let falloff_multiplier = match (prep.distance, prep.falloff) {
        (Some(distance), Some((start0, end0, minimum))) if base_total > 0.0 => {
            let (start, end) = if speed != 1.0 && prep.speed_falloff { (start0 * speed, end0 * speed) } else { (start0, end0) };
            if !start.is_finite() || !end.is_finite() || end <= start {
                1.0
            } else {
                let final_ = minimum / base_total;
                if distance <= start {
                    1.0
                } else if distance >= end {
                    final_
                } else {
                    let progress = (distance - start) / (end - start);
                    1.0 + (final_ - 1.0) * progress
                }
            }
        }
        _ => 1.0,
    };
    let pellets = pmax(prep.innate_multishot, 1.0);
    let proc_ops = ap.ops(b!("WEAPON_PROC_CHANCE"));
    let pellet_status = |chance: f64| combine(chance * pellets, &[&proc_ops]) / pellets;
    let mut magazine = combine(prep.base_magazine, &[&ap.ops(b!("WEAPON_CLIP_MAX"))]);
    if prep.base_magazine > 0.0 && !prep.incarnon_form {
        // 四舍五入到整发，例如 Tonkor 的 1 × 1.55。
        magazine = pmax(1.0, (((magazine as f32) as f64) + 0.5).floor());
    }
    if prep.reload_every_shot {
        magazine = 1.0;
    }
    let reload_base = match prep.shell {
        Some((start, per, end)) => Num::F(start + per * magazine + end),
        None => prep.reload_base,
    };
    let speed_ops = ap.ops(b!("WEAPON_RELOAD_SPEED"));
    // 装填速度减幅达到 100% 时使用 RELOAD_NEVER_SECONDS，避免负时长让回放时钟倒退。
    let divide = |base: Num| -> Num {
        if !base.truthy() || speed_ops.is_empty() {
            return base;
        }
        let scale = combine(1.0, &[&speed_ops]);
        if scale > 0.0 { Num::F(base.f() / scale) } else { Num::F(RELOAD_NEVER_SECONDS) }
    };
    let mut reload_time = divide(combine_num(reload_base, &[&ap.ops(b!("WEAPON_RELOAD_TIME"))]));
    if let Some((delay, regen, animation, partial)) = prep.battery {
        reload_time = Num::F(match animation {
            Some(animation) => pmax(divide(Num::F(animation)).f(), partial + magazine / regen),
            None => divide(delay).f() + magazine / regen,
        });
    }
    if prep.incarnon_form {
        reload_time = Num::F(0.0);
        if let Some(pool) = prep.incarnon_pool {
            magazine = pool;
            if let Some(seconds) = prep.incarnon_cycle_seconds {
                reload_time = divide(Num::F(seconds));
            }
        } else if prep.incarnon_cycle {
            unsupported.insert("incarnon-cycle-data-unavailable".to_string());
        }
    }
    let mut per_pellet_proc = prep.per_pellet_proc;
    let mut critchance = prep.crit_chance;
    let crit_ops = ap.ops(b!("WEAPON_CRIT_CHANCE"));
    for &i in ap.idx(b!("RUNTIME_INCARNON_STAT_CONVERSION")) {
        let r = rows[i];
        match r.runtime_kind.as_deref() {
            Some("WEAPON_PROC_CHANCE") => {
                let source = pellet_status(per_pellet_proc);
                let bonus = pmin(r.runtime_threshold.unwrap_or(f64::NAN), pmax(0.0, source) * r.value);
                critchance = Num::F(if critchance.truthy() { critchance.f() } else { 0.0 } + bonus);
            }
            Some("WEAPON_CRIT_CHANCE") => {
                let source = combine_num(critchance, &[&crit_ops]).f();
                let bonus = pmin(r.runtime_threshold.unwrap_or(f64::NAN), pmax(0.0, source) * r.value);
                per_pellet_proc += bonus;
            }
            _ => {}
        }
    }
    let mut crit_chance = combine_num(critchance, &[&crit_ops]);
    let per_status_crit = pysum(ap.values(b!("WEAPON_CRIT_CHANCE_IF_VICTIM_PROC_ACTIVE")));
    if per_status_crit.truthy() {
        // Galariak Prime 按目标状态种数结算最终暴击率。
        crit_chance = Num::F(crit_chance.f() + per_status_crit.f() * vsc);
    }
    let fire_rate = if !ap.empty(b!("RUNTIME_LOCK_FIRE_RATE")) {
        prep.fire_rate
    } else {
        combine_num(prep.fire_rate, &[&ap.ops(b!("WEAPON_FIRE_RATE"))])
    };
    let charge_rate = pmax(1e-9, combine(1.0, &[&ap.ops(b!("WEAPON_CHARGE_RATE"))]));
    let charge_fire_scale = if prep.charge_scales { pmax(fire_rate.f() / pmax(prep.fire_rate.f(), 1e-9), 1e-9) } else { 1.0 };
    let charge_seconds = prep.charge_seconds / (charge_fire_scale * charge_rate);
    let bow_draw = prep.bow_draw.map(|d| d / (charge_fire_scale * charge_rate));
    let flat: Vec<f64> = ap.idx(b!("WEAPON_CRIT_DAMAGE")).iter().filter(|&&i| rows[i].op == OP_ADD || rows[i].op == OP_ADD_BASE)
        .map(|&i| rows[i].value).collect();
    let relative: Vec<(u8, f64)> = ap.ops_where(b!("WEAPON_CRIT_DAMAGE"), |r| !(r.op == OP_ADD || r.op == OP_ADD_BASE));
    let crit_base = prep.crit_multiplier.add(pysum(flat.iter().copied())).f();
    let mut crit_multiplier = combine(quantize(crit_base), &[&relative]);
    let anti = combine(0.0, &[&ap.ops(b!("WEAPON_CRIT_UNDER_PROC_THRESHOLD_MAX"))]);
    if anti > 0.0 && vsc < anti {
        crit_multiplier += combine(0.0, &[&ap.ops(b!("WEAPON_CRIT_UNDER_PROC_THRESHOLD_BONUS"))]);
    }
    let low_threshold = if ap.empty(b!("WEAPON_LOW_CRIT_VALUE")) { None } else { Some(combine(0.0, &[&ap.ops(b!("WEAPON_LOW_CRIT_VALUE"))])) };
    let mut low_bonus = if low_threshold.is_some() {
        combine(quantize(combine(crit_base, &[&ap.ops(b!("WEAPON_LOW_CRIT_DAMAGE"))])), &[&relative])
            - combine(quantize(crit_base), &[&relative])
    } else {
        0.0
    };
    let mut multishot = if prep.native_ignore_fire_iterations || !ap.empty(b!("RUNTIME_LOCK_MULTISHOT")) {
        prep.innate_multishot
    } else {
        pmax(0.0, combine(prep.innate_multishot, &[&ap.ops(b!("WEAPON_FIRE_ITERATIONS"))]))
    };
    let continuous = prep.continuous;
    let mut pool_cost = 0.0;
    let pool_ops = ap.ops(b!("WEAPON_AMMO_POOL_MULTISHOT_DAMAGE"));
    let mut pool_multiplier = 1.0;
    let mut pool_factor = None;
    if !pool_ops.is_empty() {
        let source_base = psum(base.iter().map(|e| e.1));
        pool_multiplier = if source_base != 0.0 { combine(source_base, &[&pool_ops]) / source_base } else { 1.0 };
    }
    if !pool_ops.is_empty() && multishot > 1.0 {
        pool_cost = multishot - 1.0;
        let amplified = 1.0 + (multishot - 1.0) * pool_multiplier;
        if continuous {
            multishot = amplified;
        } else {
            let factor = amplified / multishot;
            pool_factor = Some(factor);
            compression_mult *= factor;
        }
    }
    let mut copy_factor = None;
    if let Some((share, copy_cc, copy_cm, copy_proc)) = prep.copy {
        if multishot > 1.0 && !continuous {
            let extra = multishot - 1.0;
            let total = 1.0 + extra * share;
            let factor = total / multishot;
            copy_factor = Some(factor);
            compression_mult *= factor;
            let copy_chance = combine(copy_cc.unwrap_or(critchance).f(), &[&crit_ops]);
            let copy_multiplier = combine(quantize(copy_cm.unwrap_or(prep.crit_multiplier.f()) + pysum(flat.iter().copied()).f()), &[&relative]);
            let cc = crit_chance.f();
            let blended = (cc + extra * share * copy_chance) / total;
            if blended > 0.0 {
                crit_multiplier = 1.0 + (cc * (crit_multiplier - 1.0) + extra * share * copy_chance * (copy_multiplier - 1.0)) / (total * blended);
            }
            crit_chance = Num::F(blended);
            if let Some(p) = copy_proc {
                per_pellet_proc = (per_pellet_proc + extra * p) / multishot;
            }
        }
    }
    // 近战伤害 MOD 会进入异常伤害基数。
    let mut dot_scale = pmax(0.0, combine(1.0, &[&scale_ops]));
    if prep.melee {
        let melee_ops: Vec<(u8, f64)> = melee_idx.iter().map(|&i| (rows[i].op, rows[i].value)).collect();
        dot_scale = pmax(0.0, co::combine(dot_scale, &[&melee_ops], Some(dot_scale)));
    }
    let mut dot_basis = elemental_base_total * dot_scale * compression_mult;
    // 结算持续伤害的元素倍率。
    let typed = |name: &str| -> Vec<(u8, f64)> {
        pre_idx.iter().chain(post_idx.iter()).filter(|&&i| nonempty(&rows[i].damage_type) == Some(name))
            .map(|&i| (rows[i].op, rows[i].value)).collect()
    };
    let mut dot_elements: Vec<(&str, f64)> = vec![
        ("DT_SLASH", 1.0),
        ("DT_FIRE", pmax(combine(1.0, &[&typed("DT_FIRE")]), 0.0)),
        ("DT_POISON", pmax(combine(1.0, &[&typed("DT_POISON")]), 0.0)),
        ("DT_ELECTRICITY", pmax(combine(1.0, &[&typed("DT_ELECTRICITY")]), 0.0)),
        ("DT_GAS", pmax(combine(1.0, &[&typed("DT_GAS")]), 0.0)),
    ];
    let weapon_crit_chance = crit_chance;
    let mut shield_crit = 1.0;
    let mut orb = None;
    if prep.orb_passes != 0 && !prep.radial_only && !prep.melee {
        let stacks = prep.orb_stacks;
        let electric = if stacks == 1 { 1.0 } else { 1.0 + 4.0 * stacks as f64 / 3.0 };
        let special = prep.orb_special;
        let shields = prep.orb_passes * if special { 2 } else { 1 };
        let atten = pow(0.66, shields as f64);
        let electric_bonus = prep.orb_passes as f64 * electric;
        orb = Some((electric_bonus, special, prep.orb_passes, atten));
        dot_basis *= atten;
        dot_elements[3].1 += electric_bonus;
        crit_chance = Num::F(crit_chance.f() + 0.25 * shields as f64);
        crit_multiplier *= 1.25;
        low_bonus *= 1.25;
        shield_crit = 1.25;
    }
    if let Some((chance, set_to)) = prep.super_crit {
        // Gotva Prime 的触发事件为下一次射击设定暴击率。
        let armed = chance * pmin(1.0, pmax(0.0, pellet_status(per_pellet_proc)));
        crit_chance = Num::F(crit_chance.f() + armed * (set_to - crit_chance.f()));
    }
    let mut status_chance = pellet_status(per_pellet_proc);
    if let Some((chance, set_to)) = prep.super_status {
        // Sagek Prime 的暴击为下一次射击设定异常触发率。
        let armed = chance * pmin(1.0, pmax(0.0, crit_chance.f()));
        status_chance = status_chance + armed * (set_to - status_chance);
    }
    let mut final_damage = Num::I(1);
    let mut typed_final: Vec<(D, String, f64)> = Vec::new();
    for &i in ap.idx(b!("RUNTIME_FINAL_DAMAGE")) {
        let r = rows[i];
        match nonempty(&r.damage_type) {
            None => final_damage = Num::F(final_damage.f() * (1.0 + r.value)),
            Some(name) => {
                let d = r.dtype.unwrap();
                match typed_final.iter_mut().find(|x| x.0 == d) {
                    Some(x) => x.2 *= 1.0 + r.value,
                    None => typed_final.push((d, name.to_string(), 1.0 * (1.0 + r.value))),
                }
            }
        }
    }
    let final_spec = if final_damage.f() != 1.0 || !typed_final.is_empty() {
        dot_basis *= final_damage.f();
        Some((final_damage, Dict(typed_final.iter().map(|x| (x.0, x.2)).collect())))
    } else {
        None
    };
    crit_multiplier = crit_multiplier + pysum(ap.values(b!("RUNTIME_FINAL_CRIT_DAMAGE"))).f();
    let frozen_bonus = if prep.melee { pysum(ap.values(b!("RUNTIME_FROZEN_DAMAGE"))) } else { Num::F(0.0) };
    let frozen_idx = ap.idx(b!("RUNTIME_FROZEN_DAMAGE"));
    let frozen_forced = frozen_bonus.truthy() && frozen_idx.iter().any(|&i| rows[i].runtime_chance.unwrap_or(0.0) > 0.0);
    let mut frozen_factor = None;
    if frozen_forced {
        let share = frozen_idx.iter().map(|&i| rows[i].runtime_chance.unwrap_or(0.0))
            .fold(None, |acc: Option<f64>, x| Some(match acc { None => x, Some(a) => pmax(a, x) })).unwrap();
        frozen_factor = Some(1.0 + frozen_bonus.f() * pmin(share, 1.0));
    }
    let mut forced: Vec<(String, f64)> = prep.forced.clone();
    let mut extra_hits: Vec<(String, f64, bool)> = Vec::new();
    for &i in ap.idx(b!("RUNTIME_EXTRA_HIT")) {
        let r = rows[i];
        if let Some(d) = nonempty(&r.damage_type) {
            if r.value > 0.0 {
                extra_hits.push((d.to_string(), r.value, r.runtime_chance.map_or(false, |c| c != 0.0)));
            }
        }
    }
    for &i in ap.idx(b!("RUNTIME_EXTRA_HIT")) {
        let r = rows[i];
        if let (Some(d), Some(c)) = (nonempty(&r.damage_type), r.runtime_chance) {
            if r.value > 0.0 && c != 0.0 {
                match forced.iter_mut().find(|x| x.0 == d) {
                    Some(x) => x.1 += c,
                    None => forced.push((d.to_string(), 0.0 + c)),
                }
            }
        }
    }
    // 结算异况超量。
    let co_ops: Vec<(u8, f64)> = co_idx.iter().map(|&i| (rows[i].op, rows[i].value)).collect();
    let melee_ops: Vec<(u8, f64)> = melee_idx.iter().map(|&i| (rows[i].op, rows[i].value)).collect();
    let slam_idx: Vec<usize> = ap.idx(b!("WEAPON_MELEE_SLAM_DAMAGE_BONUS")).to_vec();
    let spec = co::CoSpec {
        victim_status_count: vsc,
        damage_scale_effects: scale_ops.clone(),
        ordinary_scale,
        compression_mult: spec_compression_mult,
        gunco_independent: prep.gunco_independent,
        gunco_fraction,
        victim_damage,
        co_fraction,
        melee: prep.melee,
        melee_damage_effects: melee_ops,
        slam_effects: slam_idx.iter().map(|&i| (rows[i].op, rows[i].value)).collect(),
        base: Dict(base.clone()),
        base_total,
        elemental_base_total,
        pre: pre_idx.iter().map(|&i| element_of(rows[i])).collect(),
        post: post_idx.iter().map(|&i| element_of(rows[i])).collect(),
        innate: innate.clone(),
        pool_factor,
        copy_factor,
        orb: orb.map(|(e, s, p, a)| (e, s, p as f64, a)),
        final_damage: final_spec.as_ref().map(|(f, d)| (f.f(), d.clone())),
        frozen_factor,
    };
    let codamage = co::co_damage(&spec, &co_ops, &el);
    let status_ops: Vec<(u8, f64)> = co_ops.iter().map(|&(op, v)| (op, v * vsc)).collect();
    let status_multiplier = combine(1.0, &[&status_ops]);

    // ── 按 ModdedStats 的字段顺序结算 ──
    let crit_eff = crit_multiplier
        + match low_threshold {
            Some(t) if crit_chance.f() < t => low_bonus,
            _ => 0.0,
        };
    let damage_types = codamage.damage_types;
    let effective_crit_multiplier = crit_eff;
    let spread_multiplier = combine(1.0, &[&ap.ops(b!("WEAPON_SPREAD"))]);
    let recoil_multiplier = combine(1.0, &[&ap.ops(b!("WEAPON_RECOIL"))]);
    let ammo_max_multiplier = combine(1.0, &[&ap.ops(b!("WEAPON_AMMO_MAX"))]);
    let headshot_bonus_multiplier = combine(1.0, &[&ap.ops(b!("WEAPON_HEADSHOT_MULTIPLIER"))]);
    let proc_damage_multiplier = combine(1.0, &[&ap.ops(b!("WEAPON_PROC_DAMAGE"))]);
    let proc_duration_multiplier = combine(1.0, &[&ap.ops(b!("WEAPON_PROC_TIME"))]);
    // `damage._typed_durations` 先应用通用时长效果，再应用特定异常的时长效果。
    let shared = ap.ops(b!("WEAPON_PROC_TIME"));
    let mut typed: Vec<(String, Vec<(u8, f64)>)> = Vec::new();
    for &i in ap.idx(b!("RUNTIME_TYPED_PROC_TIME")) {
        if let Some(d) = rows[i].damage_type.clone().filter(|d| !d.is_empty()) {
            match typed.iter_mut().find(|x| x.0 == d) {
                Some(x) => x.1.push((rows[i].op, rows[i].value)),
                None => typed.push((d, vec![(rows[i].op, rows[i].value)])),
            }
        }
    }
    let durations = typed.into_iter().map(|(d, own)| (d, combine(1.0, &[&shared, &own]))).collect();
    // `damage._typed_stack_caps` 按顺序累加各异常额外层数。
    let mut caps: Vec<(String, f64)> = Vec::new();
    for &i in ap.idx(b!("RUNTIME_TYPED_PROC_MAX_STACK")) {
        if let Some(d) = rows[i].damage_type.clone().filter(|d| !d.is_empty()) {
            match caps.iter_mut().find(|x| x.0 == d) {
                Some(x) => x.1 += rows[i].value,
                None => caps.push((d, 0.0 + rows[i].value)),
            }
        }
    }
    let proc_duration_types = durations;
    let proc_max_stacks = caps;
    let status_condition_damage_multiplier = status_multiplier;
    let initial_combo = combine(0.0, &[&ap.ops(b!("WEAPON_MELEE_COMBO_INITIAL_BONUS"))]);
    let combo_duration = combine(if prep.melee { 5.0 } else { 0.0 }, &[&ap.ops(b!("WEAPON_MELEE_COMBO_DURATION_BONUS"))]);
    let heavy_attack_efficiency = combine(0.0, &[&ap.ops(b!("WEAPON_MELEE_COMBO_USAGE_EFFICIENCY"))]);
    let combo_extra_chance = pysum(ap.values(b!("WEAPON_MELEE_COMBO_GAIN_EXTRA_CHANCE")));
    let combo_miss_chance = pysum(ap.values(b!("WEAPON_MELEE_COMBO_GAIN_MISS_CHANCE")));
    let heavy_charge_speed = pmax(0.05, 1.0 + pysum(ap.values(b!("WEAPON_MELEE_HEAVY_CHARGE_SPEED"))).f());
    let slam_damage_ratio = codamage.slam_damage_ratio;
    let faction_damage_multipliers = faction;
    let nocrit_chance = if ap.empty(b!("WEAPON_NOCRIT_CHANCE")) {
        1.0
    } else {
        pmin(1.0, pmax(0.0, combine(0.0, &[&ap.ops(b!("WEAPON_NOCRIT_CHANCE"))])))
    };
    let noncrit_damage_multiplier = 1.0 + pmax(0.0, combine(1.0, &[&ap.ops(b!("WEAPON_NOCRIT_DAMAGE"))]) - 1.0) * nocrit_chance;
    let unsupported = unsupported.into_iter().collect();
    let mut seen = std::collections::HashSet::new();
    let unique = skipped.iter().filter(|s| seen.insert(s.as_str())).cloned().collect();
    let skipped = unique;
    let transform_time_scale = divide(Num::F(1.0));
    let area_direct = false;
    let stack_native_fraction = 1.0;
    let stackable_hit_event = false;
    let dot_base_damage = pmax(0.0, dot_basis);
    let dot_elemental_multipliers = dot_elements;
    let impact_proc_conversions = impact;
    let independent_proc_chances = independent;
    let innate_types = prep.innate.iter().map(|(dtype, value)| (dtype.clone(), base_total * codamage.scale * value)).collect();
    let innate_damage_types = innate_types;
    let double_crit_chance = pmax(combine(1.0, &[&ap.ops(b!("WEAPON_DOUBLE_CRIT_CHANCE"))]) - 1.0, 0.0);
    let first_shot_damage_bonus = pmax(combine(1.0, &[&ap.ops(b!("WEAPON_INIT_DAMAGE_MOD"))]) - 1.0, 0.0);
    let weakpoint_damage_bonus = pmax(combine(1.0, &[&ap.ops(b!("WEAPON_DAMAGE_WEAKPOINT"))]) - 1.0, 0.0);
    let wcc = b!("WEAPON_CRIT_CHANCE_WEAKPOINT");
    let body = b!("WEAPON_CRIT_CHANCE_BODY_PART");
    let weakpoint_crit_chance_bonus = pysum(ap.idx(wcc).iter().filter(|&&i| rows[i].op == OP_ADD).map(|&i| rows[i].value));
    let relative_wp = pysum(ap.idx(wcc).iter().filter(|&&i| rows[i].op == OP_SM).map(|&i| rows[i].value))
        .add(pysum(ap.idx(body).iter().filter(|&&i| rows[i].symbol.as_deref() == Some("HEAD") && rows[i].op == OP_SM)
            .map(|&i| rows[i].value)));
    let weakpoint_crit_chance_relative = relative_wp;
    let bodyshot_crit_multiplier = combine(1.0, &[&ap.ops_where(body, |r| r.symbol.as_deref() == Some("TORSO"))]);
    let base_crit_chance = combine_num(critchance, &[&ap.ops_where(b!("WEAPON_CRIT_CHANCE"), |r| r.op == OP_ADD_BASE || r.set_base)]);
    // 弹药效率上限为 75%，包括主要压缩在内的效果至少保留四分之一的基础消耗。
    let ammo_ops = ap.ops(b!("WEAPON_AMMO_CONSUME_RATE"));
    let ammo_cost = pmax(0.0, combine(prep.ammo_cost, &[&ammo_ops])) * pmax(0.0, 1.0 - compression_ammo);
    // 无限弹药的 SET 0 直接设定消耗，不受效率下限约束。
    let ammo_set = ammo_ops.iter().any(|&(op, _)| op == co::SET);
    let ammo_cost = if ammo_set { ammo_cost } else { pmax(prep.ammo_cost * (1.0 - AMMO_EFFICIENCY_CAP), ammo_cost) };
    let co_bucket = co_idx.into_iter().map(|i| ap.indexes[i]).collect();
    let co_assumed_count = vsc;
    let radial_only = prep.radial_only;
    let ignore_headshot_bonus = prep.ignore_headshot_bonus;
    let explosion_radius_multiplier = combine(1.0, &[&ap.ops(b!("WEAPON_EXPLOSION_RADIUS"))]);
    let native_forced_procs = forced;
    let incarnon_charge_multiplier = pmax(0.0, combine(1.0, &[&ap.ops(b!("WEAPON_ADD_CHARGE_COUNTER_MULTIPLIER"))]));
    let low_crit_threshold = low_threshold;
    let low_crit_damage_base = low_threshold.map(|_| crit_multiplier);
    let low_crit_damage_bonus = low_bonus;
    let ammo_pool_multishot_cost = pool_cost;
    let ammo_pool_damage_bonus = if pool_ops.is_empty() { 0.0 } else { pool_multiplier - 1.0 };
    let mut victim_crit: Vec<(u8, f64)> = crit_ops.clone();
    victim_crit.extend(ap.ops(b!("WEAPON_VICTIM_CRIT_CHANCE")));
    let victim_full_crit_chance = combine_num(critchance, &[&victim_crit]).sub(weapon_crit_chance);
    let victim_cd = pysum(ap.values(b!("WEAPON_VICTIM_CRIT_DAMAGE")));
    let victim_full_crit_damage = (combine(quantize(crit_base + victim_cd.f()), &[&relative]) - combine(quantize(crit_base), &[&relative])) * shield_crit;
    let victim_low_health_multiplier = codamage.victim_low_health_multiplier;
    let projectile_speed_multiplier = speed;
    let charge_rate_multiplier = charge_rate;
    let bow_draw_seconds = bow_draw;
    let hit_probability = 1.0;
    let crit_procs = (!ap.empty(b!("WEAPON_SLASH_PROC_ON_CRIT_CHANCE")))
        .then(|| pysum(ap.values(b!("WEAPON_SLASH_PROC_ON_CRIT_CHANCE"))));
    let crit_proc_chances = crit_procs;
    let cold_stack_damage = if cold_forced != 0.0 { Num::F(0.0) } else { cold_rate };
    let base_damage_bracket = codamage.base_bracket;
    let frozen_damage_multiplier = if frozen_forced { 1.0 } else { 1.0 + frozen_bonus.f() };
    let reflection_explosion_chance = pmin(1.0, pmax(0.0, combine(0.0, &[&ap.ops(b!("WEAPON_REFLECTION_EXPLOSION_CHANCE"))])));
    let throw_damage_bonus = pysum(ap.values(b!("RUNTIME_THROW_DAMAGE")));

    let spec = need_spec.then(|| ModsSpecOutput {
        scale_effects: scale_effects.into_iter().map(|effect| match effect {
            Scale::Installed(i) => Scale::Installed(ap.indexes[i]),
            other => other,
        }).collect(),
        ordinary_scale,
        compression_multiplier: spec_compression_mult,
        gunco_independent: prep.gunco_independent,
        gunco_fraction,
        victim_damage,
        co_fraction,
        melee: prep.melee,
        melee_effects: melee_idx.iter().map(|&i| ap.indexes[i]).collect(),
        slam_effects: slam_idx.iter().map(|&i| ap.indexes[i]).collect(),
        base_scaled: base_scaled.then_some((Dict(base), base_total)),
        elemental_base_total,
        pre: pre_idx.iter().map(|&i| ap.indexes[i]).collect(),
        post: post_idx.iter().map(|&i| ap.indexes[i]).collect(),
        innate: prep.innate.clone(),
        pool_factor,
        copy_factor,
        orb,
        final_damage: final_spec.map(|(factor, _)| (factor, typed_final.into_iter().map(|(_, name, v)| (name, v)).collect())),
        frozen_factor,
    });
    Ok(ModsOutput {
        fields: ModdedFields {
            damage_types,
            crit_chance,
            crit_multiplier: effective_crit_multiplier,
            status_chance,
            multishot,
            fire_rate,
            magazine,
            reload_time,
            spread_multiplier,
            recoil_multiplier,
            ammo_max_multiplier,
            headshot_bonus_multiplier,
            proc_damage_multiplier,
            proc_duration_multiplier,
            proc_duration_types,
            proc_max_stacks,
            status_condition_damage_multiplier,
            initial_combo,
            combo_duration,
            heavy_attack_efficiency,
            combo_extra_chance,
            combo_miss_chance,
            heavy_charge_speed,
            slam_damage_ratio,
            faction_damage_multipliers,
            noncrit_damage_multiplier,
            unsupported,
            skipped,
            falloff_multiplier,
            transform_time_scale,
            area_direct,
            stack_native_fraction,
            stackable_hit_event,
            dot_base_damage,
            dot_elemental_multipliers,
            impact_proc_conversions,
            independent_proc_chances,
            innate_damage_types,
            double_crit_chance,
            first_shot_damage_bonus,
            weakpoint_damage_bonus,
            weakpoint_crit_chance_bonus,
            weakpoint_crit_chance_relative,
            bodyshot_crit_multiplier,
            base_crit_chance,
            ammo_cost,
            co_bucket,
            co_assumed_count,
            continuous,
            radial_only,
            ignore_headshot_bonus,
            explosion_radius_multiplier,
            native_forced_procs,
            incarnon_charge_multiplier,
            low_crit_threshold,
            low_crit_damage_base,
            low_crit_damage_bonus,
            ammo_pool_multishot_cost,
            ammo_pool_damage_bonus,
            victim_full_crit_chance,
            victim_full_crit_damage,
            victim_low_health_multiplier,
            projectile_speed_multiplier,
            charge_rate_multiplier,
            charge_seconds,
            bow_draw_seconds,
            hit_probability,
            crit_proc_chances,
            extra_hits,
            cold_stack_damage,
            base_damage_bracket,
            frozen_damage_multiplier,
            reflection_explosion_chance,
            throw_damage_bonus,
        },
        spec,
    })
}
