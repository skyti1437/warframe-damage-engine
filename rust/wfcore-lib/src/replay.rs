//! 事件回放、属性视图、伤害结算与目标状态的推进。

use crate::co;
use crate::counters::{self, Timer};
use crate::fx::{FxHashMap, FxIndexMap};
use crate::heat::HeatChains;
use crate::proc::{stack_alive, CappedPool, DotInstance, KillAttribution, StatusPresence, ProcPile, PileOutcome, PileKernel, arrival_kernel};
use crate::pymath::{ceil_i, floor_i, pmax, pmin, pow, psum, trunc_i, PySum};
use crate::tables::{self, Core, TargetTables};
use crate::stackable::{self, Projectiles};
use std::rc::Rc;
use std::cell::OnceCell;
use std::cmp::Ordering;
use std::collections::{BinaryHeap, VecDeque};

const REPLAY_EVENT_LIMIT: u64 = 10_000_000;

fn unsupported(reason: &str) -> ReplayError {
    ReplayError::Unsupported(reason.to_owned())
}

fn invalid_value(reason: &str) -> ReplayError {
    ReplayError::Invalid(InvalidReplay::Value(reason.to_owned()))
}

fn invalid_index(reason: &'static str) -> ReplayError {
    ReplayError::Invalid(InvalidReplay::Index(reason))
}

fn invalid_zero_division(reason: &'static str) -> ReplayError {
    ReplayError::Invalid(InvalidReplay::ZeroDivision(reason))
}

fn form_name(form: u8) -> &'static str {
    match form { BASE => "base", INCARNON => "incarnon", _ => "selected" }
}

/// 调用方持有的对象标识，在一次回放及其批处理分支中保持有效。
pub type ObjectId = usize;

#[derive(Debug, PartialEq, Eq)]
pub enum ReplayError {
    Unsupported(String),
    Runaway(String),
    Invalid(InvalidReplay),
}

#[derive(Debug, PartialEq, Eq)]
pub enum InvalidReplay {
    Callback,
    Value(String),
    Index(&'static str),
    ZeroDivision(&'static str),
}

impl std::fmt::Display for ReplayError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unsupported(message) | Self::Runaway(message) => f.write_str(message),
            Self::Invalid(InvalidReplay::Callback) => f.write_str("回放回调失败"),
            Self::Invalid(InvalidReplay::Value(message)) => f.write_str(message),
            Self::Invalid(InvalidReplay::Index(message) | InvalidReplay::ZeroDivision(message)) => f.write_str(message),
        }
    }
}

impl std::error::Error for ReplayError {}

pub type ReplayResult<T> = Result<T, ReplayError>;

#[derive(Clone, Copy)]
pub enum StackKey {
    Object(ObjectId),
    Named(&'static str),
}

#[derive(Clone, Copy)]
pub enum StackValue {
    Float(f64),
    Index(usize),
    Flag(bool),
}

pub type SourceStacks = Vec<(StackKey, StackValue)>;

/// 已结算的来源叠层和当前射击状态，供调用方提供属性视图。
pub struct ResolveInput {
    pub form: &'static str,
    pub stacks: SourceStacks,
    pub live_types: Option<f64>,
    pub last_round: bool,
}

/// 同一回放分支的类型注册表与异况超量配置缓存。
pub struct ViewContext<'a> {
    pub types: &'a mut Types,
    pub target_faction: &'a str,
    pub faction_bracket: f64,
    co_specs: &'a mut FxHashMap<ObjectId, Rc<co::CoSpec>>,
}

impl ViewContext<'_> {
    pub fn co_spec<E>(
        &mut self,
        object: ObjectId,
        extract: impl FnOnce(&mut Types) -> Result<co::CoSpec, E>,
    ) -> Result<Rc<co::CoSpec>, E> {
        if let Some(spec) = self.co_specs.get(&object) {
            return Ok(spec.clone());
        }
        let spec = Rc::new(extract(self.types)?);
        self.co_specs.insert(object, spec.clone());
        Ok(spec)
    }
}

/// 回放按需读取外部属性。实现者只需为启用的玩法提供对应回调。
pub trait ReplayProvider {
    fn resolve(&self, input: ResolveInput) -> ReplayResult<(ObjectId, Option<ObjectId>)>;
    fn stats(&self, object: ObjectId, context: &mut ViewContext<'_>) -> ReplayResult<Stats>;
    fn stats_links(&self, _object: ObjectId) -> ReplayResult<StatsLinks> {
        Ok(StatsLinks::default())
    }
    fn hit_spec(&self, _id: i64, _types: &mut Types) -> ReplayResult<HitSpec> {
        Err(missing_provider("hit_spec"))
    }
    fn stackable_profile(&self, _profile: ObjectId) -> ReplayResult<stackable::Profile> {
        Err(missing_provider("stackable_profile"))
    }
    fn refresh_hook(&self, _object: ObjectId, _required: bool) -> ReplayResult<Option<ObjectId>> {
        Ok(None)
    }
    fn has_refresh(&self, object: ObjectId) -> ReplayResult<bool> {
        Ok(self.refresh_hook(object, false)?.is_some())
    }
    fn refresh(&self, _hook: ObjectId, _stacks: SourceStacks, _live: f64, _initial: Option<bool>) -> ReplayResult<ObjectId> {
        Err(missing_provider("refresh"))
    }
    fn zymos_carrier(&self, _object: ObjectId, _stacks: SourceStacks, _weights: Vec<(i64, f64)>) -> ReplayResult<ObjectId> {
        Err(missing_provider("zymos_carrier"))
    }
    fn zymos_children(&self, _object: ObjectId, _snapshot: ObjectId, _stacks: SourceStacks, _weight: f64,
                      _weights: Vec<(i64, f64)>) -> ReplayResult<Vec<(ObjectId, ObjectId, f64)>> {
        Err(missing_provider("zymos_children"))
    }
    fn check_signals(&self) -> ReplayResult<()> {
        Ok(())
    }
}

fn missing_provider(method: &str) -> ReplayError {
    ReplayError::Unsupported(format!("缺少回放回调 {method}"))
}

/// 属性视图引用的子部件及其事件参数。
#[derive(Default)]
pub struct StatsLinks {
    pub live_co_unit: Option<ObjectId>,
    pub additional: Vec<ObjectId>,
    pub reload_empty: Vec<ObjectId>,
    pub reload_partial: Vec<ObjectId>,
    pub reload_lands_at: f64,
    pub stackable: Option<(ObjectId, Vec<(String, ObjectId)>)>,
    pub finite_fields: Vec<(ObjectId, f64, f64, usize)>,
    pub source_fields: Vec<(ObjectId, f64, f64, usize)>,
    pub nox_projectile: Option<(ObjectId, ObjectId)>,
    pub owned_embed: Option<(ObjectId, f64, f64)>,
    pub lingering: Option<(ObjectId, f64, f64)>,
    pub embedded: Option<(ObjectId, f64, f64)>,
}

pub type D = u32;

#[derive(Clone)]
pub struct Types {
    names: Vec<String>,
    index: FxHashMap<String, D>,
}
impl Default for Types {
    fn default() -> Self {
        Self::new()
    }
}

impl Types {
    pub fn new() -> Self {
        let mut t = Types { names: Vec::new(), index: FxHashMap::default() };
        for name in [
            "DT_FIRE", "DT_FREEZE", "DT_GAS", "DT_EXPLOSION", "DT_VIRAL", "DT_CORROSIVE", "DT_MAGNETIC",
            "DT_PUNCTURE", "DT_ELECTRICITY", "DT_SLASH", "DT_CINEMATIC", "DT_VOID", "DT_RADIANT",
            "DT_IMPACT", "DT_POISON", "DT_SENTIENT", "DT_RADIATION",
        ] {
            t.id(name);
        }
        t
    }
    pub fn id(&mut self, name: &str) -> D {
        if let Some(&i) = self.index.get(name) {
            return i;
        }
        let i = self.names.len() as D;
        self.names.push(name.to_string());
        self.index.insert(name.to_string(), i);
        i
    }
    pub fn get(&self, name: &str) -> Option<D> {
        self.index.get(name).copied()
    }

    pub fn name(&self, d: D) -> &str {
        &self.names[d as usize]
    }
}
const FIRE: D = 0;
const FREEZE: D = 1;
const GAS: D = 2;
const EXPLOSION: D = 3;
const VIRAL: D = 4;
const CORROSIVE: D = 5;
const MAGNETIC: D = 6;
const PUNCTURE: D = 7;
const ELECTRICITY: D = 8;
const SLASH: D = 9;
const CINEMATIC: D = 10;
const VOID: D = 11;
const RADIANT: D = 12;
const TAU: D = 15;
const RADIATION: D = 16;

fn duration_of(d: D) -> Option<f64> {
    match d {
        VIRAL => Some(6.0),
        CORROSIVE => Some(8.0),
        MAGNETIC => Some(6.0),
        FREEZE => Some(6.0),
        PUNCTURE => Some(10.0),
        TAU => Some(8.0),
        RADIATION => Some(12.0),
        _ => None,
    }
}
fn cap_of(d: D) -> Option<f64> {
    match d {
        VIRAL | CORROSIVE | MAGNETIC | FREEZE | GAS => Some(10.0),
        PUNCTURE => Some(5.0),
        _ => None,
    }
}
fn pooled_index(d: D) -> Option<usize> {
    match d {
        VIRAL => Some(0),
        CORROSIVE => Some(1),
        MAGNETIC => Some(2),
        PUNCTURE => Some(3),
        _ => None,
    }
}

pub type Vector = Vec<(D, f64)>;
/// 位模式键依次包含属性视图、瞄准、穿刺、冰冷、寒颤、冻结和次要侵损的权重 ID。
type PacketKey = (usize, u64, u64, u64, u64, u64, u64);

// ── 属性视图 ──

/// 调用方提供的属性值，子部件引用由回放初始化时解析。
#[derive(Clone, Default)]
pub struct Stats {
    pub obj: ObjectId,
    pub core: Core,
    pub magazine: f64,
    pub ammo_cost: f64,
    pub fire_rate: f64,
    pub reload_time: f64,
    pub transform_time_scale: f64,
    pub stack_native_fraction: f64,
    pub stackable_hit_event: bool,
    pub delayed_radial_hit_event: bool,
    pub multishot: f64,
    pub continuous: bool,
    pub radial_only: bool,
    pub radial_victim_crit: bool,
    pub crit_chance: f64,
    pub crit_multiplier: f64,
    pub bodyshot_crit_multiplier: f64,
    pub weakpoint_crit_chance_bonus: f64,
    pub base_crit_chance: f64,
    pub weakpoint_crit_chance_relative: f64,
    pub status_chance: f64,
    pub proc_duration_multiplier: f64,
    pub proc_damage_multiplier: f64,
    pub damage_types: Vector,
    pub native_forced_procs: Vector,
    pub independent_proc_chances: Vector,
    pub has_crit_procs: bool,
    pub first_shot_damage_bonus: f64,
    pub cold_stack_damage: f64,
    pub base_damage_bracket: f64,
    pub frozen_damage_multiplier: f64,
    pub requires_full_ammo: bool,
    pub quanta: Option<(f64, f64, f64)>, // 主射弹药、动作时长、循环时长。
    pub zymos: Option<(f64, f64)>, // 寄生延迟和下一帧的载体消耗时间。
    pub snapshot_enervate: bool,
    pub no_enemy_contact: bool,
    pub contact_shiver: bool,
    pub contact_shiver_scale: f64,
    pub contact_shiver_fixed: Option<f64>,
    pub bow_draw: Option<f64>,
    pub charge_time: f64,
    pub charge_before_fire: bool,
    pub full_clip_charge: bool,
    pub native_fire_cooldown: f64,
    pub charge_volley: bool,
    pub initial_combo: f64,
    pub heavy_charge_speed: f64,
    // 投掷战刃的飞行速度、挥发弹跳概率与奋力一掷加成。
    pub projectile_speed_multiplier: f64,
    pub reflection_explosion_chance: f64,
    pub throw_damage_bonus: f64,
    pub combo_duration: f64,
    pub combo_extra_chance: f64,
    pub combo_miss_chance: f64,
    pub heavy_attack_efficiency: f64,
    pub additional: Vec<usize>,
    pub head_only: bool,
    pub body_only: bool,
    pub impact_delay: f64,
    pub persist_after_kill: bool,
    pub lingering: Option<(usize, f64, f64)>,
    pub owned_embed: Option<(usize,f64,f64)>,
    pub nox_projectile: Option<(usize,usize)>,
    pub live_co_unit: Option<usize>,
    pub source_fields: Vec<(usize,f64,f64,usize)>,
    pub finite_fields: Vec<(usize,f64,f64,usize)>,
    pub embedded: Option<(usize, f64, f64)>,
    pub primary_delay: f64,
    pub fixed_radial: bool,
    pub victim_full_crit_chance: f64,
    pub victim_full_crit_damage: f64,
    pub victim_low_health_multiplier: f64,
    pub ammo_pool_multishot_cost: f64,
    pub ammo_pool_damage_bonus: f64,
    pub charge_pool_cost: f64,
    /// 异况超量效果组由（操作，值）组成，非空时该视图随活跃状态种数变化。
    pub co_bucket: Vec<(u8, f64)>,
    /// `_co_spec` 用于派生非整数活跃状态数的属性。
    pub co_spec: Option<Rc<co::CoSpec>>,
    pub slam_damage_ratio: f64,
    pub incarnon_charge_multiplier: f64,
    /// `_stackable` 包含弹组部署规则与伤害部件。
    pub stackable: Option<(Rc<stackable::Profile>, stackable::Parts)>,
    /// 装填投射物（`_reload_projectiles`）：空仓部件、非空仓部件与落点时间比例。
    pub reload_empty: Vec<usize>,
    pub reload_partial: Vec<usize>,
    pub reload_lands_at: f64,
}

impl Stats {
    /// 按 `dataclasses.replace` 的规则，仅保留已声明字段。
    fn drop_private(&mut self) {
        self.bow_draw = None;
        self.charge_time = 0.0;
        self.charge_volley = false;
        self.full_clip_charge = false;
        self.primary_delay = 0.0;
        self.lingering = None;
        self.owned_embed=None;
        self.nox_projectile=None;
        self.source_fields.clear();
        self.finite_fields.clear();
        self.embedded = None;
        self.charge_pool_cost = 1.0;
        self.requires_full_ammo = false;
        self.quanta = None;
        self.zymos = None;
        self.snapshot_enervate = false;
        self.no_enemy_contact=false;
        self.contact_shiver = false;
        self.contact_shiver_scale=1.0;
        self.contact_shiver_fixed=None;
        self.head_only = false;
        self.body_only = false;
        self.impact_delay = 0.0;
        self.persist_after_kill = false;
        self.fixed_radial = false;
        self.additional = Vec::new();
        self.co_spec = None;
        self.stackable = None;
        self.reload_empty = Vec::new();
        self.reload_partial = Vec::new();
        self.reload_lands_at = 0.0;
    }

}

/// WFSim M61 中，击碎护盾时传到生命值的溢出伤害比例。
const SHIELD_GATE_LEAK: f64 = 0.05;

// ── 事件 ──

#[derive(Clone)]
enum Kind {
    Fire,
    ChargedFire,
    ReloadComplete(bool),
    Heat(i64),
    Blast(usize),
    BlastAll,
    IndependentDot(usize),
    GroupedDot(D, u64),
    Dot(D, f64, f64, u8),
    Actor(String),
    Energy(usize),
    Syndicate,
    ReloadShell(f64, f64),
    Transform(u8),
    DelayedRadial(usize, f64),
    NoxPayload(usize,f64),
    QuantaPrimary(f64),
    ZymosCarrier(usize, f64),
    ZymosSpawn(usize, ObjectId, f64),
    ZymosHit(usize, usize),
    DelayedAttack(usize, f64),
    Lingering(usize),
    OwnedEmbed(u64,usize),
    SourceField(u64,usize,usize,bool),
    FiniteField(usize,usize),
    EmbeddedTick(usize),
    EmbeddedDeath(usize),
    MeleeRecovery(u64, f64),
    MeleeBuffBoundary,
    /// 遥控爆雷批次，参数为属性视图、目标代次和爆炸部件。
    Detonation(Rc<Vec<(usize, i64, bool)>>),
    /// 弹组生命周期参数为分组、令牌和动作。
    StackLife(usize, i64, u8),
    /// 弹组子弹体参数为分组、倍率和命中数。
    StackChild(usize, f64, f64),
}

#[derive(Clone)]
struct Event {
    t: f64,
    seq: u64,
    kind: Kind,
    gen: Option<i64>,
}

/// 同一持续伤害实例的第 k 次剩余伤害位于 `t + (first + k)`，序号为 `base + 1 + k`。
#[derive(Clone)]
struct TickChain {
    t: f64,
    first: i64,
    count: usize,
    base: u64,
    next: usize,
    gen: Option<i64>,
}
impl PartialEq for Event {
    fn eq(&self, o: &Self) -> bool {
        self.t == o.t && self.seq == o.seq
    }
}
impl Eq for Event {}
impl PartialOrd for Event {
    fn partial_cmp(&self, o: &Self) -> Option<Ordering> {
        Some(self.cmp(o))
    }
}
impl Ord for Event {
    fn cmp(&self, o: &Self) -> Ordering {
        o.t.total_cmp(&self.t).then(o.seq.cmp(&self.seq))
    }
}

// ── 触发来源 ──

/// `combat_replay._settle` 判定空仓时免费弹药效果所属的分支。
fn settle(q: &mut Source, on: bool) {
    if on {
        let mut end = q.expires;
        let mut first = true;
        for x in q.expiries.iter() {
            if first || x.0 > end {
                end = x.0;
                first = false;
            }
        }
        q.stacks = q.max_stacks;
        if q.decay == "per-stack" || q.decay == "union" {
            q.expiries = vec![(end, q.max_stacks)];
        }
    } else {
        q.stacks = 0.0;
        q.expiries.clear();
    }
}

/// `combat_replay._union` 计算独立判定中至少一项维持单层效果的概率。
fn union_of(expiries: &[(f64, f64)]) -> f64 {
    let mut idle = 1.0;
    for x in expiries {
        idle *= 1.0 - x.1;
    }
    1.0 - idle
}

#[derive(Clone)]
struct Source {
    id: ObjectId,
    kind: Option<String>,
    stacks: f64,
    expires: f64,
    max_stacks: f64,
    duration: f64,
    per_event: f64,
    decay: String,
    no_refresh: bool,
    free_ammo: bool,
    // 近战被动参数包括尖幡按连击档位计算的持续时间、多尔克拉夫的攻击次数和弧电振子锤震地消耗的层数。
    duration_per_tier: f64,
    charges: f64,
    charges_left: f64,
    consume_on: Option<String>,
    lock_on: Option<String>,
    locked: bool,
    deferred: f64,
    initial_stacks: f64,
    hold: bool,
    reset_on: Vec<String>,
    toggle_on: Option<String>,
    victim_proc_type: Option<D>,
    required_count: f64,
    required_time: Option<f64>,
    actor_only: bool,
    minimum_consecutive: i64,
    energy_threshold: f64,
    expiries: Vec<(f64, f64)>,
    progress: VecDeque<(f64, f64)>,
    run_distribution: Option<FxIndexMap<i64, f64>>,
}

/// 触发来源的初始值和规则。
#[derive(Default)]
pub struct SourceInput {
    pub id: ObjectId,
    pub kind: Option<String>,
    pub stacks: f64,
    pub expires: f64,
    pub max_stacks: f64,
    pub duration: f64,
    pub per_event: f64,
    pub decay: String,
    pub no_refresh: bool,
    pub free_ammo: bool,
    pub duration_per_tier: f64,
    pub charges: f64,
    pub consume_on: Option<String>,
    pub lock_on: Option<String>,
    pub initial_stacks: f64,
    pub hold: bool,
    pub reset_on: Vec<String>,
    pub toggle_on: Option<String>,
    pub victim_proc_type: Option<D>,
    pub required_count: f64,
    pub required_time: Option<f64>,
    pub actor_only: bool,
    pub minimum_consecutive: i64,
    pub energy_threshold: f64,
}

impl From<SourceInput> for Source {
    fn from(input: SourceInput) -> Self {
        Self {
            id: input.id,
            kind: input.kind,
            stacks: input.stacks,
            expires: input.expires,
            max_stacks: input.max_stacks,
            duration: input.duration,
            per_event: input.per_event,
            decay: input.decay,
            no_refresh: input.no_refresh,
            free_ammo: input.free_ammo,
            duration_per_tier: input.duration_per_tier,
            charges: input.charges,
            charges_left: Default::default(),
            consume_on: input.consume_on,
            lock_on: input.lock_on,
            locked: Default::default(),
            deferred: Default::default(),
            initial_stacks: input.initial_stacks,
            hold: input.hold,
            reset_on: input.reset_on,
            toggle_on: input.toggle_on,
            victim_proc_type: input.victim_proc_type,
            required_count: input.required_count,
            required_time: input.required_time,
            actor_only: input.actor_only,
            minimum_consecutive: input.minimum_consecutive,
            energy_threshold: input.energy_threshold,
            expiries: Default::default(),
            progress: Default::default(),
            run_distribution: Default::default(),
        }
    }
}

struct Packet {
    vector: Vector,
    procs: Vector,
    rows: FxIndexMap<D, Row>,
    non: FxIndexMap<D, f64>,
}
struct Row {
    duration: f64,
    seed: f64,
    accumulator: f64,
    shared: bool,
    coefficient: f64,
    branches: Vec<tables::LandingBranch>,
    kernels: OnceCell<((u64, u64), Rc<[Rc<PileKernel>]>)>,
}

impl Clone for Row {
    fn clone(&self) -> Self {
        // 派生伤害包可改变种子、累积项或分支权重。复制语义字段，重新构建概率核缓存。
        Self {
            duration: self.duration, seed: self.seed, accumulator: self.accumulator,
            shared: self.shared, coefficient: self.coefficient,
            branches: self.branches.clone(), kernels: OnceCell::new(),
        }
    }
}

#[derive(Clone)]
struct Blast {
    count: f64,
    per: f64,
    end: f64,
    hit_probability: f64,
    origin_mode: u8,
    proc_source: usize,
}

#[derive(Clone)]
struct EmbeddedEntity {
    part: usize,
    blast: usize,
    expires: f64,
    weight: f64,
    generation: i64,
    alive: bool,
}

#[derive(Clone)]
struct Replay<'a> {
    provider: &'a dyn ReplayProvider,
    score_only: bool,
    types: Types,
    // 配置
    window: f64,
    head: f64,
    rate: f64,
    status_rate_multiplier: f64,
    damage_rate_multiplier: f64,
    full: [f64; 3],
    ehp: f64,
    armour_base: f64,
    elemental_armour_strip: f64,
    forced_procs: Vector,
    faction_deltas: Vec<(D, f64)>,
    armour_bypass: Vec<D>,
    shield_bypass: Vec<D>,
    extra_hits: Vec<(D, f64)>,
    extra_scale: f64,
    weapon_slot: String,
    distance: Option<f64>,
    punchthrough: f64,
    live_types_enabled: bool,
    beam_floor: Option<f64>,
    burst: Option<(bool, i64, f64, bool)>,
    burst_rate: Option<f64>,
    infinite_ammo: bool,
    victim_before: Vec<(D, f64)>,
    syndicate: Option<(String, D, f64)>,
    target_faction: String,
    faction_bracket: f64,
    tables: TargetTables,
    battery: Option<(f64, f64, f64)>,
    spool: Option<SpoolSpec>,
    rate_spool: Option<RateSpoolSpec>,
    shell: Option<(f64, f64, f64)>,
    shot_combo: Option<ShotCombo>,
    reload_after_every_shot: bool,
    refill_growth: bool,
    restore: Option<Restore>,
    pending_restore: f64,
    recharges: i64,
    delayed_attacks: i64,
    reload_shell_events: Vec<(f64, f64, &'static str, u8)>,
    derived: FxHashMap<(usize, u8, u64), usize>,
    selected_incarnon: bool,
    projectile_sequence_length: usize,
    projectile_sequence_index: usize,
    source_field_token:u64,
    source_field_packets:Vec<u64>,
    source_field_activations:u64,
    finite_field_packets:Vec<u64>,
    finite_field_activations:u64,
    physical_lane_length: usize,
    physical_lane_phase: usize,
    physical_lane_selected: Option<usize>,
    lane_stratum: f64,
    volley_rounds: f64,
    volley_shot_resume: Option<f64>,
    melee: Option<MeleeSpec>,
    melee_count: f64,
    melee_heavy_multiplier: f64,
    melee_last_hit: f64,
    melee_combo_at: f64,
    melee_recovery_token: u64,
    melee_recovery_initial: f64,
    melee_buff_boundary: f64,
    melee_pending: Option<MeleeHit>,
    // `melee_sequence.Loop.index` 标记下次挥击的起始段。
    melee_index: usize,
    heavy_flag: bool,
    // 战刃爆炸使用 `__noCO` 解析禁用异况超量时的属性。
    no_co_flag: bool,
    // 正在结算重击事件，用于尖幡的击杀触发。
    melee_heavy_now: bool,
    affinity_per_kill: f64,
    affinity: f64,
    syndicate_ready: f64,
    syndicate_events: Vec<(f64, f64)>,
    // 状态
    pools: [f64; 3],
    states: FxIndexMap<D, VecDeque<(f64, f64)>>,
    pooled: [CappedPool; 4],
    tau: Option<CappedPool>,
    presence: FxIndexMap<D, StatusPresence>,
    proc_piles: FxIndexMap<D, ProcPile>,
    dots: Vec<DotInstance>,
    dot_modes: Vec<u8>,
    dot_sources: Vec<usize>,
    proc_source: Option<usize>,
    heat_proc_source: Option<usize>,
    heat_mode_sources: [Option<usize>; 2],
    damage_origin: Option<u8>,
    blasts: Vec<Blast>,
    blast_procs: VecDeque<usize>,
    heat_acc: f64,
    heat_token: i64,
    heat_chains: HeatChains,
    heat_sources: [HeatChains; 2],
    heat_source_acc: [f64; 2],
    attribution: KillAttribution,
    track_weak_kills: bool,
    puncture_strip: f64,
    frozen_until: f64,
    frozen_scale: f64,
    heap: BinaryHeap<Event>,
    seq: u64,
    gen: i64,
    kills: i64,
    weak_kill_events: f64,
    /// 弱点击杀抽样配置含探测标记、偏移、黄金步长和击杀追踪标记。探测仅为使用击杀状态的分支追踪结果，保留空仓抽样和抽样顺序。
    weak_draw: Option<(bool, f64, f64, bool)>,
    weak_index: f64,
    weak_uncertainty: f64,
    attacks: i64,
    stale: i64,
    first_kill: Option<f64>,
    last_kill: Option<f64>,
    min_interval: Option<f64>,
    totals: [f64; 5],
    /// 次要坚固赋能的倍率、窃取量、上限、生效占比及其累计值。
    fortifier: Option<(f64, f64, f64, f64)>,
    fortifier_damage: f64,
    fortifier_stolen: f64,
    self_overguard: f64,
    /// 次要强化赋能的单次异常伤害、生效占比、状态池及累计值。
    empowered: Option<(f64, f64)>,
    status_pool: Vec<D>,
    empowered_count: f64,
    empowered_by_type: FxIndexMap<D, f64>,
    /// 次要低温赋能的每次穿刺所施加的冰冷层数与生效占比。
    cryogenic: Option<(f64, f64)>,
    cryogenic_expected: f64,
    /// Latron 灵化形态每颗带穿刺异常的弹丸削减的护甲。
    puncture_armor_strip: f64,
    /// 次要妨害赋能的概率、生效占比、上次判定射击和累计值。
    encumber: Option<(f64, f64)>,
    encumber_time: f64,
    encumber_remaining: f64,
    encumber_expected: f64,
    enervate: Option<Enervate>,
    /// Onos 的对齐射击次数、射击后的等待和蓄力时长，以及命中是否叠加暴击。
    /// 可选的首个目标生命曲线，供 `body_ttk_health` 使用。
    life_curve: Option<Vec<(f64, f64, f64, usize)>>,
    onos: Option<(f64, f64, f64)>,
    onos_crits: bool,
    onos_ticks: i64,
    onos_hits: f64,
    onos_start: f64,
    onos_condition_hits: f64,
    projectiles: Option<Projectiles>,
    projectile_state: stackable::State,
    stack_profiles: FxHashMap<usize, Rc<stackable::Profile>>,
    /// 手动引爆（Talons 类地雷）的延迟、弹体上限和批量大小。
    mines: Option<(f64, f64, f64)>,
    impact_embed: bool,
    embedded_timing: (f64,f64,bool),
    embedded_entities: Vec<EmbeddedEntity>,
    pending_mines: Vec<(usize, i64, bool)>,
    pending_throws: i64,
    waiting_mines: f64,
    detonations: i64,
    mine_overflows: i64,
    form: u8,
    cycle: Option<CycleSpec>,
    gauge: f64,
    base_ammo_saved: Option<f64>,
    transforms: i64,
    shells_owed: f64,
    shell_mode_switch: bool,
    burst_base: Option<BurstSpec>,
    burst_rate_base: Option<f64>,
    burst_incarnon: Option<BurstSpec>,
    burst_rate_incarnon: Option<f64>,
    beam_floor_base: Option<f64>,
    last_burst: bool,
    tap_other: (f64, f64),
    /// 双重连击的每次命中增益、上限、持续时间、命中数和过期时间。
    tap: Option<(f64, f64, f64)>,
    tap_hits: f64,
    tap_expires: f64,
    /// 空仓装填对伤害场时长的倍率、就绪标记和已增益射击数。
    field_duration: Option<f64>,
    field_duration_ready: bool,
    field_duration_used: i64,
    /// 各层暴击视图及其索引化的权重列表。
    enervate_views: FxHashMap<(usize, u64, i64, u64, u64, u64, u64), Rc<Packet>>,
    enervate_weights: FxHashMap<Vec<(i64, u64)>, u64>,
    proc_counts: FxIndexMap<D, f64>,
    dot_by_type: FxIndexMap<D, f64>,
    magnetic_breaks: Vec<(f64, usize, f64, f64, i64)>,
    magnetic_proxy: Option<(f32, f32)>,
    magnetic_weakness: Option<f32>,
    magnetic_electric_immune: bool,
    blocks_radiation: bool,
    sources: Vec<Source>,
    ammo: f64,
    shot_in_progress: bool,
    reloads: i64,
    last_round: bool,
    current_time: f64,
    beam_progress: f64,
    beam_last: Option<f64>,
    actor_signal: bool,
    actor_signal_used: bool,
    armour_view: Option<(u64, u64, f64)>,
    // 缓存
    stats: Vec<Stats>,
    views: FxHashMap<ObjectId, usize>,
    co_specs: FxHashMap<ObjectId, Rc<co::CoSpec>>,
    model_cache: FxHashMap<(Vec<u64>, u64, bool), (usize, Option<usize>)>,
    /// `model()` 查表命中时复用键的缓冲区。
    model_key: Vec<u64>,
    /// 按实例记录尚未入堆的持续伤害跳点，见 `add_procs_data`。
    tick_chains: FxHashMap<usize, TickChain>,
    varied: FxHashMap<(usize, u64), Option<usize>>,
    elements: co::Elements,
    hit_specs: Vec<Option<Rc<HitSpec>>>,
    applied: FxHashMap<(usize, i64, u64), usize>,
    packets: FxHashMap<PacketKey, Rc<Packet>>,
    spreads: FxHashMap<(usize, u64), f64>,
    no_status: FxHashMap<(usize, D, u64), f64>,
    proc_names: Vec<Option<Rc<str>>>,
}

const DIRECT: usize = 0;
const DOT: usize = 1;
const NONDOT: usize = 2;
const SYNDICATE: usize = 3;
const ARCANE: usize = 4;
/// `combat_replay` 的形态为所选模式或灵化循环的两个形态。
const SELECTED: u8 = 0;
const BASE: u8 = 1;
const INCARNON: u8 = 2;

#[derive(Clone, Copy)]
pub struct CycleSpec {
    pub hits: f64,
    pub enter: f64,
    pub exit: f64,
    pub pool: f64,
    /// 0 为爆头（默认），1 为直接命中，2 为击杀。
    pub charge_on: u8,
    pub kill_charged: bool,
    pub charge_before_fire: bool,
    pub damage_scale: f64,
    pub source_mode_charge: bool,
}

pub type BurstSpec = (bool, i64, f64, bool);

impl Replay<'_> {
    fn vary(&mut self, st: usize, live: f64, ceil: f64) -> Option<usize> {
        let key = (st, live.to_bits());
        if let Some(&hit) = self.varied.get(&key) {
            return hit;
        }
        let result = self.vary_uncached(st, live, ceil);
        self.varied.insert(key, result);
        result
    }

