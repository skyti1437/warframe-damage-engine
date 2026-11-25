use std::sync::{Arc, Mutex};

use pyo3::prelude::*;
use pyo3::types::{PyBool, PyDict, PyFloat, PyInt, PyList, PyString, PyTuple};
use wfcore_lib::co::Dict;
use wfcore_lib::fx::FxHashMap;
use wfcore_lib::mods::{self, ClassifiedEffect, EffectInput, ModsInput, Num, Row, Scale};

fn num_to_py(num: Num, py: Python<'_>) -> Py<PyAny> {
    match num {
        Num::I(i) => i.into_pyobject(py).unwrap().into_any().unbind(),
        Num::F(f) => PyFloat::new(py, f).into_any().unbind(),
    }
}

fn extract_num(v: &Bound<'_, PyAny>) -> PyResult<Num> {
    if v.is_instance_of::<PyInt>() && !v.is_instance_of::<PyBool>() {
        Ok(Num::I(v.extract::<i64>()?))
    } else {
        Ok(Num::F(v.extract::<f64>()?))
    }
}

fn opt_str(o: &Bound<'_, PyAny>, attr: &str) -> PyResult<Option<String>> {
    let v = o.getattr(attr)?;
    if v.is_none() {
        Ok(None)
    } else {
        Ok(Some(v.extract::<String>()?))
    }
}

fn opt_f(o: &Bound<'_, PyAny>, attr: &str) -> PyResult<Option<f64>> {
    let v = o.getattr(attr)?;
    if v.is_none() {
        Ok(None)
    } else {
        Ok(Some(v.extract::<f64>()?))
    }
}

fn row_of(o: &Bound<'_, PyAny>) -> PyResult<Row> {
    Ok(EffectInput {
        upgrade: o.getattr("upgrade_type")?.extract()?,
        operation: o.getattr("operation")?.extract()?,
        damage_type: opt_str(o, "damage_type")?,
        source: opt_str(o, "source_path")?,
        value: o.getattr("value")?.extract()?,
        symbol: opt_str(o, "symbol_filter")?,
        runtime_kind: opt_str(o, "runtime_kind")?,
        runtime_damage_type: opt_str(o, "runtime_damage_type")?,
        runtime_chance: opt_f(o, "runtime_chance")?,
        runtime_threshold: opt_f(o, "runtime_threshold")?,
        runtime_multiplier: o.getattr("runtime_multiplier")?.extract()?,
    }.into())
}

#[pyclass]
pub struct ModsPrep {
    inner: mods::ModsPrep,
    classify: Py<PyAny>,
    effect_class: Py<PyAny>,
    zoom: Vec<Py<PyAny>>,
    cache: Mutex<FxHashMap<usize, Arc<Entry>>>,
}

struct Entry {
    effect: Py<PyAny>,
    status: u8,
    message: Option<String>,
    unsupported: Option<String>,
    row: Arc<Row>,
}

impl Entry {
    fn classified(&self, index: usize) -> ClassifiedEffect {
        ClassifiedEffect {
            index, row: self.row.clone(), status: self.status,
            message: self.message.clone(), unsupported: self.unsupported.clone(),
        }
    }
}

fn get<'py>(d: &Bound<'py, PyDict>, k: &str) -> PyResult<Bound<'py, PyAny>> {
    d.get_item(k)?.ok_or_else(|| pyo3::exceptions::PyKeyError::new_err(k.to_string()))
}

fn get_opt_f(d: &Bound<'_, PyDict>, k: &str) -> PyResult<Option<f64>> {
    let v = get(d, k)?;
    if v.is_none() {
        Ok(None)
    } else {
        Ok(Some(v.extract()?))
    }
}

