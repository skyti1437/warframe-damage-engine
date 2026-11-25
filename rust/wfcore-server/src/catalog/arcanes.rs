use super::{array, first_text, general, number, sort_named, text, truth};
use crate::data::EngineData;
use serde_json::{json, Value};

const EXCLUDED: &str = "/Lotus/Upgrades/CosmeticEnhancers/Offensive/SplitStatusProcsOnStackedStatusHit";

pub(super) fn rows(data: &EngineData) -> Vec<Value> {
    let mut rows = Vec::new();
    let strings = &data.incarnon_strings["data"];
    for (path, arcane) in &data.arcanes {
        let rule = &data.rules.arcanes["rules"][path];
        let event = &rule["event"];
        let enervate = event["kind"] == "enervate-joint-probability";
        let status_event = event.is_object() && !enervate;
        let support = &arcane["support"];
        let score_excluded = path == EXCLUDED;
        if !score_excluded && !status_event && (!truth(&support["manualState"]) || !truth(&support["weaponEffects"])) {
            continue;
        }
        let mut trigger = arcane["trigger"].as_object().cloned().unwrap_or_default();
        if enervate {
            trigger.insert("duration".to_owned(), Value::Null);
            trigger.insert("maxStacks".to_owned(), Value::Null);
        }
        let mut trigger = Value::Object(trigger);
        let compatibility = text(&arcane["compatibility"]);
        let slot_kind = if compatibility.contains("PlayerPowerSuit") { "warframe" } else { "weapon" };
        let max_rank = arcane["maxRank"].as_i64().unwrap_or_default().max(0);
        let official: Vec<_> = array(&arcane["effectsZhByRank"]).iter().map(official_lines).collect();
        let mut effect = official.last().map(|lines| lines.join("\n"))
            .unwrap_or_else(|| effect_summary(arcane, strings, max_rank));
        let mut condition = text(&strings[text(&trigger["conditionTag"])]).to_owned();
        if !official.is_empty() {
            if let Some((prefix, _)) = effect.split_once('：') {
                condition = prefix.to_owned();
            }
        }
        if status_event {
            condition = text(&event["condition"]).to_owned();
            effect.push('\n');
            effect.push_str(text(&event["note"]));
        }
        let description = if enervate {
            format!("{effect}\n{}", text(&event["note"]))
        } else if official.is_empty() {
            let mut bits = vec![effect.clone(), condition.clone()];
            if let Some(chance) = number(&trigger["chance"]) {
                bits.push(format!("{}%", general(chance * 100.0)));
            }
            if let Some(duration) = number(&trigger["duration"]) {
                bits.push(format!("{}s", general(duration)));
            }
            if let Some(stacks) = trigger["maxStacks"].as_i64() {
                bits.push(format!("最多 {stacks} 层"));
            }
            bits.retain(|bit| !bit.is_empty());
            bits.join(" · ")
        } else {
            effect.clone()
        };
        let levels = (0..=max_rank).map(|rank| {
            if official.is_empty() {
                let line = effect_summary(arcane, strings, rank);
                if line.is_empty() { Vec::new() } else { vec![line] }
            } else {
                official[(rank as usize).min(official.len() - 1)].clone()
            }
        }).collect::<Vec<_>>();
        let controls = &rule["controls"];
        if truth(&controls["stacks"]) {
            trigger["maxStacks"] = controls["stacks"].clone();
        }
        let icon = text(&arcane["icon"]);
        let icon_asset = (!icon.is_empty()).then(|| {
            format!("/assets/arcanes/{}", icon.trim_end_matches('/').rsplit('/').next().unwrap_or_default())
        });
        rows.push(json!({
            "path": path, "nameZh": first_text(&[&arcane["nameZh"], &arcane["nameEn"]]).trim(),
            "nameEn": first_text(&[&arcane["nameEn"], &arcane["name"]]),
            "maxRank": arcane["maxRank"], "rarity": arcane["rarity"].as_str().filter(|v| !v.is_empty()).unwrap_or("COMMON"),
            "icon": icon, "iconAsset": icon_asset, "compatibility": compatibility,
            "slotKind": slot_kind, "scoreExcluded": score_excluded,
            "equipmentType": arcane["equipmentType"], "compatibilityTags": array(&arcane["compatibilityTags"]),
            "incompatibilityTags": array(&arcane["incompatibilityTags"]),
            "descriptionSource": arcane["descriptionSource"],
            "trigger": {"chance": trigger["chance"], "duration": trigger["duration"], "maxStacks": trigger["maxStacks"],
                "conditionTag": trigger["conditionTag"], "conditionZh": condition},
            "complete": !status_event && truth(&support["complete"]),
            "runtime": truth(&arcane["runtimeScripts"]) && controls["model"] != "runtime-adapter",
            "stateControls": controls, "weaponScopes": if slot_kind == "warframe" { &rule["scopes"] } else { &Value::Null },
            "effect": effect, "effectsZhByRank": levels, "description": description, "eventModel": event["kind"],
        }));
    }
    sort_named(&mut rows);
    rows
}