    fn vary_uncached(&mut self, st: usize, live: f64, ceil: f64) -> Option<usize> {
        let mut out = st;
        if !self.stats[st].co_bucket.is_empty() {
            let spec = self.stats[st].co_spec.clone()?;
            let scaled: Vec<(u8, f64)> = self.stats[st].co_bucket.iter().map(|&(op, v)| (op, v * live / ceil)).collect();
            let damage = co::co_damage(&spec, &scaled, &self.elements);
            let mut view = self.stats[st].clone();
            view.damage_types = damage.damage_types.0;
            view.core.damage_types = view.damage_types.clone();
            view.base_damage_bracket = damage.base_bracket;
            view.victim_low_health_multiplier = damage.victim_low_health_multiplier;
            view.slam_damage_ratio = damage.slam_damage_ratio;
            view.co_bucket = scaled;
            self.stats.push(view);
            out = self.stats.len() - 1;
        }
        if let Some((part, rate, duration)) = self.stats[st].lingering {
            let varied = self.vary(part, live, ceil)?;
            if varied != part {
                out = self.own(st, out);
                self.stats[out].lingering = Some((varied, rate, duration));
            }
        }
        let finite_fields=self.stats[st].finite_fields.clone();
        if !finite_fields.is_empty() {
            let mut fields=Vec::with_capacity(finite_fields.len());
            for &(part,rate,duration,initial) in &finite_fields {
                fields.push((self.vary(part,live,ceil)?,rate,duration,initial));
            }
            if fields!=finite_fields {out=self.own(st,out);self.stats[out].finite_fields=fields;}
        }
        let source_fields=self.stats[st].source_fields.clone();
        if !source_fields.is_empty() {
            let mut fields=Vec::with_capacity(source_fields.len());
            for &(part,rate,duration,initial) in &source_fields {
                fields.push((self.vary(part,live,ceil)?,rate,duration,initial));
            }
            if fields!=source_fields {
                out=self.own(st,out);
                self.stats[out].source_fields=fields;
            }
        }
        let extra = self.stats[st].additional.clone();
        if !extra.is_empty() {
            let mut parts = Vec::with_capacity(extra.len());
            for &part in &extra {
                parts.push(self.vary(part, live, ceil)?);
            }
            if parts != extra {
                out = self.own(st, out);
                self.stats[out].additional = parts;
            }
        }
        Some(out)
    }

    /// 若 `out` 已是 `st` 的独立副本，直接使用，否则先复制。
    fn own(&mut self, st: usize, out: usize) -> usize {
        if out != st {
            return out;
        }
        let view = self.stats[st].clone();
        self.stats.push(view);
        self.stats.len() - 1
    }

    fn view(&mut self, object: ObjectId) -> ReplayResult<usize> {
        if let Some(&index) = self.views.get(&object) {
            return Ok(index);
        }
        let mut context = ViewContext {
            types: &mut self.types,
            target_faction: &self.target_faction,
            faction_bracket: self.faction_bracket,
            co_specs: &mut self.co_specs,
        };
        let mut stats = self.provider.stats(object, &mut context)?;
        stats.obj = object;
        let index = self.stats.len();
        self.stats.push(stats);
        self.views.insert(object, index);
        let links = self.provider.stats_links(object)?;
        self.stats[index].live_co_unit = links.live_co_unit.map(|id| self.view(id)).transpose()?;
        self.stats[index].additional = self.view_parts(links.additional)?;
        self.stats[index].reload_empty = self.view_parts(links.reload_empty)?;
        self.stats[index].reload_partial = self.view_parts(links.reload_partial)?;
        self.stats[index].reload_lands_at = links.reload_lands_at;
        if let Some((profile, parts)) = links.stackable {
            let profile = self.stackable_profile(profile)?;
            let mut resolved = stackable::Parts::default();
            for (name, part) in parts {
                let part = self.view(part)?;
                match name.as_str() {
                    "embed" => resolved.embed = Some(part),
                    "explosion" => resolved.explosion = Some(part),
                    "merge" => resolved.merge = Some(part),
                    "singularity" => resolved.singularity = Some(part),
                    "child" => resolved.child = Some(part),
                    "orbRadial" => resolved.orb_radial = Some(part),
                    _ => {}
                }
            }
            self.stats[index].stackable = Some((profile, resolved));
        }
        for (part, rate, duration, slot) in links.finite_fields {
            let part = self.view(part)?;
            self.stats[index].finite_fields.push((part, rate, duration, slot));
        }
        for (part, rate, duration, slot) in links.source_fields {
            let part = self.view(part)?;
            self.stats[index].source_fields.push((part, rate, duration, slot));
        }
        if let Some((tick, blast)) = links.nox_projectile {
            let tick = self.view(tick)?;
            let blast = self.view(blast)?;
            self.stats[index].nox_projectile = Some((tick, blast));
        }
        self.stats[index].owned_embed = self.view_field(links.owned_embed)?;
        self.stats[index].lingering = self.view_field(links.lingering)?;
        self.stats[index].embedded = self.view_field(links.embedded)?;
        Ok(index)
    }

    fn view_parts(&mut self, parts: Vec<ObjectId>) -> ReplayResult<Vec<usize>> {
        parts.into_iter().map(|part| self.view(part)).collect()
    }

    fn view_field(&mut self, field: Option<(ObjectId, f64, f64)>) -> ReplayResult<Option<(usize, f64, f64)>> {
        field.map(|(part, rate, duration)| Ok((self.view(part)?, rate, duration))).transpose()
    }

    /// 若持续伤害实例 `i` 尚有剩余跳点，则调度下一次。
    fn next_tick(&mut self, i: usize) {
        let Some(chain) = self.tick_chains.get_mut(&i) else { return };
        let k = chain.next;
        let event = Event { t: pmax(0.0, chain.t + (chain.first + k as i64) as f64), seq: chain.base + 1 + k as u64,
                            kind: Kind::IndependentDot(i), gen: chain.gen };
        chain.next += 1;
        if chain.next >= chain.count {
            self.tick_chains.remove(&i);
        }
        self.heap.push(event);
    }

    fn push(&mut self, t: f64, kind: Kind, gen: Option<i64>) {
        if t < self.window + 1e-9 {
            self.seq += 1;
            self.heap.push(Event { t: pmax(0.0, t), seq: self.seq, kind, gen });
        }
    }

    // ── 异常状态 ──

    fn count(&mut self, d: D) -> f64 {
        if d == FIRE {
            return self.heat_chains.count();
        }
        if d == FREEZE && self.frozen_until > self.current_time {
            return 10.0;
        }
        if d == GAS || d == ELECTRICITY {
            let pile = self.proc_piles.get_mut(&d).map(|pile| pile.moments(self.current_time)[2]).unwrap_or(0.0);
            let other = self.states.get(&d).map(|r| psum(r.iter().map(|x| x.1))).unwrap_or(0.0);
            return pile + other;
        }
        if d == EXPLOSION {
            return psum(self.blast_procs.iter().map(|&i| self.blasts[i].count));
        }
        if let Some(p) = pooled_index(d) {
            return self.pooled[p].stats().1;
        }
        let cap = if d == FREEZE && self.pools[0] > 0.0 { 4.0 } else { cap_of(d).unwrap_or(f64::INFINITY) };
        let total = match self.states.get(&d) { Some(r) => psum(r.iter().map(|x| x.1)), None => psum(std::iter::empty()) };
        pmin(total, cap)
    }

    fn live_crit(&mut self) -> (f64, f64) {
        let cold = self.count(FREEZE);
        let cd = if self.frozen_until > self.current_time {
            1.0
        } else if cold < 1.0 {
            0.1 * cold
        } else {
            0.1 + 0.05 * (cold - 1.0)
        };
        (0.05 * self.count(PUNCTURE), cd)
    }

    fn clean(&mut self, t: f64) {
        let _timer = crate::counters::Timer::start(crate::counters::CLEAN);
        if self.frozen_until >= 0.0 && t >= self.frozen_until - 1e-9 {
            let end = self.frozen_until + 6.0 * self.frozen_scale_value();
            let mut rows = VecDeque::new();
            rows.push_back((end, 3.0));
            self.states.insert(FREEZE, rows);
            self.presence.entry(FREEZE).or_default().add(end, 0.0);
            self.frozen_until = -1.0;
        }
        for rows in self.states.values_mut() {
            while let Some(&(end, _)) = rows.front() {
                if end < t - 1e-9 {
                    rows.pop_front();
                } else {
                    break;
                }
            }
        }
        for pool in self.pooled.iter_mut() {
            if pool.is_live() {
                pool.expire(t);
            }
        }
        if let Some(pool) = &mut self.tau { pool.expire(t); }
        if self.heat_chains.active() {
            self.heat_chains.expire(t);
            if !self.heat_chains.active() {
                self.heat_token += 1;
            }
        } else {
            self.heat_chains.expire(t);
        }
        for q in self.sources.iter_mut() {
            if q.duration != 0.0 && q.stacks != 0.0 && t >= q.expires {
                if q.decay == "lose_one_and_reset" {
                    let missed = 1 + trunc_i((t - q.expires) / q.duration);
                    q.stacks = pmax(0.0, q.stacks - missed as f64);
                    q.expires += missed as f64 * q.duration;
                } else if q.decay != "per-stack" && q.decay != "union" {
                    q.stacks = 0.0;
                }
            }
            if q.decay == "per-stack" {
                q.expiries.retain(|x| x.0 > t);
                q.stacks = psum(q.expiries.iter().map(|x| x.1));
            } else if q.decay == "union" {
                q.expiries.retain(|x| x.0 > t);
                q.stacks = union_of(&q.expiries);
            }
        }
    }

    fn frozen_scale_value(&self) -> f64 {
        self.frozen_scale
    }

    fn heat_strip(&mut self, t: f64) -> f64 {
        self.heat_chains.strip(t)
    }

    fn armour_factor(&mut self, t: f64) -> f64 {
        let rest = self.armour_base * (1.0 - self.elemental_armour_strip) * (1.0 - self.heat_strip(t))
            * (1.0 - self.puncture_strip);
        let version = self.pooled[1].version;
        if let Some((v, r, value)) = self.armour_view {
            if v == version && r == rest.to_bits() {
                return value;
            }
        }
        let dist = self.pooled[1].distribution();
        let mut s = PySum::new();
        for (k, w) in dist.iter().enumerate() {
            if *w > 0.0 {
                let strip = if k == 0 { 0.0 } else { pmin(0.26 + 0.06 * (k as f64 - 1.0), 0.8) };
                s.add(*w * (1.0 - armour_reduction(rest * (1.0 - strip))));
            }
        }
        let value = s.value();
        self.armour_view = Some((version, rest.to_bits(), value));
        value
    }

    fn vulnerability(&mut self, d: D) -> f64 {
        let (present, n) = self.pooled[pooled_index(d).unwrap()].stats();
        pmin(1.0 + 0.75 * present + 0.25 * n, 4.25)
    }

    // ── 触发效果 ──

    fn signal(&mut self, kind: &str, t: f64, weight: f64) {
        let _timer = crate::counters::Timer::start(crate::counters::SIGNAL);
        for i in 0..self.sources.len() {
            let q = &mut self.sources[i];
            if q.actor_only && !self.actor_signal {
                continue;
            }
            if q.toggle_on.as_deref() == Some(kind) && q.per_event != 0.0 {
                q.stacks = 1.0 - q.stacks;
                q.expires = f64::INFINITY;
                continue;
            }
            if q.consume_on.as_deref() == Some(kind) && q.per_event != 0.0 {
                if q.charges != 0.0 {
                    // 多尔克拉夫在每次攻击开始时消耗一层。
                    if q.charges_left <= 1e-9 {
                        q.stacks = 0.0;
                    }
                    if q.stacks > 0.0 {
                        q.charges_left -= weight;
                    }
                } else {
                    // 弧电振子锤的震地消耗本次攻击开始时拥有的层数。
                    q.stacks = pmin(q.max_stacks, q.deferred);
                    q.deferred = 0.0;
                    q.locked = false;
                }
                continue;
            }
            if q.lock_on.as_deref() == Some(kind) && q.per_event != 0.0 {
                q.locked = true;
                continue;
            }
            let gate_probability = match q.victim_proc_type {
                Some(d) => self.victim_before.iter().find(|x| x.0 == d).map(|x| x.1).unwrap_or(0.0),
                None => 1.0,
            };
            if gate_probability == 0.0 {
                continue;
            }
            if q.reset_on.iter().any(|r| r == kind) && q.per_event != 0.0 {
                q.stacks = 0.0;
                q.expires = f64::INFINITY;
                q.expiries.clear();
                q.progress.clear();
            }
            if q.kind.as_deref() != Some(kind) {
                continue;
            }
            let required = pmax(q.required_count, 1.0);
            let delta;
            if required > 1.0 {
                if let Some(rt) = q.required_time {
                    while let Some(&(when, _)) = q.progress.front() {
                        if when < t - rt {
                            q.progress.pop_front();
                        } else {
                            break;
                        }
                    }
                }
                q.progress.push_back((t, weight * gate_probability));
                let completed = floor_i((psum(q.progress.iter().map(|x| x.1)) + 1e-9) / required);
                let mut consumed = completed as f64 * required;
                while consumed > 1e-9 {
                    let (when, n) = match q.progress.pop_front() {
                        Some(x) => x,
                        None => break,
                    };
                    if n > consumed {
                        q.progress.push_front((when, n - consumed));
                    }
                    consumed -= n;
                }
                delta = completed as f64 * q.per_event;
            } else {
                delta = weight * gate_probability * q.per_event;
            }
            // CanReproc 为 0 时，已有增益的分支不刷新，空闲分支可以触发。
            let delta = if q.no_refresh { delta * pmax(0.0, 1.0 - q.stacks / q.max_stacks) } else { delta };
            if delta <= 0.0 {
                continue;
            }
            if q.locked {
                q.deferred = pmin(q.max_stacks, q.deferred + delta);
                continue;
            }
            if q.charges != 0.0 {
                q.charges_left = q.charges;
            }
            let mut life = q.duration;
            if q.duration_per_tier != 0.0 {
                if let Some(spec) = &self.melee {
                    // 尖幡按击杀时超过 1 倍的连击倍率，增加对应的 2 秒或 4 秒时长。
                    life += q.duration_per_tier * spec.tier(self.melee_count) as f64;
                }
            }
            if q.decay == "union" {
                // 单个部分事件判定一次，概率为 weight × p。多次事件分别判定，合并概率为 1 − (1 − p)^weight。
                let roll = if required > 1.0 || weight <= 1.0 {
                    pmin(1.0, delta)
                } else {
                    1.0 - pow(1.0 - pmin(1.0, gate_probability * q.per_event), weight)
                };
                q.expiries.push((t + life, roll));
                q.stacks = union_of(&q.expiries);
                q.expires = t + life;
                continue;
            }
            if q.decay == "per-stack" {
                q.expiries.push((t + life, delta));
                let mut excess = psum(q.expiries.iter().map(|x| x.1)) - q.max_stacks;
                while excess > 1e-9 && !q.expiries.is_empty() {
                    let (end, n) = q.expiries[0];
                    let removed = pmin(n, excess);
                    excess -= removed;
                    if removed >= n {
                        q.expiries.remove(0);
                    } else {
                        q.expiries[0] = (end, n - removed);
                    }
                }
            }
            q.stacks = pmin(q.max_stacks, q.stacks + delta);
            q.expires = if q.duration != 0.0 { t + life } else { f64::INFINITY };
        }
    }

    fn consecutive_heads(&mut self, probability: f64) {
        for q in self.sources.iter_mut() {
            if q.kind.as_deref() != Some("on-consecutive-headshot") || q.per_event == 0.0 {
                continue;
            }
            let minimum = if q.minimum_consecutive != 0 { q.minimum_consecutive } else { 1 };
            let cap = minimum + trunc_i(q.max_stacks) - 1;
            let before = q.run_distribution.clone().unwrap_or_else(|| {
                let mut m = FxIndexMap::default();
                m.insert(0i64, 1.0);
                m
            });
            let mut after: FxIndexMap<i64, f64> = FxIndexMap::default();
            after.insert(0, 1.0 - probability);
            for (run, weight) in before.iter() {
                let n = (run + 1).min(cap);
                let prev = *after.get(&n).unwrap_or(&0.0);
                after.insert(n, prev + weight * probability);
            }
            q.stacks = psum(after.iter().map(|(run, w)| (run - minimum + 1).max(0) as f64 * *w));
            q.run_distribution = Some(after);
            q.expires = f64::INFINITY;
        }
    }

    fn enervate_weights_for_callback(&self) -> Vec<(i64, f64)> {
        self.enervate.as_ref().map(|en| en.damage_marginal()).unwrap_or_else(|| vec![(0, 1.0)])
    }

    fn source_stacks(&self) -> SourceStacks {
        self.sources.iter().map(|q| (StackKey::Object(q.id), StackValue::Float(q.stacks))).collect()
    }

    fn refresh_view(&mut self, hook: ObjectId, stacks: SourceStacks, live: f64, initial: Option<bool>) -> ReplayResult<usize> {
        let object = self.provider.refresh(hook, stacks, live, initial)?;
        self.view(object)
    }

    // ── 武器属性与伤害包 ──

    fn live_proc_types(&mut self, t: f64) -> f64 {
        psum(self.presence.values_mut().map(|presence| presence.probability(t)))
    }

    fn snapshot_victim_procs(&mut self, t: f64) {
        self.victim_before.clear();
        self.victim_before.extend(self.presence.iter_mut().map(|(&d, presence)| (d, presence.probability(t))));
    }

    fn model(&mut self, t: f64) -> ReplayResult<(usize, Option<usize>)> {
        let _timer = crate::counters::Timer::start(crate::counters::MODEL);
        let live_types = if self.live_types_enabled {
            self.live_proc_types(t)
        } else {
            0.0
        };
        let mut stacks: Vec<u64> = std::mem::take(&mut self.model_key);
        stacks.clear();
        stacks.push(self.form as u64);
        stacks.push(self.volley_rounds.to_bits());
        if self.physical_lane_length>0 {stacks.push(self.physical_lane_selected.map(|v|v as u64).unwrap_or(u64::MAX-2));}
        if self.projectile_sequence_length > 0 { stacks.push(self.projectile_sequence_index as u64); }
        stacks.extend(self.sources.iter().map(|q| q.stacks.to_bits()));
        // 近战连击层数与重击标记加入缓存键，并按参考实现的额外层数名称排序。
        let mut extra: Vec<(String, f64, ObjectId)> = Vec::new();
        if let Some(spec) = &self.melee {
            let tier = spec.tier(self.melee_count) as f64;
            for (obj, name, cap) in &spec.tier_keys {
                extra.push((name.clone(), pmin(tier, *cap), *obj));
            }
        }
        if let (Some(_), true) = (self.onos, self.form != BASE) {
            let (power, hits, beam) = self.onos_values(t);
            stacks.push(u64::MAX - 1);
            stacks.push(power as u64);
            stacks.push(hits.to_bits());
            stacks.push(beam.to_bits());
        }
        let mut sorted: Vec<(String, u64)> = extra.iter().map(|(n, v, _)| (n.clone(), v.to_bits())).collect();
        if self.heavy_flag {
            sorted.push(("__heavy".to_string(), 1));
        }
        if self.no_co_flag {
            sorted.push(("__noCO".to_string(), 1));
        }
        sorted.sort();
        for (name, bits) in &sorted {
            stacks.push(u64::MAX);
            for byte in name.bytes() {
                stacks.push(byte as u64);
            }
            stacks.push(*bits);
        }
        let key = (stacks, live_types.to_bits(), self.last_round);
        if let Some(&hit) = self.model_cache.get(&key) {
            self.model_key = key.0;
            return Ok(hit);
        }
        self.model_at(key.0, extra, live_types)
    }

    fn model_at(&mut self, stacks: Vec<u64>, extra: Vec<(String, f64, ObjectId)>, live_types: f64)
        -> ReplayResult<(usize, Option<usize>)> {
        let key = (stacks, live_types.to_bits(), self.last_round);
        if let Some(&hit) = self.model_cache.get(&key) {
            return Ok(hit);
        }
        // 非整数的活跃状态数仅缩放异况超量效果组，从向上取整后的属性视图派生。
        let ceil = live_types.ceil();
        // Onos 在 `apply_mods` 后缩放已结算伤害，禁用此处的异况超量变体。
        if self.live_types_enabled && self.onos.is_none() && ceil >= 1.0 && live_types != ceil && ceil.is_finite() {
            let template_extra = extra.clone();
            let (active, radial) = self.model_at(key.0.clone(), template_extra, ceil)?;
            if let Some(a) = self.vary(active, live_types, ceil) {
                let r = match radial {
                    Some(r) => self.vary(r, live_types, ceil).map(Some),
                    None => Some(None),
                };
                if let Some(r) = r {
                    self.model_cache.insert(key, (a, r));
                    return Ok((a, r));
                }
            }
        }
        let mut stacks = self.source_stacks();
        let (power, hits, beam) = self.onos_values(self.current_time);
        stacks.push((StackKey::Named("__onosPower"), StackValue::Flag(power)));
        stacks.push((StackKey::Named("__onosHits"), StackValue::Float(hits)));
        stacks.push((StackKey::Named("__onosBeamSeconds"), StackValue::Float(beam)));
        stacks.push((StackKey::Named("__chargeVolleyRounds"), StackValue::Float(self.volley_rounds)));
        if let Some(index) = self.physical_lane_selected {
            stacks.push((StackKey::Named("__physicalProjectileIndex"), StackValue::Index(index)));
        }
        if self.projectile_sequence_length > 0 {
            stacks.push((StackKey::Named("__projectileSequenceIndex"), StackValue::Index(self.projectile_sequence_index)));
        }
        for (_, value, obj) in extra {
            stacks.push((StackKey::Object(obj), StackValue::Float(value)));
        }
        if self.heavy_flag {
            stacks.push((StackKey::Named("__heavy"), StackValue::Flag(true)));
        }
        if self.no_co_flag {
            stacks.push((StackKey::Named("__noCO"), StackValue::Flag(true)));
        }
        let started = std::time::Instant::now();
        let (active, radial) = self.provider.resolve(ResolveInput {
            form: form_name(self.form), stacks, live_types: self.live_types_enabled.then_some(live_types),
            last_round: self.last_round,
        })?;
        let resolved = std::time::Instant::now();
        crate::counters::add(crate::counters::RESOLVE, (resolved - started).as_nanos() as u64);
        let active = self.view(active)?;
        let radial = radial.map(|object| self.view(object)).transpose()?;
        crate::counters::add(crate::counters::VIEW, resolved.elapsed().as_nanos() as u64);
        self.model_cache.insert(key, (active, radial));
        Ok((active, radial))
    }