#[pymethods]
impl ModsPrep {
    #[new]
    fn new(config: &Bound<'_, PyDict>) -> PyResult<Self> {
        let mut base = Vec::new();
        for item in get(config, "base")?.try_iter()? {
            let (k, v): (String, f64) = item?.extract()?;
            base.push((mods::intern(&k), v));
        }
        let mut zoom = Vec::new();
        for z in get(config, "zoom")?.try_iter()? {
            zoom.push(z?.unbind());
        }
        let compression = {
            let v = get(config, "compression")?;
            if v.is_none() { None } else { Some(v.extract::<(f64, bool, Option<f64>, f64, bool)>()?) }
        };
        let copy = {
            let v = get(config, "copy")?;
            if v.is_none() {
                None
            } else {
                let t = v.cast::<PyTuple>()?;
                let cc = t.get_item(1)?;
                Some((t.get_item(0)?.extract::<f64>()?,
                      if cc.is_none() { None } else { Some(extract_num(&cc)?) },
                      t.get_item(2)?.extract::<Option<f64>>()?,
                      t.get_item(3)?.extract::<Option<f64>>()?))
            }
        };
        let crit_chance = {
            let v = get(config, "critChance")?;
            if v.is_none() { Num::F(0.0) } else { extract_num(&v)? }
        };
        let battery = {
            let v = get(config, "battery")?;
            if v.is_none() {
                None
            } else {
                let t = v.cast::<PyTuple>()?;
                Some((extract_num(&t.get_item(0)?)?, t.get_item(1)?.extract::<f64>()?,
                      t.get_item(2)?.extract::<Option<f64>>()?, t.get_item(3)?.extract::<f64>()?))
            }
        };
        let super_crit = {
            let v = get(config, "superCrit")?;
            if v.is_none() { None } else { Some(v.extract::<(f64, f64)>()?) }
        };
        let super_status = {
            let v = get(config, "superStatus")?;
            if v.is_none() { None } else { Some(v.extract::<(f64, f64)>()?) }
        };
        Ok(ModsPrep {
            classify: get(config, "classify")?.unbind(),
            effect_class: get(config, "effectClass")?.unbind(),
            zoom,
            inner: mods::ModsPrep {
                super_crit,
                super_status,
                melee: get(config, "melee")?.extract()?,
                base,
                compression,
                aiming: get(config, "aiming")?.extract()?,
                gunco_present: get(config, "guncoPresent")?.extract()?,
                gunco_base_fraction: get_opt_f(config, "guncoBaseFraction")?,
                gunco_exclude: get(config, "guncoExclude")?.extract()?,
                gunco_independent: get(config, "guncoIndependent")?.extract()?,
                direct_source_stack_damage: get(config, "directSourceStackDamage")?.extract()?,
                co_base_fraction: get(config, "coBaseFraction")?.extract()?,
                innate: get(config, "innate")?.extract()?,
                notes: get(config, "notes")?.extract()?,
                speed_falloff: get(config, "speedFalloff")?.extract()?,
                falloff: get(config, "falloff")?.extract()?,
                distance: get_opt_f(config, "distance")?,
                innate_multishot: get(config, "innateMultishot")?.extract()?,
                native_ignore_fire_iterations: get(config, "nativeIgnoreFireIterations")?.extract()?,
                per_pellet_proc: get(config, "perPelletProc")?.extract()?,
                incarnon_form: get(config, "incarnonForm")?.extract()?,
                incarnon_pool: get_opt_f(config, "incarnonPool")?,
                incarnon_cycle_seconds: get_opt_f(config, "incarnonCycleSeconds")?,
                incarnon_cycle: get(config, "incarnonCycle")?.extract()?,
                base_magazine: get(config, "baseMagazine")?.extract()?,
                reload_every_shot: get(config, "reloadEveryShot")?.extract()?,
                reload_base: extract_num(&get(config, "reloadBase")?)?,
                shell: get(config, "shell")?.extract()?,
                battery,
                crit_chance,
                crit_multiplier: extract_num(&get(config, "critMultiplier")?)?,
                fire_rate: extract_num(&get(config, "fireRate")?)?,
                charge_scales: get(config, "chargeScales")?.extract()?,
                charge_seconds: get(config, "chargeSeconds")?.extract()?,
                bow_draw: get_opt_f(config, "bowDraw")?,
                continuous: get(config, "continuous")?.extract()?,
                copy,
                orb_passes: get(config, "orbPasses")?.extract()?,
                orb_stacks: get(config, "orbStacks")?.extract()?,
                orb_special: get(config, "orbSpecial")?.extract()?,
                radial_only: get(config, "radialOnly")?.extract()?,
                ignore_headshot_bonus: config.get_item("ignoreHeadshotBonus")?.map(|v| v.extract()).transpose()?,
                forced: get(config, "forced")?.extract()?,
                ammo_cost: get(config, "ammoCost")?.extract()?,
            },
            cache: Mutex::new(FxHashMap::default()),
        })
    }
}

