//! Python 回放配置和回调的类型转换。

#![allow(deprecated)]

use std::cell::{OnceCell, RefCell};

use pyo3::exceptions::{PyIndexError, PyValueError, PyZeroDivisionError};
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList, PyTuple};
use wfcore_lib::{co, replay, stackable};
use wfcore_lib::fx::FxHashMap;
use wfcore_lib::replay::{
    BurstSpec, CycleSpec, D, DirectBakeStreamInput, EnervateInput, HitSpec, InvalidReplay,
    MeleeHit, MeleeSegment, MeleeSpec, ObjectId, RateSpoolSpec, ReplayError, ReplayProvider,
    ReplayResult, ResolveInput, Restore, ShotCombo, SourceInput, SourceStacks, SpoolSpec,
    StackKey, StackValue, Stats, StatsLinks, TrajectoryInput, TrajectoryOutput, Types, Vector, ViewContext,
};
use wfcore_lib::tables::{Core, Location, TargetTables};

pyo3::create_exception!(wfcore, NativeUnsupported, pyo3::exceptions::PyException);
pyo3::create_exception!(wfcore, ReplayRunaway, pyo3::exceptions::PyException);

/// 保持字典顺序，将伤害类型名称登记为 ID。
pub(crate) fn vector_of(types: &mut Types, obj: &Bound<'_, PyAny>) -> PyResult<Vector> {
    let d = obj.downcast::<PyDict>()?;
    let mut out = Vec::with_capacity(d.len());
    for (k, v) in d.iter() {
        let name: String = k.extract()?;
        out.push((types.id(&name), v.extract::<f64>()?));
    }
    Ok(out)
}

/// 提取伤害表使用的 `ModdedStats` 字段以及目标阵营对应的乘数。
pub(crate) fn core_of(obj: &Bound<'_, PyAny>, types: &mut Types, target_faction: &str, faction_bracket: f64, damage_types: &Vector,
                  native_forced_procs: &Vector, independent_proc_chances: &Vector) -> PyResult<Core> {
    let factions = obj.getattr("faction_damage_multipliers")?;
    let factions = factions.cast::<PyDict>()?;
    let faction_all = match factions.get_item("")? { Some(v) => v.extract::<f64>()?, None => 1.0 };
    let faction_target = if target_faction.is_empty() {
        1.0
    } else {
        match factions.get_item(target_faction)? { Some(v) => v.extract::<f64>()?, None => 1.0 }
    };
    let dot = obj.getattr("dot_base_damage")?;
    let mut conversions = Vec::new();
    for (k, adapters) in obj.getattr("impact_proc_conversions")?.cast::<PyDict>()?.iter() {
        let d = types.id(&k.extract::<String>()?);
        let mut rows = Vec::new();
        for adapter in adapters.try_iter()? {
            let adapter = adapter?;
            let a = adapter.cast::<PyDict>()?;
            rows.push((get_f(a, "chance", 0.0)?, get_f(a, "fireRateThreshold", f64::INFINITY)?,
                       get_f(a, "fireRateMultiplier", 1.0)?));
        }
        conversions.push((d, rows));
    }
    let mut extra_hits = Vec::new();
    for e in obj.getattr("extra_hits")?.try_iter()? {
        let e = e?;
        let t = e.cast::<PyTuple>()?;
        extra_hits.push((types.id(&t.get_item(0)?.extract::<String>()?), t.get_item(1)?.extract::<f64>()?,
                         t.get_item(2)?.is_truthy()?));
    }
    let low_threshold = obj.getattr("low_crit_threshold")?;
    let low_base = obj.getattr("low_crit_damage_base")?;
    Ok(Core {
        damage_types: damage_types.clone(),
        dot_base_damage: if dot.is_none() { None } else { Some(dot.extract::<f64>()?) },
        crit_chance: f(obj, "crit_chance")?,
        crit_multiplier: f(obj, "crit_multiplier")?,
        noncrit_damage_multiplier: f(obj, "noncrit_damage_multiplier")?,
        double_crit_chance: f(obj, "double_crit_chance")?,
        falloff_multiplier: f(obj, "falloff_multiplier")?,
        faction_all,
        faction_target,
        faction_bracket,
        multishot: f(obj, "multishot")?,
        continuous: b(obj, "continuous")?,
        radial_only: b(obj, "radial_only")?,
        ignore_headshot_bonus: b(obj, "ignore_headshot_bonus")?,
        area_direct: b(obj, "area_direct")?,
        bodyshot_crit_multiplier: f(obj, "bodyshot_crit_multiplier")?,
        weakpoint_crit_chance_bonus: f(obj, "weakpoint_crit_chance_bonus")?,
        base_crit_chance: f(obj, "base_crit_chance")?,
        weakpoint_crit_chance_relative: f(obj, "weakpoint_crit_chance_relative")?,
        headshot_bonus_multiplier: f(obj, "headshot_bonus_multiplier")?,
        weakpoint_damage_bonus: match opt_attr(obj, "weakpoint_damage_bonus")? { Some(v) => v.extract::<f64>()?, None => 0.0 },
        status_chance: f(obj, "status_chance")?,
        proc_duration_multiplier: f(obj, "proc_duration_multiplier")?,
        proc_duration_types: match opt_attr(obj, "proc_duration_types")? {
            Some(v) => vector_of(types, &v)?,
            None => Vec::new(),
        },
        proc_max_stacks: match opt_attr(obj, "proc_max_stacks")? {
            Some(v) => vector_of(types, &v)?,
            None => Vec::new(),
        },
        proc_damage_multiplier: f(obj, "proc_damage_multiplier")?,
        native_forced_procs: native_forced_procs.clone(),
        independent_proc_chances: independent_proc_chances.clone(),
        crit_proc_chances: vector_of(types, &obj.getattr("crit_proc_chances")?)?,
        impact_proc_conversions: conversions,
        fire_rate: f(obj, "fire_rate")?,
        dot_elemental_multipliers: vector_of(types, &obj.getattr("dot_elemental_multipliers")?)?,
        extra_hits,
        low_crit_threshold: if low_threshold.is_none() { None } else { Some(low_threshold.extract::<f64>()?) },
        low_crit_damage_base: if low_base.is_none() { None } else { Some(low_base.extract::<f64>()?) },
        low_crit_damage_bonus: f(obj, "low_crit_damage_bonus")?,
    })
}

const UNSUPPORTED_ATTRS: [&str; 1] = ["_mine_extra"];
/// 从 ModEffect 序列提取（操作码，值）。
fn effect_ops(list: &Bound<'_, PyAny>) -> PyResult<Vec<(u8, f64)>> {
    let mut out = Vec::new();
    for e in list.try_iter()? {
        let e = e?;
        out.push((co::op_code(&e.getattr("operation")?.extract::<String>()?), f(&e, "value")?));
    }
    Ok(out)
}

pub(crate) fn f(o: &Bound<'_, PyAny>, name: &str) -> PyResult<f64> {
    o.getattr(name)?.extract::<f64>()
}
pub(crate) fn b(o: &Bound<'_, PyAny>, name: &str) -> PyResult<bool> {
    o.getattr(name)?.is_truthy()
}
pub(crate) fn opt_attr<'py>(o: &Bound<'py, PyAny>, name: &str) -> PyResult<Option<Bound<'py, PyAny>>> {
    match o.getattr(name) {
        Ok(v) => Ok(Some(v)),
        Err(e) if e.is_instance_of::<pyo3::exceptions::PyAttributeError>(o.py()) => Ok(None),
        Err(e) => Err(e),
    }
}

