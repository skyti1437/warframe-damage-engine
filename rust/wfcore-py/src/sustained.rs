use pyo3::prelude::*;
use pyo3::types::PyDict;
use wfcore_lib::replay::Types;
use wfcore_lib::sustained::{self, SustainedInput};
use crate::replay::{core_of, opt_attr, vector_of};

#[pyclass(frozen)]
pub struct SustainedTarget {
    inner: sustained::SustainedTarget,
}

#[pymethods]
impl SustainedTarget {
    #[new]
    fn new(config: &Bound<'_, PyDict>) -> PyResult<Self> {
        let get = |k: &str| -> PyResult<Bound<'_, PyAny>> {
            config.get_item(k)?.ok_or_else(|| pyo3::exceptions::PyKeyError::new_err(k.to_string()))
        };
        let inner = sustained::SustainedTarget {
            armour_base: get("armourBase")?.extract()?,
            armour_strip: get("armourStrip")?.extract()?,
            aura_armour_reduction: get("auraArmourReduction")?.extract()?,
            elemental_armour_strip: get("elementalArmourStrip")?.extract()?,
            overguard: get("overguard")?.extract()?,
            shield: get("shield")?.extract()?,
            magnetic_stacks: get("magneticStacks")?.extract()?,
            shielded: get("shielded")?.extract()?,
            faction_deltas: get("factionDeltas")?.extract()?,
            armour_bypass: get("armourBypass")?.extract()?,
            shield_bypass: get("shieldBypass")?.extract()?,
            target_faction: get("targetFaction")?.extract()?,
            faction_bracket: match config.get_item("factionBracket")? { Some(v) => v.extract()?, None => 1.0 },
            body: get("body")?.extract()?,
            located: get("located")?.extract()?,
            head_eligible: get("headEligible")?.extract()?,
            existing_magnetic: get("existingMagnetic")?.extract()?,
        };
        Ok(Self { inner })
    }
}

#[pyfunction]
#[pyo3(signature = (target, stats, attacks_per_second, status_rate_multiplier, damage_rate_multiplier, total))]
pub fn sustained_status(
    target: &Bound<'_, SustainedTarget>,
    stats: &Bound<'_, PyAny>,
    attacks_per_second: f64,
    status_rate_multiplier: f64,
    damage_rate_multiplier: f64,
    total: bool,
) -> PyResult<f64> {
    let target = &target.get().inner;
    let mut types = Types::new();
    let damage_types = vector_of(&mut types, &stats.getattr("damage_types")?)?;
    let forced = vector_of(&mut types, &stats.getattr("native_forced_procs")?)?;
    let independent = vector_of(&mut types, &stats.getattr("independent_proc_chances")?)?;
    let core = core_of(stats, &mut types, &target.target_faction, target.faction_bracket, &damage_types, &forced, &independent)?;
    let input = SustainedInput {
        types: &types,
        core: &core,
        attacks_per_second,
        status_rate_multiplier,
        damage_rate_multiplier,
        total,
        magazine: opt_float(stats, "magazine")?,
        first_shot_bonus: opt_float(stats, "first_shot_damage_bonus")?,
    };
    Ok(sustained::sustained_status(target, input))
}

fn opt_float(stats: &Bound<'_, PyAny>, name: &str) -> PyResult<f64> {
    Ok(match opt_attr(stats, name)? {
        Some(v) if v.is_truthy()? => v.extract::<f64>()?,
        _ => 0.0,
    })
}