const CACHE_MAX: usize = 16384;
impl ModsPrep {
    /// 缓存同一个效果对象的分类与数值。
    fn entry(&self, py: Python<'_>, effect: &Bound<'_, PyAny>) -> PyResult<Arc<Entry>> {
        let key = effect.as_ptr() as usize;
        if let Some(hit) = self.cache.lock().unwrap().get(&key) {
            return Ok(hit.clone());
        }
        // 在释放缓存锁后调用 Python。
        let verdict = self.classify.bind(py).call1((effect,))?;
        let t = verdict.cast::<PyTuple>()?;
        let status: u8 = t.get_item(0)?.extract()?;
        let message: Option<String> = t.get_item(1)?.extract()?;
        let unsupported: Option<String> = t.get_item(2)?.extract()?;
        let row = Arc::new(row_of(effect)?);
        let entry = Arc::new(Entry { effect: effect.clone().unbind(), status, message, unsupported, row });
        self.cache.lock().unwrap().insert(key, entry.clone());
        Ok(entry)
    }
}

fn dict_py<'py>(py: Python<'py>, d: &Dict) -> PyResult<Bound<'py, PyDict>> {
    let out = PyDict::new(py);
    for &(k, v) in d.0.iter() {
        out.set_item(PyString::intern(py, &mods::name(k)), v)?;
    }
    Ok(out)
}