fn get_f(d: &Bound<'_, PyDict>, key: &str, default: f64) -> PyResult<f64> {
    match d.get_item(key)? {
        Some(v) if !v.is_none() => v.extract::<f64>(),
        _ => Ok(default),
    }
}
fn truthy(d: &Bound<'_, PyDict>, key: &str) -> PyResult<bool> {
    match d.get_item(key)? {
        Some(v) => v.is_truthy(),
        None => Ok(false),
    }
}

fn list_f(v: &Bound<'_, PyAny>) -> PyResult<Vec<f64>> {
    v.extract::<Vec<f64>>()
}

fn elements_from(types: &mut Types, obj: &Bound<'_, PyAny>) -> PyResult<Vec<co::Element>> {
    let mut out = Vec::new();
    for e in obj.try_iter()? {
        let e = e?;
        let dtype = e.getattr("damage_type")?;
        let dtype = if dtype.is_truthy()? {
            let name: String = dtype.extract()?;
            if name == "DT_ANY" { None } else { Some(types.id(&name)) }
        } else {
            None
        };
        out.push((dtype, f(&e, "value")?));
    }
    Ok(out)
}

/// 将异况超量配置中的类型名称转换为 ID。
fn co_spec_from(types: &mut Types, spec: &Bound<'_, PyAny>) -> PyResult<co::CoSpec> {
    let melee = b(spec, "melee")?;
    let opt = |name: &str| -> PyResult<Option<f64>> {
        let v = spec.getattr(name)?;
        Ok(if v.is_none() { None } else { Some(v.extract::<f64>()?) })
    };
    let base = co::Dict(vector_of(types, &spec.getattr("base")?)?);
    let mut innate = Vec::new();
    for e in spec.getattr("innate_entries")?.try_iter()? {
        let e = e?;
        let t = e.cast::<PyTuple>()?;
        let name: String = t.get_item(0)?.extract()?;
        let dtype = if name.is_empty() { None } else { Some(types.id(&name)) };
        innate.push((dtype, t.get_item(1)?.extract::<f64>()?));
    }
    let orb = spec.getattr("orb")?;
    let orb = if orb.is_none() {
        None
    } else {
        let t = orb.cast::<PyTuple>()?;
        Some((t.get_item(0)?.extract::<f64>()?, t.get_item(1)?.is_truthy()?,
              t.get_item(2)?.extract::<f64>()?, t.get_item(3)?.extract::<f64>()?))
    };
    let fin = spec.getattr("final")?;
    let final_damage = if fin.is_none() {
        None
    } else {
        let t = fin.cast::<PyTuple>()?;
        Some((t.get_item(0)?.extract::<f64>()?, co::Dict(vector_of(types, &t.get_item(1)?)?)))
    };
    Ok(co::CoSpec {
        victim_status_count: f(spec, "victim_status_count")?,
        damage_scale_effects: effect_ops(&spec.getattr("damage_scale_effects")?)?,
        ordinary_scale: f(spec, "ordinary_scale")?,
        compression_mult: f(spec, "compression_mult")?,
        gunco_independent: b(spec, "gunco_independent")?,
        gunco_fraction: opt("gunco_fraction")?,
        victim_damage: f(spec, "victim_damage")?,
        co_fraction: f(spec, "co_fraction")?,
        melee,
        melee_damage_effects: effect_ops(&spec.getattr("melee_damage_effects")?)?,
        slam_effects: effect_ops(&spec.getattr("slam_effects")?)?,
        base,
        base_total: f(spec, "base_total")?,
        elemental_base_total: f(spec, "elemental_base_total")?,
        pre: elements_from(types, &spec.getattr("pre")?)?,
        post: elements_from(types, &spec.getattr("post")?)?,
        innate,
        pool_factor: opt("pool_factor")?,
        copy_factor: opt("copy_factor")?,
        orb,
        final_damage,
        frozen_factor: opt("frozen_factor")?,
    })
}