    fn packet(&mut self, st: usize, aim: f64) -> ReplayResult<PacketKey> {
        let _timer = crate::counters::Timer::start(crate::counters::PACKET);
        if self.stats[st].fixed_radial {
            return Ok(self.raw_packet(st, 0.0));
        }
        let mut st = self.tau_view(st);
        let undamaged = (self.pools[1] - self.full[1]).abs() < 1e-7 && (self.pools[2] - self.full[2]).abs() < 1e-7;
        let (vfcc, vfcd) = (self.stats[st].victim_full_crit_chance, self.stats[st].victim_full_crit_damage);
        if undamaged && (vfcc != 0.0 || vfcd != 0.0) {
            st = self.derive(st, 3, 0.0, |v| {
                v.crit_chance += vfcc;
                v.crit_multiplier += vfcd;
                v.core.crit_chance = v.crit_chance;
                v.core.crit_multiplier = v.crit_multiplier;
            });
        }
        let low = self.stats[st].victim_low_health_multiplier;
        if self.full[2] > 0.0 && self.pools[2] < self.full[2] * 0.5 && low != 1.0 {
            st = self.derive(st, 4, 0.0, |v| {
                for e in v.core.damage_types.iter_mut() {
                    e.1 *= low;
                }
                v.damage_types = v.core.damage_types.clone();
            });
        }
        let s = &self.stats[st];
        let (radial_only, radial_victim_crit, csd, bracket, frozen_mult) =
            (s.radial_only, s.radial_victim_crit, s.cold_stack_damage, s.base_damage_bracket, s.frozen_damage_multiplier);
        let (mut puncture, mut cold) = self.live_crit();
        if radial_only && !radial_victim_crit {
            puncture = 0.0;
            cold = 0.0;
        }
        let contact_shiver=self.stats[st].contact_shiver;
        let contact_scale=self.stats[st].contact_shiver_scale;
        let contact_cold = if contact_shiver {self.stats[st].contact_shiver_fixed.unwrap_or_else(||self.count(FREEZE))} else { 0.0 };
        let mut shiver = 1.0;
        let mut frozen = 1.0;
        if ((cold != 0.0 && !radial_only) || contact_cold != 0.0) && (csd != 0.0 || frozen_mult != 1.0) {
            let stacks_cold = if contact_shiver {contact_cold} else {self.count(FREEZE)};
            if csd != 0.0 && bracket > 0.0 {
                shiver = (bracket + csd * pmin(stacks_cold, 10.0) * contact_scale) / bracket;
            }
            if !radial_only && (self.frozen_until > self.current_time || stacks_cold >= 10.0 - 1e-9) {
                frozen = frozen_mult;
            }
        }
        if self.enervate.is_some() && !self.stats[st].snapshot_enervate {
            return self.enervate_packet(st, aim, puncture, cold, shiver, frozen);
        }
        let fast = puncture == 0.0 && cold == 0.0 && shiver == 1.0 && frozen == 1.0;
        let key = if fast {
            (st, aim.to_bits(), 0, 0, 0, 1, 0)
        } else {
            (st, aim.to_bits(), puncture.to_bits(), cold.to_bits(), shiver.to_bits(), frozen.to_bits(), 0)
        };
        if !self.packets.contains_key(&key) {
            let core = &self.stats[st].core;
            let (vector, procs, rows, non) = if fast {
                tables::attack_data(core, aim, &self.tables)
            } else {
                tables::crit_view_packet(core, aim, puncture, cold, shiver, frozen, &self.tables)
            };
            let packet = Packet {
                vector,
                procs,
                rows: rows.into_iter().map(|(d, r)| (d, Row {
                    duration: r.duration, seed: r.seed, accumulator: r.accumulator, shared: r.shared,
                    coefficient: r.coefficient, branches: r.branches, kernels: OnceCell::new(),
                })).collect(),
                non: non.into_iter().collect(),
            };
            self.packets.insert(key, Rc::new(packet));
        }
        Ok(key)
    }

    /// `combat_replay.packet` 处理次要侵损时，用命中前的伤害分布加权各层暴击视图。
    fn enervate_packet(&mut self, st: usize, aim: f64, puncture: f64, cold: f64, shiver: f64, frozen: f64)
        -> ReplayResult<PacketKey> {
        let weights = self.enervate.as_ref().unwrap().damage_marginal();
        let bits: Vec<(i64, u64)> = weights.iter().map(|(s, w)| (*s, w.to_bits())).collect();
        let next = self.enervate_weights.len() as u64 + 1;
        let id = *self.enervate_weights.entry(bits).or_insert(next);
        let key = (st, aim.to_bits(), puncture.to_bits(), cold.to_bits(), shiver.to_bits(), frozen.to_bits(), id);
        if self.packets.contains_key(&key) {
            return Ok(key);
        }
        let mut vector: Vector = Vec::new();
        let mut procs: Vector = Vec::new();
        let mut rows: FxIndexMap<D, Row> = FxIndexMap::default();
        let mut non: FxIndexMap<D, f64> = FxIndexMap::default();
        let mut seed_sums: Vector = Vec::new();
        let mut non_sums: Vector = Vec::new();
        for &(stacks, weight) in &weights {
            let vkey = (st, aim.to_bits(), stacks, puncture.to_bits(), cold.to_bits(), shiver.to_bits(), frozen.to_bits());
            let view = match self.enervate_views.get(&vkey) {
                Some(v) => v.clone(),
                None => {
                    let core = tables::crit_view(&self.stats[st].core, stacks as f64 * 0.1, puncture, cold, shiver, frozen);
                    let (v, p, r, n) = tables::attack_data(&core, aim, &self.tables);
                    let packet = Rc::new(Packet {
                        vector: v,
                        procs: p,
                        rows: r.into_iter().map(|(d, r)| (d, Row {
                            duration: r.duration, seed: r.seed, accumulator: r.accumulator, shared: r.shared,
                            coefficient: r.coefficient, branches: r.branches, kernels: OnceCell::new(),
                        })).collect(),
                        non: n.into_iter().collect(),
                    });
                    self.enervate_views.insert(vkey, packet.clone());
                    packet
                }
            };
            for &(k, value) in &view.vector {
                ordered_add(&mut vector, k, weight * value);
            }
            for &(k, value) in &view.procs {
                ordered_add(&mut procs, k, weight * value);
            }
            for (&k, row) in &view.rows {
                let entry = rows.entry(k).or_insert_with(|| {
                    let mut base = row.clone();
                    base.branches.clear();
                    base
                });
                entry.branches.extend(row.branches.iter().map(|branch| {
                    let mut branch = *branch;
                    branch.weight *= weight;
                    branch
                }));
                let pk = view.procs.iter().find(|e| e.0 == k).map(|e| e.1).unwrap_or(0.0);
                ordered_add(&mut seed_sums, k, weight * pk * row.seed);
            }
            for (&k, &per) in &view.non {
                non.insert(k, per);
                let pk = view.procs.iter().find(|e| e.0 == k).map(|e| e.1).unwrap_or(0.0);
                ordered_add(&mut non_sums, k, weight * pk * per);
            }
        }
        let total = |d: D| procs.iter().find(|e| e.0 == d).map(|e| e.1).unwrap_or(0.0);
        for (k, row) in rows.iter_mut() {
            let p = total(*k);
            let sum = seed_sums.iter().find(|e| e.0 == *k).map(|e| e.1).unwrap_or(0.0);
            row.seed = if p != 0.0 { sum / p } else { 0.0 };
        }
        for (k, per) in non.iter_mut() {
            let p = total(*k);
            let sum = non_sums.iter().find(|e| e.0 == *k).map(|e| e.1).unwrap_or(0.0);
            *per = if p != 0.0 { sum / p } else { 0.0 };
        }
        self.packets.insert(key, Rc::new(Packet { vector, procs, rows, non }));
        Ok(key)
    }

    /// `combat_replay.enervate_hit` 按各层本次直接命中出现橙暴或红暴的概率推进状态。
    fn enervate_hit(&mut self, st: usize, t: f64, puncture: f64, aim: f64) {
        let Some(en) = self.enervate.as_ref() else { return };
        // 这个分支没有基础形态（无灵化循环），被忽略的次要射击不叠层。
        if self.stats[st].multishot <= 0.0 || (en.ignore_selected && self.form != BASE) {
            return;
        }
        let s = &self.stats[st];
        let weak = s.weakpoint_crit_chance_bonus + s.base_crit_chance * s.weakpoint_crit_chance_relative;
        let promotion = s.core.double_crit_chance;
        let pellets = if s.continuous { 1.0 } else { s.multishot };
        let probabilities: Vec<(i64, f64)> = en
            .marginal()
            .into_iter()
            .map(|(stacks, _)| {
                let cc = pmax(0.0, s.crit_chance + stacks as f64 * 0.1 + puncture);
                let mut p = (1.0 - aim) * big_crit_probability(cc * s.bodyshot_crit_multiplier, promotion);
                p += aim * big_crit_probability(cc + weak, promotion);
                (stacks, any_probability(p, pellets))
            })
            .collect();
        self.enervate.as_mut().unwrap().advance(t, Some(&probabilities));
    }

    /// 无暴击视图的 `attack_data(st, aim)`，用于滞留场和固定范围伤害。
    fn raw_packet(&mut self, st: usize, aim: f64) -> PacketKey {
        let st = self.tau_view(st);
        let key = (st, aim.to_bits(), 0, 0, 0, 1, 0);
        if !self.packets.contains_key(&key) {
            let (vector, procs, rows, non) = tables::attack_data(&self.stats[st].core, aim, &self.tables);
            let packet = Packet {
                vector,
                procs,
                rows: rows.into_iter().map(|(d, r)| (d, Row {
                    duration: r.duration, seed: r.seed, accumulator: r.accumulator, shared: r.shared,
                    coefficient: r.coefficient, branches: r.branches, kernels: OnceCell::new(),
                })).collect(),
                non: non.into_iter().collect(),
            };
            self.packets.insert(key, Rc::new(packet));
        }
        key
    }

    /// 按派生来源缓存属性视图，例如转速变化后的弹丸数和首发加成。相同属性生成相同的伤害表。
    fn derive(&mut self, st: usize, kind: u8, param: f64, change: impl FnOnce(&mut Stats)) -> usize {
        let key = (st, kind, param.to_bits());
        if let Some(&i) = self.derived.get(&key) {
            return i;
        }
        let mut view = self.stats[st].clone();
        change(&mut view);
        self.stats.push(view);
        let i = self.stats.len() - 1;
        self.derived.insert(key, i);
        i
    }

    fn hit_spec(&mut self, id: i64) -> ReplayResult<Rc<HitSpec>> {
        let i = id as usize;
        if let Some(Some(spec)) = self.hit_specs.get(i) {
            return Ok(spec.clone());
        }
        let spec = Rc::new(self.provider.hit_spec(id, &mut self.types)?);
        if self.hit_specs.len() <= i {
            self.hit_specs.resize(i + 1, None);
        }
        self.hit_specs[i] = Some(spec.clone());
        Ok(spec)
    }

    /// 派生 `melee_sequence.apply_hit` 与近战复制赋能的属性视图。
    fn melee_apply(&mut self, st: usize, id: i64, heavy_multiplier: f64) -> ReplayResult<usize> {
        let key = (st, id, heavy_multiplier.to_bits());
        if let Some(&hit) = self.applied.get(&key) {
            return Ok(hit);
        }
        let hit = self.hit_spec(id)?;
        let mut v = self.stats[st].clone();
        let mut factor = hit.atten;
        if hit.slam {
            let ratio = v.slam_damage_ratio;
            factor *= if ratio != 0.0 { ratio } else { 1.0 };
        }
        if hit.throw && v.throw_damage_bonus != 0.0 {
            // 奋力一掷仅在投掷本体的基础伤害乘区加成。
            let bracket = if v.base_damage_bracket != 0.0 { v.base_damage_bracket } else { 1.0 };
            factor *= 1.0 + v.throw_damage_bonus / bracket;
        }
        if hit.combo_multiplier {
            factor *= heavy_multiplier;
        }
        let mut damage: Vector = v.damage_types.iter()
            .map(|&(k, x)| (k, x * factor * hit.pct.iter().find(|e| e.0 == k).map(|e| e.1).unwrap_or(1.0)))
            .collect();
        if let Some(target) = hit.target {
            let mut moved = PySum::new();
            for name in ["DT_IMPACT", "DT_PUNCTURE", "DT_SLASH"] {
                let d = self.types.id(name);
                let popped = match damage.iter().position(|e| e.0 == d) {
                    Some(i) => damage.remove(i).1,
                    None => 0.0,
                };
                moved.add(popped);
            }
            let moved = moved.value();
            match damage.iter_mut().find(|e| e.0 == target) {
                Some(e) => e.1 += moved,
                None => damage.push((target, 0.0 + moved)),
            }
        }
        let mut crit_chance = v.crit_chance;
        let mut crit_multiplier = v.crit_multiplier;
        if let Some(f) = hit.crit_chance {
            crit_chance *= f;
        }
        if let Some(f) = hit.crit_multiplier {
            crit_multiplier *= f;
        }
        if !hit.can_crit {
            crit_chance = 0.0;
        }
        let mut forced = v.native_forced_procs.clone();
        for &d in &hit.extra_procs {
            match forced.iter_mut().find(|e| e.0 == d) {
                Some(e) => e.1 += 1.0,
                None => forced.push((d, 0.0 + 1.0)),
            }
        }
        v.drop_private();
        v.damage_types = damage.clone();
        v.core.damage_types = damage;
        v.core.dot_base_damage = v.core.dot_base_damage.map(|x| x * factor);
        v.crit_chance = crit_chance;
        v.core.crit_chance = crit_chance;
        v.crit_multiplier = crit_multiplier;
        v.core.crit_multiplier = crit_multiplier;
        v.status_chance *= hit.proc_atten;
        v.core.status_chance = v.status_chance;
        v.native_forced_procs = forced.clone();
        v.core.native_forced_procs = forced;
        // 挥发弹跳爆炸使用配装的弹跳概率。
        v.multishot = if hit.bounce { pmin(1.0, pmax(0.0, v.reflection_explosion_chance)) } else { 1.0 };
        v.radial_only = hit.radial;
        v.core.radial_only = hit.radial;
        if let Some((chance, uptime)) = hit.duplicate {
            let c = pmin(pmax(v.crit_chance, 0.0), 2.0);
            let yellow = if c < 1.0 { c } else { 2.0 - c };
            let f = 1.0 + chance * uptime * yellow;
            if f != 1.0 {
                v.multishot *= f;
                v.drop_private();
            }
        }
        v.core.multishot = v.multishot;
        self.stats.push(v);
        let i = self.stats.len() - 1;
        self.applied.insert(key, i);
        Ok(i)
    }

    /// `melee_sequence.Loop.next` 返回路线的下一次命中与所需秒数。
    fn advance_melee_recovery(&mut self, t: f64, initial: f64, snap: Option<f64>) {
        let spec = self.melee.as_ref().unwrap();
        let before = self.melee_count;
        let tier_before = spec.tier(before);
        if before < initial {
            self.melee_count = pmin(initial,before+pmax(0.0,t-self.melee_combo_at)*spec.recovery_per_second);
            if let Some(target) = snap { self.melee_count=pmax(self.melee_count,pmin(initial,target)); }
        }
        self.melee_combo_at = t;
        let tier_after = spec.tier(self.melee_count);
        if tier_after > tier_before {
            self.signal("on-combo-tier-increase", t, (tier_after-tier_before) as f64);
        }
    }

    fn schedule_melee_recovery(&mut self, t: f64, initial: f64) {
        let spec=self.melee.as_ref().unwrap();
        let rate=spec.recovery_per_second;
        if rate <= 0.0 || self.melee_count >= initial { return; }
        let next=spec.base_hits+spec.tier(self.melee_count) as f64*spec.tier_step-0.5;
        let target=pmin(initial,next);
        self.push(t+pmax(1e-9,(target-self.melee_count)/rate),Kind::MeleeRecovery(self.melee_recovery_token,target),None);
    }

    fn sync_melee_recovery(&mut self, t: f64) -> ReplayResult<()> {
        if self.melee_pending.is_none() { return Ok(()); }
        let active=self.model(t)?.0;
        let initial=pmin((pmax(0.0,self.stats[active].initial_combo) as f32).trunc() as f64,self.melee.as_ref().unwrap().cap);
        if initial != self.melee_recovery_initial {
            self.melee_recovery_initial=initial;
            self.melee_recovery_token+=1;
            self.schedule_melee_recovery(t,initial);
        }
        let mut boundary=f64::INFINITY;
        for q in &self.sources {
            if q.stacks==0.0 { continue; }
            if q.decay=="per-stack" || q.decay=="union" {
                for &(when,_) in &q.expiries {
                    if when>t+1e-9 { boundary=pmin(boundary,when); }
                }
            } else if q.expires>t+1e-9 { boundary=pmin(boundary,q.expires); }
        }
        if boundary != self.melee_buff_boundary {
            self.melee_buff_boundary=boundary;
            if boundary.is_finite() { self.push(boundary,Kind::MeleeBuffBoundary,None); }
        }
        Ok(())
    }

    fn melee_next(&mut self, speed: f64, charge: f64, projectile: f64) -> ReplayResult<(MeleeHit, f64)> {
        let segments = &self.melee.as_ref().expect("melee route").segments;
        if segments.is_empty() {
            return Err(invalid_index("list index out of range"));
        }
        let mut waited = 0.0;
        for _ in 0..segments.len() + 1 {
            let segment = &segments[self.melee_index];
            self.melee_index = (self.melee_index + 1) % segments.len();
            let mut fixed = segment.fixed;
            if segment.charge_scaled {
                if charge == 0.0 {
                    return Err(invalid_zero_division("float division by zero"));
                }
                fixed = segment.fixed / charge;
            }
            if segment.flight_scaled {
                fixed = segment.fixed / pmax(projectile, 1e-9);
            }
            waited += fixed + segment.anim / pmax(speed, 1e-9);
            if let Some(hit) = segment.hit {
                return Ok((hit, waited));
            }
        }
        Err(invalid_value("melee route has no hits"))
    }

    fn spread(&mut self, st: usize, aim: f64) -> f64 {
        let key = (st, aim.to_bits());
        if let Some(&v) = self.spreads.get(&key) {
            return v;
        }
        let s = &self.stats[st];
        let weak = s.weakpoint_crit_chance_bonus + s.base_crit_chance * s.weakpoint_crit_chance_relative;
        let mut mean = 0.0;
        let mut square = 0.0;
        for (weight, chance) in [(1.0 - aim, s.crit_chance * s.bodyshot_crit_multiplier), (aim, s.crit_chance + weak)] {
            if weight <= 0.0 {
                continue;
            }
            let chance = pmax(0.0, chance);
            let fr = chance - chance.floor();
            let tier = floor_i(chance);
            let low = tables::tier_factor(tier, s.crit_multiplier);
            let high = if fr > 0.0 { tables::tier_factor(tier + 1, s.crit_multiplier) } else { low };
            let m = (1.0-fr)*low + fr*high;
            mean += weight * m;
            square += weight * (m * m + fr * (1.0 - fr) * (high-low).powi(2));
        }
        let value = if mean <= 0.0 {
            0.0
        } else {
            let pellets = if s.continuous { 1.0 } else { pmax(s.multishot, 1.0) };
            pmax(0.0, square / (mean * mean) - 1.0) / pellets
        };
        self.spreads.insert(key, value);
        value
    }

    /// 为每个类型生成一次 `proc-<type>`。
    fn proc_signal(&mut self, d: D) -> Rc<str> {
        let i = d as usize;
        if self.proc_names.len() <= i {
            self.proc_names.resize(i + 1, None);
        }
        if let Some(name) = &self.proc_names[i] {
            return name.clone();
        }
        let name: Rc<str> = Rc::from(format!("proc-{}", self.types.name(d)));
        self.proc_names[i] = Some(name.clone());
        name
    }

    fn no_status_probability(&mut self, st: usize, d: D, aim: f64) -> ReplayResult<f64> {
        let key = (st, d, aim.to_bits());
        if let Some(&hit) = self.no_status.get(&key) {
            return Ok(hit);
        }
        let value = self.no_status_probability_uncached(st, d, aim);
        self.no_status.insert(key, value);
        Ok(value)
    }

    /// 强制异常在回放期间固定，按属性视图、伤害类型和瞄准参数缓存。
    fn no_status_probability_uncached(&self, st: usize, d: D, aim: f64) -> f64 {
        let multishot = self.stats[st].multishot;
        let extra = self.forced_procs.iter().find(|x| x.0 == d).map(|x| x.1).unwrap_or(0.0);
        tables::no_status_probability(&self.stats[st].core, d, aim, multishot, extra)
    }

    fn tau_view(&mut self, st: usize) -> usize {
        let count = self.tau.as_mut().map_or(0.0, |pool| pool.stats().1);
        if count <= 0.0 || self.stats[st].status_chance <= 0.0 { return st; }
        self.derive(st, 42, count, |v| {
            v.status_chance *= 1.0 + 0.1 * count;
            v.core.status_chance = v.status_chance;
        })
    }

    // ── 伤害 ──

    fn faction_delta(&self, d: D) -> f64 {
        self.faction_deltas.iter().find(|x| x.0 == d).map(|x| x.1).unwrap_or(0.0)
    }

    fn health_value(&mut self, d: D, raw: f64, t: f64) -> f64 {
        let mut mult = pmax(0.0, 1.0 + self.faction_delta(d)) * self.vulnerability(VIRAL);
        if !self.armour_bypass.contains(&d) {
            mult *= self.armour_factor(t);
        }
        raw * mult
    }

    fn resume_time(&mut self, t: f64) -> ReplayResult<Option<f64>> {
        if self.shot_in_progress && self.volley_shot_resume.is_some() { return Ok(self.volley_shot_resume); }
        let (active, _) = self.model(t)?;
        let a = &self.stats[active];
        if self.shot_in_progress && a.magazine > 0.0 && !self.infinite_ammo && self.ammo - pmax(a.ammo_cost, 0.0) <= 1e-9 {
            return Ok(Some(t + 1.0 / pmax(a.fire_rate, 1e-9) + a.reload_time));
        }
        let mut best: Option<f64> = None;
        for e in self.heap.iter() {
            if matches!(e.kind, Kind::Fire | Kind::ChargedFire | Kind::ReloadComplete(_)) {
                best = Some(match best {
                    None => e.t,
                    Some(b) => pmin(b, e.t),
                });
            }
        }
        Ok(best)
    }

    fn magnetic_break(&mut self, t: f64, pool_index: usize, weapon_source: bool) -> ReplayResult<bool> {
        let dist = self.pooled[pooled_index(MAGNETIC).unwrap()].distribution();
        let probability = psum(dist.iter().skip(1).copied());
        if probability <= 0.0 { return Ok(false); }
        let stacks = psum(dist.iter().enumerate().map(|(n, p)| n as f64 * p));
        let dist = dist.clone();
        let active = match self.proc_source { Some(st)=>st, None=>self.model(t)?.0 };
        let core=&self.stats[active].core;
        let scale=core.duration_scale(ELECTRICITY);
        let faction=core.faction_all*core.faction_target*core.faction_bracket;
        let electric=pmax(core.dot_elemental_multipliers.iter().find(|e|e.0==ELECTRICITY).map(|e|e.1).unwrap_or(1.0),0.0);
        let status=pmax(core.proc_damage_multiplier,0.0) as f32;
        let landing=faction*electric;
        let mut direct = 0.0;
        let mut child = 0.0;
        let mut outcomes = vec![PileOutcome {probability:dist[0],count:0,seed:0.0,accumulator:0.0,landing:1.0,weak:false}];
        for (n, weight) in dist.into_iter().enumerate().skip(1).filter(|(_, weight)| *weight != 0.0) {
            let mut retained = (((self.full[pool_index] as f32) * 0.005_f32) * n as f32).trunc();
            if let (Some(weapon), Some((weakness, attenuation))) = (self.magnetic_weakness, self.magnetic_proxy) {
                let divisor = 1.0_f32 + weapon.max(0.0) * weakness;
                retained = (((retained / divisor + 0.5_f32).floor()) * status) * attenuation;
            } else {
                retained *= status;
            }
            direct += retained as f64 * weight;
            let seed = 0.5 * (((retained * status) as f64 * faction) as f32) as f64;
            child += seed * weight;
            outcomes.push(PileOutcome {probability:weight,count:1,seed,accumulator:0.5,landing,weak:false});
        }
        let per_tick=child/probability*landing;
        self.magnetic_breaks.push((t,pool_index,stacks,per_tick,self.gen));
        if direct > 0.0 && self.settle_damage(&vec![(ELECTRICITY,direct*faction)],t,DIRECT,WeakKill::Flag(false),0.0,true,None,weapon_source,true)? {
            return Ok(true);
        }
        if scale <= 0.0 || self.magnetic_electric_immune { return Ok(false); }
        let duration=6.0*scale;
        self.presence.entry(ELECTRICITY).or_default().add(t+duration,1.0-probability);
        *self.proc_counts.entry(ELECTRICITY).or_insert(0.0)+=probability;
        self.add_pile(ELECTRICITY,t,duration,&[Rc::new(PileKernel::new(&outcomes))],active);
        let signal=self.proc_signal(ELECTRICITY);
        self.signal(&signal,t,probability);
        self.signal("on-proc",t,probability);
        Ok(false)
    }

    #[allow(clippy::too_many_arguments)]
    fn take(&mut self, vector: &Vector, t: f64, source: usize, weak_kill: WeakKill, cv2: f64, extra: bool) -> ReplayResult<bool> {
        self.take_with_event(vector, t, source, weak_kill, cv2, extra, None)
    }

    fn take_with_event(&mut self, vector: &Vector, t: f64, source: usize, weak_kill: WeakKill, cv2: f64, extra: bool, damage_event: Option<(f64, f64)>) -> ReplayResult<bool> {
        self.take_with_source_credit(vector,t,source,weak_kill,cv2,extra,damage_event,true)
    }

    fn take_with_source_credit(&mut self, vector: &Vector, t: f64, source: usize, weak_kill: WeakKill, cv2: f64, extra: bool, damage_event: Option<(f64,f64)>, weapon_source: bool) -> ReplayResult<bool> {
        self.settle_damage(vector,t,source,weak_kill,cv2,extra,damage_event,weapon_source,false)
    }

