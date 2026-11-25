use super::{array, entries, number, text, truth};
use crate::data::corrections::merge;
use serde_json::{json, Map, Value};
use std::collections::BTreeSet;

pub(super) fn kitguns(data: &Value) -> Value {
    let mut catalog = Map::new();
    for (key, label) in [("barrels", "chambers"), ("handles", "grips"), ("clips", "loaders")] {
        let rows = entries(&data[key]).filter(|(_, row)| row.is_object()).map(|(path, row)| {
            let overrides = array(&row["overrides"]);
            let mut entry = named(path, row);
            merge(&mut entry, &json!({
                "slot": row["slot"],
                "gunTypes": overrides.iter().filter(|v| v.is_object()).map(|v| text(&v["gunType"])).collect::<BTreeSet<_>>(),
                "disposition": row["omegaAttenuation"], "primaryDisposition": row["primaryOmegaAttenuation"],
                "primaryModes": array(&row["primaryModes"]).len(), "secondaryModes": array(&row["secondaryModes"]).len(),
            }));
            if key == "barrels" {
                let modes = |kind| array(&row[kind]).iter().filter(|v| v.is_object()).map(mode_summary).collect::<Vec<_>>();
                entry["modes"] = json!({"primary": modes("primaryModes"), "secondary": modes("secondaryModes")});
            } else {
                entry["overrides"] = json!(overrides.iter().filter(|v| v.is_object()).map(|v| json!({
                    "gunType": text(&v["gunType"]), "barrelType": text(&v["barrelType"]), "damage": v["damage"],
                    "fireRate": v["fireRate"], "critChance": v["critChance"], "critMultiplier": v["critMultiplier"],
                    "procChance": v["procChance"], "magazineSize": v["magazineSize"], "reloadTime": v["reloadTime"],
                    "traceDistance": v["traceDistance"]
                })).collect::<Vec<_>>());
            }
            entry
        }).collect::<Vec<_>>();
        catalog.insert(label.to_owned(), json!(rows));
    }
    Value::Object(catalog)
}

fn mode_summary(mode: &Value) -> Value {
    let damage = &mode["damage"];
    let radial = &damage["radial"];
    let types = numeric_types(&damage["types"]);
    let total = if damage["total"].is_number() {
        damage["total"].clone()
    } else {
        let total = types.values().filter_map(number).sum::<f64>();
        json!((total != 0.0).then_some(total))
    };
    json!({
        "total": total, "types": types, "critChance": damage["critChance"], "critMultiplier": damage["critMultiplier"],
        "procChance": damage["procChance"], "fireRate": mode["fireRate"], "trigger": mode["trigger"],
        "unsupported": array(&mode["unsupported"]),
        "radial": if truth(radial) { json!({"total": radial["total"], "types": numeric_types(&radial["types"]),
            "radius": radial["radius"], "falloff": radial["falloff"], "procChance": radial["procChance"]}) } else { Value::Null }
    })
}

pub(super) fn zaws(data: &Value) -> Value {
    let mut catalog = Map::new();
    for key in ["strikes", "grips", "links"] {
        let rows = entries(&data[key]).filter(|(_, row)| row.is_object()).map(|(path, row)| {
            let round = |field: &str| number(&row[field]).and_then(|v| format!("{v:.4}").parse::<f64>().ok());
            let mut entry = named(path, row);
            let fields = match key {
                "strikes" => json!({
                    "disposition": row["omegaAttenuation"], "oneHanded": row["oneHanded"], "twoHanded": row["twoHanded"],
                    "damage": {"total": row["damage"]["total"], "types": numeric_types(&row["damage"]["types"])},
                    "critChance": row["critChance"], "critMultiplier": row["critMultiplier"],
                    "procChance": row["procChance"], "speedMod": row["speedMod"],
                }),
                "grips" => json!({"handed": row["handed"], "damageBonus": round("damageBonus"), "baseSpeed": round("baseSpeed")}),
                _ => json!({"speedBonus": round("speedBonus"), "critChance": row["critChance"],
                    "statusChance": row["statusChance"], "damageBonus": round("damageBonus")}),
            };
            merge(&mut entry, &fields);
            entry
        }).collect::<Vec<_>>();
        catalog.insert(key.to_owned(), json!(rows));
    }
    Value::Object(catalog)
}

fn numeric_types(types: &Value) -> Map<String, Value> {
    entries(types).filter(|(_, value)| value.is_number()).map(|(key, value)| (key.clone(), value.clone())).collect()
}

fn named(path: &str, row: &Value) -> Value {
    json!({"path": row["path"].as_str().filter(|v| !v.is_empty()).unwrap_or(path),
        "nameZh": text(&row["nameZh"]), "nameEn": text(&row["nameEn"])})
}