fn extract_stats<'py>(
    obj: &Bound<'py, PyAny>,
    context: &mut ViewContext<'_>,
    provider: &PythonProvider<'py>,
) -> PyResult<Stats> {
    let types = &mut *context.types;
    for attr in UNSUPPORTED_ATTRS {
        if let Some(v) = opt_attr(obj, attr)? {
            if v.is_truthy()? {
                return Err(NativeUnsupported::new_err(attr));
            }
        }
    }
    let damage_types = vector_of(types, &obj.getattr("damage_types")?)?;
    let native_forced_procs = vector_of(types, &obj.getattr("native_forced_procs")?)?;
    let independent_proc_chances = vector_of(types, &obj.getattr("independent_proc_chances")?)?;
    let bow_draw = match opt_attr(obj, "_bow_draw")? {
        Some(v) => Some(v.extract::<f64>()?),
        None => None,
    };
    let charge_time = match opt_attr(obj, "_charge_time")? {
        Some(v) => v.extract::<f64>()?,
        None => 0.0,
    };
    let requires_full_ammo = match opt_attr(obj, "_requires_full_ammo")? {
        Some(v) => v.is_truthy()?,
        None => false,
    };
    let quanta = match opt_attr(obj, "_quanta_strategy")? {
        Some(v) => {
            let d = v.cast::<PyDict>()?;
            Some((get_f(d, "primaryAmmoCost", 0.0)?, get_f(d, "primaryFireSeconds", 0.25)?,
                  get_f(d, "cycleSeconds", 0.0)?))
        },
        None => None,
    };
    let core = core_of(obj, types, context.target_faction, context.faction_bracket, &damage_types, &native_forced_procs, &independent_proc_chances)?;
    Ok(Stats {
        core,
        magazine: f(obj, "magazine")?,
        ammo_cost: f(obj, "ammo_cost")?,
        fire_rate: f(obj, "fire_rate")?,
        reload_time: f(obj, "reload_time")?,
        transform_time_scale: f(obj, "transform_time_scale")?,
        stack_native_fraction: f(obj, "stack_native_fraction")?,
        stackable_hit_event: b(obj, "stackable_hit_event")?,
        delayed_radial_hit_event: b(obj, "delayed_radial_hit_event")?,
        multishot: f(obj, "multishot")?,
        continuous: b(obj, "continuous")?,
        radial_only: b(obj, "radial_only")?,
        radial_victim_crit: b(obj, "radial_victim_crit")?,
        crit_chance: f(obj, "crit_chance")?,
        crit_multiplier: f(obj, "crit_multiplier")?,
        bodyshot_crit_multiplier: f(obj, "bodyshot_crit_multiplier")?,
        weakpoint_crit_chance_bonus: f(obj, "weakpoint_crit_chance_bonus")?,
        base_crit_chance: f(obj, "base_crit_chance")?,
        weakpoint_crit_chance_relative: f(obj, "weakpoint_crit_chance_relative")?,
        status_chance: f(obj, "status_chance")?,
        proc_duration_multiplier: f(obj, "proc_duration_multiplier")?,
        proc_damage_multiplier: f(obj, "proc_damage_multiplier")?,
        damage_types,
        native_forced_procs,
        independent_proc_chances,
        has_crit_procs: b(obj, "crit_proc_chances")?,
        first_shot_damage_bonus: f(obj, "first_shot_damage_bonus")?,
        cold_stack_damage: f(obj, "cold_stack_damage")?,
        base_damage_bracket: f(obj, "base_damage_bracket")?,
        frozen_damage_multiplier: f(obj, "frozen_damage_multiplier")?,
        requires_full_ammo,
        quanta,
        contact_shiver_scale: match opt_attr(obj, "_contact_shiver_scale")? {Some(v)=>v.extract()?,None=>1.0},
        contact_shiver_fixed: opt_attr(obj,"_contact_shiver_fixed")?.filter(|v|!v.is_none()).map(|v|v.extract::<f64>()).transpose()?,
        no_enemy_contact:match opt_attr(obj,"_no_enemy_contact")? {Some(v)=>v.is_truthy()?,None=>false},
        contact_shiver: match opt_attr(obj, "_contact_shiver")? { Some(v) => v.is_truthy()?, None => false },
        snapshot_enervate: match opt_attr(obj, "_snapshot_enervate")? { Some(v) => v.is_truthy()?, None => false },
        zymos: match opt_attr(obj, "_zymos_policy")? {
            Some(v) => {
                let d = v.cast::<PyDict>()?;
                Some((get_f(d, "embedDelaySeconds", 1.1)?, get_f(d, "frameSeconds", 1.0/60.0)?))
            },
            None => None,
        },
        bow_draw,
        charge_time,
        charge_before_fire: match opt_attr(obj,"_charge_before_fire")? { Some(v) => v.is_truthy()?, None => false },
        full_clip_charge: match opt_attr(obj,"_full_clip_charge")? { Some(v) => v.is_truthy()?, None => false },
        native_fire_cooldown: match opt_attr(obj,"_native_fire_cooldown_seconds")? { Some(v) => v.extract::<f64>()?, None => 0.0 },
        charge_volley: match opt_attr(obj,"_charge_volley")? { Some(v) => v.is_truthy()?, None => false },
        initial_combo: {
            let v = obj.getattr("initial_combo")?;
            if v.is_truthy()? { v.extract::<f64>()? } else { 0.0 }
        },
        heavy_charge_speed: f(obj, "heavy_charge_speed")?,
        projectile_speed_multiplier: match opt_attr(obj, "projectile_speed_multiplier")? {
            Some(v) => v.extract::<f64>()?,
            None => 1.0,
        },
        reflection_explosion_chance: match opt_attr(obj, "reflection_explosion_chance")? {
            Some(v) => v.extract::<f64>()?,
            None => 0.0,
        },
        throw_damage_bonus: match opt_attr(obj, "throw_damage_bonus")? {
            Some(v) => v.extract::<f64>()?,
            None => 0.0,
        },
        combo_duration: f(obj, "combo_duration")?,
        combo_extra_chance: f(obj, "combo_extra_chance")?,
        combo_miss_chance: f(obj, "combo_miss_chance")?,
        heavy_attack_efficiency: f(obj, "heavy_attack_efficiency")?,
        head_only: match opt_attr(obj, "_head_only")? { Some(v) => v.is_truthy()?, None => false },
        body_only: match opt_attr(obj, "_body_only")? { Some(v) => v.is_truthy()?, None => false },
        impact_delay: match opt_attr(obj, "_impact_delay")? { Some(v) => v.extract::<f64>()?, None => 0.0 },
        persist_after_kill: match opt_attr(obj, "_persist_after_kill")? { Some(v) => v.is_truthy()?, None => false },
        primary_delay: match opt_attr(obj, "_primary_delay_seconds")? {
            Some(v) if v.is_truthy()? => v.extract::<f64>()?,
            _ => 0.0,
        },
        fixed_radial: match opt_attr(obj, "_fixed_radial")? { Some(v) => v.is_truthy()?, None => false },
        victim_full_crit_chance: f(obj, "victim_full_crit_chance")?,
        victim_full_crit_damage: f(obj, "victim_full_crit_damage")?,
        victim_low_health_multiplier: f(obj, "victim_low_health_multiplier")?,
        ammo_pool_multishot_cost: f(obj, "ammo_pool_multishot_cost")?,
        ammo_pool_damage_bonus: f(obj, "ammo_pool_damage_bonus")?,
        charge_pool_cost: match opt_attr(obj, "_charge_pool_cost")? { Some(v) => v.extract::<f64>()?, None => 1.0 },
        co_bucket: effect_ops(&obj.getattr("co_bucket")?)?,
        co_spec: match opt_attr(obj, "_co_spec")? {
            Some(spec) if !spec.is_none() => Some(context.co_spec(provider.keep(spec.clone()), |types| co_spec_from(types, &spec))?),
            _ => None,
        },
        slam_damage_ratio: f(obj, "slam_damage_ratio")?,
        incarnon_charge_multiplier: f(obj, "incarnon_charge_multiplier")?,
        ..Stats::default()
    })
}

/// 在回放期间保留对象引用，保证标识不会因地址复用而失效。
struct PythonProvider<'py> {
    py: Python<'py>,
    helper: &'py Bound<'py, PyAny>,
    resolver: OnceCell<Bound<'py, PyAny>>,
    objects: RefCell<FxHashMap<ObjectId, Bound<'py, PyAny>>>,
    error: RefCell<Option<PyErr>>,
}

impl<'py> PythonProvider<'py> {
    fn new(py: Python<'py>, helper: &'py Bound<'py, PyAny>) -> Self {
        Self {
            py,
            helper,
            resolver: OnceCell::new(),
            objects: RefCell::default(),
            error: RefCell::default(),
        }
    }

    fn keep(&self, object: Bound<'py, PyAny>) -> ObjectId {
        let id = object.as_ptr() as ObjectId;
        self.objects.borrow_mut().entry(id).or_insert(object);
        id
    }