    fn settle_damage(&mut self, vector: &Vector, t: f64, source: usize, weak_kill: WeakKill, cv2: f64, extra: bool, damage_event: Option<(f64,f64)>, weapon_source: bool, magnetic_packet: bool) -> ReplayResult<bool> {
        let _timer = crate::counters::Timer::start(crate::counters::TAKE);
        let clamped;
        let vector = if vector.iter().any(|entry| entry.1 < 0.0) {
            clamped = vector.iter().map(|&(dtype, raw)| (dtype, pmax(0.0, raw))).collect::<Vector>();
            &clamped
        } else {
            vector
        };
        if let Some((alignment, _, _)) = self.onos {
            if self.onos_crits && self.form != BASE && (self.onos_ticks as f64) < alignment
                && (source == DOT || source == NONDOT) && vector.iter().any(|e| e.1 > 0.0)
            {
                self.onos_hits = pmin(50.0, self.onos_hits + 1.0);
            }
        }
        let mut left = 1.0;
        let mut applied = 0.0;
        let mut earlier_hit = 0.0;
        let mut health_hit = 0.0;
        let mut remaining = 0.0;
        let mut overguard_broke = false;
        let mut shield_broke = false;
        if self.pools[0] > 0.0 {
            let mut value = psum(vector.iter().map(|(k, raw)| raw * if *k == VOID || *k == RADIANT { 1.5 } else { 1.0 }))
                * self.vulnerability(MAGNETIC);
            let fortifier = if weapon_source && source != SYNDICATE { self.fortifier } else { None };
            if let Some((multiplier, _, _, uptime)) = fortifier {
                value *= 1.0 + (multiplier - 1.0) * uptime;
            }
            if value <= 0.0 {
                return Ok(false);
            }
            let fraction = pmin(left, self.pools[0] / value);
            let drain = value * fraction;
            self.pools[0] = pmax(0.0, self.pools[0] - drain);
            earlier_hit += drain;
            if let Some((_, steal, cap, uptime)) = fortifier {
                self.fortifier_damage += drain;
                let stolen = drain * steal * uptime;
                self.fortifier_stolen += stolen;
                self.self_overguard = pmin(cap, self.self_overguard + stolen);
            }
            applied += drain;
            left -= fraction;
            if self.pools[0] <= 1e-9 {
                self.pools[0] = 0.0;
                overguard_broke = true;
            }
        }
        if left > 0.0 && self.pools[1] > 0.0 {
            let shield = psum(
                vector
                    .iter()
                    .filter(|(k, _)| !self.shield_bypass.contains(k))
                    .map(|(k, raw)| raw * pmax(0.0, 1.0 + self.faction_delta(*k))),
            ) * self.vulnerability(MAGNETIC);
            let mut s = PySum::new();
            for (k, raw) in vector.iter() {
                if self.shield_bypass.contains(k) {
                    s.add(self.health_value(*k, *raw, t));
                }
            }
            let bypass = s.value();
            let fraction = pmin(
                pmin(left, if shield > 0.0 { self.pools[1] / shield } else { f64::INFINITY }),
                if bypass > 0.0 { self.pools[2] / bypass } else { f64::INFINITY },
            );
            let a = pmin(self.pools[1], shield * fraction);
            let bb = pmin(self.pools[2], bypass * fraction);
            self.pools[1] -= a;
            self.pools[2] -= bb;
            applied += a + bb;
            earlier_hit += a;
            health_hit += bypass * fraction;
            remaining += bb;
            left -= fraction;
            if self.pools[1] <= 1e-9 {
                self.pools[1] = 0.0;
                shield_broke = true;
            }
        }
        if left > 0.0 && self.pools[1] <= 1e-9 && self.pools[2] > 0.0 {
            let mut s = PySum::new();
            for (k, raw) in vector.iter() {
                s.add(self.health_value(*k, *raw, t));
            }
            let value = s.value();
            let mut amount = value * left;
            if shield_broke {
                // WFSim M61 中，破盾命中的溢出伤害有 5% 传入生命值，直接弱点命中绕过护盾门。
                let weak = match weak_kill {
                    WeakKill::Flag(true) => self.head,
                    WeakKill::Flag(false) => 0.0,
                    WeakKill::Value(v) => v,
                };
                amount *= weak + (1.0 - weak) * SHIELD_GATE_LEAK;
            }
            let drain = pmin(self.pools[2], amount);
            self.pools[2] -= drain;
            applied += drain;
            health_hit += amount;
            remaining += drain;
        }
        self.totals[source] += applied;
        if source == DOT && vector.len() == 1 {
            let d = vector[0].0;
            let prev = *self.dot_by_type.get(&d).unwrap_or(&0.0);
            self.dot_by_type.insert(d, prev + applied);
        }
        let weak = match weak_kill {
            WeakKill::Flag(true) => self.head,
            WeakKill::Flag(false) => 0.0,
            WeakKill::Value(v) => v,
        };
        if self.track_weak_kills {
            self.attribution.add(t, earlier_hit, health_hit, cv2, weak, source == DIRECT && !magnetic_packet);
        }
        if let (Some(curve), 0) = (self.life_curve.as_mut(), self.gen) {
            // 单个目标生命内记录时间、生命伤害、方差和来源。
            let hit = health_hit + earlier_hit;
            curve.push((t, hit, pow(hit, 2.0) * cv2, source));
        }
        // 原生 AGC 桥在移除当前目标前发送通知。
        if applied > 0.0 {
            if let Some((hits, critical)) = damage_event {
                self.signal("on-hit", t, hits);
                self.signal("on-crit", t, critical);
            }
        }
        if self.ehp > 0.0 && self.pools[0] <= 1e-7 && self.pools[2] <= 1e-7 {
            if overguard_broke { self.signal("on-overguard-broken", t, 1.0); }
            if shield_broke { self.signal("on-shield-broken", t, 1.0); }
            let resume = self.resume_time(t)?;
            let weak = if self.track_weak_kills && !magnetic_packet {
                self.attribution.crossing(t, remaining, resume)
                    .map_err(|_| invalid_index("list index out of range"))?.0
            } else { 0.0 };
            self.die(t, weak, weapon_source && source != SYNDICATE, weapon_source)?;
            return Ok(true);
        }
        if overguard_broke {
            let killed = self.magnetic_break(t, 0, weapon_source && source != SYNDICATE)?;
            self.signal("on-overguard-broken", t, 1.0);
            if killed { return Ok(true); }
        }
        if shield_broke {
            let killed = self.magnetic_break(t, 1, weapon_source && source != SYNDICATE)?;
            self.signal("on-shield-broken", t, 1.0);
            if killed { return Ok(true); }
        }
        if weapon_source && !self.extra_hits.is_empty() && source == DIRECT && !extra {
            let trigger = psum(vector.iter().map(|x| x.1));
            for i in 0..self.extra_hits.len() {
                let (d, share) = self.extra_hits[i];
                if trigger > 0.0 {
                    let v = vec![(d, trigger * share * self.extra_scale)];
                    if self.take(&v, t, source, weak_kill, cv2, true)? {
                        return Ok(true);
                    }
                }
            }
        }
        Ok(false)
    }

    fn die(&mut self, t: f64, weak_kill: f64, weapon_kill: bool, allow_kill_triggers: bool) -> ReplayResult<()> {
        if let Some(c) = self.cycle {
            if c.charge_on == 2 && (if c.source_mode_charge { self.damage_origin.unwrap_or(self.form) } else { self.form }) == BASE && weapon_kill {
                self.gauge = pmin(c.hits, self.gauge + 1.0); // 对应原生 AddChargeConditions PRIMARY_KILL。
            }
        }
        self.kills += 1;
        self.gen += 1;
        if self.impact_embed && self.waiting_mines != 0.0 {
            // 单目标策略在代次交界处清除伤害场，包括窗口内再无后续跳点的情况。
            for entity in self.embedded_entities.iter_mut() {
                if entity.alive {
                    entity.alive = false;
                    self.stale += 1;
                }
            }
            self.waiting_mines = 0.0;
        }
        if self.first_kill.is_none() {
            self.first_kill = Some(t);
        }
        if let Some(last) = self.last_kill {
            self.min_interval = Some(match self.min_interval {
                None => t - last,
                Some(m) => pmin(m, t - last),
            });
        }
        self.last_kill = Some(t);
        self.pools = self.full;
        self.states.clear();
        for pool in self.pooled.iter_mut() {
            pool.reset();
        }
        self.tau = None;
        self.presence.clear();
        self.attribution.reset();
        self.frozen_until = -1.0;
        for pile in self.proc_piles.values_mut() { pile.reset(); }
        self.blast_procs.clear();
        self.heat_token += 1;
        self.heat_chains.reset();
        self.heat_proc_source = None;
        self.heat_mode_sources = [None; 2];
        for chain in self.heat_sources.iter_mut() { chain.reset(); }
        self.puncture_strip = 0.0;
        if !allow_kill_triggers { return Ok(()); }
        self.signal("on-kill", t, 1.0);
        let slot = self.weapon_slot.to_lowercase();
        if slot.contains("longgun") || slot == "primary" || slot == "rifle" {
            self.signal("on-primary-kill", t, 1.0);
        }
        if slot.contains("melee") {
            self.signal("on-melee-kill", t, 1.0);
            if self.melee_heavy_now && weapon_kill {
                self.signal("on-melee-heavy-kill", t, 1.0);
            }
        }
        if let Some(distance) = self.distance {
            if distance <= 5.0 {
                self.signal("on-close-kill", t, 1.0);
            }
        }
        let mut weak_probability = pmin(1.0, pmax(0.0, weak_kill));
        if let Some((probe, offset, golden, track_kills)) = self.weak_draw {
            if (!probe || track_kills) && weapon_kill && 1e-9 < weak_probability && weak_probability < 1.0 - 1e-9 {
                if probe {
                    self.weak_uncertainty = pmax(self.weak_uncertainty, pmin(weak_probability, 1.0 - weak_probability));
                } else {
                    let u = (offset + self.weak_index * golden) % 1.0;
                    self.weak_index += 1.0;
                    weak_probability = if u < weak_probability { 1.0 } else { 0.0 };
                }
            }
        }
        if weak_probability != 0.0 {
            self.weak_kill_events += weak_probability;
            self.signal("on-headshot-kill", t, weak_probability);
        }
        if let Some(restore) = self.restore.clone() {
            if weapon_kill && self.magazine_form() && (restore.kind == "on-kill" || restore.kind == "on-headshot-kill") {
                let chance = restore.chance * if restore.kind == "on-headshot-kill" { weak_probability } else { 1.0 } * restore.fraction;
                if self.shot_in_progress {
                    self.pending_restore = 1.0 - (1.0 - self.pending_restore) * (1.0 - chance);
                } else {
                    let magazine = { let i = self.model(t)?.0; self.stats[i].magazine };
                    self.ammo += pmax(0.0, magazine - self.ammo) * chance;
                }
            }
        }
        if let Some((_, _, fill)) = self.syndicate {
            if weapon_kill && t >= self.syndicate_ready {
                self.affinity += self.affinity_per_kill;
                if self.affinity >= fill {
                    self.push(t, Kind::Syndicate, None);
                }
            }
        }
        Ok(())
    }

    fn add_procs(&mut self, st: usize, aim: f64, t: f64, key: PacketKey) -> ReplayResult<()> {
        self.add_procs_scaled(st, aim, t, key, 1.0)
    }

    fn add_procs_scaled(&mut self, st: usize, aim: f64, t: f64, key: PacketKey, scale: f64) -> ReplayResult<()> {
        let _timer = crate::counters::Timer::start(crate::counters::ADD_PROCS);
        let data = self.packets[&key].clone();
        if scale == 1.0 {
            return self.add_procs_data(st, aim, t, &data, true);
        }
        let mut scaled = Packet { vector: Vec::new(), procs: data.procs.clone(),
                                  rows: data.rows.clone(), non: data.non.clone() };
        for row in scaled.rows.values_mut() {
            if row.branches.is_empty() {
                row.seed *= scale;
                row.accumulator *= scale;
            } else {
                for branch in &mut row.branches { branch.landing *= scale; }
            }
        }
        for value in scaled.non.values_mut() { *value *= scale; }
        self.add_procs_data(st, aim, t, &scaled, true)
    }

    /// `status_arcanes.encumber_roll` 返回射击触发额外异常的概率及合并种子倍率。
    fn encumber_roll(&self, st: usize, chance: f64, aim: f64) -> (f64, f64) {
        let s = &self.stats[st];
        let pool = &self.status_pool;
        let m = pmax(s.multishot, 0.0);
        let low = m.floor();
        let frac = m - low;
        let mut q = 1.0;
        for (d, p) in &s.independent_proc_chances {
            if pool.contains(d) {
                q *= 1.0 - pmin(pmax(*p, 0.0), 1.0);
            }
        }
        for (d, n) in &s.native_forced_procs {
            if pool.contains(d) {
                q *= 1.0 - pmin(pmax(*n, 0.0), 1.0);
            }
        }
        for (d, _) in &s.core.crit_proc_chances {
            if pool.contains(d) {
                q *= 1.0 - ((1.0 - aim) * tables::crit_proc_probability(&s.core, *d, false)
                    + aim * tables::crit_proc_probability(&s.core, *d, true));
            }
        }
        let forced = pmin(psum(self.forced_procs.iter().filter(|x| pool.contains(&x.0)).map(|x| pmax(x.1, 0.0))), 1.0);
        let sc = if s.damage_types.iter().any(|(k, v)| pool.contains(k) && *v > 0.0) { pmax(s.status_chance, 0.0) } else { 0.0 };
        let mut probability = 0.0;
        let mut seed = 0.0;
        for (pellets, weight) in [(low, 1.0 - frac), (low + 1.0, frac)] {
            if pellets <= 0.0 || weight <= 0.0 {
                continue;
            }
            let (p, merged) = if s.continuous {
                let source = 1.0 - (1.0 - pmin(sc * pellets, 1.0)) * q * (1.0 - forced);
                (chance * source, pellets)
            } else {
                let source = 1.0 - (1.0 - pmin(sc, 1.0)) * q;
                let p = 1.0 - pow(1.0 - chance * source, pellets);
                (1.0 - (1.0 - p) * (1.0 - chance * forced * (1.0 - source)), 1.0)
            };
            probability += weight * p;
            seed += weight * p * merged;
        }
        (probability, if probability != 0.0 { seed / probability } else { 1.0 })
    }

    /// 展平 `_stackable` 规则并按对象缓存。
    fn stackable_profile(&mut self, object: ObjectId) -> ReplayResult<Rc<stackable::Profile>> {
        if let Some(profile) = self.stack_profiles.get(&object) {
            return Ok(profile.clone());
        }
        let profile = Rc::new(self.provider.stackable_profile(object)?);
        self.stack_profiles.insert(object, profile.clone());
        Ok(profile)
    }

    /// 按弹组状态机生成的顺序处理命中和事件，对应 `combat_replay.deliver_projectile`。
    fn apply_stack_actions(&mut self, actions: Vec<stackable::Action>, t: f64) -> ReplayResult<()> {
        for action in actions {
            match action {
                stackable::Action::Hit { part, scale, weight } => {
                    let scale = 1.0 + (scale - 1.0) * self.stats[part].stack_native_fraction;
                    let mut part = part;
                    if scale != 1.0 {
                        part = self.derive(part, 14, scale, |v| {
                            for e in v.core.damage_types.iter_mut() {
                                e.1 *= scale;
                            }
                            v.damage_types = v.core.damage_types.clone();
                            if let Some(dot) = v.core.dot_base_damage {
                                v.core.dot_base_damage = Some(dot * scale);
                            }
                            v.drop_private();
                        });
                    }
                    let part = self.derive(part, 15, weight, |v| {
                        v.multishot = weight;
                        v.core.multishot = weight;
                        v.drop_private();
                    });
                    let area_direct = self.stats[part].core.area_direct;
                    let aim = if area_direct { self.head } else { 0.0 };
                    let key = self.packet(part, aim)?;
                    let packet = self.packets[&key].clone();
                    let cv2 = self.spread(part, aim);
                    let damage_event = if self.stats[part].stackable_hit_event {
                        let puncture = if !self.stats[part].radial_only || self.stats[part].radial_victim_crit { self.live_crit().0 } else { 0.0 };
                        let s = &self.stats[part];
                        let full = (self.pools[1]-self.full[1]).abs()<1e-7 && (self.pools[2]-self.full[2]).abs()<1e-7;
                        let cc = s.crit_chance + if full { s.victim_full_crit_chance } else { 0.0 };
                        let weak = s.weakpoint_crit_chance_bonus + s.base_crit_chance*s.weakpoint_crit_chance_relative;
                        let weights = self.enervate.as_ref().map(|en| en.damage_marginal()).unwrap_or_else(|| vec![(0,1.0)]);
                        let body_multiplier = if s.radial_only { 1.0 } else { s.bodyshot_crit_multiplier };
                        let critical = psum(weights.iter().map(|(stacks,weight)| {
                            let cc = pmax(0.0,cc+*stacks as f64*0.1+puncture);
                            weight*((1.0-aim)*pmin(pmax(cc*body_multiplier,0.0),1.0)
                                +aim*pmin(pmax(cc+weak,0.0),1.0))
                        }));
                        Some((s.multishot,critical*s.multishot))
                    } else { None };
                    self.proc_source = Some(part);
                    if !self.take_with_event(&packet.vector, t, DIRECT, WeakKill::Value(aim), cv2, false, damage_event)? {
                        self.add_procs(part, aim, t, key)?;
                    }
                    self.noncrit_no_proc_hit(part, aim, t);
                    if area_direct {
                        self.signal("on-headshot", t, aim * weight);
                        self.consecutive_heads(aim);
                    }
                    let puncture = if self.stats[part].radial_only { 0.0 } else { self.live_crit().0 };
                    if self.stats[part].stackable_hit_event { self.enervate_hit(part, t, puncture, aim); }
                }
                stackable::Action::Life { time, group, token, action, generation } => {
                    self.push(time, Kind::StackLife(group, token, action), if generation < 0 { None } else { Some(generation) });
                }
                stackable::Action::Child { time, group, multiplier, hits, generation: _ } => {
                    // 子弹体在固定目标位置独立存在，即使母弹体已击杀目标。
                    self.push(time, Kind::StackChild(group, multiplier, hits), None);
                }
            }
        }
        Ok(())
    }

    /// `combat_replay.queue_mines` 在延迟结束时引爆待处理批次。
    fn queue_mines(&mut self, t: f64) {
        let batch = std::mem::take(&mut self.pending_mines);
        self.pending_throws = 0;
        let mines = psum(batch.iter().filter(|m| !m.2).map(|m| self.stats[m.0].multishot));
        self.waiting_mines += mines;
        let delay = self.mines.unwrap().0;
        self.push(t + delay, Kind::Detonation(Rc::new(batch)), None);
    }

    /// `combat_replay.model` 的 Onos 状态包括蓄力标记、暴击命中数和光束持续秒数。
    fn onos_values(&self, t: f64) -> (bool, f64, f64) {
        match self.onos {
            Some((alignment, _, ramp)) => {
                let beam = pmin(ramp, pmax(0.0, t - self.onos_start));
                // 对应 `round(x, 6)`，先正确舍入十进制再读回浮点数。
                let beam: f64 = format!("{:.6}", beam).parse().unwrap();
                (self.onos_ticks as f64 >= alignment, self.onos_hits, beam)
            }
            None => (false, 0.0, 0.0),
        }
    }

    /// 基础形态，或未启用循环且未选灵化形态。
    fn magazine_form(&self) -> bool {
        self.form == BASE || (self.cycle.is_none() && !self.selected_incarnon)
    }

    /// 灵化形态，或已明确选择灵化形态。
    fn incarnon_form(&self) -> bool {
        self.form == INCARNON || self.selected_incarnon
    }

    /// `combat_replay.form_burst` 读取当前形态的连发及其射速。
    fn form_burst(&self) -> (Option<BurstSpec>, Option<f64>) {
        if self.cycle.is_none() {
            (self.burst, self.burst_rate)
        } else if self.form == BASE {
            (self.burst_base, self.burst_rate_base)
        } else {
            (self.burst_incarnon, self.burst_rate_incarnon)
        }
    }

    /// 次要强化赋能在一次武器异常上施加伤害，击杀目标时返回 true。
    fn empowered_hit(&mut self, st: usize, d: D, n: f64, t: f64, weapon_status: bool) -> ReplayResult<bool> {
        let Some((damage, uptime)) = self.empowered else { return Ok(false) };
        if !weapon_status || !self.status_pool.contains(&d) {
            return Ok(false);
        }
        let n = n * uptime;
        if n <= 0.0 {
            return Ok(false);
        }
        let core = &self.stats[st].core;
        let faction = core.faction_all * core.faction_target * core.faction_bracket;
        self.empowered_count += n;
        let before = self.totals[ARCANE];
        let previous_source = self.proc_source;
        self.proc_source = Some(st);
        let died = self.take(&vec![(d, n * damage * faction)], t, ARCANE, WeakKill::Flag(false), 0.0, false)?;
        self.proc_source = previous_source;
        // 按参考实现从左到右计算 `get(d, 0.0) + totals['arcane'] - before`。
        let prev = *self.empowered_by_type.get(&d).unwrap_or(&0.0);
        self.empowered_by_type.insert(d, prev + self.totals[ARCANE] - before);
        Ok(died)
    }

    fn pile_outcomes(&self, st: usize, row: &Row, expected_count: f64) -> Rc<[Rc<PileKernel>]> {
        let s = &self.stats[st];
        let pellets = pmax(s.multishot, 0.0);
        if pellets <= 0.0 { return Rc::from([]); }
        let instances = if s.continuous { 1.0 } else { pellets };
        let key = (instances.to_bits(), expected_count.to_bits());
        if let Some((cached_key, kernels)) = row.kernels.get() {
            if *cached_key == key { return kernels.clone(); }
        }
        // 仅计时实际构建，缓存命中跳过计时。
        let _timer = crate::counters::Timer::start(crate::counters::PILE_BUILD);
        let branches = &row.branches;
        let declared = psum(branches.iter().map(|b| b.weight*b.procs));
        if declared <= 0.0 || expected_count <= 0.0 { return Rc::from([]); }
        let scale = expected_count / declared;
        let mut outcomes = Vec::new();
        for b in branches {
            let n = b.procs / instances * scale;
            let mut a = pow(b.absence,scale);
            if a >= 1.0 && n > 0.0 { a = 1.0 - pmin(n,1.0); }
            let cap = ceil_i(n / pmax(1.0-a,1e-300)).max(1) as usize;
            match arrival_kernel(a,n,cap) {
                None => outcomes.push(PileOutcome { probability:b.weight,count:0,seed:0.0,accumulator:0.0,landing:1.0,weak:false }),
                Some(kernel) => for (j,q) in kernel {
                    outcomes.push(PileOutcome { probability:b.weight*q,count:j,seed:b.seed,accumulator:b.accumulator,landing:b.landing,weak:b.weak });
                }
            }
        }
        let whole = floor_i(instances) as usize;
        let kernel = Rc::new(PileKernel::new(&outcomes));
        let mut result = vec![kernel; whole];
        let fraction = instances - whole as f64;
        if fraction > 0.0 {
            let mut last = vec![PileOutcome { probability:1.0-fraction,count:0,seed:0.0,accumulator:0.0,landing:1.0,weak:false }];
            last.extend(outcomes.iter().copied().map(|mut o| { o.probability *= fraction; o }));
            result.push(Rc::new(PileKernel::new(&last)));
        }
        let result: Rc<[Rc<PileKernel>]> = Rc::from(result);
        if row.kernels.get().is_none() {
            let _ = row.kernels.set((key, result.clone()));
        }
        result
    }

    fn add_pile(&mut self, d:D, t:f64, duration:f64, outcomes:&[Rc<PileKernel>], source:usize) {
        for outcome in outcomes {
            let schedule = self.proc_piles.entry(d).or_insert_with(||ProcPile::new(if d==GAS {Some(10)} else {None}))
                .add(t,duration,outcome.clone(),Some(self.damage_origin.unwrap_or(self.form)),Some(source));
            if let Some((when,token))=schedule { self.push(when,Kind::GroupedDot(d,token),Some(self.gen)); }
        }
    }

