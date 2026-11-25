//! Bounded damage preview for exported projectile events.
use super::{json, num, rows, text, truth, Map, Request, SimResult, Value};
use super::evaluate::panel_wire;
use super::weapon::{self, Effect, Weapon};
use std::collections::{BTreeSet, HashMap};
use wfcore_lib::replay::Types;

const IDENTITY_FIELDS: &[&str] = &[
    "id", "kind", "trigger", "projectile", "countRange", "aggregate", "support", "damageRange",
    "range", "radius", "multipliers", "multiplierRange", "baseSource", "conditionText",
];

pub(super) fn preview(weapon: &Weapon<'_>, req: &Request<'_>, effects: &[Effect], skipped: &[String],
    count: usize) -> SimResult<Value> {
    let mode = &weapon.modes[req.mode];
    let entries = rows(&mode["projectileEvents"]);
    if entries.is_empty() { return Ok(Value::Null); }
    let support = &mode["projectileEventSupport"];
    let adapters = rows(&support["runtimeAdapters"]);
    let derived = mode.get("projectileDerivedEvents").filter(|v| truth(v))
        .map_or_else(|| rows(&mode["derivedEvents"]), rows);
    let mut unsupported = BTreeSet::new();
    for row in std::iter::once(support).chain(adapters).chain(derived).chain(entries) {
        unsupported.extend(rows(&row["unsupported"]).iter().filter_map(Value::as_str).map(str::to_owned));
    }
    let mut events = Vec::with_capacity(entries.len());
    let mut by_source = HashMap::new();
    let profile = weapon.profile(req.mode);
    let stackable = &profile["stackable"];
    let is_blob = |event: &Value| {
        stackable["kind"] == "stug" && matches!(text(&event["kind"]), "explosion" | "embedDeath")
            && truth(&mode["damage"]["projectile"]) && event["projectile"] == mode["damage"]["projectile"]
            && event["damage"]["types"] == stackable["explosion"]["damage"]
    };
    let stug_explosion = entries.iter().find(|event| event["kind"] == "explosion" && is_blob(event));
    for entry in entries {
        let mut row: Value = Value::Object(IDENTITY_FIELDS.iter().filter_map(|key| {
            entry.get(key).filter(|v| !v.is_null()).map(|v| ((*key).to_owned(), v.clone()))
        }).collect());
        row["unsupported"] = json!(rows(&entry["unsupported"]));
        if truth(&entry["damage"]["types"]) {
            let mut mode: Map<_, _> = ["fireRate", "reloadTime", "falloffRange", "falloffMinDamage", "nativeIgnoreFireIterations"]
                .iter().filter_map(|key| mode.get(key).filter(|value| !value.is_null()).map(|value| ((*key).to_owned(), value.clone()))).collect();
            let mut damage = entry["damage"].clone();
            damage.as_object_mut().unwrap().remove("components");
            mode.insert("multishot".into(), json!(1));
            mode.insert("damage".into(), damage);
            let blob = is_blob(entry);
            let filtered;
            let effects = if blob && stackable["explosion"]["takesConditionOverload"] != true {
                filtered = effects.iter().filter(|effect| effect.upgrade != "WEAPON_DAMAGE_IF_VICTIM_PROC_ACTIVE").cloned().collect::<Vec<_>>();
                &filtered
            } else { effects };
            let fields = weapon.reduce(req.mode, &Value::Object(mode), profile, effects, req.scenario.cycle, count as f64,
                false, req.scenario.heavy, skipped)?;
            if blob {
                row["hitClassification"] = json!("area-direct");
                if entry["kind"] == "embedDeath" {
                    if let Some(explosion) = stug_explosion {
                        row["aggregate"] = json!(false);
                        row["alternativeOf"] = explosion["id"].clone();
                    }
                }
            }
            let mut types = Types::new();
            let core = weapon::core(&fields, &mut types, "", 1.0);
            let panel = panel_wire(&fields, &core, &types);
            let total = num(&panel["totalDamage"]);
            let average = num(&panel["averageHit"]);
            let copies = fields.multishot;
            row["perOccurrence"] = json!({"moddedDamage":total, "averageHit":average, "copyMultiplier":copies});
            let [a, b] = [num(&entry["countRange"][0]), num(&entry["countRange"][1])];
            let [low, high] = [a.min(b), a.max(b)];
            row["contribution"] = json!({
                "moddedDamage":[total * copies * low, total * copies * high],
                "averageHit":[average * copies * low, average * copies * high],
            });
            unsupported.extend(fields.unsupported);
            if let Some(source) = entry["damage"]["source"].as_str().filter(|s| !s.is_empty()) {
                by_source.insert(source, (total, average));
            }
        }
        events.push(row);
    }
    for (entry, event) in entries.iter().zip(&mut events) {
        if entry["kind"] != "scriptRadial" { continue; }
        let (Some(&(total, average)), [a, b]) = (by_source.get(text(&entry["baseSource"])), rows(&entry["multiplierRange"])) else {
            unsupported.insert("script-radial-base-unresolved".into());
            continue;
        };
        let (a, b) = (num(a), num(b));
        let (low, high) = (a.min(b), a.max(b));
        event["derivedFrom"] = entry["baseSource"].clone();
        event["perOccurrenceRange"] = json!({"moddedDamage":[total*low,total*high], "averageHit":[average*low,average*high]});
    }
    let mut totals = Map::new();
    for kind in ["moddedDamage", "averageHit"] {
        let (mut minimum, mut maximum, mut known) = (0.0, 0.0, 0.0);
        for event in events.iter().filter(|row| row["aggregate"] == true) {
            if let [low, high] = rows(&event["contribution"][kind]) {
                let (low, high) = (num(low), num(high));
                minimum += low;
                maximum += high;
                if low == high { known += low; }
            }
        }
        totals.insert(kind.into(), json!({"known":known,"minimum":minimum,"maximum":maximum}));
    }
    let excluded: Vec<_> = events.iter().filter(|event| event["aggregate"] != true).map(|event| &event["id"])
        .chain(derived.iter().filter_map(|event| event.get("id").filter(|value| !value.is_null()))).collect();
    let partial = !unsupported.is_empty() || !excluded.is_empty() || events.iter().any(|event| {
        event["aggregate"] == true && event["countRange"][0] != event["countRange"][1]
    });
    Ok(json!({"status":if partial {"partial"} else {"exact"}, "state":mode["projectileEventState"].as_object().cloned().unwrap_or_default(),
        "events":events,"runtimeAdapters":adapters,"derivedEvents":derived,"totals":totals,"excluded":excluded,"unsupported":unsupported}))
}

pub(super) fn metrics(panel: &Value, heavy: &Value, events: &Value) -> Value {
    let mut result = super::evaluate::metrics(panel, heavy);
    let hit = num(&events["totals"]["averageHit"]["minimum"]);
    if hit > 0.0 {
        let (rate, magazine, reload) = (num(&panel["fireRate"]), num(&panel["magazine"]), num(&panel["reloadTime"]));
        let burst = hit * rate;
        let sustained = if magazine != 0.0 && rate != 0.0 {
            let cycle = magazine / rate + reload;
            if cycle != 0.0 { hit * magazine / cycle } else { 0.0 }
        } else { burst };
        result["shotDamage"] = json!(hit);
        result["burstDps"] = json!(burst);
        result["attackDps"] = json!(burst);
        result["sustainedDps"] = json!(sustained);
    }
    result
}