    fn object(&self, id: ObjectId) -> Bound<'py, PyAny> {
        self.objects.borrow()[&id].clone()
    }

    fn convert<T>(&self, result: PyResult<T>) -> ReplayResult<T> {
        result.map_err(|error| {
            *self.error.borrow_mut() = Some(error);
            ReplayError::Invalid(InvalidReplay::Callback)
        })
    }

    fn into_python<T>(&self, result: ReplayResult<T>) -> PyResult<T> {
        result.map_err(|error| match error {
            ReplayError::Unsupported(message) => NativeUnsupported::new_err(message),
            ReplayError::Runaway(message) => ReplayRunaway::new_err(message),
            ReplayError::Invalid(InvalidReplay::Value(message)) => PyValueError::new_err(message),
            ReplayError::Invalid(InvalidReplay::Index(message)) => PyIndexError::new_err(message),
            ReplayError::Invalid(InvalidReplay::ZeroDivision(message)) => PyZeroDivisionError::new_err(message),
            ReplayError::Invalid(InvalidReplay::Callback) => self.error.borrow_mut().take()
                .expect("回调失败时保留原始异常"),
        })
    }

    fn stacks(&self, values: SourceStacks) -> PyResult<Bound<'py, PyDict>> {
        let out = PyDict::new(self.py);
        for (key, value) in values {
            let key = match key {
                StackKey::Object(id) => self.object(id),
                StackKey::Named(name) => name.into_pyobject(self.py)?.into_any(),
            };
            match value {
                StackValue::Float(v) => out.set_item(key, v)?,
                StackValue::Index(v) => out.set_item(key, v)?,
                StackValue::Flag(v) => out.set_item(key, v)?,
            }
        }
        Ok(out)
    }

    fn weights(&self, weights: Vec<(i64, f64)>) -> PyResult<Bound<'py, PyDict>> {
        let out = PyDict::new(self.py);
        for (key, value) in weights {
            out.set_item(key, value)?;
        }
        Ok(out)
    }

    fn extract_links(&self, obj: &Bound<'py, PyAny>) -> PyResult<StatsLinks> {
        let ids = |value: Bound<'py, PyAny>| -> PyResult<Vec<ObjectId>> {
            value.try_iter()?.map(|item| item.map(|obj| self.keep(obj))).collect()
        };
        let field = |value: Bound<'py, PyAny>| -> PyResult<(ObjectId, f64, f64)> {
            let row = value.cast::<PyTuple>()?;
            Ok((self.keep(row.get_item(0)?), row.get_item(1)?.extract()?, row.get_item(2)?.extract()?))
        };
        let fields = |attr: &str| -> PyResult<Vec<(ObjectId, f64, f64, usize)>> {
            let Some(rows) = opt_attr(obj, attr)? else { return Ok(Vec::new()); };
            rows.try_iter()?.map(|row| {
                let row = row?;
                let row = row.cast::<PyTuple>()?;
                Ok((self.keep(row.get_item(0)?), row.get_item(1)?.extract()?, row.get_item(2)?.extract()?, row.get_item(3)?.extract()?))
            }).collect()
        };
        let optional_field = |attr: &str| -> PyResult<Option<(ObjectId, f64, f64)>> {
            match opt_attr(obj, attr)? {
                Some(value) if value.is_truthy()? => field(value).map(Some),
                _ => Ok(None),
            }
        };
        let mut out = StatsLinks {
            live_co_unit: opt_attr(obj, "_live_co_unit")?.map(|unit| self.keep(unit)),
            additional: opt_attr(obj, "_additional_radials")?.map(ids).transpose()?.unwrap_or_default(),
            ..StatsLinks::default()
        };
        if let Some(thrown) = opt_attr(obj, "_reload_projectiles")? {
            if thrown.is_truthy()? {
                let dict = thrown.cast::<PyDict>()?;
                out.reload_lands_at = get_f(dict, "landsAt", 0.0)?;
                out.reload_empty = dict.get_item("empty")?.map(ids).transpose()?.unwrap_or_default();
                out.reload_partial = dict.get_item("partial")?.map(ids).transpose()?.unwrap_or_default();
            }
        }
        if let Some(spec) = opt_attr(obj, "_stackable")? {
            if spec.is_truthy()? {
                let row = spec.cast::<PyTuple>()?;
                let profile = self.keep(row.get_item(0)?);
                let parts = row.get_item(1)?.cast::<PyDict>()?.iter().map(|(name, part)| {
                    Ok((name.extract()?, self.keep(part)))
                }).collect::<PyResult<_>>()?;
                out.stackable = Some((profile, parts));
            }
        }
        out.finite_fields = fields("_finite_fields")?;
        out.source_fields = fields("_source_owned_fields")?;
        if let Some(parts) = opt_attr(obj, "_nox_projectile")? {
            let row = parts.cast::<PyTuple>()?;
            out.nox_projectile = Some((self.keep(row.get_item(0)?), self.keep(row.get_item(1)?)));
        }
        out.owned_embed = opt_attr(obj, "_owned_embed")?.map(field).transpose()?;
        out.lingering = optional_field("_lingering")?;
        out.embedded = optional_field("_embedded")?;
        Ok(out)
    }

    fn extract_hit_spec(&self, id: i64, types: &mut Types) -> PyResult<HitSpec> {
        let r = self.helper.call_method1("hit_spec", (id,))?;
        let t = r.cast::<PyTuple>()?;
        let opt = |v: Bound<'py, PyAny>| -> PyResult<Option<f64>> {
            Ok(if v.is_none() { None } else { Some(v.extract::<f64>()?) })
        };
        let target = t.get_item(4)?;
        let duplicate = t.get_item(11)?;
        let mut extra = Vec::new();
        for d in t.get_item(8)?.try_iter()? {
            extra.push(types.id(&d?.extract::<String>()?));
        }
        Ok(HitSpec {
            atten: t.get_item(0)?.extract()?,
            slam: t.get_item(1)?.extract()?,
            combo_multiplier: t.get_item(2)?.extract()?,
            pct: vector_of(types, &t.get_item(3)?)?,
            target: if target.is_none() { None } else { Some(types.id(&target.extract::<String>()?)) },
            crit_chance: opt(t.get_item(5)?)?,
            crit_multiplier: opt(t.get_item(6)?)?,
            can_crit: t.get_item(7)?.extract()?,
            extra_procs: extra,
            proc_atten: t.get_item(9)?.extract()?,
            radial: t.get_item(10)?.extract()?,
            duplicate: if duplicate.is_none() { None } else { Some(duplicate.extract::<(f64, f64)>()?) },
            throw: t.get_item(12)?.extract()?,
            bounce: t.get_item(13)?.extract()?,
            no_co: t.get_item(14)?.extract()?,
        })
    }

    fn extract_stackable_profile(&self, profile: ObjectId) -> PyResult<stackable::Profile> {
        let profile = self.object(profile);
        let r = self.helper.call_method1("stackable_profile", (profile,))?;
        let d = r.cast::<PyDict>()?;
        Ok(stackable::Profile {
            kind: get_f(d, "kind", 3.0)? as u8,
            embed_seconds: get_f(d, "embedSeconds", 0.0)?,
            max_life: get_f(d, "maxLife", 0.0)?,
            embed_interval: get_f(d, "embedInterval", 0.0)?,
            max_stack: get_f(d, "maxStack", 0.0)?,
            merge_low: get_f(d, "mergeLow", 0.0)?,
            merge_high: get_f(d, "mergeHigh", 0.0)?,
            merge_native: get_f(d, "mergeNative", 0.0)?,
            attach_at_stack: get_f(d, "attachAtStack", f64::INFINITY)?,
            singularity_interval: get_f(d, "singularityInterval", 0.0)?,
            reset_life: truthy(d, "resetLifeOnStack")?,
            stacked_life: get_f(d, "stackedLife", 0.0)?,
            initial_stack: get_f(d, "initialStack", 1.0)?,
            child: truthy(d, "child")?,
            child_max: get_f(d, "childMaxCount", 0.0)?,
            child_min: get_f(d, "childMinCount", 0.0)?,
            child_bounces: get_f(d, "childBounceCount", 0.0)?,
            child_life: get_f(d, "childLife", 0.0)?,
            child_death_multiplier: get_f(d, "childDeathMultiplier", 1.0)?,
            child_active_limit: get_f(d, "childActiveLimit", 70.0)?,
        })
    }

    fn call_resolver(&self, input: ResolveInput) -> PyResult<(ObjectId, Option<ObjectId>)> {
        let stacks = self.stacks(input.stacks)?;
        let live = match input.live_types {
            Some(value) => value.into_pyobject(self.py)?.into_any(),
            None => 0i64.into_pyobject(self.py)?.into_any(),
        };
        let resolver = self.resolver.get().expect("先提取回放解析器");
        let result = resolver.call1((input.form, stacks, live, input.last_round))?;
        let result = result.cast::<PyTuple>()?;
        let active = self.keep(result.get_item(0)?);
        let radial = result.get_item(1)?;
        Ok((active, (!radial.is_none()).then(|| self.keep(radial))))
    }
}