    /// `add_procs` 处理一个伤害包的异常。次要低温等派生异常的 `weapon` 为 false，避免重复触发武器回调。
    fn add_procs_data(&mut self, st: usize, aim: f64, t: f64, data: &Packet, weapon: bool) -> ReplayResult<()> {
        let generation = self.gen;
        if self.puncture_armor_strip != 0.0 && !self.stats[st].radial_only && weapon {
            let s = &self.stats[st];
            let forced = s.native_forced_procs.iter().find(|x| x.0 == PUNCTURE).map(|x| x.1).unwrap_or(0.0)
                + self.forced_procs.iter().find(|x| x.0 == PUNCTURE).map(|x| x.1).unwrap_or(0.0);
            let probability = if forced != 0.0 {
                1.0
            } else {
                let status = pmax(0.0, s.status_chance);
                let expected = tables::expected_status_procs(&s.core, &Vec::new(), false);
                let puncture = expected.iter().find(|x| x.0 == PUNCTURE).map(|x| x.1).unwrap_or(0.0);
                let share = pmin(1.0, pmax(0.0, puncture / pmax(status * s.multishot, 1e-9)));
                let whole = status.floor();
                1.0 - pow(1.0 - share, whole) * (1.0 - (status - whole) * share)
            };
            let bullets = if s.continuous { 1.0 } else { s.multishot };
            self.puncture_strip = pmin(1.0, self.puncture_strip + self.puncture_armor_strip * bullets * probability);
        }
        if let Some((chance, uptime)) = self.encumber {
            if weapon && self.status_rate_multiplier > 0.0 {
                if (t - self.encumber_time).abs() > 1e-9 {
                    self.encumber_time = t;
                    self.encumber_remaining = 1.0;
                }
                let (probability, merged_seed) = self.encumber_roll(st, chance, aim);
                let weight = self.encumber_remaining * probability * uptime;
                self.encumber_remaining *= 1.0 - probability * uptime;
                if weight > 0.0 {
                    self.encumber_expected += weight;
                    // 从状态池中随机施加一项异常，种子来自对池内每种异常各施加一次的探测。
                    let pool = self.status_pool.clone();
                    let probe = self.derive(st, 8, 0.0, |v| {
                        v.status_chance = 0.0;
                        v.core.status_chance = 0.0;
                        v.continuous = false;
                        v.core.continuous = false;
                        v.multishot = 1.0;
                        v.core.multishot = 1.0;
                        v.independent_proc_chances = pool.iter().map(|&d| (d, 1.0)).collect();
                        v.core.independent_proc_chances = v.independent_proc_chances.clone();
                        v.core.impact_proc_conversions = Vec::new();
                        v.core.crit_proc_chances = Vec::new();
                        v.has_crit_procs = false;
                        v.drop_private();
                    });
                    let key = self.packet(probe, aim)?;
                    let probed = self.packets[&key].clone();
                    let mut rows = probed.rows.clone();
                    for r in rows.values_mut() {
                        r.seed *= merged_seed;
                        for branch in &mut r.branches { branch.seed *= merged_seed; }
                    }
                    let mut non = probed.non.clone();
                    for v in non.values_mut() {
                        *v *= merged_seed;
                    }
                    let share = weight / pool.len() as f64;
                    let derived = Packet { vector: Vec::new(), procs: pool.iter().map(|&d| (d, share)).collect(), rows, non };
                    self.add_procs_data(st, aim, t, &derived, false)?;
                }
            }
        }
        if self.gen != generation {
            return Ok(()); // 派生异常已击杀原目标。
        }
        let status_view = self.tau_view(st);
        let sorted;
        let procs: &Vector = if self.empowered.is_some() {
            // 按参考实现的类型名称排序，保证分组顺序稳定。
            let mut copy = data.procs.clone();
            let types = &self.types;
            copy.sort_by(|a, b| types.name(a.0).cmp(types.name(b.0)));
            sorted = copy;
            &sorted
        } else {
            &data.procs
        };
        for &(d, n0) in procs.iter() {
            let scale = self.stats[st].core.duration_scale(d);
            let n = n0 * pmax(self.status_rate_multiplier, 0.0);
            if n <= 0.0 || (d == FREEZE && self.frozen_until > t) || (self.blocks_radiation && d == RADIATION) {
                continue;
            }
            let prev = *self.proc_counts.get(&d).unwrap_or(&0.0);
            self.proc_counts.insert(d, prev + n);
            let name = self.proc_signal(d);
            self.signal(&name, t, n);
            self.signal("on-proc", t, n);
            if scale <= 0.0 {
                if self.empowered_hit(st, d, n, t, weapon)? {
                    return Ok(());
                }
                continue;
            }
            let row = data.rows.get(&d);
            let duration = match &row {
                Some(r) => r.duration,
                None => duration_of(d).unwrap_or(6.0) * scale,
            };
            if duration <= 0.0 {
                if self.empowered_hit(st, d, n, t, weapon)? {
                    return Ok(());
                }
                continue;
            }
            let _ns = crate::counters::Timer::start(crate::counters::NOSTATUS);
            let mut absence = pow(self.no_status_probability(status_view, d, aim)?, pmax(self.status_rate_multiplier, 0.0));
            drop(_ns);
            if absence >= 1.0 && n > 0.0 {
                absence = 1.0 - pmin(n, 1.0);
            }
            if !weapon {
                absence = pmax(absence, 1.0 - pmin(n, 1.0));
            }
            let pile_events = if d == GAS || d == ELECTRICITY {
                row.as_ref().map(|r| self.pile_outcomes(st, r, n))
            } else { None };
            if let Some(ref events) = pile_events {
                absence = events.iter().map(|outcomes|
                    psum(outcomes.rows.iter().filter(|r| r.count == 0).map(|r| r.values[0])))
                    .fold(1.0, |a, b| a * b);
            }
            { let _t = crate::counters::Timer::start(crate::counters::PRESENCE); self.presence.entry(d).or_default().add(t + duration, absence); }
            if d == FIRE && row.is_some() {
                let r = row.unwrap();
                if !self.heat_chains.active() {
                    self.heat_token += 1;
                    let token = self.heat_token;
                    self.push(t + 1.0, Kind::Heat(token), Some(self.gen));
                }
                { let _t = crate::counters::Timer::start(crate::counters::HEAT); self.heat_chains.add(t, absence, n, n * r.seed, duration, scale); }
                self.heat_acc = r.accumulator;
                self.heat_proc_source = Some(st);
                if self.cycle.map(|c|c.source_mode_charge).unwrap_or(false) {
                    let own=if self.form==BASE {0} else {1};
                    for i in 0..2 {
                        self.heat_sources[i].add_source(t,absence,if i==own {n} else {0.0},if i==own {n*r.seed} else {0.0},duration,scale,i!=own);
                    }
                    self.heat_source_acc[own]=r.accumulator;
                    self.heat_mode_sources[own]=Some(st);
                }
            } else if (d == GAS || d == ELECTRICITY) && row.is_some() {
                self.add_pile(d,t,duration,&pile_events.unwrap(),st);
            } else if d == TAU {
                self.tau.get_or_insert_with(|| CappedPool::new(10.0)).add(t + duration, absence, n);
            } else if let Some(p) = pooled_index(d) {
                { let _t = crate::counters::Timer::start(crate::counters::POOLED); self.pooled[p].add(t + duration, absence, n); }
            } else {
                self.states.entry(d).or_default().push_back((t + duration, n));
                if let Some(c) = cap_of(d) {
                    if d != GAS {
                        let cap = if d == FREEZE && self.pools[0] > 0.0 { 4.0 } else { c };
                        let rows = self.states.get_mut(&d).unwrap();
                        let mut excess = psum(rows.iter().map(|x| x.1)) - cap;
                        while excess > 1e-9 && !rows.is_empty() {
                            let (end, number) = rows.pop_front().unwrap();
                            let remove = pmin(number, excess);
                            if number > remove {
                                rows.push_front((end, number - remove));
                            }
                            excess -= remove;
                        }
                    }
                }
                if d == FREEZE && self.count(d) >= 10.0 - 1e-9 && self.pools[0] <= 0.0 {
                    self.frozen_until = t + 3.0 * scale;
                    self.frozen_scale = scale;
                }
                if let Some(r) = row {
                    let first_tick = 1;
                    let end_tick = floor_i(duration + 1e-9) + 1;
                    let mut per = r.seed;
                    if !r.shared {
                        per += r.coefficient;
                    }
                    self.dots.push(DotInstance { dtype: d, per, count: n, end: t + duration, dist: None });
                    self.dot_modes.push(self.form);
                    self.dot_sources.push(st);
                    let instance = self.dots.len() - 1;
                    // 给所有跳点预留与一次性入堆相同的序号，每次弹出上一项后只推入下一项，保持弹出顺序并控制堆的大小。
                    let mut count = 0;
                    for j in first_tick..end_tick {
                        if t + (j as f64) < self.window + 1e-9 {
                            count += 1;
                        } else {
                            break;
                        }
                    }
                    if count > 0 {
                        let base = self.seq;
                        self.seq += count as u64;
                        self.heap.push(Event { t: pmax(0.0, t + first_tick as f64), seq: base + 1,
                                               kind: Kind::IndependentDot(instance), gen: Some(self.gen) });
                        if count > 1 {
                            self.tick_chains.insert(instance, TickChain { t, first: first_tick, count, base, next: 1,
                                                                          gen: Some(self.gen) });
                        }
                    }
                } else if let Some(&per) = data.non.get(&d) {
                    self.states.get_mut(&d).unwrap().pop_back();
                    // WFSim M108 中，叠至十层时引爆，导致满层的射击所剩部分按参考顺序舍弃。
                    let room = pmax(0.0, 10.0 - psum(self.blast_procs.iter().map(|&i| self.blasts[i].count)));
                    let kept = pmin(n, room);
                    if kept > 0.0 {
                        let hit_probability = pmin(kept, 1.0 - self.no_status_probability(st, d, aim)?);
                        self.blasts.push(Blast { count: kept, per, end: t + 1.5 * scale, hit_probability, origin_mode:self.form, proc_source: st });
                        let blast = self.blasts.len() - 1;
                        self.blast_procs.push_back(blast);
                        let end = self.blasts[blast].end;
                        self.push(end, Kind::Blast(blast), Some(self.gen));
                        if psum(self.blast_procs.iter().map(|&i| self.blasts[i].count)) >= 10.0 {
                            self.push(t, Kind::BlastAll, Some(self.gen));
                        }
                    }
                }
            }
            if self.empowered_hit(st, d, n, t, weapon)? {
                return Ok(());
            }
        }
        // 次要低温的穿刺转冰冷沿用普通异常流程，派生施加不会再次触发自身。
        if let Some((cold_stacks, uptime)) = self.cryogenic {
            let puncture = data.procs.iter().find(|x| x.0 == PUNCTURE).map(|x| x.1).unwrap_or(0.0);
            let extra_cold = pmax(puncture, 0.0) * cold_stacks * uptime;
            if extra_cold > 0.0 {
                let before = *self.proc_counts.get(&FREEZE).unwrap_or(&0.0);
                let derived = Packet { vector: Vec::new(), procs: vec![(FREEZE, extra_cold)],
                                       rows: FxIndexMap::default(), non: FxIndexMap::default() };
                self.add_procs_data(st, aim, t, &derived, false)?;
                self.cryogenic_expected += *self.proc_counts.get(&FREEZE).unwrap_or(&0.0) - before;
            }
        }
        Ok(())
    }

    fn radial_hit(&mut self,part:usize,sniper_factor:f64,t:f64,notify:bool)->ReplayResult<()> {
        self.radial_hit_at(part,sniper_factor,t,notify,None,0.0)
    }
    fn radial_hit_at(&mut self,part:usize,sniper_factor:f64,t:f64,notify:bool,weight:Option<f64>,aim:f64)->ReplayResult<()> {
        let part = if let Some(hook) = self.provider.refresh_hook(self.stats[part].obj, false)? {
            let stacks = self.source_stacks();
            let live = self.live_proc_types(t);
            self.refresh_view(hook, stacks, live, None)?
        } else if let Some(unit) = self.stats[part].live_co_unit {
            let live = self.live_proc_types(t);
            self.vary(unit, live, 1.0)
                .ok_or_else(|| invalid_value("live radial CO template missing"))?
        } else { part };
        let part=if let Some(ms)=weight {self.derive(part,34,ms,|v| {v.multishot=ms;v.core.multishot=ms;})}else{part};
        let admitted=self.stats[part].delayed_radial_hit_event || (weight.is_some() && !self.stats[part].radial_only);
        if notify && admitted {
            self.snapshot_victim_procs(t);
        }
        let rkey = self.packet(part, aim)?;
        let rvector: Vector = if sniper_factor == 1.0 {
            self.packets[&rkey].vector.clone()
        } else {
            self.packets[&rkey].vector.iter().map(|(k, v)| (*k, v * sniper_factor)).collect()
        };
        let cv2 = self.spread(part, aim);
        let damage_event = if notify && admitted {
            let puncture = if !self.stats[part].radial_only || self.stats[part].radial_victim_crit { self.live_crit().0 } else { 0.0 };
            let s = &self.stats[part];
            let full = (self.pools[1]-self.full[1]).abs()<1e-7 && (self.pools[2]-self.full[2]).abs()<1e-7;
            let cc = s.crit_chance + puncture + if full { s.victim_full_crit_chance } else { 0.0 };
            let weights = if s.snapshot_enervate { vec![(0,1.0)] } else {
                self.enervate.as_ref().map(|en| en.damage_marginal()).unwrap_or_else(|| vec![(0,1.0)])
            };
            let weak=s.weakpoint_crit_chance_bonus+s.base_crit_chance*s.weakpoint_crit_chance_relative;
            let critical=psum(weights.iter().map(|(stacks,w)| w*((1.0-aim)*pmin(pmax((cc+*stacks as f64*0.1)*if s.radial_only {1.0}else{s.bodyshot_crit_multiplier},0.0),1.0)
                +aim*pmin(pmax(cc+*stacks as f64*0.1+weak,0.0),1.0))));
            Some((s.multishot, critical*s.multishot))
        } else { None };
        self.proc_source = Some(part);
        if !self.take_with_event(&rvector, t, DIRECT, WeakKill::Value(aim), cv2, false, damage_event)? {
            self.add_procs_scaled(part, aim, t, rkey, sniper_factor)?;
        }
        self.noncrit_no_proc_hit(part, aim, t);
        Ok(())
    }

    fn noncrit_no_proc_hit(&mut self, st: usize, aim: f64, t: f64) {
        if !self.sources.iter().any(|q| q.kind.as_deref() == Some("on-noncrit-no-proc")) {
            return;
        }
        let puncture = if self.stats[st].radial_only && !self.stats[st].radial_victim_crit { 0.0 } else { self.live_crit().0 };
        let s = &self.stats[st];
        let weak = s.weakpoint_crit_chance_bonus + s.base_crit_chance * s.weakpoint_crit_chance_relative;
        let marginal = if s.snapshot_enervate { vec![(0,1.0)] } else {
            self.enervate.as_ref().map(|en| en.damage_marginal()).unwrap_or_else(|| vec![(0,1.0)])
        };
        let mut noncrit = 0.0;
        for (stacks, weight) in marginal {
            let cc = pmax(0.0, s.crit_chance + stacks as f64 * 0.1 + puncture);
            noncrit += weight * ((1.0 - aim) * pmax(0.0, 1.0 - cc * s.bodyshot_crit_multiplier) + aim * pmax(0.0, 1.0 - cc - weak));
        }
        let mut no_proc = pmax(0.0, 1.0 - s.status_chance);
        for (_, chance) in &s.independent_proc_chances {
            no_proc *= pmax(0.0, 1.0 - chance);
        }
        for (_, c) in &s.native_forced_procs {
            no_proc *= pmax(0.0, 1.0 - c);
        }
        for (_, c) in &self.forced_procs {
            no_proc *= pmax(0.0, 1.0 - c);
        }
        let instances = if s.continuous { 1.0 } else { s.multishot };
        self.signal("on-noncrit-no-proc", t, instances * noncrit * no_proc);
    }

    fn beam_tick(&mut self, time: f64, interval: f64, floor: f64) -> f64 {
        if let Some(last) = self.beam_last {
            let idle = time - last - interval;
            if idle > 0.8 {
                self.beam_progress = pmax(0.0, self.beam_progress - (idle - 0.8) / 2.0);
            }
        }
        let value = floor + (1.0 - floor) * self.beam_progress;
        self.beam_progress = pmin(1.0, self.beam_progress + interval / 0.6);
        self.beam_last = Some(time);
        value
    }
}

#[derive(Clone, Copy)]
pub struct SpoolSpec {
    pub maximum: f64,
    pub initial: f64,
    pub rate_start: f64,
    pub rate_shots: f64,
    pub ammo_divisor: f64,
    pub spindown: f64,
}

#[derive(Clone, Copy)]
pub enum RateSpoolSpec {
    Reference { start: f64, end: f64, over: f64 },
    Native { start: f32, end: f32, maximum: u32, source_rpm: f64, source_rate: f64 },
}

impl RateSpoolSpec {
    fn refire(self, rate: f64, cycles: i64) -> (f64, Option<f64>) {
        match self {
            Self::Reference { start, end, over } => (rate * (start + (end - start) * pmin(cycles as f64 / over, 1.0)), None),
            Self::Native { start, end, maximum, source_rpm, source_rate } => {
                // 每次自动射击循环在 REFIRE_WAIT 前推进两次计数。
                let count = ((cycles + 1) * 2).min(i64::from(maximum));
                let rps = (rate * source_rpm / source_rate) as f32 * (1.0_f32 / 60.0_f32);
                let progress = count as f32 / maximum as f32;
                let refire = rps * (start + (end - start) * progress);
                (f64::from(refire), Some(f64::from(1.0_f32 / refire)))
            }
        }
    }
}

#[derive(Clone)]
pub struct ShotCombo {
    pub incarnon: bool,
    pub min: f64,
    pub seconds: f64,
    pub tier_bonus: f64,
    pub tier_factor: f64,
    count: f64,
    last_hit: f64,
}

impl ShotCombo {
    pub fn new(incarnon: bool, min: f64, seconds: f64, tier_bonus: f64, tier_factor: f64) -> Self {
        Self { incarnon, min, seconds, tier_bonus, tier_factor, count: 0.0, last_hit: 0.0 }
    }
}

impl ShotCombo {
    fn multiplier(&self, hits: f64) -> f64 {
        let hits = (hits + 1e-9).floor();
        if self.min <= 0.0 || hits < self.min {
            return 1.0;
        }
        let mut threshold = self.min;
        let mut value = 1.0 + self.tier_bonus;
        while hits >= threshold * self.tier_factor {
            threshold *= self.tier_factor;
            value += self.tier_bonus;
        }
        value
    }
    fn at(&self, t: f64) -> f64 {
        if self.seconds <= 0.0 {
            return self.count;
        }
        let lost = ((t - self.last_hit) / self.seconds + 1e-9).floor();
        if lost > 0.0 { pmax(0.0, self.count - lost) } else { self.count }
    }
    fn shot(&mut self, t: f64, pellets: f64) -> f64 {
        let now = self.at(t);
        let pellets = pmax(pellets, 0.0);
        let whole = pellets.floor();
        let part = pellets - whole;
        // 先计入命中，再结算本次连击倍率（sniper.py，WFSim M118）。
        let mut sum = PySum::new();
        let mut i = 0.0;
        while i < whole {
            sum.add(self.multiplier(now + i + 1.0));
            i += 1.0;
        }
        let mut total = sum.value();
        total += part * self.multiplier(now + whole + 1.0);
        let factor = if pellets > 0.0 { total / pellets } else { self.multiplier(now) };
        if pellets > 0.0 {
            self.count = now + pellets;
            self.last_hit = t;
        }
        factor
    }
}

#[derive(Clone)]
pub struct Restore {
    pub kind: String,
    pub chance: f64,
    pub fraction: f64,
    pub amount: f64,
    pub once_per_shot: bool,
    pub victim_proc_type: Option<D>,
}

/// `apply_hit` 读取的一次近战命中的参数。
pub struct HitSpec {
    pub atten: f64,
    pub slam: bool,
    pub combo_multiplier: bool,
    pub pct: Vector,
    pub target: Option<D>,
    pub crit_chance: Option<f64>,
    pub crit_multiplier: Option<f64>,
    pub can_crit: bool,
    pub extra_procs: Vec<D>,
    pub proc_atten: f64,
    pub radial: bool,
    pub duplicate: Option<(f64, f64)>,
    // 战刃的投掷本体、挥发弹跳，以及异况超量无效的爆炸段。
    pub throw: bool,
    pub bounce: bool,
    pub no_co: bool,
}

/// `enervate.EnervateState` 按插入顺序保存层数与重置后的高阶暴击数的联合分布。
#[derive(Clone)]
struct Enervate {
    reset_after: i64,
    min_interval: f64,
    fixed: bool,
    uptime: f64,
    ignore_selected: bool,
    last: f64,
    hits: f64,
    resets: f64,
    states: Vec<((i64, i64), f64)>,
    timed_states: Option<Vec<((i64, i64, u64), f64)>>,
}

fn ordered_add<K: PartialEq + Copy>(list: &mut Vec<(K, f64)>, key: K, value: f64) {
    match list.iter_mut().find(|e| e.0 == key) {
        Some(e) => e.1 += value,
        None => list.push((key, 0.0 + value)),
    }
}

fn indexed_add<K: Eq + std::hash::Hash + Copy>(list: &mut Vec<(K, f64)>, index: &mut FxHashMap<K, usize>, key: K, value: f64) {
    if let Some(&at) = index.get(&key) {
        list[at].1 += value;
    } else {
        index.insert(key, list.len());
        list.push((key, 0.0 + value));
    }
}

impl Enervate {
    fn marginal(&self) -> Vec<(i64, f64)> {
        let mut out = Vec::new();
        for &((s, _), p) in &self.states {
            ordered_add(&mut out, s, p);
        }
        out
    }

    fn damage_marginal(&self) -> Vec<(i64, f64)> {
        let mut weights: Vec<(i64, f64)> = self.marginal().into_iter().map(|(s, p)| (s, p * self.uptime)).collect();
        match weights.iter_mut().find(|e| e.0 == 0) {
            Some(e) => e.1 = e.1 + 1.0 - self.uptime,
            None => weights.push((0, 1.0 - self.uptime)),
        }
        weights.retain(|e| e.1 > 0.0);
        weights
    }

    fn advance(&mut self, t: f64, probabilities: Option<&[(i64, f64)]>) {
        self.advance_probability(t, probabilities, 1.0);
    }

    fn advance_probability(&mut self, t: f64, probabilities: Option<&[(i64, f64)]>, event_probability: f64) {
        let event_probability = pmin(1.0, pmax(0.0, event_probability));
        if event_probability == 0.0 { return; }
        if event_probability < 1.0 || self.timed_states.is_some() {
            self.advance_probabilistic(t, probabilities, event_probability);
            return;
        }
        if self.fixed || self.uptime == 0.0 || t - self.last < self.min_interval - 1e-9 {
            return;
        }
        self.last = t;
        self.hits += 1.0;
        let mut following: Vec<((i64, i64), f64)> = Vec::new();
        let mut index = FxHashMap::default();
        for &((s, b), mass) in &self.states {
            let given = probabilities.and_then(|ps| ps.iter().find(|e| e.0 == s).map(|e| e.1)).unwrap_or(0.0);
            let prob = pmin(1.0, pmax(0.0, given));
            indexed_add(&mut following, &mut index, (s + 1, b), mass * (1.0 - prob));
            if b + 1 >= self.reset_after {
                indexed_add(&mut following, &mut index, (0, 0), mass * prob);
                self.resets += mass * prob;
            } else {
                indexed_add(&mut following, &mut index, (s + 1, b + 1), mass * prob);
            }
        }
        following.retain(|e| e.1 > 0.0);
        self.states = following;
    }

    fn advance_probabilistic(&mut self, t: f64, probabilities: Option<&[(i64, f64)]>, probability: f64) {
        if self.fixed || self.uptime == 0.0 { return; }
        let states = self.timed_states.take().unwrap_or_else(||
            self.states.iter().map(|&((s,b),p)| ((s,b,self.last.to_bits()),p)).collect());
        let mut following = Vec::new();
        let mut index = FxHashMap::default();
        for ((s,b,last),mass) in states {
            if t-f64::from_bits(last) < self.min_interval-1e-9 {
                indexed_add(&mut following,&mut index,(s,b,last),mass);
                continue;
            }
            indexed_add(&mut following,&mut index,(s,b,f64::NEG_INFINITY.to_bits()),mass*(1.0-probability));
            let accepted = mass*probability;
            self.hits += accepted;
            let given = probabilities.and_then(|ps| ps.iter().find(|e| e.0==s).map(|e|e.1)).unwrap_or(0.0);
            let prob = pmin(1.0,pmax(0.0,given));
            indexed_add(&mut following,&mut index,(s+1,b,t.to_bits()),accepted*(1.0-prob));
            if b+1>=self.reset_after {
                indexed_add(&mut following,&mut index,(0,0,t.to_bits()),accepted*prob);
                self.resets += accepted*prob;
            } else {
                indexed_add(&mut following,&mut index,(s+1,b+1,t.to_bits()),accepted*prob);
            }
        }
        following.retain(|e|e.1>0.0);
        let mut totals = Vec::new();
        let mut total_index = FxHashMap::default();
        self.last = f64::NEG_INFINITY;
        for &((s,b,last),mass) in &following {
            indexed_add(&mut totals,&mut total_index,(s,b),mass);
            self.last = self.last.max(f64::from_bits(last));
        }
        self.states = totals;
        self.timed_states = Some(following);
    }
}

/// `enervate.big_crit_probability` 计算可提升一级时至少达到二阶暴击的概率。
fn big_crit_probability(chance: f64, promotion: f64) -> f64 {
    let chance = pmax(0.0, chance);
    let promotion = pmin(1.0, pmax(0.0, promotion));
    pmin(1.0, pmax(0.0, chance - 1.0)) + pmin(pmin(1.0, pmax(0.0, chance)), pmax(0.0, 2.0 - chance)) * promotion
}

/// `enervate.any_probability` 处理整数颗弹丸及一颗按概率产生的弹丸。
fn any_probability(p: f64, pellets: f64) -> f64 {
    let n = pmax(0.0, pellets);
    let whole = n.trunc();
    1.0 - pow(1.0 - p, whole) * (1.0 - (n - whole) * p)
}

#[derive(Clone)]
pub struct MeleeSpec {
    pub base_hits: f64,
    pub tier_step: f64,
    pub cap: f64,
    pub base_multiplier: f64,
    pub recovery_per_second: f64,
    pub tier_keys: Vec<(ObjectId, String, f64)>,
    pub segments: Vec<MeleeSegment>,
}

/// 近战路线的一段，先用对应的蓄力或飞行速度缩放固定时长，再用攻速缩放动画时长，最后按需结算一次命中。
#[derive(Clone)]
pub struct MeleeSegment {
    pub fixed: f64,
    pub anim: f64,
    pub charge_scaled: bool,
    pub flight_scaled: bool,
    pub hit: Option<MeleeHit>,
}

#[derive(Clone, Copy)]
pub struct MeleeHit {
    pub id: i64,
    pub heavy: bool,
    pub combo_hits: f64,
    pub consumes: bool,
    // 位 1 标记震地开始，位 2 标记震地结束。
    pub phase: u8,
}

impl MeleeSpec {
    fn multiplier(&self, count: f64) -> f64 {
        let count = pmin(((pmax(count, 0.0) as f32) + 0.5_f32).floor() as f64, self.cap);
        if count < self.base_hits {
            return 1.0;
        }
        self.base_multiplier + 1.0 + floor_i((count - self.base_hits) / self.tier_step + 1e-9) as f64
    }
    fn tier(&self, count: f64) -> i64 {
        (self.multiplier(count) - 1.0).round_ties_even() as i64
    }
}

#[derive(Clone, Copy)]
enum WeakKill {
    Flag(bool),
    Value(f64),
}

fn armour_reduction(armour: f64) -> f64 {
    if armour <= 0.0 {
        return 0.0;
    }
    let effective = pmin(armour, 2700.0);
    0.9 * (effective / 2700.0).sqrt()
}

