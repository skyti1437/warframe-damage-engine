use super::{array, entries, first, number, text, truth};
use crate::data::EngineData;
use serde_json::{json, Map, Value};

pub(super) fn stats(data: &EngineData) -> Value {
    let rules = &data.rules.riven;
    let types = entries(&rules["types"]).map(|(kind, traits)| {
        let template = template(&data.riven_templates, kind);
        let rows = entries(traits).filter_map(|(code, row)| {
            let rule = &rules["traits"][code];
            let recipe = array(&data.riven_templates["fusionRecipes"]).iter().find(|recipe| {
                recipe["weaponType"] == *kind && recipe["result"] == *code
            });
            if array(&rules["fusionCodes"]).iter().any(|v| v == code) && recipe.is_none() {
                return None;
            }
            let stat = |field: &str| array(&template["stats"]).iter().find(|stat| {
                truth(&row[field]) && stat["tag"] == row[field]
            }).unwrap_or(&Value::Null);
            let value = stat("valueTag");
            let upgrades = array(&value["upgrades"]);
            let upgrade = if value["tag"] == "WeaponCritChanceMod" && upgrades.len() == 2 {
                upgrades.iter().find(|upgrade| !truth(&upgrade["validModifiers"]))
            } else {
                upgrades.first()
            };
            let base = upgrade.and_then(|upgrade| number(&upgrade["rawValue"]))
                .map(|value| json!(value.abs() * if rule[1] == "flat" { 90.0 } else { 9000.0 }))
                .unwrap_or_else(|| row["defaultBase"].clone());
            let face = stat("faceTag");
            let icon = text(first(&[&face["icon"], &row["icon"]])).trim_start_matches('/');
            let art = if !icon.is_empty() && data.root.join("frontend/assets/mods/art").join(icon).is_file() {
                json!(format!("/assets/mods/art/{icon}"))
            } else { Value::Null };
            let mut out = json!({
                "code": code, "label": rule[0], "baseValue": base, "unit": rule[1],
                "canPositive": rule[2], "canNegative": rule[3],
                "objectiveSupported": array(&data.rules.mods["supportedUpgrades"]).contains(&rule[4]),
                "art": art, "namePrefix": first(&[&face["prefix"], &row["prefix"]]),
                "nameSuffix": first(&[&face["suffix"], &row["suffix"]]),
            });
            if let Some(recipe) = recipe {
                let label = |value: &Value| first(&[&rules["traits"][text(value)][0], value]).clone();
                out["isFusion"] = json!(true);
                out["fusionRecipe"] = json!({
                    "primary": recipe["primary"], "primaryLabel": label(&recipe["primary"]),
                    "secondary": recipe["secondary"], "secondaryLabel": label(&recipe["secondary"]),
                });
            }
            Some(out)
        }).collect::<Vec<_>>();
        (kind.clone(), json!(rows))
    }).collect::<Map<_, _>>();
    Value::Object(types)
}

fn template<'a>(templates: &'a Value, kind: &str) -> &'a Value {
    let mut candidates = array(&templates["templates"]).iter().filter(|template| template["weaponType"] == kind);
    let first = candidates.next();
    let preferred = match kind {
        "melee" => "PlayerMeleeWeaponRandomModRare",
        "pistol" => "LotusPistolRandomModRare",
        _ => "",
    };
    first.into_iter().chain(candidates).find(|template| template["id"] == preferred)
        .or(first).unwrap_or(&Value::Null)
}