impl ReplayProvider for PythonProvider<'_> {
    fn resolve(&self, input: ResolveInput) -> ReplayResult<(ObjectId, Option<ObjectId>)> {
        self.convert(self.call_resolver(input))
    }

    fn stats(&self, object: ObjectId, context: &mut ViewContext<'_>) -> ReplayResult<Stats> {
        self.convert(extract_stats(&self.object(object), context, self))
    }

    fn stats_links(&self, object: ObjectId) -> ReplayResult<StatsLinks> {
        self.convert(self.extract_links(&self.object(object)))
    }

    fn hit_spec(&self, id: i64, types: &mut Types) -> ReplayResult<HitSpec> {
        self.convert(self.extract_hit_spec(id, types))
    }

    fn stackable_profile(&self, profile: ObjectId) -> ReplayResult<stackable::Profile> {
        self.convert(self.extract_stackable_profile(profile))
    }

    fn refresh_hook(&self, object: ObjectId, required: bool) -> ReplayResult<Option<ObjectId>> {
        let obj = self.object(object);
        let hook = if required {
            obj.getattr("_live_hit_refresh").map(Some)
        } else {
            opt_attr(&obj, "_live_hit_refresh")
        };
        self.convert(hook.map(|value| value.map(|hook| self.keep(hook))))
    }

    fn has_refresh(&self, object: ObjectId) -> ReplayResult<bool> {
        self.convert(opt_attr(&self.object(object), "_live_hit_refresh").map(|hook| hook.is_some()))
    }

    fn refresh(&self, hook: ObjectId, stacks: SourceStacks, live: f64, initial: Option<bool>) -> ReplayResult<ObjectId> {
        self.convert((|| {
            let hook = self.object(hook);
            let stacks = self.stacks(stacks)?;
            let result = match initial {
                Some(initial) => hook.call1((stacks, live, initial))?,
                None => hook.call1((stacks, live))?,
            };
            Ok(self.keep(result))
        })())
    }

    fn zymos_carrier(&self, object: ObjectId, stacks: SourceStacks, weights: Vec<(i64, f64)>) -> ReplayResult<ObjectId> {
        self.convert((|| {
            let stacks = self.stacks(stacks)?;
            let weights = self.weights(weights)?;
            let result = self.object(object).getattr("_zymos_carrier")?.call1((stacks, weights))?;
            Ok(self.keep(result))
        })())
    }

    fn zymos_children(&self, object: ObjectId, snapshot: ObjectId, stacks: SourceStacks, weight: f64,
                      weights: Vec<(i64, f64)>) -> ReplayResult<Vec<(ObjectId, ObjectId, f64)>> {
        self.convert((|| {
            let stacks = self.stacks(stacks)?;
            let weights = self.weights(weights)?;
            let result = self.object(object).getattr("_zymos_children")?.call1((self.object(snapshot), stacks, weight, weights))?;
            result.try_iter()?.map(|child| {
                let child = child?;
                let row = child.cast::<PyTuple>()?;
                Ok((self.keep(row.get_item(0)?), self.keep(row.get_item(1)?), row.get_item(2)?.extract()?))
            }).collect()
        })())
    }

    fn check_signals(&self) -> ReplayResult<()> {
        self.convert(self.py.check_signals())
    }
}