impl Replay<'_> {
    pub fn run(&mut self) -> ReplayResult<()> {
        let mut first_round = true;
        let mut volley_pending: Option<f64> = None;
        let mut burst_round: i64 = 0;
        let mut burst_size: i64 = 1;
        let mut burst_head_probability = 1.0;
        let mut last_fire_time = f64::NEG_INFINITY;
        let mut spool_shots: i64 = 0;
        let mut rate_spool_shots: i64 = 0;
        let mut rate_spool_due = 0.0;
        let mut events: u64 = 0;
        while let Some(event) = self.heap.pop() {
            events += 1;
            if events > REPLAY_EVENT_LIMIT {
                return Err(ReplayError::Runaway(format!("replay exceeded {REPLAY_EVENT_LIMIT} events")));
            }
            if let Kind::IndependentDot(i) = &event.kind {
                self.next_tick(*i);
            }
            let t = event.t;
            if t < self.current_time - 1e-9 {
                // 负时长导致时钟倒退会重复回放窗口，沿用参考实现的错误分支。
                return Err(unsupported("replay-clock-went-backwards"));
            }
            self.current_time = t;
            self.proc_source = None;
            self.melee_heavy_now = false;
            if self.melee_pending.is_some() {
                self.advance_melee_recovery(t,self.melee_recovery_initial,None);
            }
            self.clean(t);
            if let Some(g) = event.gen {
                if g != self.gen {
                    continue;
                }
            }
            match event.kind {
                Kind::MeleeBuffBoundary => { self.melee_buff_boundary=f64::INFINITY; }
                Kind::MeleeRecovery(token,snap) => {
                    if token != self.melee_recovery_token { continue; }
                    let active=self.model(t)?.0;
                    let initial=pmin((pmax(0.0,self.stats[active].initial_combo) as f32).trunc() as f64,self.melee.as_ref().unwrap().cap);
                    self.advance_melee_recovery(t,initial,Some(snap));
                    self.schedule_melee_recovery(t,initial);
                }
                Kind::Energy(i) => {
                    let q = &mut self.sources[i];
                    q.stacks = pmin(q.max_stacks, q.stacks + q.per_event);
                    q.expires = if q.duration != 0.0 { t + q.duration } else { f64::INFINITY };
                }
                Kind::Heat(token) => {
                    if self.heat_chains.active() && token == self.heat_token {
                        let generation = self.gen;
                        let value = self.heat_chains.seed() + self.heat_acc * self.heat_chains.alive();
                        let cv2 = 1.0 / pmax(self.heat_chains.count(), 1e-9);
                        if self.cycle.map(|c|c.source_mode_charge).unwrap_or(false) {
                            for i in 0..2 {
                                self.heat_sources[i].expire(t);
                                let amount=self.heat_sources[i].seed()+self.heat_source_acc[i]*self.heat_sources[i].alive();
                                if amount>0.0 {
                                    self.damage_origin=Some(if i==0 {BASE} else {INCARNON});
                                    let variance=1.0/pmax(self.heat_sources[i].count(),1e-9);
                                    self.proc_source = self.heat_mode_sources[i];
                                    self.take(&vec![(FIRE,amount)],t,DOT,WeakKill::Flag(false),variance,false)?;
                                    self.damage_origin=None;
                                }
                                if self.gen!=generation {break;}
                            }
                        } else {
                            self.proc_source = self.heat_proc_source;
                            self.take(&vec![(FIRE, value)], t, DOT, WeakKill::Flag(false), cv2, false)?;
                        }
                        if self.gen == generation {
                            self.push(t + 1.0, Kind::Heat(token), Some(self.gen));
                        }
                    }
                }
                Kind::Blast(_) | Kind::BlastAll => {
                    let affected: Vec<usize> = match event.kind {
                        Kind::BlastAll => self.blast_procs.iter().copied().collect(),
                        Kind::Blast(i) => vec![i],
                        _ => unreachable!(),
                    };
                    let value = psum(affected.iter().map(|&i| self.blasts[i].count * self.blasts[i].per));
                    let procs = psum(affected.iter().map(|&i| self.blasts[i].count));
                    let origin=if self.cycle.map(|c|c.source_mode_charge).unwrap_or(false) {
                        affected.last().map(|&i|self.blasts[i].origin_mode)
                    } else {None};
                    for &i in &affected {
                        self.blasts[i].count = 0.0;
                    }
                    while let Some(&front) = self.blast_procs.front() {
                        if self.blasts[front].count == 0.0 {
                            self.blast_procs.pop_front();
                        } else {
                            break;
                        }
                    }
                    if value != 0.0 {
                        self.damage_origin=origin;
                        self.proc_source = affected.last().map(|&i| self.blasts[i].proc_source);
                        self.take(&vec![(EXPLOSION, value)], t, NONDOT, WeakKill::Flag(false), 1.0 / procs, false)?;
                        self.damage_origin=None;
                        // 只有自然到期触发次要侵损命中，十层引爆跳过。
                        if let Kind::Blast(i) = event.kind {
                            if let Some(en) = self.enervate.as_mut() {
                                en.advance_probability(t, None, self.blasts[i].hit_probability);
                            }
                        }
                    }
                }
                Kind::GroupedDot(d, token) => {
                    let tick = self.proc_piles.get_mut(&d).and_then(|pile|pile.tick(t,token));
                    if let Some(tick) = tick {
                        let generation=self.gen;
                        if tick.damage > 0.0 {
                            self.damage_origin=tick.origin;
                            self.proc_source=tick.source;
                            self.take(&vec![(d,tick.damage)],t,DOT,WeakKill::Value(tick.weak_damage/tick.damage),1.0/pmax(tick.count,1e-9),false)?;
                            self.damage_origin=None;
                            self.proc_source=None;
                        }
                        if self.gen==generation {
                            if let Some((when,next_token))=tick.schedule { self.push(when,Kind::GroupedDot(d,next_token),Some(self.gen)); }
                        }
                    }
                }
                Kind::IndependentDot(i) => {
                    let alive = stack_alive(&self.dots[i]);
                    let (count, per, end, d) = (self.dots[i].count, self.dots[i].per, self.dots[i].end, self.dots[i].dtype);
                    if count * alive > 0.0 && t <= end + 1e-9 {
                        let dtype = if d == SLASH { CINEMATIC } else { d };
                        self.damage_origin=Some(self.dot_modes[i]);
                        self.proc_source = Some(self.dot_sources[i]);
                        self.take(&vec![(dtype, count * alive * per)], t, DOT, WeakKill::Flag(false), 1.0 / count, false)?;
                        self.damage_origin=None;
                    }
                }
                Kind::Dot(d, value, end, origin) => {
                    if t <= end + 1e-9 {
                        let dtype = if d == SLASH { CINEMATIC } else { d };
                        self.damage_origin=Some(origin);
                        self.take(&vec![(dtype, value)], t, DOT, WeakKill::Flag(false), 0.0, false)?;
                        self.damage_origin=None;
                    }
                }
                Kind::Actor(ref kind) => {
                    self.actor_signal = true;
                    self.actor_signal_used = true;
                    let kind = kind.clone();
                    self.signal(&kind, t, 1.0);
                    self.actor_signal = false;
                }
                Kind::Syndicate => {
                    let (id, d, fill) = match &self.syndicate {
                        Some(x) => x.clone(),
                        None => continue,
                    };
                    if t < self.syndicate_ready || self.affinity < fill {
                        continue;
                    }
                    self.affinity = 0.0;
                    self.syndicate_ready = t + 30.0;
                    let before = self.totals[SYNDICATE];
                    let died = self.take(&vec![(d, 1000.0)], t, SYNDICATE, WeakKill::Flag(false), 0.0, false)?;
                    self.syndicate_events.push((t, self.totals[SYNDICATE] - before));
                    if !died && id != "justice" && !(self.blocks_radiation && d == RADIATION) {
                        let name = self.types.name(d).to_string();
                        let duration = if name == "DT_CORROSIVE" { 8.0 } else if name == "DT_RADIATION" { 12.0 } else { 6.0 };
                        if let Some(p) = pooled_index(d) {
                            self.pooled[p].add(t + duration, 0.0, 1.0);
                        } else {
                            self.states.entry(d).or_default().push_back((t + duration, 1.0));
                        }
                        self.presence.entry(d).or_default().add(t + duration, 0.0);
                        if name == "DT_GAS" || name == "DT_POISON" {
                            let ticks: Vec<i64> = if name == "DT_GAS" { (0..6).collect() } else { (1..7).collect() };
                            for j in ticks {
                                self.push(t + j as f64, Kind::Dot(d, 500.5, t + 6.0, INCARNON), Some(self.gen));
                            }
                        }
                    }
                }
                Kind::OwnedEmbed(token,part)=> {
                    if token!=self.source_field_token {self.stale+=1;continue;}
                    self.radial_hit(part,1.0,t,true)?;
                }
                Kind::ZymosCarrier(active, parent_weight) => {
                    // 已入队的 953 头部伤害包先结算，再从当前事件清理后的状态读取来源层数。
                    let snapshot = self.provider.zymos_carrier(
                        self.stats[active].obj, self.source_stacks(), self.enervate_weights_for_callback(),
                    )?;
                    let frame = self.stats[active].zymos.unwrap().1;
                    self.push(t + frame, Kind::ZymosSpawn(active, snapshot, parent_weight), Some(self.gen));
                }
                Kind::ZymosSpawn(active, snapshot, parent_weight) => {
                    let children = self.provider.zymos_children(
                        self.stats[active].obj, snapshot, self.source_stacks(), parent_weight, self.enervate_weights_for_callback(),
                    )?;
                    for (contact, blast, arrival) in children {
                        let contact = self.view(contact)?;
                        let blast = self.view(blast)?;
                        self.push(t + arrival, Kind::ZymosHit(contact, blast), Some(self.gen));
                    }
                }
                Kind::ZymosHit(contact, blast) => {
                    let generation = self.gen;
                    self.radial_hit(contact, 1.0, t, true)?;
                    if self.gen == generation { self.radial_hit(blast, 1.0, t, true)?; }
                }
                Kind::QuantaPrimary(cost) => {
                    self.ammo -= pmax(cost, 0.0);
                    self.signal("on-fire", t, 1.0);
                }
                Kind::NoxPayload(part,weight) => {
                    self.radial_hit_at(part,1.0,t,true,Some(weight),0.0)?;
                }
                Kind::DelayedRadial(part, sniper_factor) => {
                    self.delayed_attacks += 1;
                    self.radial_hit(part, sniper_factor, t, true)?;
                }
                Kind::DelayedAttack(part, aim) => {
                    self.delayed_attacks += 1;
                    let key = self.packet(part, aim)?;
                    let vector: Vector = self.packets[&key].vector.iter().map(|(k, v)| (*k, v * self.damage_rate_multiplier)).collect();
                    let cv2 = self.spread(part, aim);
                    let radial_only = self.stats[part].radial_only;
                    self.proc_source = Some(part);
                    if !self.take(&vector, t, DIRECT, WeakKill::Flag(!radial_only), cv2, false)? {
                        self.add_procs(part, aim, t, key)?;
                    }
                    let puncture = if radial_only { 0.0 } else { self.live_crit().0 };
                    self.enervate_hit(part, t, puncture, aim);
                    let (ms, cc) = (self.stats[part].multishot, self.stats[part].crit_chance);
                    self.signal("on-hit", t, ms);
                    self.signal("on-headshot", t, aim * ms);
                    self.signal("on-crit", t, pmin(cc, 1.0) * ms);
                    self.noncrit_no_proc_hit(part, aim, t);
                }
                Kind::EmbeddedTick(id) | Kind::EmbeddedDeath(id) => {
                    let death=matches!(event.kind,Kind::EmbeddedDeath(_));
                    let entity=self.embedded_entities[id].clone();
                    if !entity.alive {continue;}
                    if entity.generation!=self.gen {
                        self.embedded_entities[id].alive=false;
                        self.waiting_mines=pmax(0.0,self.waiting_mines-entity.weight);
                        self.stale+=1;continue;
                    }
                    if !death && t>entity.expires+1e-9 {continue;}
                    let part=if death {entity.blast}else{entity.part};
                    if death {
                        self.embedded_entities[id].alive=false;self.detonations+=1;
                        self.waiting_mines=pmax(0.0,self.waiting_mines-entity.weight);
                    }
                    self.radial_hit_at(part,1.0,t,true,Some(entity.weight),0.0)?;
                }
                Kind::FiniteField(slot,part)=> {
                    self.finite_field_packets[slot]+=1;
                    self.snapshot_victim_procs(t);
                    let raw=self.raw_packet(part,0.0);
                    let vector:Vector=self.packets[&raw].vector.iter().map(|(d,v)|(*d,v*self.damage_rate_multiplier)).collect();
                    let cv2=self.spread(part,0.0);
                    self.proc_source = Some(part);
                    if !self.take_with_source_credit(&vector,t,DIRECT,WeakKill::Flag(false),cv2,false,None,false)? {
                        let statuses=self.packets[&raw].clone();
                        self.add_procs_data(part,0.0,t,&statuses,false)?;
                    }
                }
                Kind::SourceField(token,slot,part,initial)=> {
                    if token!=self.source_field_token {self.stale+=1;continue;}
                    self.source_field_packets[slot]+=1;
                    self.snapshot_victim_procs(t);
                    let stacks = self.source_stacks();
                    let live = psum(self.victim_before.iter().map(|(_, p)| *p));
                    let hook = self.provider.refresh_hook(self.stats[part].obj, true)?
                        .ok_or_else(|| invalid_value("source field refresh hook missing"))?;
                    let part = self.refresh_view(hook, stacks, live, Some(initial))?;
                    if self.stats[part].delayed_radial_hit_event {
                        self.radial_hit(part,self.damage_rate_multiplier,t,true)?;
                    } else {
                        let raw=self.raw_packet(part,0.0);
                        let vector:Vector=self.packets[&raw].vector.iter().map(|(d,v)|(*d,v*self.damage_rate_multiplier)).collect();
                        let cv2=self.spread(part,0.0);
                        self.proc_source = Some(part);
                        if !self.take(&vector,t,DIRECT,WeakKill::Flag(false),cv2,false)? {
                            self.add_procs(part,0.0,t,raw)?;
                        }
                    }
                }
                Kind::Lingering(part) => {
                    let part = if let Some(hook) = self.provider.refresh_hook(self.stats[part].obj, false)? {
                        let live = self.live_proc_types(t);
                        let stacks = self.source_stacks();
                        self.refresh_view(hook, stacks, live, None)?
                    } else if let Some(unit)=self.stats[part].live_co_unit {
                        let live = self.live_proc_types(t);
                        self.vary(unit,live,1.).ok_or_else(|| invalid_value("live field CO template missing"))?
                    } else {part};
                    if self.stats[part].delayed_radial_hit_event {
                        self.radial_hit(part,1.,t,true)?;
                        continue;
                    }
                    let raw = self.raw_packet(part, 0.0);
                    let packet = self.packets[&raw].clone();
                    let cv2 = self.spread(part, 0.0);
                    self.proc_source = Some(part);
                    if !self.take(&packet.vector, t, DIRECT, WeakKill::Flag(false), cv2, false)? {
                        let key = self.packet(part, 0.0)?;
                        self.add_procs(part, 0.0, t, key)?;
                    }
                }
                Kind::StackLife(group, token, action) => {
                    let mut out = Vec::new();
                    if let Some(p) = self.projectiles.as_mut() {
                        p.life(&mut out, t, group, token, action);
                    }
                    self.apply_stack_actions(out, t)?;
                }
                Kind::StackChild(group, multiplier, hits) => {
                    let mut out = Vec::new();
                    if let Some(p) = self.projectiles.as_mut() {
                        p.child(&mut out, group, multiplier, hits);
                    }
                    self.apply_stack_actions(out, t)?;
                }
                Kind::Detonation(batch) => {
                    self.detonations += 1;
                    let mines = psum(batch.iter().filter(|m| !m.2).map(|m| self.stats[m.0].multishot));
                    self.waiting_mines = pmax(0.0, self.waiting_mines - mines);
                    for &(part, generation, _) in batch.iter() {
                        if generation != self.gen {
                            self.stale += 1;
                            continue;
                        }
                        let key = self.packet(part, 0.0)?;
                        let packet = self.packets[&key].clone();
                        let cv2 = self.spread(part, 0.0);
                        self.proc_source = Some(part);
                        if !self.take(&packet.vector, t, DIRECT, WeakKill::Flag(false), cv2, false)? {
                            self.add_procs(part, 0.0, t, key)?;
                        }
                        self.enervate_hit(part, t, 0.0, 0.0);
                        let (ms, cc) = (self.stats[part].multishot, self.stats[part].crit_chance);
                        self.signal("on-hit", t, ms);
                        self.signal("on-crit", t, pmin(cc, 1.0) * ms);
                    }
                }
                Kind::Transform(next) => {
                    if self.tap.is_some() {
                        // 切换形态时，`ConsecutiveHits.swap` 分别保留两个形态的连击数。
                        let saved = (self.tap_hits, pmax(self.tap_expires - t, 0.0));
                        let (hits, remaining) = self.tap_other;
                        self.tap_hits = hits;
                        self.tap_expires = if remaining != 0.0 { t + remaining } else { f64::NEG_INFINITY };
                        self.tap_other = saved;
                    }
                    self.form = next;
                    if self.onos.is_some() {
                        self.onos_ticks = 0;
                        self.onos_hits = 0.0;
                        self.onos_start = t;
                    }
                    self.signal("on-transform", t, 1.0);
                    self.transforms += 1;
                    first_round = true;
                    burst_round = 0;
                    let (active, _) = self.model(t)?;
                    if self.shell.is_some() && self.shell_mode_switch {
                        let loaded = if self.form == INCARNON { pmin(1.0, self.shells_owed) } else { self.shells_owed };
                        if loaded > 0.0 {
                            self.signal("on-shell-reloaded", t, loaded);
                            self.reload_shell_events.push((t, loaded, "mode-switch", self.form));
                            self.shells_owed -= loaded;
                        }
                    }
                    let c = self.cycle.unwrap();
                    if c.kill_charged {
                        // 次要射击期间仍保留主射弹匣。
                        if self.form == INCARNON {
                            self.base_ammo_saved = Some(self.ammo);
                            self.ammo = c.pool;
                        } else {
                            self.ammo = match self.base_ammo_saved { Some(a) => a, None => self.stats[active].magazine };
                        }
                    } else {
                        self.ammo = if self.form == INCARNON { c.pool } else { self.stats[active].magazine };
                    }
                    if self.form == BASE {
                        self.gauge = 0.0;
                    }
                    let windup = if self.form == INCARNON && c.charge_before_fire {
                        self.stats[active].charge_time
                    } else { 0.0 };
                    self.push(t + windup, Kind::Fire, None);
                }
                Kind::ReloadShell(amount, limit) => {
                    self.ammo = pmin(limit, self.ammo + amount);
                    self.signal("on-shell-reloaded", t, amount);
                    self.reload_shell_events.push((t, amount, "reload", self.form));
                }
                Kind::ReloadComplete(was_empty) => {
                    if self.field_duration.is_some() && was_empty {
                        self.field_duration_ready = true;
                    }
                    let old_magazine = if self.refill_growth { { let i = self.model(t)?.0; self.stats[i].magazine } } else { 0.0 };
                    self.signal("on-reload-complete", t, 1.0);
                    if was_empty {
                        self.signal("on-reload-empty-complete", t, 1.0);
                    }
                    if self.refill_growth {
                        let grown = { let i = self.model(t)?.0; self.stats[i].magazine };
                        self.ammo = pmin(grown, self.ammo + pmax(0.0, grown - old_magazine));
                    }
                    self.push(t, Kind::Fire, None);
                }
                Kind::Fire | Kind::ChargedFire => {
                    let charged = matches!(event.kind, Kind::ChargedFire);
                    let _timer = crate::counters::Timer::start(crate::counters::FIRE);
                    if t >= self.window - 1e-9 || self.rate <= 0.0 {
                        continue;
                    }
                    let (active, _) = self.model(t)?;
                    let a = &self.stats[active];
                    let magazine_form = self.magazine_form();
                    self.last_round = magazine_form
                        && a.magazine > 0.0 && self.ammo > 0.0 && self.ammo <= pmax(a.ammo_cost, 1.0) + 1e-9;
                    let (mut active, radial) = self.model(t)?;
                    if self.projectile_sequence_length > 0 && self.stats[active].multishot != 1.0 {
                        return Err(unsupported("companion-projectile-sequence-multishot-unmodeled"));
                    }
                    let speed = self.stats[active].fire_rate;
                    if speed <= 0.0 {
                        continue;
                    }
                    // `combat_replay` 的速度状态记录本次挥击读取的层数与连击档位。
                    let speed_state: Option<(Vec<u64>, i64)> = self.melee.as_ref().map(|spec| {
                        (self.sources.iter().map(|q| q.stacks.to_bits()).collect(), spec.tier(self.melee_count) as i64)
                    });
                    let mut melee_hit: Option<MeleeHit> = None;
                    if self.melee.is_some() {
                        let pending = match self.melee_pending {
                            None => {
                                self.melee_count = pmin((pmax(0.0,self.stats[active].initial_combo) as f32).trunc() as f64, self.melee.as_ref().unwrap().cap);
                                self.melee_combo_at = t;
                                let (charge, projectile) =
                                    (self.stats[active].heavy_charge_speed, self.stats[active].projectile_speed_multiplier);
                                let (hit, wait) = self.melee_next(speed, charge, projectile)?;
                                self.melee_pending = Some(hit);
                                self.melee_recovery_initial=self.melee_count;
                                self.sync_melee_recovery(t)?;
                                self.push(t + wait, Kind::Fire, None);
                                continue;
                            }
                            Some(hit) => hit,
                        };
                        let spec = self.hit_spec(pending.id)?;
                        if spec.bounce && !(self.stats[active].reflection_explosion_chance > 0.0) {
                            // 未装挥发弹跳卡时，战刃反弹不触发额外效果。
                            let (charge, projectile) =
                                (self.stats[active].heavy_charge_speed, self.stats[active].projectile_speed_multiplier);
                            let (hit, wait) = self.melee_next(speed, charge, projectile)?;
                            self.melee_pending = Some(hit);
                            self.push(t + wait, Kind::Fire, None);
                            continue;
                        }
                        melee_hit = Some(pending);
                        let initial = pmin((pmax(0.0,self.stats[active].initial_combo) as f32).trunc() as f64, self.melee.as_ref().unwrap().cap);
                        self.advance_melee_recovery(t,initial,None);
                        if t - self.melee_last_hit > pmax(self.stats[active].combo_duration, 0.0) + 1e-9
                            && self.melee_last_hit > f64::NEG_INFINITY
                            && self.melee_count > initial
                        {
                            let dropped = {
                                let spec = self.melee.as_ref().unwrap();
                                spec.tier(self.melee_count) > spec.tier(initial)
                            };
                            self.melee_count = initial;
                            if dropped {
                                self.signal("on-combo-tier-reset", t, 1.0);
                            }
                        }
                        self.melee_heavy_multiplier = self.melee.as_ref().unwrap().multiplier(self.melee_count);
                        self.melee_heavy_now = pending.heavy;
                        // 攻击开始时多尔克拉夫消耗计数，震地先锁定弧电振子锤的层数再读取。
                        self.signal("on-melee-attack", t, 1.0);
                        if pending.phase & 1 != 0 {
                            self.signal("on-melee-slam-start", t, 1.0);
                        }
                        self.heavy_flag = pending.heavy;
                        self.no_co_flag = spec.no_co;
                        let (heavy_active, _) = self.model(t)?;
                        self.heavy_flag = false;
                        self.no_co_flag = false;
                        active = self.melee_apply(heavy_active, pending.id, self.melee_heavy_multiplier)?;
                    }
                    let mut radial = if self.melee.is_some() { None } else { radial };
                    if let (Some((delay_seconds, max_projectiles, _)), false) = (self.mines, self.impact_embed) {
                        // 新的投掷可能超过部署上限前，玩家会先引爆已有的一批弹体。
                        let gen = self.gen;
                        self.pending_mines.retain(|m| m.1 == gen);
                        if self.pending_mines.is_empty() {
                            self.pending_throws = 0;
                        }
                        if !self.pending_mines.is_empty() {
                            let deployed = psum(self.pending_mines.iter().filter(|m| !m.2).map(|m| self.stats[m.0].multishot));
                            if deployed + self.stats[active].multishot > max_projectiles + 1e-9 {
                                self.queue_mines(t);
                                self.push(t + pmax(delay_seconds, 1.0 / speed), Kind::Fire, None);
                                continue;
                            }
                        }
                    }
                    if let Some(c) = self.cycle {
                        if self.form == BASE && self.gauge >= c.hits - 1e-9 {
                            if self.shell.is_some() {
                                let m = self.stats[active].magazine;
                                self.shells_owed = pmax(0.0, (m - pmax(self.ammo, 0.0) - 1e-9).ceil());
                            }
                            self.push(t + c.enter * (if c.kill_charged { 1.0 } else { self.stats[active].transform_time_scale }), Kind::Transform(INCARNON), None);
                            continue;
                        }
                    }
                    let (magazine, ammo_cost) = (self.stats[active].magazine, self.stats[active].ammo_cost);
                    if let Some((empty_delay, partial_delay, regen)) = self.battery {
                        if !self.stats[active].full_clip_charge && last_fire_time.is_finite() {
                            let delay = if self.ammo <= 1e-9 { empty_delay } else { partial_delay };
                            self.ammo = pmin(magazine, self.ammo + pmax(0.0, t - last_fire_time - delay) * regen);
                        }
                    }
                    if self.infinite_ammo && magazine > 0.0 && !(self.cycle.is_some() && self.form == INCARNON) {
                        self.ammo = pmax(self.ammo, magazine);
                    }
                    let mut ammo_cost = ammo_cost;
                    if let Some((probe, offset, golden, _)) = self.weak_draw {
                        // 在空仓时，对有概率生效的无限弹药分支分别结算继续免费射击或装填。
                        if self.ammo <= 1e-9 && ammo_cost > 1e-9 && magazine > 0.0 {
                            let mut idle = 1.0;
                            let mut any = false;
                            for q in self.sources.iter() {
                                if q.free_ammo && q.stacks > 0.0 {
                                    any = true;
                                    idle *= 1.0 - pmin(1.0, q.stacks / q.max_stacks);
                                }
                            }
                            let presence = 1.0 - idle;
                            if any && 1e-9 < presence && presence < 1.0 - 1e-9 {
                                if probe {
                                    self.weak_uncertainty = pmax(self.weak_uncertainty, pmin(presence, 1.0 - presence));
                                } else {
                                    let u = (offset + self.weak_index * golden) % 1.0;
                                    self.weak_index += 1.0;
                                    let on = u < presence;
                                    let mut held: Option<usize> = None;
                                    if on {
                                        for (i, q) in self.sources.iter().enumerate() {
                                            if q.free_ammo && q.stacks > 0.0
                                                && held.map_or(true, |h| q.stacks > self.sources[h].stacks) {
                                                held = Some(i);
                                            }
                                        }
                                    }
                                    for (i, q) in self.sources.iter_mut().enumerate() {
                                        if q.free_ammo && q.stacks > 0.0 {
                                            settle(q, held == Some(i));
                                        }
                                    }
                                    let (a, r) = self.model(t)?;
                                    active = a;
                                    radial = r;
                                    ammo_cost = self.stats[active].ammo_cost;
                                }
                            }
                        }
                    }
                    let reserved_primary = self.stats[active].quanta.map_or(0.0, |q| q.0);
                    let insufficient = self.stats[active].requires_full_ammo
                        && self.ammo + 1e-9 < pmax(ammo_cost + reserved_primary, 0.0);
                    // 仅当无法开火时才装填，免费射击在空仓时可以继续。
                    if ((self.ammo <= 1e-9 && ammo_cost > 1e-9) || insufficient) && magazine > 0.0 {
                        if let (Some(c), INCARNON) = (self.cycle, self.form) {
                            self.push(t + c.exit * (if c.kill_charged { 1.0 } else { self.stats[active].transform_time_scale }), Kind::Transform(BASE), None);
                            continue;
                        }
                        if self.battery.is_some() {
                            self.recharges += 1;
                            self.ammo = magazine;
                            let reload = self.stats[active].reload_time;
                            self.push(t + reload, Kind::Fire, None);
                            continue;
                        }
                        self.reloads += 1;
                        first_round = true;
                        burst_round = 0;
                        let was_empty = self.ammo <= 1e-9;
                        let before_reload_ammo = self.ammo;
                        if self.shell.is_none() || !magazine_form {
                            self.ammo = magazine + pmin(self.ammo, 0.0);
                        }
                        if was_empty {
                            self.signal("on-reload-empty", t, 1.0);
                        }
                        self.signal("on-reload", t, 1.0);
                        for q in self.sources.iter_mut() {
                            if q.kind.as_deref() == Some("on-burst-hit") {
                                q.stacks = 0.0;
                            }
                        }
                        let (reloading, _) = self.model(t)?;
                        let mut reload_time = self.stats[reloading].reload_time;
                        if let (Some((start, per_shell, end)), true) = (self.shell, magazine_form) {
                            let reload_magazine = self.stats[reloading].magazine;
                            let target_ammo = reload_magazine + pmin(before_reload_ammo, 0.0);
                            let missing = pmax(0.0, target_ammo - before_reload_ammo);
                            let total_rounds = ceil_i(missing - 1e-9);
                            let raw_full_time = start + per_shell * reload_magazine + end;
                            let speed_scale = if raw_full_time > 0.0 { reload_time / raw_full_time } else { 1.0 };
                            for i in 0..total_rounds.max(0) {
                                let amount = pmin(1.0, pmax(0.0, missing - i as f64));
                                self.push(t + (start + per_shell * (i + 1) as f64) * speed_scale,
                                          Kind::ReloadShell(amount, target_ammo), None);
                            }
                            reload_time = (start + per_shell * total_rounds as f64 + end) * speed_scale;
                        }
                        for q in self.sources.iter_mut() {
                            let k = q.kind.as_deref();
                            if (k == Some("on-reload-empty") || k == Some("on-reload")) && q.duration == 0.0 && !q.hold {
                                q.stacks = if q.per_event == 0.0 { q.initial_stacks } else { 0.0 };
                            }
                        }
                        // 喷毒者的空仓装填投出大号手雷，并在装填中途落地。
                        let thrown = if was_empty { self.stats[reloading].reload_empty.clone() } else { self.stats[reloading].reload_partial.clone() };
                        let lands_at = self.stats[reloading].reload_lands_at;
                        for part in thrown {
                            self.push(t + reload_time * lands_at, Kind::DelayedRadial(part, 1.0), Some(self.gen));
                        }
                        if let Some(spool) = self.spool {
                            if reload_time >= spool.spindown {
                                spool_shots = 0;
                            }
                        }
                        self.push(t + reload_time, Kind::ReloadComplete(was_empty), None);
                        continue;
                    }
                    if self.stats[active].charge_before_fire && !charged {
                        self.push(t + self.stats[active].charge_time, Kind::ChargedFire, None);
                        continue;
                    }
                    if self.stats[active].charge_volley {
                        let loaded = match volley_pending.take() {
                            Some(n) => n,
                            None => {
                                let n=pmax(1.0,pmax(0.0,pmin(self.ammo,self.stats[active].magazine)).floor());
                                volley_pending=Some(n);
                                self.push(t+n*self.stats[active].charge_time,Kind::Fire,None);
                                continue;
                            }
                        };
                        self.volley_rounds=loaded;
                        let views=self.model(t)?;
                        active=views.0;radial=views.1;
                        self.volley_rounds=1.0;
                        let a=&self.stats[active];
                        let shot_cost=a.ammo_cost*loaded;
                        let mut next_ammo=self.ammo-shot_cost;
                        let mut reload_wait=0.0;
                        if !self.infinite_ammo && next_ammo<=1e-9 && shot_cost>0.0 {
                            next_ammo=a.magazine;reload_wait=a.reload_time;
                        } else if self.infinite_ammo { next_ammo=a.magazine; }
                        let next_loaded=pmax(1.0,pmax(0.0,pmin(next_ammo,a.magazine)).floor());
                        self.volley_shot_resume=Some(t+1.0/pmax(a.fire_rate,1e-9)+reload_wait+next_loaded*a.charge_time);
                        active=self.derive(active,34,loaded,|v| {
                            for entry in v.core.damage_types.iter_mut() { entry.1*=loaded; }
                            v.damage_types=v.core.damage_types.clone();
                            v.core.dot_base_damage=v.core.dot_base_damage.map(|x|x*loaded);
                            v.ammo_cost*=loaded;v.charge_time=0.0;
                        });
                        if let Some(part)=radial {
                            radial=Some(self.derive(part,35,loaded,|v| {
                                for entry in v.core.damage_types.iter_mut() { entry.1*=loaded; }
                                v.damage_types=v.core.damage_types.clone();
                                v.core.dot_base_damage=v.core.dot_base_damage.map(|x|x*loaded);
                                v.ammo_cost*=loaded;v.charge_time=0.0;
                            }));
                        }
                    }
                    let charge_scale = if self.form == INCARNON {
                        self.cycle.map(|c| c.damage_scale).unwrap_or(1.0)
                    } else { 1.0 };
                    if charge_scale != 1.0 {
                        let scaled = |v: &mut Stats| {
                            for entry in v.core.damage_types.iter_mut() { entry.1 *= charge_scale; }
                            v.damage_types = v.core.damage_types.clone();
                            v.core.dot_base_damage = v.core.dot_base_damage.map(|x| x * charge_scale);
                        };
                        active = self.derive(active, 36, charge_scale, scaled);
                        if let Some(part) = radial { radial = Some(self.derive(part, 37, charge_scale, scaled)); }
                    }
                    if let Some((remaining_clip, count, _, _)) = self.form_burst().0 {
                        if burst_round == 0 {
                            burst_size = if remaining_clip {
                                ceil_i(self.ammo / pmax(ammo_cost, 1e-9) - 1e-9).max(1)
                            } else {
                                count
                            };
                        }
                    }
                    let mut speed = speed;
                    if let Some(spool) = self.spool {
                        let pellets = pmin(spool.maximum, spool.initial + spool_shots as f64);
                        speed *= spool.rate_start + (1.0 - spool.rate_start) * pmin(spool_shots as f64 / spool.rate_shots, 1.0);
                        active = self.derive(active, 2, pellets, |v| {
                            v.multishot = v.multishot * pellets / spool.maximum;
                            v.core.multishot = v.multishot;
                            v.status_chance = v.status_chance * spool.maximum / pellets;
                            v.core.status_chance = v.status_chance;
                            v.ammo_cost = v.ammo_cost * (pellets / spool.ammo_divisor).ceil()
                                / (spool.maximum / spool.ammo_divisor).ceil();
                            v.drop_private();
                        });
                        spool_shots += 1;
                    }
                    let mut native_refire_delay = None;
                    if let (Some(spool), true) = (self.rate_spool, self.form != INCARNON) {
                        if t > rate_spool_due + 1e-9 {
                            rate_spool_shots = 0;
                        }
                        (speed, native_refire_delay) = spool.refire(speed, rate_spool_shots);
                        rate_spool_shots += 1;
                    }
                    self.attacks += 1;
                    self.shot_in_progress = true;
                    if let Some((per_hit, maximum, duration)) = self.tap {
                        // 无循环时使用所选模式，`selectedIncarnon` 为真时使用灵化形态。
                        let incarnon_form = self.incarnon_form();
                        let has_radial = radial.is_some() && self.melee.is_none();
                        let hits = self.stats[active].multishot * if incarnon_form && has_radial { 2.0 } else { 1.0 };
                        // 对应 `ConsecutiveHits.shot` 的连续命中。
                        let factor = {
                            if t >= self.tap_expires {
                                self.tap_hits = 0.0;
                            }
                            if hits <= 0.0 {
                                self.tap_hits = 0.0;
                                self.tap_expires = f64::NEG_INFINITY;
                                1.0
                            } else {
                                self.tap_hits += hits;
                                self.tap_expires = t + duration;
                                1.0 + per_hit * pmin(maximum, pmax(self.tap_hits - 1.0, 0.0))
                            }
                        };
                        let scaled = |v: &mut Stats| {
                            for e in v.core.damage_types.iter_mut() {
                                e.1 *= factor;
                            }
                            v.damage_types = v.core.damage_types.clone();
                            if let Some(dot) = v.core.dot_base_damage {
                                v.core.dot_base_damage = Some(dot * factor);
                            }
                            v.drop_private();
                        };
                        if !incarnon_form {
                            active = self.derive(active, 9, factor, scaled);
                        }
                        if let (Some(r), true) = (radial, has_radial) {
                            radial = Some(self.derive(r, 9, factor, scaled));
                        }
                    }
                    last_fire_time = t;
                    let generation = self.gen;
                    let finite_fields=self.stats[active].finite_fields.clone();
                    let source_owned_fields=self.stats[active].source_fields.clone();
                    let owned_embed=self.stats[active].owned_embed;
                    let nox_projectile=self.stats[active].nox_projectile;
                    if !source_owned_fields.is_empty() || owned_embed.is_some() {self.source_field_token+=1;}
                    if self.last_burst && self.form == BASE && self.ammo <= 3.0 {
                        active = self.derive(active, 10, 0.0, |v| {
                            v.multishot *= 6.0;
                            v.core.multishot = v.multishot;
                            v.drop_private();
                        });
                    }
                    let physical_first_round = first_round;
                    let refreshed_contact_first_factor=if first_round && self.provider.has_refresh(self.stats[active].obj)? {1.0+self.stats[active].first_shot_damage_bonus}else{1.0};
                    if first_round && self.stats[active].first_shot_damage_bonus != 0.0 {
                        let factor = 1.0 + self.stats[active].first_shot_damage_bonus;
                        active = self.derive(active, 1, factor, |v| {
                            for e in v.core.damage_types.iter_mut() {
                                e.1 *= factor;
                            }
                            v.damage_types = v.core.damage_types.clone();
                            if let Some(dot) = v.core.dot_base_damage {
                                v.core.dot_base_damage = Some(dot * factor);
                            }
                        });
                    }
                    first_round = false;
                    let pool_cost = self.stats[active].ammo_pool_multishot_cost;
                    if pool_cost != 0.0 && self.incarnon_form() && self.ammo < 1.0 + pool_cost {
                        let afforded = pmax(self.ammo - 1.0, 0.0);
                        let normal = 1.0 + pool_cost;
                        let factor = 1.0 + self.stats[active].ammo_pool_damage_bonus;
                        let previous = 1.0 + pool_cost * factor;
                        let reduced = 1.0 + afforded * factor;
                        if self.stats[active].continuous {
                            active = self.derive(active, 6, afforded, |v| {
                                v.multishot = reduced;
                                v.core.multishot = reduced;
                                v.ammo_pool_multishot_cost = afforded;
                                v.drop_private();
                            });
                        } else {
                            let adjustment = (reduced / (1.0 + afforded)) / (previous / normal);
                            active = self.derive(active, 7, afforded, |v| {
                                v.multishot = 1.0 + afforded;
                                v.core.multishot = v.multishot;
                                v.ammo_pool_multishot_cost = afforded;
                                for e in v.core.damage_types.iter_mut() {
                                    e.1 *= adjustment;
                                }
                                v.damage_types = v.core.damage_types.clone();
                                if let Some(dot) = v.core.dot_base_damage {
                                    v.core.dot_base_damage = Some(dot * adjustment);
                                }
                                v.drop_private();
                            });
                        }
                    }
                    if self.impact_embed && (self.stats[active].embedded.is_none() || radial.is_none()) {
                        return Err(invalid_value("embedded detonation lacks native field/blast parts"));
                    }
                    // 一次射击消耗已准备的伤害场增益，包括击杀目标的射击。
                    let field_duration_boost = self.field_duration_ready;
                    self.field_duration_ready = false;
                    let s = &self.stats[active];
                    let quanta = s.quanta;
                    let no_contact=quanta.is_some() || s.no_enemy_contact;
                    let mut quanta_last_impact = 0.0;
                    let mut hit_count = if no_contact { 0.0 } else if s.continuous { 1.0 } else { pmax(s.multishot, 0.0) };
                    let radial_only = s.radial_only;
                    let continuous = s.continuous;
                    let mut sniper_factor = 1.0;
                    if !radial_only && (!self.incarnon_form() || self.shot_combo.as_ref().is_some_and(|c| c.incarnon)) {
                        if let Some(combo) = self.shot_combo.as_mut() {
                            sniper_factor = combo.shot(t, hit_count);
                        }
                    }
                    self.snapshot_victim_procs(t);
                    let shot_puncture = if radial_only { 0.0 } else { self.live_crit().0 };
                    let aim = if radial_only || no_contact { 0.0 } else { self.head };
                    let primary_delay = self.stats[active].primary_delay;
                    let stack = self.stats[active].stackable.clone();
                    let mut projectile_wait = 0.0;
                    if self.physical_lane_length>0 {
                        let total=self.stats[active].multishot;
                        let whole=total.floor();
                        let uniform=(self.lane_stratum+(self.attacks-1) as f64*((5.0_f64.sqrt()-1.0)/2.0)).rem_euclid(1.0);
                        let number=whole as usize+if total-whole>uniform {1}else{0};
                        hit_count=number as f64;
                        let phase=self.physical_lane_phase;
                        self.physical_lane_phase=(phase+number)%self.physical_lane_length;
                        for n in 0..number {
                            if self.gen!=generation {self.stale+=1;continue;}
                            self.snapshot_victim_procs(t);
                            self.physical_lane_selected=Some((phase+n)%self.physical_lane_length);
                            let (mut part,blast)=self.model(t)?;
                            if physical_first_round && self.stats[part].first_shot_damage_bonus != 0.0 {
                                let factor=1.0+self.stats[part].first_shot_damage_bonus;
                                part=self.derive(part,1,factor,|v| {
                                    for entry in v.core.damage_types.iter_mut() {entry.1*=factor;}
                                    v.damage_types=v.core.damage_types.clone();
                                    v.core.dot_base_damage=v.core.dot_base_damage.map(|x|x*factor);
                                });
                            }
                            if self.stats[part].delayed_radial_hit_event {
                                self.radial_hit(part,self.damage_rate_multiplier,t,true)?;
                            } else {
                                let key=self.packet(part,0.0)?;
                                let vector:Vector=self.packets[&key].vector.iter().map(|(d,v)|(*d,v*self.damage_rate_multiplier)).collect();
                                let cv2=self.spread(part,0.0);
                                self.proc_source = Some(part);
                                let died=self.take(&vector,t,DIRECT,WeakKill::Flag(false),cv2,false)?;
                                self.noncrit_no_proc_hit(part,0.0,t);
                                if !died {self.add_procs_scaled(part,0.0,t,key,1.0)?;}
                                self.signal("on-hit",t,1.0);
                                let crit=pmin(pmax(self.stats[part].crit_chance*self.stats[part].bodyshot_crit_multiplier,0.0),1.0);
                                self.signal("on-crit",t,crit);
                            }
                            self.enervate_hit(part,t,shot_puncture,0.0);
                            self.signal("on-body-hit",t,1.0);
                            if self.gen==generation {if let Some(blast)=blast {
                                self.snapshot_victim_procs(t);
                                let notify=self.stats[blast].delayed_radial_hit_event;
                                self.radial_hit(blast,self.damage_rate_multiplier,t,notify)?;
                            }}
                        }
                        self.physical_lane_selected=None;
                    } else if let Some((tick_part,blast_part))=nox_projectile {
                        let ms=pmax(0.0,self.stats[active].multishot);
                        let whole=ms.floor() as usize;
                        for slot in 0..whole+if ms>whole as f64 {1}else{0} {
                            let weight=if slot<whole {1.0}else{ms-whole as f64};
                            if self.gen!=generation {self.stale+=1;continue;}
                            self.radial_hit_at(active,self.damage_rate_multiplier*sniper_factor*refreshed_contact_first_factor,t,true,Some(weight),aim)?;
                            self.signal("on-body-hit",t,(1.0-aim)*weight);
                            self.signal("on-headshot",t,aim*weight);
                            let contact=self.derive(active,34,weight,|v| {v.multishot=weight;v.core.multishot=weight;});
                            self.enervate_hit(contact,t,shot_puncture,aim);
                            if self.gen!=generation {continue;}
                            self.radial_hit_at(tick_part,1.0,t,true,Some(weight),0.0)?;
                            for (age,survival) in [(1.0,1.0),(2.0,1.0),(3.0,0.5)] {
                                self.push(t+age,Kind::NoxPayload(tick_part,weight*survival),Some(generation));
                            }
                            self.push(t+3.1125,Kind::NoxPayload(blast_part,weight),Some(generation));
                        }
                    } else if let Some((profile, parts)) = stack.clone() {
                        if self.projectiles.is_none() {
                            self.projectiles = Some(Projectiles::new(self.projectile_state.clone()));
                        }
                        let mut out = Vec::new();
                        let multishot = self.stats[active].multishot;
                        projectile_wait = self.projectiles.as_mut().unwrap().deploy(&mut out, t, &profile, parts, multishot, generation);
                        self.apply_stack_actions(out, t)?;
                    } else if let (Some((_, max_projectiles, _)), false) = (self.mines, self.impact_embed) {
                        if self.stats[active].multishot > max_projectiles {
                            self.mine_overflows += 1;
                            active = self.derive(active, 12, max_projectiles, |v| {
                                v.multishot = max_projectiles;
                                v.core.multishot = max_projectiles;
                                v.drop_private();
                            });
                        }
                        self.pending_mines.push((active, generation, false));
                        // 自带爆炸部件的地雷同时引爆该部件。
                        if let Some(r) = radial {
                            let mut parts = vec![r];
                            parts.extend(self.stats[r].additional.iter().copied());
                            let ms = self.stats[active].multishot;
                            for part in parts {
                                let extra = self.derive(part, 13, ms, |v| {
                                    v.multishot = ms;
                                    v.core.multishot = ms;
                                    v.drop_private();
                                });
                                self.pending_mines.push((extra, generation, true));
                            }
                        }
                        self.pending_throws += 1;
                    } else if self.impact_embed {
                        // 随后按顺序结算接触及立即命中的附属伤害包。
                    } else if quanta.is_some() {
                        // 消耗弹药的主射脉冲命中立方体，敌方伤害来自延迟爆炸部件。
                    } else if no_contact {
                        // 此次接触回调为空，只有已发出的连锁和到期伤害包能命中。
                    } else if primary_delay != 0.0 {
                        self.push(t + primary_delay, Kind::DelayedAttack(active, aim), Some(generation));
                    } else {
                        let key = self.packet(active, aim)?;
                        let floor = if self.form == BASE { self.beam_floor_base } else { self.beam_floor };
                        let ramp = match (continuous, floor) {
                            (true, Some(floor)) => self.beam_tick(t, 1.0 / speed, floor),
                            _ => 1.0,
                        };
                        let vector: Vector = self.packets[&key]
                            .vector
                            .iter()
                            .map(|(k, v)| (*k, v * self.damage_rate_multiplier * ramp * sniper_factor))
                            .collect();
                        let cv2 = self.spread(active, aim);
                        self.proc_source = Some(active);
                        let died = self.take(&vector, t, DIRECT, WeakKill::Flag(!radial_only), cv2, false)?;
                        self.noncrit_no_proc_hit(active, aim, t);
                        if !finite_fields.is_empty() && self.stats[active].multishot>0.0 {
                            self.finite_field_activations+=1;
                            if self.finite_field_packets.is_empty() {self.finite_field_packets=vec![0;finite_fields.len()];}
                            for (slot,(part,rate,duration,initial)) in finite_fields.iter().copied().enumerate() {
                                for _ in 0..initial {self.push(t,Kind::FiniteField(slot,part),Some(generation));}
                                let mut tick=1;
                                while tick as f64/rate<duration-1e-9 {
                                    self.push(t+tick as f64/rate,Kind::FiniteField(slot,part),Some(generation));tick+=1;
                                }
                            }
                        }
                        if !died {
                            self.add_procs_scaled(active, aim, t, key, sniper_factor)?;
                            if !source_owned_fields.is_empty() && self.gen==generation && self.stats[active].multishot>0.0 {
                                self.source_field_activations+=1;
                                if self.source_field_packets.is_empty() {self.source_field_packets=vec![0;source_owned_fields.len()];}
                                for (slot,(part,rate,duration,initial)) in source_owned_fields.iter().copied().enumerate() {
                                    for _ in 0..initial {self.push(t,Kind::SourceField(self.source_field_token,slot,part,true),Some(generation));}
                                    let mut tick=1;
                                    while tick as f64/rate<duration-1e-9 {
                                        self.push(t+tick as f64/rate,Kind::SourceField(self.source_field_token,slot,part,false),Some(generation));tick+=1;
                                    }
                                }
                            }
                            if let Some((part, tick_rate, duration)) = self.stats[active].lingering {
                                let mut duration = duration;
                                if let Some(multiplier) = self.field_duration {
                                    if field_duration_boost && magazine_form && self.gen == generation {
                                        duration *= multiplier;
                                        self.field_duration_used += 1;
                                    }
                                }
                                if self.gen == generation {
                                    let ticks = ceil_i(duration * tick_rate - 1e-9);
                                    for tick in 0..ticks.max(0) {
                                        self.push(t + tick as f64 / tick_rate, Kind::Lingering(part), Some(self.gen));
                                    }
                                }
                            }
                        }
                    }
                    if self.impact_embed {
                        let (field,tick_rate,duration)=self.stats[active].embedded.unwrap();
                        let ms=pmax(0.0,self.stats[active].multishot);
                        let whole=ms.floor() as usize;
                        let count=whole+if ms>whole as f64 {1}else{0};
                        let (interval,death_delay,inclusive)=self.embedded_timing;
                        let mut last_death:f64=0.0;
                        for slot in 0..count {
                            let weight=if slot<whole {1.0}else{ms-whole as f64};
                            if self.gen!=generation {self.stale+=1;break;}
                            self.radial_hit_at(active,self.damage_rate_multiplier*sniper_factor*refreshed_contact_first_factor,t,true,Some(weight),aim)?;
                            self.signal("on-body-hit",t,(1.0-aim)*weight);
                            self.signal("on-headshot",t,aim*weight);
                            let contact=self.derive(active,34,weight,|v| {v.multishot=weight;v.core.multishot=weight;});
                            self.enervate_hit(contact,t,shot_puncture,aim);
                            if self.gen!=generation {break;}
                            let lifetime=pmin(duration,self.mines.unwrap().0+slot as f64*interval);
                            let id=self.embedded_entities.len();
                            self.waiting_mines+=weight;
                            self.embedded_entities.push(EmbeddedEntity {part:field,blast:radial.unwrap(),expires:t+lifetime,weight,generation,alive:true});
                            self.radial_hit_at(field,1.0,t,true,Some(weight),0.0)?;
                            self.push(t+lifetime+death_delay,Kind::EmbeddedDeath(id),None);
                            let mut tick=1;
                            while (tick as f64/tick_rate)<lifetime-1e-9 || (inclusive && (tick as f64/tick_rate-lifetime).abs()<1e-9) {
                                self.push(t+tick as f64/tick_rate,Kind::EmbeddedTick(id),None);tick+=1;
                            }
                            last_death=pmax(last_death,lifetime+death_delay);
                        }
                        projectile_wait=pmax(0.0,last_death-1.0/speed);
                    }
                    let mut parts: Vec<usize> = Vec::new();
                    if let (Some(r), None, true) = (radial, self.mines, nox_projectile.is_none() && stack.is_none() && self.physical_lane_length==0) {
                        parts.push(r);
                        parts.extend(self.stats[r].additional.iter().copied());
                    }
                    for mut part in parts {
                        if self.gen != generation && !self.stats[part].persist_after_kill {
                            self.stale += 1;
                            continue;
                        }
                        let head_only = self.stats[part].head_only;
                        if head_only || self.stats[part].body_only {
                            let ms = self.stats[part].multishot * if head_only { aim } else { 1.0 - aim };
                            part = self.derive(part, 5, aim, |v| {
                                v.multishot = ms;
                                v.core.multishot = ms;
                            });
                            if self.stats[part].multishot == 0.0 {
                                continue;
                            }
                        }
                        let delay = pmax(0.0, self.stats[part].impact_delay);
                        if quanta.is_some() { quanta_last_impact = pmax(quanta_last_impact, delay); }
                        if delay != 0.0 {
                            let generation = if self.stats[part].persist_after_kill { None } else { Some(generation) };
                            self.push(t + delay, Kind::DelayedRadial(part, sniper_factor), generation);
                        } else {
                            self.radial_hit(part, sniper_factor, t, true)?;
                        }
                    }
                    if let Some((part,rate,duration))=owned_embed {
                        if self.gen==generation {
                            for tick in 0..ceil_i(duration*rate-1e-9).max(0) {
                                self.push(t+tick as f64/rate,Kind::OwnedEmbed(self.source_field_token,part),Some(generation));
                            }
                        }
                    }
                    if let Some((embed_delay, _)) = self.stats[active].zymos {
                        let parent_weight = self.head * self.stats[active].multishot;
                        if parent_weight > 0.0 {
                            self.push(t + embed_delay, Kind::ZymosCarrier(active, parent_weight), Some(generation));
                        }
                    }
                    if self.form_burst().0.is_some() {
                        if hit_count > 0.0 && burst_round == burst_size - 1 {
                            self.signal("on-burst-hit", t, 1.0);
                        }
                        burst_head_probability *= 1.0 - pow(1.0 - aim, hit_count);
                        if burst_round == burst_size - 1 {
                            self.signal("on-burst-headshot", t, burst_head_probability);
                            burst_head_probability = 1.0;
                        }
                    }
                    if nox_projectile.is_none() && !no_contact && self.physical_lane_length==0 && primary_delay == 0.0 && self.mines.is_none() && stack.is_none() {
                        self.enervate_hit(active, t, shot_puncture, aim);
                    }
                    if let Some((cost, fire_seconds, _)) = quanta {
                        self.push(t + fire_seconds, Kind::QuantaPrimary(cost), None);
                    }
                    self.signal("on-fire", t, 1.0);
                    let delayed = primary_delay != 0.0 || self.mines.is_some() || stack.is_some();
                    let through = if no_contact { 0.0 } else { self.punchthrough };
                    if through != 0.0 {
                        self.signal("on-punchthrough", t, through);
                        if self.form == BASE || !self.selected_incarnon {
                            self.signal("on-onos-damage-condition", t, 1.0);
                        }
                        if through >= 2.0 {
                            self.signal("on-three-enemy-punchthrough", t, 1.0);
                        }
                    }
                    if let Some((alignment, _, _)) = self.onos {
                        if self.incarnon_form() && self.onos_ticks as f64 >= alignment {
                            self.onos_condition_hits += hit_count;
                            let completed = (self.onos_condition_hits / 2.0).floor();
                            self.onos_condition_hits -= completed * 2.0;
                            if completed != 0.0 {
                                self.signal("on-onos-damage-condition", t, completed);
                            }
                        }
                    }
                    if nox_projectile.is_none() && !no_contact && !delayed && self.physical_lane_length==0 {
                    self.signal("on-hit", t, hit_count);
                    if aim < 1.0 {
                        self.signal("on-body-hit", t, (1.0 - aim) * hit_count);
                    }
                    self.signal("on-headshot", t, aim * hit_count);
                    self.consecutive_heads(aim);
                    let s = &self.stats[active];
                    let crit = pmin(
                        (1.0 - aim) * pmin(pmax(s.crit_chance * s.bodyshot_crit_multiplier, 0.0), 1.0)
                            + aim
                                * pmin(
                                    pmax(
                                        s.crit_chance
                                            + (s.weakpoint_crit_chance_bonus + s.base_crit_chance * s.weakpoint_crit_chance_relative),
                                        0.0,
                                    ),
                                    1.0,
                                ),
                        1.0,
                    ) * hit_count;
                    self.signal("on-crit", t, crit);
                    }
                    if let Some(c) = self.cycle {
                        let a = &self.stats[active];
                        if self.form == BASE && !a.radial_only && c.charge_on != 2 {
                            let per = if c.charge_on == 1 { 1.0 } else { self.head };
                            self.gauge = pmin(c.hits, self.gauge + per * a.multishot * a.incarnon_charge_multiplier);
                        }
                    }
                    let s = &self.stats[active];
                    let cost = if self.incarnon_form() {
                        s.charge_pool_cost + s.ammo_pool_multishot_cost
                    } else if s.bow_draw.is_some() && s.ammo_cost == 0.0 {
                        1.0
                    } else {
                        pmax(s.ammo_cost, 0.0)
                    };
                    self.ammo -= cost;
                    if self.reload_after_every_shot && magazine_form {
                        self.ammo = pmin(self.ammo, 0.0);
                    }
                    let magazine_now = self.stats[active].magazine;
                    if let Some(restore) = self.restore.clone() {
                        if !no_contact && (restore.kind == "on-headshot" || restore.kind == "on-hit") && magazine_form {
                            let eligible = match restore.victim_proc_type {
                                None => 1.0,
                                Some(d) => self.victim_before.iter().find(|x| x.0 == d).map(|x| x.1).unwrap_or(0.0),
                            };
                            if eligible != 0.0 {
                                let probability = if restore.kind == "on-headshot" { 1.0 - pow(1.0 - aim, hit_count) } else { 1.0 };
                                let events = if restore.once_per_shot { 1.0 } else { hit_count };
                                let restored = (pmax(0.0, magazine_now - self.ammo) * restore.fraction + restore.amount * events)
                                    * probability * restore.chance * eligible;
                                self.ammo = pmin(magazine_now, self.ammo + restored);
                            }
                        }
                    }
                    if self.pending_restore != 0.0 {
                        self.ammo += pmax(0.0, magazine_now - self.ammo) * self.pending_restore;
                        self.pending_restore = 0.0;
                    }
                    self.shot_in_progress = false;
                    let s = &self.stats[active];
                    let mut delay = match s.bow_draw {
                        Some(v) => v,
                        None => native_refire_delay.unwrap_or_else(|| 1.0 / speed) + if s.charge_before_fire || (self.form == INCARNON
                            && self.cycle.map(|c| c.charge_before_fire).unwrap_or(false)) {
                                0.0
                            } else { s.charge_time },
                    };
                    if let (Some((alignment, post_fire, _)), true) = (self.onos, self.form != BASE) {
                        if self.onos_ticks as f64 >= alignment {
                            self.onos_ticks = 0;
                            self.onos_hits = 0.0;
                            delay = post_fire;
                            self.onos_start = t + delay;
                        } else {
                            self.onos_ticks += 1;
                            if self.onos_crits {
                                self.onos_hits = pmin(50.0, self.onos_hits + hit_count);
                            }
                        }
                    }
                    delay += projectile_wait;
                    if let Some((_, _, cycle)) = quanta {
                        delay = pmax(delay, pmax(cycle, quanta_last_impact));
                    }
                    if delay == 0.0 && self.ammo > 0.0 {
                        delay = if s.reload_time != 0.0 { s.reload_time } else { 1.0 / speed };
                    }
                    if let Some(hit) = melee_hit {
                        let tier_before = self.melee.as_ref().unwrap().tier(self.melee_count);
                        let s = &self.stats[active];
                        let gained = hit.combo_hits * (1.0 + pmax(s.combo_extra_chance, 0.0));
                        self.melee_count += gained * pmin(pmax(1.0 - s.combo_miss_chance, 0.0), 1.0);
                        if hit.consumes {
                            let retained = pmin(pmax(s.heavy_attack_efficiency, 0.0), 0.9) as f32;
                            let cost = 1.0_f32-retained;
                            if cost >= 1.0 {
                                self.melee_count = 0.0;
                            } else {
                                let rounded = (pmax(0.0,self.melee_count) as f32 + 0.5_f32).floor();
                                let spent = (rounded * cost).floor();
                                self.melee_count = pmax(0.0,(self.melee_count as f32-spent) as f64);
                            }
                        }
                        let cap = self.melee.as_ref().unwrap().cap;
                        self.melee_count = pmin(self.melee_count, cap);
                        self.melee_last_hit = t;
                        self.melee_combo_at = t;
                        self.melee_recovery_token += 1;
                        // 近战事件包括重击命中、连击档位提升和重置。
                        let tier_after = self.melee.as_ref().unwrap().tier(self.melee_count);
                        if hit.heavy {
                            self.signal("on-melee-heavy-hit", t, 1.0);
                        }
                        if tier_after > tier_before {
                            self.signal("on-combo-tier-increase", t, (tier_after - tier_before) as f64);
                        } else if tier_after < tier_before {
                            self.signal("on-combo-tier-reset", t, 1.0);
                        }
                        if hit.phase & 2 != 0 {
                            self.signal("on-melee-slam", t, 1.0);
                        }
                        if self.hit_spec(hit.id)?.throw {
                            // 奋力一掷在循环中缓存每次投掷事件。
                            self.signal("on-glaive-throw", t, 1.0);
                        }
                        let mut after = active;
                        let mut speed = speed;
                        let now_state: Option<(Vec<u64>, i64)> = self.melee.as_ref().map(|spec| {
                            (self.sources.iter().map(|q| q.stacks.to_bits()).collect(), spec.tier(self.melee_count) as i64)
                        });
                        if now_state != speed_state {
                            // 若本次命中改变增益，下次挥击读取新的速度。
                            after = self.model(t)?.0;
                            speed = self.stats[after].fire_rate;
                        }
                        let charge = self.stats[after].heavy_charge_speed;
                        let projectile = self.stats[after].projectile_speed_multiplier;
                        let (next, wait) = self.melee_next(speed, charge, projectile)?;
                        self.melee_pending = Some(next);
                        delay = wait;
                        let initial=pmin((pmax(0.0,self.stats[after].initial_combo) as f32).trunc() as f64,self.melee.as_ref().unwrap().cap);
                        self.schedule_melee_recovery(t,initial);
                    }
                    let (form_burst, form_burst_rate) = self.form_burst();
                    if let Some((_, _, delay_seconds, ignore_rate)) = form_burst {
                        burst_round += 1;
                        if burst_round < burst_size && self.ammo > 0.0 {
                            let rate = match form_burst_rate {
                                Some(r) if r != 0.0 => r,
                                _ => speed,
                            };
                            delay = delay_seconds / if ignore_rate { 1.0 } else { pmax(1.0, speed / rate) };
                        } else {
                            burst_round = 0;
                        }
                    }
                    if let (Some((delay_seconds, _, batch_size)), false) = (self.mines,self.impact_embed) {
                        if self.pending_throws as f64 >= batch_size || self.ammo <= 1e-9 {
                            self.queue_mines(t);
                            delay = pmax(delay, delay_seconds);
                        }
                    }
                    if self.projectile_sequence_length > 0 {
                        self.projectile_sequence_index = (self.projectile_sequence_index+1)%self.projectile_sequence_length;
                    }
                    self.volley_shot_resume=None;
                    delay = pmax(delay, self.stats[active].native_fire_cooldown);
                    rate_spool_due = t + delay;
                    self.push(t + delay, Kind::Fire, None);
                }
            }
            if self.melee.is_some() { self.sync_melee_recovery(t)?; }
        }
        Ok(())
    }
}