#[pyfunction]
#[pyo3(signature = (prep, effects, skipped, victim_status_count, need_spec))]
pub fn apply_mods<'py>(
    py: Python<'py>,
    prep: PyRef<'py, ModsPrep>,
    effects: &Bound<'py, PyAny>,
    skipped: Vec<String>,
    victim_status_count: &Bound<'py, PyAny>,
    need_spec: bool,
) -> PyResult<(Bound<'py, PyDict>, Py<PyAny>)> {
    let vsc: f64 = victim_status_count.extract()?;
    // 解析器会生成新的缩放效果，缓存到上限后清空。
    {
        let mut cache = prep.cache.lock().unwrap();
        if cache.len() >= CACHE_MAX {
            cache.clear();
        }
    }
    // 先提取输入效果，再按需提取瞄准效果。
    let mut entries: Vec<Arc<Entry>> = Vec::new();
    for e in effects.try_iter()? {
        entries.push(prep.entry(py, &e?)?);
    }
    let effects = entries.iter().enumerate().map(|(i, entry)| entry.classified(i)).collect();
    let mut zoom_entries = Vec::new();
    let zoom = prep.zoom.iter().enumerate().map(|(i, obj)| {
        let entry = prep.entry(py, obj.bind(py))?;
        let classified = entry.classified(entries.len() + i);
        zoom_entries.push(entry);
        Ok::<_, PyErr>(classified)
    });
    let input = ModsInput { effects, skipped, victim_status_count: vsc, need_spec };
    let result = mods::apply_mods(&prep.inner, input, zoom)?;
    entries.extend(zoom_entries);
    let list = |indexes: &[usize]| PyList::new(py, indexes.iter().map(|&i| entries[i].effect.clone_ref(py)));
    let fields = result.fields;
    let out = PyDict::new(py);
    out.set_item("damage_types", dict_py(py, &fields.damage_types)?)?;
    out.set_item("crit_chance", num_to_py(fields.crit_chance, py))?;
    out.set_item("crit_multiplier", fields.crit_multiplier)?;
    out.set_item("status_chance", fields.status_chance)?;
    out.set_item("multishot", fields.multishot)?;
    out.set_item("fire_rate", num_to_py(fields.fire_rate, py))?;
    out.set_item("magazine", fields.magazine)?;
    out.set_item("reload_time", num_to_py(fields.reload_time, py))?;
    out.set_item("spread_multiplier", fields.spread_multiplier)?;
    out.set_item("recoil_multiplier", fields.recoil_multiplier)?;
    out.set_item("ammo_max_multiplier", fields.ammo_max_multiplier)?;
    out.set_item("headshot_bonus_multiplier", fields.headshot_bonus_multiplier)?;
    out.set_item("proc_damage_multiplier", fields.proc_damage_multiplier)?;
    out.set_item("proc_duration_multiplier", fields.proc_duration_multiplier)?;
    let values = PyDict::new(py);
    for (key, value) in fields.proc_duration_types {
        values.set_item(key, value)?;
    }
    out.set_item("proc_duration_types", values)?;
    let values = PyDict::new(py);
    for (key, value) in fields.proc_max_stacks {
        values.set_item(key, value)?;
    }
    out.set_item("proc_max_stacks", values)?;
    out.set_item("status_condition_damage_multiplier", fields.status_condition_damage_multiplier)?;
    out.set_item("initial_combo", fields.initial_combo)?;
    out.set_item("combo_duration", fields.combo_duration)?;
    out.set_item("heavy_attack_efficiency", fields.heavy_attack_efficiency)?;
    out.set_item("combo_extra_chance", num_to_py(fields.combo_extra_chance, py))?;
    out.set_item("combo_miss_chance", num_to_py(fields.combo_miss_chance, py))?;
    out.set_item("heavy_charge_speed", fields.heavy_charge_speed)?;
    out.set_item("slam_damage_ratio", fields.slam_damage_ratio)?;
    let values = PyDict::new(py);
    for (key, value) in fields.faction_damage_multipliers {
        values.set_item(key, value)?;
    }
    out.set_item("faction_damage_multipliers", values)?;
    out.set_item("noncrit_damage_multiplier", fields.noncrit_damage_multiplier)?;
    out.set_item("unsupported", fields.unsupported)?;
    out.set_item("skipped", fields.skipped)?;
    out.set_item("falloff_multiplier", fields.falloff_multiplier)?;
    out.set_item("transform_time_scale", num_to_py(fields.transform_time_scale, py))?;
    out.set_item("area_direct", fields.area_direct)?;
    out.set_item("stack_native_fraction", fields.stack_native_fraction)?;
    out.set_item("stackable_hit_event", fields.stackable_hit_event)?;
    out.set_item("dot_base_damage", fields.dot_base_damage)?;
    let values = PyDict::new(py);
    for (key, value) in fields.dot_elemental_multipliers {
        values.set_item(key, value)?;
    }
    out.set_item("dot_elemental_multipliers", values)?;
    let conversions = PyDict::new(py);
    for (dtype, values) in fields.impact_proc_conversions {
        let rows = PyList::empty(py);
        for (chance, threshold, multiplier) in values {
            let row = PyDict::new(py);
            row.set_item("chance", chance)?;
            row.set_item("fireRateThreshold", threshold)?;
            row.set_item("fireRateMultiplier", multiplier)?;
            rows.append(row)?;
        }
        conversions.set_item(dtype, rows)?;
    }
    out.set_item("impact_proc_conversions", conversions)?;
    let values = PyDict::new(py);
    for (key, value) in fields.independent_proc_chances {
        values.set_item(key, value)?;
    }
    out.set_item("independent_proc_chances", values)?;
    let values = PyDict::new(py);
    for (key, value) in fields.innate_damage_types {
        values.set_item(key, value)?;
    }
    out.set_item("innate_damage_types", values)?;
    out.set_item("innate_element_candidates", py.None())?;
    out.set_item("heavy_damage", py.None())?;
    out.set_item("heavy_components", PyList::empty(py))?;
    out.set_item("double_crit_chance", fields.double_crit_chance)?;
    out.set_item("first_shot_damage_bonus", fields.first_shot_damage_bonus)?;
    out.set_item("weakpoint_damage_bonus", fields.weakpoint_damage_bonus)?;
    out.set_item("weakpoint_crit_chance_bonus", num_to_py(fields.weakpoint_crit_chance_bonus, py))?;
    out.set_item("weakpoint_crit_chance_relative", num_to_py(fields.weakpoint_crit_chance_relative, py))?;
    out.set_item("bodyshot_crit_multiplier", fields.bodyshot_crit_multiplier)?;
    out.set_item("base_crit_chance", num_to_py(fields.base_crit_chance, py))?;
    out.set_item("ammo_cost", fields.ammo_cost)?;
    out.set_item("co_bucket", list(&fields.co_bucket)?)?;
    out.set_item("co_assumed_count", fields.co_assumed_count)?;
    out.set_item("continuous", fields.continuous)?;
    out.set_item("radial_only", fields.radial_only)?;
    if let Some(ignored) = fields.ignore_headshot_bonus {
        out.set_item("radial_victim_crit", false)?;
        out.set_item("ignore_headshot_bonus", ignored)?;
    }
    out.set_item("explosion_radius_multiplier", fields.explosion_radius_multiplier)?;
    let values = PyDict::new(py);
    for (key, value) in fields.native_forced_procs {
        values.set_item(key, value)?;
    }
    out.set_item("native_forced_procs", values)?;
    out.set_item("incarnon_charge_multiplier", fields.incarnon_charge_multiplier)?;
    out.set_item("low_crit_threshold", fields.low_crit_threshold)?;
    out.set_item("low_crit_damage_base", fields.low_crit_damage_base)?;
    out.set_item("low_crit_damage_bonus", fields.low_crit_damage_bonus)?;
    out.set_item("ammo_pool_multishot_cost", fields.ammo_pool_multishot_cost)?;
    out.set_item("ammo_pool_damage_bonus", fields.ammo_pool_damage_bonus)?;
    out.set_item("victim_full_crit_chance", num_to_py(fields.victim_full_crit_chance, py))?;
    out.set_item("victim_full_crit_damage", fields.victim_full_crit_damage)?;
    out.set_item("victim_low_health_multiplier", fields.victim_low_health_multiplier)?;
    out.set_item("projectile_speed_multiplier", fields.projectile_speed_multiplier)?;
    out.set_item("charge_rate_multiplier", fields.charge_rate_multiplier)?;
    out.set_item("charge_seconds", fields.charge_seconds)?;
    out.set_item("bow_draw_seconds", fields.bow_draw_seconds)?;
    out.set_item("spread_min_deg", py.None())?;
    out.set_item("spread_max_deg", py.None())?;
    out.set_item("hit_probability", fields.hit_probability)?;
    let procs = PyDict::new(py);
    if let Some(value) = fields.crit_proc_chances {
        procs.set_item("DT_SLASH", num_to_py(value, py))?;
    }
    out.set_item("crit_proc_chances", procs)?;
    out.set_item("extra_hits", PyTuple::new(py, fields.extra_hits)?)?;
    out.set_item("cold_stack_damage", num_to_py(fields.cold_stack_damage, py))?;
    out.set_item("base_damage_bracket", fields.base_damage_bracket)?;
    out.set_item("frozen_damage_multiplier", fields.frozen_damage_multiplier)?;
    out.set_item("reflection_explosion_chance", fields.reflection_explosion_chance)?;
    out.set_item("throw_damage_bonus", num_to_py(fields.throw_damage_bonus, py))?;
    let Some(spec) = result.spec else { return Ok((out, py.None())); };
    let effect_class = prep.effect_class.bind(py);
    let mut scale_objs = Vec::new();
    for effect in spec.scale_effects {
        match effect {
            Scale::Installed(i) => scale_objs.push(entries[i].effect.clone_ref(py)),
            Scale::Synthetic(kind, op, value, source) => {
                let kwargs = PyDict::new(py);
                if let Some(source) = source {
                    kwargs.set_item("source_path", source)?;
                }
                scale_objs.push(effect_class.call((kind, op, value), Some(&kwargs))?.unbind());
            }
        }
    }
    let (base, base_total) = match spec.base_scaled {
        Some((base, total)) => (dict_py(py, &base)?.into_any().unbind(), PyFloat::new(py, total).into_any().unbind()),
        None => (py.None(), py.None()),
    };
    let final_damage = match spec.final_damage {
        Some((factor, values)) => {
            let typed = PyDict::new(py);
            for (name, value) in values {
                typed.set_item(name, value)?;
            }
            (num_to_py(factor, py), typed).into_pyobject(py)?.into_any().unbind()
        }
        None => py.None(),
    };
    let parts: Vec<Py<PyAny>> = vec![
        victim_status_count.clone().unbind(),
        PyList::new(py, scale_objs)?.into_any().unbind(),
        PyFloat::new(py, spec.ordinary_scale).into_any().unbind(),
        PyFloat::new(py, spec.compression_multiplier).into_any().unbind(),
        PyBool::new(py, spec.gunco_independent).to_owned().into_any().unbind(),
        spec.gunco_fraction.into_pyobject(py)?.into_any().unbind(),
        PyFloat::new(py, spec.victim_damage).into_any().unbind(),
        PyFloat::new(py, spec.co_fraction).into_any().unbind(),
        PyBool::new(py, spec.melee).to_owned().into_any().unbind(),
        list(&spec.melee_effects)?.into_any().unbind(),
        list(&spec.slam_effects)?.into_any().unbind(),
        base,
        base_total,
        PyFloat::new(py, spec.elemental_base_total).into_any().unbind(),
        list(&spec.pre)?.into_any().unbind(),
        list(&spec.post)?.into_any().unbind(),
        PyList::new(py, spec.innate)?.into_any().unbind(),
        spec.pool_factor.into_pyobject(py)?.into_any().unbind(),
        spec.copy_factor.into_pyobject(py)?.into_any().unbind(),
        spec.orb.into_pyobject(py)?.into_any().unbind(),
        final_damage,
        spec.frozen_factor.into_pyobject(py)?.into_any().unbind(),
    ];
    Ok((out, PyTuple::new(py, parts)?.into_any().unbind()))
}