fn extract_input<'py>(py: Python<'py>, config: &Bound<'py, PyDict>, provider: &PythonProvider<'py>) -> PyResult<TrajectoryInput> {
    let item = |k: &str| -> PyResult<Bound<'py, PyAny>> {
        config.get_item(k)?.ok_or_else(|| PyValueError::new_err(format!("config.{k} missing")))
    };
    let mut types = Types::new();
    let vec_of = |obj: &Bound<'py, PyAny>, types: &mut Types| -> PyResult<Vector> {
        let mut out = Vec::new();
        if obj.is_none() {
            return Ok(out);
        }
        for (k, v) in obj.downcast::<PyDict>()?.iter() {
            out.push((types.id(&k.extract::<String>()?), v.extract::<f64>()?));
        }
        Ok(out)
    };
    let names_of = |obj: &Bound<'py, PyAny>, types: &mut Types| -> PyResult<Vec<D>> {
        let mut out = Vec::new();
        for v in obj.try_iter()? {
            out.push(types.id(&v?.extract::<String>()?));
        }
        Ok(out)
    };
    let full = list_f(&item("full")?)?;
    let forced_procs = vec_of(&item("forcedProcs")?, &mut types)?;
    let faction_deltas = vec_of(&item("factionDeltas")?, &mut types)?;
    let armour_bypass = names_of(&item("armourBypass")?, &mut types)?;
    let shield_bypass = names_of(&item("shieldBypass")?, &mut types)?;
    let mut extra_hits = Vec::new();
    for e in item("extraHits")?.try_iter()? {
        let e = e?;
        let t = e.downcast::<PyTuple>()?;
        extra_hits.push((types.id(&t.get_item(0)?.extract::<String>()?), t.get_item(1)?.extract::<f64>()?));
    }
    let victim_before = vec_of(&item("victimBefore")?, &mut types)?;
    let burst_of = |key: &str| -> PyResult<Option<BurstSpec>> {
        let v = item(key)?;
        if v.is_none() {
            Ok(None)
        } else {
            let d = v.downcast::<PyDict>()?;
            Ok(Some((truthy(d, "remainingClip")?, match d.get_item("count")? {
                Some(c) if !c.is_none() => c.extract::<f64>()? as i64,
                _ => 0,
            }, get_f(d, "delay_seconds", 0.0)?, truthy(d, "ignoreFireRate")?)))
        }
    };
    let opt_f = |key: &str| -> PyResult<Option<f64>> {
        let v = item(key)?;
        Ok(if v.is_none() { None } else { Some(v.extract::<f64>()?) })
    };
    let burst = burst_of("burst")?;
    let burst_rate = {
        let v = item("burstRate")?;
        if v.is_none() { None } else { Some(v.extract::<f64>()?) }
    };
    let beam_floor = {
        let v = item("beamFloor")?;
        if v.is_none() { None } else { Some(v.extract::<f64>()?) }
    };
    let distance = {
        let v = item("distance")?;
        if v.is_none() { None } else { Some(v.extract::<f64>()?) }
    };
    let rate: f64 = item("rate")?.extract()?;
    let target_faction: String = item("targetFaction")?.extract()?;
    let opt_dict = |key: &str| -> PyResult<Option<Bound<'py, PyDict>>> {
        let v = item(key)?;
        Ok(if v.is_none() { None } else { Some(v.cast::<PyDict>()?.clone()) })
    };
    let battery = match opt_dict("battery")? {
        Some(d) => Some((get_f(&d, "delayEmptySeconds", 0.0)?, get_f(&d, "delayPartialSeconds", 0.0)?, get_f(&d, "regenPerSecond", 0.0)?)),
        None => None,
    };
    let spool = match opt_dict("spool")? {
        Some(d) => Some(SpoolSpec {
            maximum: get_f(&d, "maximum", 1.0)?, initial: get_f(&d, "initial", 0.0)?, rate_start: get_f(&d, "rateStart", 1.0)?,
            rate_shots: get_f(&d, "rateShots", 1.0)?, ammo_divisor: get_f(&d, "ammoDivisor", 1.0)?, spindown: get_f(&d, "spindown", f64::INFINITY)?,
        }),
        None => None,
    };
    let rate_spool = match opt_dict("fireRateSpool")? {
        Some(d) => {
            let start = get_f(&d, "start", 1.0)?;
            let end = get_f(&d, "end", 1.0)?;
            let maximum = get_f(&d, "nativeMaxShots", 0.0)? as u32;
            Some(if maximum > 0 {
                RateSpoolSpec::Native { start: start as f32, end: end as f32, maximum,
                    source_rpm: get_f(&d, "sourceRpm", 0.0)?, source_rate: get_f(&d, "sourceRate", 1.0)? }
            } else {
                RateSpoolSpec::Reference { start, end, over: get_f(&d, "overShots", 1.0)? }
            })
        }
        None => None,
    };
    let shell = match opt_dict("shellReload")? {
        Some(d) => Some((get_f(&d, "start", 0.0)?, get_f(&d, "perShell", 0.0)?, get_f(&d, "end", 0.0)?)),
        None => None,
    };
    let shell_mode_switch = match opt_dict("shellReload")? {
        Some(d) => truthy(&d, "modeSwitchLoadsShell")?,
        None => false,
    };
    let projectile_state = {
        let d = item("projectileState")?;
        let d = d.cast::<PyDict>()?;
        let opt = |key: &str| -> PyResult<Option<f64>> {
            Ok(match d.get_item(key)? { Some(v) if !v.is_none() => Some(v.extract::<f64>()?), _ => None })
        };
        stackable::State {
            isolated: truthy(d, "isolated")?,
            manual: truthy(d, "manual")?,
            detonate_after: opt("detonateAfterShots")?,
            wait: get_f(d, "detonationWaitSeconds", 0.0)?,
            child_hits: get_f(d, "stugChildHits", 0.0)?,
            bounce_hits: get_f(d, "stugBounceHits", 0.0)?,
            bounce_delay: get_f(d, "stugBounceDelaySeconds", 0.0)?,
            child_detonate: opt("stugChildDetonateSeconds")?,
            mq_augment: truthy(d, "mqAugmentEquipped")?,
        }
    };
    let cycle = match opt_dict("cycle")? {
        Some(d) => Some(CycleSpec {
            hits: get_f(&d, "hits", 0.0)?,
            enter: get_f(&d, "enterSeconds", 0.0)?,
            exit: get_f(&d, "exitSeconds", 0.0)?,
            pool: get_f(&d, "pool", 0.0)?,
            charge_on: match d.get_item("chargeOn")? {
                Some(v) if !v.is_none() => match v.extract::<String>()?.as_str() {
                    "direct_hits" => 1,
                    "kills" => 2,
                    _ => 0,
                },
                _ => 0,
            },
            kill_charged: truthy(&d, "killCharged")?,
            charge_before_fire: truthy(&d, "chargeBeforeFire")?,
            damage_scale: get_f(&d, "damageScale", 1.0)?,
            source_mode_charge: truthy(&d,"sourceModeKillChargeTrue")?,
        }),
        None => None,
    };
    let shot_combo = match opt_dict("sniperCombo")? {
        Some(d) => Some(ShotCombo::new(
            truthy(&d, "incarnon")?, get_f(&d, "min", 0.0)?, get_f(&d, "seconds", 0.0)?,
            get_f(&d, "tierBonus", 0.0)?, get_f(&d, "tierFactor", 1.0)?,
        )),
        None => None,
    };
    let restore = match opt_dict("magazineRestore")? {
        Some(d) => Some(Restore {
            kind: d.get_item("kind")?.map(|v| v.extract::<String>()).transpose()?.unwrap_or_default(),
            chance: get_f(&d, "chance", 1.0)?,
            fraction: get_f(&d, "fraction", 0.0)?,
            amount: get_f(&d, "amount", 0.0)?,
            once_per_shot: truthy(&d, "oncePerShot")?,
            victim_proc_type: match d.get_item("victimProcType")? {
                Some(v) if v.is_truthy()? => Some(types.id(&v.extract::<String>()?)),
                _ => None,
            },
        }),
        None => None,
    };
    let tables = {
        let t = item("tables")?;
        let t = t.cast::<PyDict>()?;
        let loc = |v: Bound<'py, PyAny>| -> PyResult<Location> {
            let x = v.cast::<PyTuple>()?;
            Ok(Location { hit_multiplier: x.get_item(0)?.extract()?, hit_critical_multiplier: x.get_item(1)?.extract()?,
                          head: x.get_item(2)?.extract()? })
        };
        let pair = |key: &str| -> PyResult<[Location; 2]> {
            let v = t.get_item(key)?.ok_or_else(|| PyValueError::new_err(key.to_string()))?;
            let l = v.cast::<PyList>()?;
            Ok([loc(l.get_item(0)?)?, loc(l.get_item(1)?)?])
        };
        let damage = pair("damage")?;
        let dot = pair("dot")?;
        let dot_multiplier = vec_of(&t.get_item("dotMultiplier")?.unwrap(), &mut types)?;
        TargetTables {
            target_faction: !target_faction.is_empty(),
            damage,
            dot,
            dot_head_eligible: t.get_item("dotHeadEligible")?.unwrap().extract()?,
            dot_multiplier,
            forced_procs: forced_procs.clone(),
        }
    };
    let melee = {
        let v = item("melee")?;
        if v.is_none() {
            None
        } else {
            let d = v.cast::<PyDict>()?;
            let mut tier_keys = Vec::new();
            if let Some(keys) = d.get_item("tierKeys")? {
                for (k, cap) in keys.cast::<PyDict>()?.iter() {
                    tier_keys.push((provider.keep(k.clone()), k.extract::<String>()?, cap.extract::<f64>()?));
                }
            }
            let mut segments = Vec::new();
            for row in d.get_item("segments")?.ok_or_else(|| PyValueError::new_err("melee.segments missing"))?.cast::<PyList>()?.iter() {
                let t = row.cast::<PyTuple>()?;
                let hit = t.get_item(4)?;
                segments.push(MeleeSegment {
                    fixed: t.get_item(0)?.extract()?,
                    anim: t.get_item(1)?.extract()?,
                    charge_scaled: t.get_item(2)?.extract()?,
                    flight_scaled: t.get_item(3)?.extract()?,
                    hit: if hit.is_none() {
                        None
                    } else {
                        Some(MeleeHit {
                            id: hit.extract()?,
                            heavy: t.get_item(5)?.extract()?,
                            combo_hits: t.get_item(6)?.extract()?,
                            consumes: t.get_item(7)?.extract()?,
                            phase: t.get_item(8)?.extract()?,
                        })
                    },
                });
            }
            Some(MeleeSpec {
                base_hits: get_f(d, "baseHits", 20.0)?,
                tier_step: get_f(d, "tierStep", 20.0)?,
                cap: get_f(d, "cap", 220.0)?,
                base_multiplier: get_f(d, "baseMultiplier", 1.0)?,
                recovery_per_second: get_f(d, "recoveryPerSecond", 0.0)?,
                tier_keys,
                segments,
            })
        }
    };
    let syndicate = {
        let v = item("syndicate")?;
        if v.is_none() {
            None
        } else {
            let d = v.cast::<PyDict>()?;
            let id: String = d.get_item("id")?.map(|x| x.extract::<String>()).transpose()?.unwrap_or_default();
            let dtype: String = d.get_item("damageType")?.map(|x| x.extract::<String>()).transpose()?.unwrap_or_default();
            Some((id, types.id(&dtype), get_f(d, "affinityToFill", 0.0)?))
        }
    };
    let mut sources = Vec::new();
    for q in item("sources")?.downcast::<PyList>()?.iter() {
        let d = q.downcast::<PyDict>()?.clone();
        let kind = match d.get_item("kind")? {
            Some(v) if !v.is_none() => Some(v.extract::<String>()?),
            _ => None,
        };
        let reset_on = match d.get_item("resetOn")? {
            Some(v) if !v.is_none() => v.extract::<Vec<String>>()?,
            _ => Vec::new(),
        };
        let toggle_on = match d.get_item("toggleOn")? {
            Some(v) if !v.is_none() => Some(v.extract::<String>()?),
            _ => None,
        };
        let victim_proc_type = match d.get_item("victimProcType")? {
            Some(v) if v.is_truthy()? => Some(types.id(&v.extract::<String>()?)),
            _ => None,
        };
        let consume_on = match d.get_item("consumeOn")? {
            Some(v) if !v.is_none() => Some(v.extract::<String>()?),
            _ => None,
        };
        let lock_on = match d.get_item("lockOn")? {
            Some(v) if !v.is_none() => Some(v.extract::<String>()?),
            _ => None,
        };
        let required_time = match d.get_item("requiredTime")? {
            Some(v) if v.is_truthy()? => Some(v.extract::<f64>()?),
            _ => None,
        };
        let minimum = match d.get_item("minimumConsecutive")? {
            Some(v) if v.is_truthy()? => v.extract::<f64>()? as i64,
            _ => 0,
        };
        sources.push(SourceInput {
            id: provider.keep(d.get_item("id")?.unwrap_or_else(|| py.None().into_bound(py))),
            kind,
            stacks: get_f(&d, "stacks", 0.0)?,
            expires: get_f(&d, "expires", f64::INFINITY)?,
            max_stacks: get_f(&d, "maxStacks", 1.0)?,
            duration: get_f(&d, "duration", 0.0)?,
            per_event: get_f(&d, "perEvent", 1.0)?,
            decay: match d.get_item("decay")? {
                Some(v) if !v.is_none() => v.extract::<String>()?,
                _ => "reset".to_string(),
            },
            no_refresh: truthy(&d, "noRefresh")?,
            free_ammo: truthy(&d, "freeAmmo")?,
            duration_per_tier: get_f(&d, "durationPerTier", 0.0)?,
            charges: get_f(&d, "charges", 0.0)?,
            consume_on,
            lock_on,
            initial_stacks: get_f(&d, "initialStacks", 0.0)?,
            hold: truthy(&d, "hold")?,
            reset_on,
            toggle_on,
            victim_proc_type,
            required_count: get_f(&d, "requiredCount", 1.0)?,
            required_time,
            actor_only: truthy(&d, "actorOnly")?,
            minimum_consecutive: minimum,
            energy_threshold: get_f(&d, "energyThreshold", 0.0)?,
        });
    }
    let window: f64 = item("window")?.extract()?;
    let arcane = |key: &str| -> PyResult<Option<Bound<'py, PyDict>>> {
        let v = item(key)?;
        Ok(if v.is_none() { None } else { Some(v.cast::<PyDict>()?.clone()) })
    };
    let fortifier = match arcane("fortifier")? {
        Some(d) => Some((get_f(&d, "multiplier", 1.0)?, get_f(&d, "steal", 0.0)?, get_f(&d, "cap", 0.0)?,
                         get_f(&d, "uptime", 1.0)?)),
        None => None,
    };
    let empowered = match arcane("empowered")? {
        Some(d) => Some((get_f(&d, "damage", 0.0)?, get_f(&d, "uptime", 1.0)?)),
        None => None,
    };
    let enervate = match arcane("enervate")? {
        Some(d) => {
            let fixed_stacks = match d.get_item("fixedStacks")? {
                Some(v) if !v.is_none() => Some(v.extract::<f64>()?),
                _ => None,
            };
            Some(EnervateInput {
                reset_after: get_f(&d, "resetAfter", 1.0)? as i64,
                min_interval: get_f(&d, "minInterval", 0.0)?,
                fixed_stacks,
                uptime: get_f(&d, "uptime", 1.0)?,
                ignore_selected: truthy(&d, "ignoreSelected")?,
            })
        }
        None => None,
    };
    let tap = match arcane("doubleTap")? {
        Some(d) => Some((get_f(&d, "perHit", 0.0)?, get_f(&d, "maximum", 0.0)?, get_f(&d, "duration", 0.0)?)),
        None => None,
    };
    let field_duration = match arcane("fieldDurationOnEmptyReload")? {
        Some(d) => Some(get_f(&d, "multiplier", 1.0)?),
        None => None,
    };
    let encumber = match arcane("encumber")? {
        Some(d) => Some((get_f(&d, "chance", 0.0)?, get_f(&d, "uptime", 1.0)?)),
        None => None,
    };
    let cryogenic = match arcane("cryogenic")? {
        Some(d) => Some((get_f(&d, "coldStacks", 0.0)?, get_f(&d, "uptime", 1.0)?)),
        None => None,
    };
    let mut status_pool = Vec::new();
    for name in item("statusPool")?.try_iter()? {
        status_pool.push(types.id(&name?.extract::<String>()?));
    }
    provider.resolver.set(item("resolver")?).expect("每次回放只提取一次解析器");
    Ok(TrajectoryInput {
        window,
        rate,
        forced_procs,
        faction_deltas,
        armour_bypass,
        shield_bypass,
        extra_hits,
        distance,
        beam_floor,
        burst,
        burst_rate,
        victim_before,
        syndicate,
        battery,
        spool,
        rate_spool,
        shell,
        shot_combo,
        restore,
        target_faction,
        tables,
        melee,
        fortifier,
        empowered,
        status_pool,
        cryogenic,
        encumber,
        projectile_state,
        cycle,
        shell_mode_switch,
        tap,
        field_duration,
        sources,
        types,
        head: item("head")?.extract()?,
        status_rate_multiplier: item("statusRateMultiplier")?.extract()?,
        damage_rate_multiplier: item("damageRateMultiplier")?.extract()?,
        full: [
            *full.first().ok_or_else(|| PyIndexError::new_err("list index out of range"))?,
            *full.get(1).ok_or_else(|| PyIndexError::new_err("list index out of range"))?,
            *full.get(2).ok_or_else(|| PyIndexError::new_err("list index out of range"))?,
        ],
        ehp: item("ehp")?.extract()?,
        armour_base: item("armourBase")?.extract()?,
        elemental_armour_strip: item("elementalArmourStrip")?.extract()?,
        extra_scale: item("extraScale")?.extract()?,
        weapon_slot: item("weaponSlot")?.extract()?,
        punchthrough: item("punchthrough")?.extract()?,
        live_types_enabled: item("liveTypes")?.is_truthy()?,
        infinite_ammo: item("infiniteAmmo")?.is_truthy()?,
        reload_after_every_shot: item("reloadAfterEveryShot")?.is_truthy()?,
        refill_growth: item("refillMagazineGrowth")?.is_truthy()?,
        faction_bracket: get_f(&config, "factionBracket", 1.0)?,
        selected_incarnon: item("selectedIncarnon")?.is_truthy()?,
        projectile_sequence_length: match opt_dict("projectileSequence")? {
            Some(d) => get_f(&d,"length",0.0)? as usize,
            None => 0,
        },
        physical_lane_length: match opt_dict("physicalProjectileLanes")? {Some(d)=>get_f(&d,"length",0.)? as usize,None=>0},
        physical_lane_phase: match opt_dict("physicalProjectileLanes")? {Some(d)=>get_f(&d,"initialPhase",0.)? as usize,None=>0},
        lane_stratum: get_f(&config,"laneStratum",0.5)?,
        affinity_per_kill: item("affinityPerKill")?.extract()?,
        weak_draw: match opt_dict("weakDraw")? {
            Some(d) => Some((truthy(&d, "probe")?, get_f(&d, "offset", 0.0)?, get_f(&d, "golden", 0.0)?, match d.get_item("trackKills")? { Some(v)=>v.is_truthy()?, None=>true })),
            None => None,
        },
        puncture_armor_strip: item("punctureArmorStrip")?.extract()?,
        mines: match opt_dict("manualDetonation")? {
            Some(d) => Some((get_f(&d, "delaySeconds", 0.0)?, get_f(&d, "maxProjectiles", 0.0)?, get_f(&d, "batchSize", 1.0)?)),
            None => None,
        },
        life_curve: item("lifeCurve")?.is_truthy()?,
        onos: match opt_dict("onosCharge")? {
            Some(d) => Some((get_f(&d, "alignmentShots", 0.0)?, get_f(&d, "postFireSeconds", 0.0)?, get_f(&d, "rampSeconds", 0.0)?)),
            None => None,
        },
        onos_crits: item("onosCrits")?.is_truthy()?,
        impact_embed: match opt_dict("manualDetonation")? {
            Some(d) => match d.get_item("impactThenEmbed")? { Some(v) => v.is_truthy()?, None => false },
            None => false,
        },
        embedded_timing: match opt_dict("manualDetonation")? {
            Some(d)=>(get_f(&d,"destroyIntervalSeconds",0.050000001)?,get_f(&d,"terminalDelaySeconds",0.1125)?,
                match d.get_item("naturalExpiryIncludesCoincidentTick")? {Some(v)=>v.is_truthy()?,None=>true}),
            None=>(0.050000001,0.1125,true),
        },
        burst_base: burst_of("burstBase")?,
        burst_rate_base: opt_f("burstRateBase")?,
        burst_incarnon: burst_of("burstIncarnon")?,
        burst_rate_incarnon: opt_f("burstRateIncarnon")?,
        beam_floor_base: opt_f("beamFloorBase")?,
        last_burst: item("lastBurst")?.is_truthy()?,
        magnetic_proxy: match config.get_item("magneticProxy")? {
            Some(v) if !v.is_none() => Some(v.extract()?),
            _ => None,
        },
        magnetic_weakness: match config.get_item("magneticWeaknessBonus")? {
            Some(v) if !v.is_none() => Some(v.extract()?),
            _ => None,
        },
        magnetic_electric_immune: truthy(config, "magneticElectricImmune")?,
        blocks_radiation: truthy(config, "blocksRadiation")?,
        enervate,
        puncture_cap: item("punctureCap")?.extract()?,
        initial: provider.keep(item("stats")?),
        actor_events: {
            let mut events = Vec::new();
            for actor in item("actorEvents")?.try_iter()? {
                let actor = actor?;
                let row = actor.cast::<PyTuple>()?;
                events.push((row.get_item(0)?.extract()?, row.get_item(1)?.extract()?, row.get_item(2)?.extract()?));
            }
            events
        },
        energy_rate: item("energyRate")?.extract()?,
        score_only: truthy(config, "scoreOnly")?,
    })
}