// ── 回放入口 ──

/// 回放的固定参数，伤害类型 ID 由同一个 `Types` 实例分配。
#[derive(Default)]
pub struct TrajectoryInput {
    pub window: f64,
    pub rate: f64,
    pub forced_procs: Vector,
    pub faction_deltas: Vec<(D, f64)>,
    pub armour_bypass: Vec<D>,
    pub shield_bypass: Vec<D>,
    pub extra_hits: Vec<(D, f64)>,
    pub distance: Option<f64>,
    pub beam_floor: Option<f64>,
    pub burst: Option<(bool, i64, f64, bool)>,
    pub burst_rate: Option<f64>,
    pub victim_before: Vec<(D, f64)>,
    pub syndicate: Option<(String, D, f64)>,
    pub battery: Option<(f64, f64, f64)>,
    pub spool: Option<SpoolSpec>,
    pub rate_spool: Option<RateSpoolSpec>,
    pub shell: Option<(f64, f64, f64)>,
    pub shot_combo: Option<ShotCombo>,
    pub restore: Option<Restore>,
    pub target_faction: String,
    pub tables: TargetTables,
    pub melee: Option<MeleeSpec>,
    pub fortifier: Option<(f64, f64, f64, f64)>,
    pub empowered: Option<(f64, f64)>,
    pub status_pool: Vec<D>,
    pub cryogenic: Option<(f64, f64)>,
    pub encumber: Option<(f64, f64)>,
    pub projectile_state: stackable::State,
    pub cycle: Option<CycleSpec>,
    pub shell_mode_switch: bool,
    pub tap: Option<(f64, f64, f64)>,
    pub field_duration: Option<f64>,
    pub sources: Vec<SourceInput>,
    pub types: Types,
    pub head: f64,
    pub status_rate_multiplier: f64,
    pub damage_rate_multiplier: f64,
    pub full: [f64; 3],
    pub ehp: f64,
    pub armour_base: f64,
    pub elemental_armour_strip: f64,
    pub extra_scale: f64,
    pub weapon_slot: String,
    pub punchthrough: f64,
    pub live_types_enabled: bool,
    pub infinite_ammo: bool,
    pub reload_after_every_shot: bool,
    pub refill_growth: bool,
    pub faction_bracket: f64,
    pub selected_incarnon: bool,
    pub projectile_sequence_length: usize,
    pub physical_lane_length: usize,
    pub physical_lane_phase: usize,
    pub lane_stratum: f64,
    pub affinity_per_kill: f64,
    pub weak_draw: Option<(bool, f64, f64, bool)>,
    pub puncture_armor_strip: f64,
    pub mines: Option<(f64, f64, f64)>,
    pub life_curve: bool,
    pub onos: Option<(f64, f64, f64)>,
    pub onos_crits: bool,
    pub impact_embed: bool,
    pub embedded_timing: (f64,f64,bool),
    pub burst_base: Option<BurstSpec>,
    pub burst_rate_base: Option<f64>,
    pub burst_incarnon: Option<BurstSpec>,
    pub burst_rate_incarnon: Option<f64>,
    pub beam_floor_base: Option<f64>,
    pub last_burst: bool,
    pub magnetic_proxy: Option<(f32, f32)>,
    pub magnetic_weakness: Option<f32>,
    pub magnetic_electric_immune: bool,
    pub blocks_radiation: bool,
    pub enervate: Option<EnervateInput>,
    pub puncture_cap: f64,
    pub initial: ObjectId,
    pub actor_events: Vec<(f64, f64, String)>,
    pub energy_rate: f64,
    pub score_only: bool,
}