fn official_lines(value: &Value) -> Vec<String> {
    let parts = value.as_array().map(Vec::as_slice).unwrap_or_else(|| std::slice::from_ref(value));
    let mut lines = Vec::new();
    for part in parts {
        let formatted = strip_color_tags(text(part)).replace("\\n", "\n");
        for raw in formatted.split('\n') {
            let mut line = String::new();
            let mut whitespace = raw.trim().chars().peekable();
            while let Some(ch) = whitespace.next() {
                if ch.is_whitespace() && whitespace.peek().is_some_and(|next| next.is_whitespace()) {
                    while whitespace.next_if(|next| next.is_whitespace()).is_some() {}
                    line.push(' ');
                } else {
                    line.push(ch);
                }
            }
            if !line.is_empty() && !lines.contains(&line) {
                lines.push(line);
            }
        }
    }
    lines
}

fn strip_color_tags(mut value: &str) -> String {
    let mut out = String::new();
    while let Some((head, tail)) = value.split_once('<') {
        out.push_str(head);
        if let Some((tag, rest)) = tail.split_once('>') {
            if tag.trim_start_matches('/').strip_prefix("DT_")
                .is_some_and(|tag| !tag.is_empty() && tag.bytes().all(|v| v.is_ascii_uppercase() || v == b'_'))
            {
                value = rest;
                continue;
            }
        }
        out.push('<');
        value = tail;
    }
    out.push_str(value);
    out
}

fn effect_summary(arcane: &Value, strings: &Value, rank: i64) -> String {
    let mut lines = Vec::new();
    let effects = array(&arcane["effects"]).iter().chain(
        array(&arcane["conditionalEffects"]).iter().flat_map(|row| array(&row["effects"]))
    );
    for effect in effects {
        let Some(label) = effect_label(text(&effect["upgradeType"])) else { continue };
        let amount = number(&effect["valuePerRank"]).unwrap_or(1.0) * (rank + 1) as f64;
        if amount != 0.0 && matches!(text(&effect["operation"]), "ADD" | "STACKING_MULTIPLY") {
            let line = format!("{:+.1}% {label}", amount * 100.0);
            if !lines.contains(&line) {
                lines.push(line);
            }
        }
    }
    if lines.is_empty() {
        text(&strings[text(&arcane["trigger"]["conditionTag"])]).to_owned()
    } else {
        lines.join("；")
    }
}

fn effect_label(upgrade: &str) -> Option<&'static str> {
    Some(match upgrade {
        "WEAPON_DAMAGE_AMOUNT" => "基伤",
        "WEAPON_MELEE_DAMAGE" => "近战基伤",
        "WEAPON_CRIT_CHANCE" => "暴击几率",
        "WEAPON_CRIT_DAMAGE" => "暴击伤害",
        "WEAPON_PROC_CHANCE" => "触发几率",
        "WEAPON_FIRE_ITERATIONS" => "多重射击",
        "WEAPON_FIRE_RATE" => "射速",
        "WEAPON_MELEE_FIRE_RATE" => "攻速",
        "WEAPON_MAGAZINE_CAPACITY" => "弹匣",
        "WEAPON_RELOAD_SPEED" => "装填速度",
        "WEAPON_PROJECTILE_SPEED" => "弹速",
        "WEAPON_RANGE" => "攻击范围",
        "WEAPON_DAMAGE_IF_VICTIM_PROC_ACTIVE" => "对异常目标伤害",
        "WEAPON_DAMAGE_WEAKPOINT" => "弱点伤害",
        "WEAPON_CRIT_CHANCE_WEAKPOINT" => "弱点暴击几率",
        "WEAPON_MELEE_HEAVY_ATTACK_DAMAGE" => "重击伤害",
        "WEAPON_FREEZE_DAMAGE" => "冰冻伤害",
        "WEAPON_FIRE_DAMAGE" => "火焰伤害",
        "WEAPON_ELECTRICITY_DAMAGE" => "电击伤害",
        "WEAPON_POISON_DAMAGE" => "毒素伤害",
        "WEAPON_IMPACT_DAMAGE" => "冲击伤害",
        "WEAPON_PUNCTURE_DAMAGE" => "穿刺伤害",
        "WEAPON_SLASH_DAMAGE" => "切割伤害",
        "WEAPON_AREA_OF_EFFECT" => "范围伤害",
        "WEAPON_HEADSHOT_DAMAGE" => "爆头伤害",
        "WEAPON_FIRST_ATTACK_DAMAGE" => "首发伤害",
        _ => return None,
    })
}