#[pyfunction]
pub fn trajectory<'py>(py: Python<'py>, config: &Bound<'py, PyDict>, helper: &'py Bound<'py, PyAny>)
    -> PyResult<Bound<'py, PyDict>> {
    let provider = PythonProvider::new(py, helper);
    let input = extract_input(py, config, &provider)?;
    let result = provider.into_python(replay::trajectory(input, &provider))?;
    write_output(py, result)
}

#[pyfunction]
pub fn trajectory_health_batch<'py>(py: Python<'py>, config: &Bound<'py, PyDict>, helper: &'py Bound<'py, PyAny>,
    targets: Vec<([f64; 3], f64)>) -> PyResult<Vec<(f64, f64, f64, i64)>> {
    replay::validate_health_targets(&targets).map_err(|error| PyValueError::new_err(error.to_string()))?;
    let provider = PythonProvider::new(py, helper);
    let input = extract_input(py, config, &provider)?;
    let rows = provider.into_python(replay::trajectory_health_batch(input, &provider, targets))?;
    Ok(rows.into_iter().map(|row| (row.kpm, row.dps, row.damage, row.mine_overflows)).collect())
}

#[pyfunction]
pub fn direct_bake_stream<'py>(py: Python<'py>, config: &Bound<'py, PyDict>, helper: &'py Bound<'py, PyAny>)
    -> PyResult<(Vec<f64>, f64, f64)> {
    let provider = PythonProvider::new(py, helper);
    let trajectory = extract_input(py, config, &provider)?;
    provider.into_python(replay::direct_bake_stream(DirectBakeStreamInput { trajectory }, &provider))
}