pub struct DirectBakeStreamInput {
    pub trajectory: TrajectoryInput,
}

pub struct EnervateInput {
    pub reset_after: i64,
    pub min_interval: f64,
    pub fixed_stacks: Option<f64>,
    pub uptime: f64,
    pub ignore_selected: bool,
}

impl From<EnervateInput> for Enervate {
    fn from(input: EnervateInput) -> Self {
        let uptime = pmin(1.0, pmax(0.0, input.uptime));
        let mut states = vec![((0i64, 0i64), 1.0)];
        if let Some(value) = input.fixed_stacks {
            let value = pmax(0.0, value);
            let lo = value.trunc();
            let frac = value - lo;
            states = vec![((lo as i64, 0), 1.0 - frac)];
            if frac != 0.0 {
                states.push(((lo as i64 + 1, 0), frac));
            }
        }
        Self {
            reset_after: input.reset_after,
            min_interval: input.min_interval,
            fixed: input.fixed_stacks.is_some(),
            uptime,
            ignore_selected: input.ignore_selected,
            last: f64::NEG_INFINITY,
            hits: 0.0,
            resets: 0.0,
            states,
            timed_states: None,
        }
    }
}

fn prepare_replay(input: TrajectoryInput, provider: &dyn ReplayProvider) -> ReplayResult<Replay<'_>> {
    let setup_timer = Timer::start(counters::SETUP);
    let mut types = input.types;
    let window = input.window;
    let rate = input.rate;
    let full = input.full;
    let elements = {
        let mut id = |n: &str| types.id(n);
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
    };
    let mut replay = Replay {
        provider,
        score_only: input.score_only,
        window,
        head: input.head,
        rate,
        status_rate_multiplier: input.status_rate_multiplier,
        damage_rate_multiplier: input.damage_rate_multiplier,
        full,
        ehp: input.ehp,
        armour_base: input.armour_base,
        elemental_armour_strip: input.elemental_armour_strip,
        forced_procs: input.forced_procs,
        faction_deltas: input.faction_deltas,
        armour_bypass: input.armour_bypass,
        shield_bypass: input.shield_bypass,
        extra_hits: input.extra_hits,
        extra_scale: input.extra_scale,
        weapon_slot: input.weapon_slot,
        distance: input.distance,
        punchthrough: input.punchthrough,
        live_types_enabled: input.live_types_enabled,
        beam_floor: input.beam_floor,
        burst: input.burst,
        burst_rate: input.burst_rate,
        infinite_ammo: input.infinite_ammo,
        victim_before: input.victim_before,
        syndicate: input.syndicate,
        battery: input.battery,
        spool: input.spool,
        rate_spool: input.rate_spool,
        shell: input.shell,
        shot_combo: input.shot_combo,
        reload_after_every_shot: input.reload_after_every_shot,
        refill_growth: input.refill_growth,
        restore: input.restore,
        pending_restore: 0.0,
        recharges: 0,
        delayed_attacks: 0,
        reload_shell_events: Vec::new(),
        derived: FxHashMap::default(),
        target_faction: input.target_faction,
        faction_bracket: input.faction_bracket,
        tables: input.tables,
        selected_incarnon: input.selected_incarnon,
        melee: input.melee,
        projectile_sequence_length: input.projectile_sequence_length,
        projectile_sequence_index: 0,
        source_field_token:0,source_field_packets:Vec::new(),source_field_activations:0,finite_field_packets:Vec::new(),finite_field_activations:0,
        physical_lane_length: input.physical_lane_length,
        physical_lane_phase: input.physical_lane_phase,
        physical_lane_selected:None,lane_stratum: input.lane_stratum,
        volley_rounds: 1.0,
        volley_shot_resume: None,
        melee_count: 0.0,
        melee_heavy_multiplier: 1.0,
        melee_last_hit: f64::NEG_INFINITY,
        melee_combo_at: 0.0,
        melee_recovery_token: 0,
        melee_recovery_initial: 0.0,
        melee_buff_boundary: f64::INFINITY,
        melee_pending: None,
        melee_index: 0,
        heavy_flag: false,
        no_co_flag: false,
        melee_heavy_now: false,
        affinity_per_kill: input.affinity_per_kill,
        affinity: 0.0,
        syndicate_ready: 0.0,
        syndicate_events: Vec::new(),
        pools: full,
        states: FxIndexMap::default(),
        // 武器可提高穿刺叠层上限，例如 Evensong。
        pooled: [CappedPool::new(10.0), CappedPool::new(10.0), CappedPool::new(10.0),
                 CappedPool::new(input.puncture_cap)],
        tau: None,
        presence: FxIndexMap::default(),
        proc_piles: FxIndexMap::default(),
        dots: Vec::new(),
        dot_modes: Vec::new(),
        dot_sources: Vec::new(),
        proc_source: None,
        heat_proc_source: None,
        heat_mode_sources: [None; 2],
        damage_origin: None,
        blasts: Vec::new(),
        blast_procs: VecDeque::new(),
        heat_acc: 0.0,
        heat_token: 0,
        heat_chains: HeatChains::new(),
        heat_sources: [HeatChains::new(),HeatChains::new()],
        heat_source_acc: [0.0;2],
        attribution: KillAttribution::new(
            pmax(1.0, if rate > 0.0 { 1.0 / rate } else { 0.0 }),
            if rate > 0.0 { Some(1.0 / rate) } else { None },
        ),
        track_weak_kills: true,
        puncture_strip: 0.0,
        frozen_until: -1.0,
        heap: BinaryHeap::new(),
        seq: 0,
        gen: 0,
        kills: 0,
        weak_kill_events: 0.0,
        weak_draw: input.weak_draw,
        weak_index: 0.0,
        weak_uncertainty: 0.0,
        attacks: 0,
        stale: 0,
        first_kill: None,
        last_kill: None,
        min_interval: None,
        totals: [0.0; 5],
        fortifier: input.fortifier,
        fortifier_damage: 0.0,
        fortifier_stolen: 0.0,
        self_overguard: 0.0,
        empowered: input.empowered,
        status_pool: input.status_pool,
        empowered_count: 0.0,
        empowered_by_type: FxIndexMap::default(),
        cryogenic: input.cryogenic,
        cryogenic_expected: 0.0,
        puncture_armor_strip: input.puncture_armor_strip,
        encumber: input.encumber,
        encumber_time: f64::NEG_INFINITY,
        encumber_remaining: 1.0,
        encumber_expected: 0.0,
        enervate: input.enervate.map(Enervate::from),
        mines: input.mines,
        life_curve: input.life_curve.then(Vec::new),
        onos: input.onos,
        onos_crits: input.onos_crits,
        onos_ticks: 0,
        onos_hits: 0.0,
        onos_start: 0.0,
        onos_condition_hits: 0.0,
        projectiles: None,
        projectile_state: input.projectile_state,
        stack_profiles: FxHashMap::default(),
        impact_embed: input.impact_embed,
        embedded_timing: input.embedded_timing,
        embedded_entities: Vec::new(),
        pending_mines: Vec::new(),
        pending_throws: 0,
        waiting_mines: 0.0,
        detonations: 0,
        mine_overflows: 0,
        form: if input.cycle.is_some() { BASE } else { SELECTED },
        cycle: input.cycle,
        gauge: 0.0,
        base_ammo_saved: None,
        transforms: 0,
        shells_owed: 0.0,
        shell_mode_switch: input.shell_mode_switch,
        burst_base: input.burst_base,
        burst_rate_base: input.burst_rate_base,
        burst_incarnon: input.burst_incarnon,
        burst_rate_incarnon: input.burst_rate_incarnon,
        beam_floor_base: input.beam_floor_base,
        last_burst: input.last_burst,
        tap_other: (0.0, 0.0),
        tap: input.tap,
        tap_hits: 0.0,
        tap_expires: f64::NEG_INFINITY,
        field_duration: input.field_duration,
        field_duration_ready: false,
        field_duration_used: 0,
        enervate_views: FxHashMap::default(),
        enervate_weights: FxHashMap::default(),
        proc_counts: FxIndexMap::default(),
        dot_by_type: FxIndexMap::default(),
        magnetic_breaks: Vec::new(),
        magnetic_proxy: input.magnetic_proxy,
        magnetic_weakness: input.magnetic_weakness,
        magnetic_electric_immune: input.magnetic_electric_immune,
        blocks_radiation: input.blocks_radiation,
        sources: input.sources.into_iter().map(Source::from).collect(),
        ammo: 0.0,
        shot_in_progress: false,
        reloads: 0,
        last_round: false,
        current_time: 0.0,
        beam_progress: 0.0,
        beam_last: None,
        actor_signal: false,
        actor_signal_used: false,
        armour_view: None,
        stats: Vec::new(),
        views: FxHashMap::default(),
        co_specs: FxHashMap::default(),
        model_cache: FxHashMap::default(),
        model_key: Vec::new(),
        tick_chains: FxHashMap::default(),
        varied: FxHashMap::default(),
        elements,
        hit_specs: Vec::new(),
        applied: FxHashMap::default(),
        packets: FxHashMap::default(),
        spreads: FxHashMap::default(),
        no_status: FxHashMap::default(),
        proc_names: Vec::new(),
        types,
        frozen_scale: 1.0,
    };
    if replay.score_only {
        // 仅计分时，若击杀归因影响分支、弹药恢复或触发来源的生命周期，仍保留归因。
        let signal = "on-headshot-kill";
        replay.track_weak_kills = replay.weak_draw.is_some()
            || replay.restore.as_ref().is_some_and(|r| r.kind == signal)
            || replay.sources.iter().any(|s| s.kind.as_deref() == Some(signal)
                || s.consume_on.as_deref() == Some(signal) || s.lock_on.as_deref() == Some(signal)
                || s.toggle_on.as_deref() == Some(signal) || s.reset_on.iter().any(|r| r == signal));
    }
    // 先读取初始属性与弹药，再按参考顺序调度事件。
    replay.view(input.initial)?;
    let (active, _) = replay.model(0.0)?;
    replay.ammo = replay.stats[active].magazine;
    replay.push(0.0, Kind::Fire, None);
    for (first, interval, kind) in input.actor_events {
        let total = if interval > 0.0 { trunc_i(pmax(0.0, window - first) / interval) + 1 } else { 1 };
        for i in 0..total.min(10000) {
            replay.push(first + i as f64 * interval, Kind::Actor(kind.clone()), None);
        }
    }
    let energy_rate = input.energy_rate;
    for i in 0..replay.sources.len() {
        let threshold = replay.sources[i].energy_threshold;
        if threshold != 0.0 && energy_rate > 0.0 {
            let interval = threshold / energy_rate;
            for n in 1..trunc_i(window / interval) + 1 {
                replay.push(n as f64 * interval, Kind::Energy(i), None);
            }
        }
    }
    drop(setup_timer);
    Ok(replay)
}

pub struct HealthBatchOutput {
    pub kpm: f64,
    pub dps: f64,
    pub damage: f64,
    pub mine_overflows: i64,
}

pub fn validate_health_targets(targets: &[([f64; 3], f64)]) -> ReplayResult<()> {
    if targets.is_empty() || targets.len() > 64 {
        return Err(invalid_value("health batch requires 1..64 targets"));
    }
    if targets.iter().any(|(pools, ehp)| !ehp.is_finite() || *ehp < 0.0
        || pools.iter().any(|v| !v.is_finite() || *v < 0.0)) {
        return Err(invalid_value("health batch requires finite nonnegative pools"));
    }
    Ok(())
}

pub fn trajectory_health_batch(input: TrajectoryInput, provider: &dyn ReplayProvider, targets: Vec<([f64; 3], f64)>)
    -> ReplayResult<Vec<HealthBatchOutput>> {
    let _timer = Timer::start(counters::TOTAL);
    validate_health_targets(&targets)?;
    let prepared = prepare_replay(input, provider)?;
    if prepared.weak_draw.is_some() || prepared.life_curve.is_some() {
        return Err(unsupported("health batch needs an ordinary score replay"));
    }
    let mut result = Vec::with_capacity(targets.len());
    for (full, ehp) in targets {
        provider.check_signals()?;
        let mut replay = prepared.clone();
        replay.full = full;
        replay.pools = full;
        replay.ehp = ehp;
        replay.run()?;
        if replay.proc_piles.values().any(|p| p.radial_floor_limited) {
            return Err(unsupported("electric-gas-radial-floor-state-limit"));
        }
        let score = replay.kills as f64 + if ehp != 0.0 { 1.0 - psum(replay.pools) / ehp } else { 0.0 };
        let damage = psum(replay.totals);
        let kpm = if replay.window != 0.0 { score * 60.0 / replay.window } else { 0.0 };
        let dps = if replay.window != 0.0 { damage / replay.window } else { 0.0 };
        result.push(HealthBatchOutput { kpm, dps, damage, mine_overflows: replay.mine_overflows });
    }
    Ok(result)
}

/// 收集固定目标状态下的独立直伤序列。
pub fn direct_bake_stream(input: DirectBakeStreamInput, provider: &dyn ReplayProvider)
    -> ReplayResult<(Vec<f64>, f64, f64)> {
    let mut replay = prepare_replay(input.trajectory, provider)?;
    if replay.head != 0.0 || replay.full[0] != 0.0 || replay.full[1] != 0.0
        || !replay.sources.is_empty() || replay.melee.is_some() || replay.cycle.is_some()
        || replay.shot_combo.is_some() || replay.restore.is_some() || replay.syndicate.is_some()
        || replay.empowered.is_some() || replay.cryogenic.is_some() || replay.encumber.is_some()
        || replay.enervate.is_some() || replay.onos.is_some() || replay.mines.is_some()
        || replay.projectiles.is_some() || replay.fortifier.is_some() || replay.weak_draw.is_some()
        || replay.live_types_enabled || !replay.extra_hits.is_empty() || !replay.forced_procs.is_empty()
        || replay.life_curve.is_some() || replay.puncture_armor_strip != 0.0
        || !replay.victim_before.is_empty() || replay.field_duration.is_some() {
        return Err(unsupported("GPU stream requires independent fixed-state direct hits"));
    }
    let (active, radial) = replay.model(0.0)?;
    let st = &replay.stats[active];
    if radial.is_some() || st.status_chance > 0.0 || !st.native_forced_procs.is_empty()
        || !st.independent_proc_chances.is_empty() || st.has_crit_procs
        || !st.core.impact_proc_conversions.is_empty() || !st.core.extra_hits.is_empty()
        || st.impact_delay != 0.0 || st.primary_delay != 0.0 || st.lingering.is_some()
        || !st.additional.is_empty() || st.persist_after_kill || st.stackable.is_some()
        || !st.reload_empty.is_empty() || !st.reload_partial.is_empty()
        || st.victim_full_crit_chance != 0.0 || st.victim_full_crit_damage != 0.0
        || st.victim_low_health_multiplier != 1.0 || st.continuous
        || st.head_only || st.body_only || st.radial_only || !st.co_bucket.is_empty() {
        return Err(unsupported("GPU stream cannot discard statuses or target-dependent parts"));
    }
    let health = replay.full[2];
    replay.full = [0.0, 0.0, 1e250];
    replay.pools = replay.full;
    replay.ehp = 1e250;
    replay.life_curve = Some(Vec::new());
    replay.run()?;
    if replay.kills != 0 || replay.proc_counts.values().any(|v| *v != 0.0)
        || replay.totals[1..].iter().any(|v| *v != 0.0) {
        return Err(unsupported("GPU stream observed non-direct damage"));
    }
    let rows = replay.life_curve.as_ref().unwrap();
    if rows.len() > 100000 || rows.iter().any(|r| r.3 != DIRECT || !r.1.is_finite() || r.1 < 0.0) {
        return Err(unsupported("GPU stream event bounds exceeded"));
    }
    Ok((rows.iter().map(|r| r.1).collect(), health, replay.window))
}


pub type NamedDamage = Vec<(String, f64)>;
pub type LifeCurve = Vec<(f64, f64, f64, usize)>;

pub struct TrajectoryOutput {
    pub kills: i64,
    pub attacks: i64,
    pub pools: [f64; 3],
    pub totals: [f64; 5],
    pub weak_uncertainty: f64,
    pub weak_index: f64,
    pub mine_overflows: i64,
    pub last_round: bool,
    pub victim_before: NamedDamage,
    pub actor_signal_used: bool,
    pub source_stacks: Vec<f64>,
    pub details: Option<TrajectoryDetails>,
}

pub struct TrajectoryDetails {
    pub fortifier: (f64, f64, f64),
    pub empowered: (f64, NamedDamage),
    pub cryogenic_expected: f64,
    pub encumber_expected: f64,
    pub transforms: i64,
    pub life_curve: Option<LifeCurve>,
    pub projectiles: Option<(f64, f64, i64, i64, i64)>,
    pub mines: (i64, f64, f64, i64),
    pub field_duration: (i64, bool),
    pub enervate: Option<(Vec<(i64, i64, f64)>, f64, f64, f64)>,
    pub gen: i64,
    pub weak_kill_events: f64,
    pub finite_field_packets: Vec<u64>,
    pub finite_field_activations: u64,
    pub source_field_packets: Vec<u64>,
    pub source_field_activations: u64,
    pub physical_projectile_phase: Option<usize>,
    pub stale: i64,
    pub first_kill: Option<f64>,
    pub min_interval: Option<f64>,
    pub proc_counts: NamedDamage,
    pub dot_by_type: NamedDamage,
    pub magnetic_breaks: Vec<(f64, &'static str, f64, f64, i64)>,
    pub reloads: i64,
    pub recharges: i64,
    pub delayed_attacks: i64,
    pub reload_shell_events: Vec<(f64, f64, &'static str, &'static str)>,
    pub syndicate_events: Vec<(f64, String, f64)>,
}

pub fn trajectory(input: TrajectoryInput, provider: &dyn ReplayProvider) -> ReplayResult<TrajectoryOutput> {
    let _timer = Timer::start(counters::TOTAL);
    let mut replay = prepare_replay(input, provider)?;
    replay.run()?;
    if replay.proc_piles.values().any(|p| p.radial_floor_limited) {
        return Err(unsupported("electric-gas-radial-floor-state-limit"));
    }
    let _result_timer = Timer::start(counters::RESULT);
    let names = |values: &FxIndexMap<D, f64>| -> NamedDamage {
        values.iter().map(|(&key, &value)| (replay.types.name(key).to_owned(), value)).collect()
    };
    let details = (!replay.score_only).then(|| {
        let pending = psum(replay.pending_mines.iter().map(|m| replay.stats[m.0].multishot));
        TrajectoryDetails {
            fortifier: (replay.fortifier_damage, replay.fortifier_stolen, replay.self_overguard),
            empowered: (replay.empowered_count, names(&replay.empowered_by_type)),
            cryogenic_expected: replay.cryogenic_expected,
            encumber_expected: replay.encumber_expected,
            transforms: replay.transforms,
            life_curve: replay.life_curve,
            projectiles: replay.projectiles.map(|p| (p.deployed, p.merged, p.explosions, p.singularities, p.child_projectiles)),
            mines: (replay.detonations, pending, replay.waiting_mines, replay.mine_overflows),
            field_duration: (replay.field_duration_used, replay.field_duration_ready),
            enervate: replay.enervate.map(|en| {
                let states = en.states.into_iter().map(|((s, b), p)| (s, b, p)).collect();
                (states, en.hits, en.resets, en.last)
            }),
            gen: replay.gen,
            weak_kill_events: replay.weak_kill_events,
            finite_field_packets: replay.finite_field_packets,
            finite_field_activations: replay.finite_field_activations,
            source_field_packets: replay.source_field_packets,
            source_field_activations: replay.source_field_activations,
            physical_projectile_phase: (replay.physical_lane_length > 0).then_some(replay.physical_lane_phase),
            stale: replay.stale,
            first_kill: replay.first_kill,
            min_interval: replay.min_interval,
            proc_counts: names(&replay.proc_counts),
            dot_by_type: names(&replay.dot_by_type),
            magnetic_breaks: replay.magnetic_breaks.into_iter().map(|(t, pool, stacks, per, gen)| {
                (t, if pool == 0 { "overguard" } else { "shield" }, stacks, per, gen)
            }).collect(),
            reloads: replay.reloads,
            recharges: replay.recharges,
            delayed_attacks: replay.delayed_attacks,
            reload_shell_events: replay.reload_shell_events.into_iter().map(|(t, rounds, source, form)| {
                (t, rounds, source, form_name(form))
            }).collect(),
            syndicate_events: replay.syndicate_events.into_iter().map(|(t, damage)| {
                (t, replay.syndicate.as_ref().map(|x| x.0.clone()).unwrap_or_default(), damage)
            }).collect(),
        }
    });
    Ok(TrajectoryOutput {
        kills: replay.kills,
        attacks: replay.attacks,
        pools: replay.pools,
        totals: replay.totals,
        weak_uncertainty: replay.weak_uncertainty,
        weak_index: replay.weak_index,
        mine_overflows: replay.mine_overflows,
        last_round: replay.last_round,
        victim_before: replay.victim_before.iter().map(|&(key, value)| (replay.types.name(key).to_owned(), value)).collect(),
        actor_signal_used: replay.actor_signal_used,
        source_stacks: replay.sources.iter().map(|q| q.stacks).collect(),
        details,
    })
}