fn write_output(py: Python<'_>, result: TrajectoryOutput) -> PyResult<Bound<'_, PyDict>> {
    let out = PyDict::new(py);
    let names = |values: replay::NamedDamage| -> PyResult<Bound<'_, PyDict>> {
        let dict = PyDict::new(py);
        for (name, value) in values {
            dict.set_item(name, value)?;
        }
        Ok(dict)
    };
    out.set_item("kills", result.kills)?;
    if let Some(details) = result.details {
        out.set_item("fortifier", details.fortifier)?;
        out.set_item("empowered", (details.empowered.0, names(details.empowered.1)?))?;
        out.set_item("cryogenicExpected", details.cryogenic_expected)?;
        out.set_item("encumberExpected", details.encumber_expected)?;
        out.set_item("transforms", details.transforms)?;
        if let Some(curve) = details.life_curve {
            out.set_item("lifeCurve", curve)?;
        }
        if let Some(projectiles) = details.projectiles {
            out.set_item("projectiles", projectiles)?;
        }
        out.set_item("mines", details.mines)?;
        out.set_item("fieldDuration", details.field_duration)?;
        if let Some(enervate) = details.enervate {
            out.set_item("enervate", enervate)?;
        }
        out.set_item("gen", details.gen)?;
        out.set_item("weakKillEvents", details.weak_kill_events)?;
        out.set_item("weakUncertainty", result.weak_uncertainty)?;
        out.set_item("weakIndex", result.weak_index)?;
        out.set_item("attacks", result.attacks)?;
        out.set_item("finiteFieldPackets", details.finite_field_packets)?;
        out.set_item("finiteFieldActivations", details.finite_field_activations)?;
        out.set_item("sourceOwnedFieldPackets", details.source_field_packets)?;
        out.set_item("sourceOwnedFieldActivations", details.source_field_activations)?;
        if let Some(phase) = details.physical_projectile_phase {
            out.set_item("physicalProjectilePhase", phase)?;
        }
        out.set_item("stale", details.stale)?;
        out.set_item("firstKill", details.first_kill)?;
        out.set_item("minInterval", details.min_interval)?;
        out.set_item("pools", result.pools.to_vec())?;
        out.set_item("totals", result.totals.to_vec())?;
        out.set_item("procCounts", names(details.proc_counts)?)?;
        out.set_item("dotByType", names(details.dot_by_type)?)?;
        let breaks = PyList::empty(py);
        for (time, pool, stacks, per, gen) in details.magnetic_breaks {
            let row = PyDict::new(py);
            row.set_item("time", time)?;
            row.set_item("pool", pool)?;
            row.set_item("stacks", stacks)?;
            row.set_item("seedPerTick", per)?;
            row.set_item("targetGeneration", gen)?;
            breaks.append(row)?;
        }
        out.set_item("magneticBreaks", breaks)?;
        out.set_item("reloads", details.reloads)?;
        out.set_item("recharges", details.recharges)?;
        out.set_item("delayedAttackEvents", details.delayed_attacks)?;
        let shells = PyList::empty(py);
        for (time, rounds, source, form) in details.reload_shell_events {
            let row = PyDict::new(py);
            row.set_item("time", time)?;
            row.set_item("rounds", rounds)?;
            row.set_item("source", source)?;
            row.set_item("form", form)?;
            shells.append(row)?;
        }
        out.set_item("reloadShellEvents", shells)?;
        let events = PyList::empty(py);
        for (time, id, damage) in details.syndicate_events {
            let row = PyDict::new(py);
            row.set_item("time", time)?;
            row.set_item("id", id)?;
            row.set_item("damage", damage)?;
            events.append(row)?;
        }
        out.set_item("syndicateEvents", events)?;
    } else {
        out.set_item("attacks", result.attacks)?;
        out.set_item("pools", result.pools.to_vec())?;
        out.set_item("totals", result.totals.to_vec())?;
        out.set_item("weakUncertainty", result.weak_uncertainty)?;
        out.set_item("weakIndex", result.weak_index)?;
        out.set_item("mineOverflows", result.mine_overflows)?;
    }
    out.set_item("lastRound", result.last_round)?;
    out.set_item("victimBefore", names(result.victim_before)?)?;
    out.set_item("actorSignalUsed", result.actor_signal_used)?;
    out.set_item("sourceStacks", result.source_stacks)?;
    Ok(out)
}
