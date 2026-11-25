use super::{num, rows, text, SimResult};
use crate::data::{retain_fields, EngineData, Records};
use std::collections::HashMap;
use serde_json::{json, Map, Value};

pub(crate) struct SimCatalog {
    pub weapons: Records,
    mods: Records,
    variants: Records,
    aliases: HashMap<String, usize>,
    pub arcanes: Records,
    pub utility: Records,
    pub stances: Vec<Value>,
    pub enemies: Records,
    pub build_options: Value,
}

impl SimCatalog {
    pub fn from_catalog(mut catalog: Value) -> Self {
        let mut take = |key: &str| catalog[key].take();
        let utility = Self::index(take("utilityMods"), &[]);
        let mods = Self::index(take("manualMods"), &[]);
        let mut aliases = HashMap::new();
        if let Value::Object(paths) = take("manualModAliases") {
            for (alias, path) in paths {
                if !mods.contains_key(&alias) {
                    if let Some(index) = mods.get_index_of(text(&path)).or_else(|| aliases.get(text(&path)).copied()) {
                        aliases.insert(alias, index);
                    }
                }
            }
        }
        let mut variants = Self::index(take("manualModVariants"), &[]);
        variants.retain(|path, _| !utility.contains_key(path));
        Self {
            weapons: Self::index(take("weapons"), &["path", "nameEn", "nameZh", "weaponType", "rivenType", "available", "issue",
                "buildAvailable", "buildIssue", "analysisAvailable", "analysisIssue", "exalted", "disposition", "fireModes", "innateElementCandidates"]),
            enemies: Self::index(take("enemies"), &["path", "nameInternal", "nameEn", "nameZh", "complete", "unsupported", "defenseSources", "hasHeadProxy"]),
            mods, variants, aliases, utility,
            build_options: take("buildOptions"),
            arcanes: Self::index(take("arcanes"), &["path", "nameEn", "nameZh", "nameInternal", "slotKind", "maxRank", "trigger", "scoreExcluded"]),
            stances: match take("stances") { Value::Array(rows) => rows, _ => Vec::new() },
        }
    }

    fn index(value: Value, fields: &[&str]) -> Records {
        let Value::Array(rows) = value else { return Records::new(); };
        rows.into_iter().map(|mut row| {
            let path = text(&row["path"]).to_owned();
            if !fields.is_empty() { retain_fields(&mut row, fields); }
            (path, row)
        }).collect()
    }

    pub fn mod_at(&self, path: &str) -> Option<&Value> {
        self.variants.get(path).or_else(|| self.mods.get(path)).or_else(|| {
            self.aliases.get(path).and_then(|&index| self.mods.get_index(index)).map(|(_, row)| row)
        })
    }
}

#[derive(Clone)]
pub(super) struct Scenario<'a> {
    pub aiming: bool,
    pub cycle: bool,
    pub heavy: bool,
    pub cloaked: bool,
    pub spread: bool,
    pub combo: u64,
    pub distance: Option<f64>,
    pub nearest: Option<f64>,
    pub policy: &'a str,
    pub gauge_heads: bool,
    pub state: Option<&'a Map<String, Value>>,
}

#[derive(Clone)]
pub(super) struct Request<'a> {
    pub body: &'a Map<String, Value>,
    pub melee: bool,
    pub stance_polarity: Value,
    pub weapon: &'a Value,
    pub mode: usize,
    pub exported_mode_panel: bool,
    pub target: Value,
    pub damage_model: String,
    pub enhancements: Value,
    pub enhancement_notes: Vec<String>,
    pub level: u64,
    pub steel: bool,
    pub strip: f64,
    pub elemental_strip: f64,
    pub aim: f64,
    pub window: f64,
    pub forced: Vec<(String, f64)>,
    pub scenario: Scenario<'a>,
    pub slot_count: usize,
    pub slots: Vec<Value>,
    pub polarity_source: &'static str,
    pub exilus_polarity: Value,
    pub choices: &'a Value,
}

fn number(value: &Value, field: &str, min: f64, max: f64) -> SimResult<f64> {
    value.as_f64().filter(|v| v.is_finite() && (min..=max).contains(v))
        .ok_or_else(|| format!("{field} 必须在 {min} 到 {max} 之间"))
}

fn integer(value: &Value, field: &str, min: u64, max: u64) -> SimResult<u64> {
    value.as_u64().filter(|v| (min..=max).contains(v))
        .ok_or_else(|| format!("{field} 必须为 {min} 到 {max} 之间的整数"))
}

fn boolean(value: Option<&Value>, field: &str) -> SimResult<bool> {
    value.map_or(Ok(false), |v| v.as_bool().ok_or_else(|| format!("{field} 必须为布尔值")))
}

fn object<'a>(value: &'a Value, field: &str) -> SimResult<&'a Map<String, Value>> {
    value.as_object().ok_or_else(|| format!("{field} 必须为对象"))
}

fn check_keys(fields: &Map<String, Value>, allowed: &[&str], label: &str) -> SimResult<()> {
    let mut unknown: Vec<_> = fields.keys().filter(|key| !allowed.contains(&key.as_str())).cloned().collect();
    unknown.sort_unstable();
    if unknown.is_empty() { Ok(()) } else { Err(format!("{label} 包含不支持的字段 {}", unknown.join(", "))) }
}

fn strip_ratio(body: &Map<String, Value>, key: &str, alias: &str) -> SimResult<f64> {
    let read = |name| body.get(name).map(|v| number(v, name, 0.0, 1.0)).transpose();
    let (left, right) = (read(key)?, read(alias)?);
    if left.zip(right).is_some_and(|(a, b)| a != b) {
        return Err(format!("{key} 与 {alias} 冲突"));
    }
    Ok(left.or(right).unwrap_or(0.0))
}

pub(super) fn polarity(value: &Value) -> Value {
    let token = text(value).trim().to_ascii_uppercase();
    if matches!(token.as_str(), "" | "AP_UNIVERSAL" | "AP_NONE" | "NONE" | "NULL") {
        Value::Null
    } else {
        json!(token)
    }
}

impl<'a> Request<'a> {
    pub fn parse_weapon(data: &'a EngineData, catalog: &'a SimCatalog, body: &'a Map<String, Value>,
        weapon: &'a Value) -> SimResult<Self> {
        let get = |key: &str| body.get(key).unwrap_or(&Value::Null);
        let mode = integer(body.get("fireMode").unwrap_or(&json!(0)), "fireMode", 0, u64::MAX)? as usize;
        if mode >= rows(&weapon["fireModes"]).len() {
            return Err("fireMode 超出射击模式范围".into());
        }
        let damage_model = if get("damageModel").is_null() || get("damageModel") == "" {
            "target".to_owned()
        } else { text(get("damageModel")).trim().to_ascii_lowercase() };
        if !matches!(damage_model.as_str(), "target" | "panel" | "custom") {
            return Err("damageModel 必须为 panel、target 或 custom".into());
        }
        let selected_enemy = catalog.enemies.get(text(get("enemy")).trim());
        let enemy = match selected_enemy {
            Some(value) => value,
            None if damage_model == "target" => return Err("enemy 必须是完整的离线目标路径".into()),
            None => catalog.enemies.first().map(|(_, v)| v).ok_or("缺少离线目标记录")?,
        };
        if enemy["complete"] == false {
            return Err(format!("敌人防御模型不完整 {}", rows(&enemy["unsupported"]).iter().map(text).collect::<Vec<_>>().join(", ")));
        }
        let mut enhancement_notes = Vec::new();
        let target = if damage_model == "custom" {
            let snapshot = get("defenseSnapshot");
            custom_target(if snapshot.is_null() { get("customDefense") } else { snapshot })?
        } else {
            if !get("defenseSnapshot").is_null() { return Err("defenseSnapshot 需要 damageModel=custom".into()); }
            if damage_model == "target" && enemy["defenseSources"]["baseLevel"] == "default-1" {
                enhancement_notes.push("enemy-base-level-unverified".to_owned());
            }
            data.enemy(text(&enemy["path"])).ok_or("enemy 缺少战斗数据")?
        };
        let nested = if get("scenario").is_null() { None } else { Some(object(get("scenario"), "scenario")?) };
        if body.contains_key("conditionalStacks") || nested.is_some_and(|s| s.contains_key("conditionalStacks")) {
            return Err("conditionalStacks 由已装备的 MOD 推导".into());
        }
        if let Some(nested) = nested {
            check_keys(nested, &["aiming", "incarnonCycle", "heavyAttack", "distance", "nearestEnemyDistance",
                "cloaked", "comboTier", "triggerPolicy", "detonationDelaySeconds", "detonationBatchSize",
                "combatState", "modelSpread", "headshotChance", "companionPlayerStacks"], "scenario")?;
        }
        let scenario_value = |key: &str| nested.and_then(|s| s.get(key)).or_else(|| body.get(key));
        let optional_distance = |key| scenario_value(key).filter(|v| !v.is_null())
            .map(|v| number(v, key, 0.0, f64::MAX)).transpose();
        let aim = scenario_value("headshotChance").map_or(Ok(0.0), |v| number(v, "headshotChance", 0.0, 1.0))?;
        if aim > 0.0 && damage_model != "custom" && enemy["hasHeadProxy"] != true {
            return Err("headshotChance 需要已验证的敌人头部命中代理".into());
        }
        let policy = scenario_value("triggerPolicy").map_or("emergent", text);
        if !matches!(policy, "emergent" | "assumed_max") {
            return Err("triggerPolicy 必须为 emergent 或 assumed_max".into());
        }
        let state = scenario_value("combatState").map(|v| object(v, "combatState")).transpose()?;
        if let Some(state) = state {
            check_keys(state, &["gaugeChargingHeadshots", "channeling", "overshields", "frameArmor", "sprintMultiplier",
                "energyPool", "frameShield", "aimGliding", "sliding", "infiniteAmmo", "zymosChildOverlap",
                "meleeRoute", "meleeStance", "meleeCombo", "meleeTargetDistance", "selfShieldBreakForced",
                "selfShieldBreakAt", "selfShieldBreakInterval", "selfOverguardBreakAt", "selfOverguardBreakInterval",
                "selfShieldHitAt", "selfShieldHitInterval", "equipFromPrimaryAt", "equipFromPrimaryInterval",
                "abilityUseAt", "abilityUseInterval", "energySpent", "energySpendPerSecond", "abilityUses"], "combatState")?;
            for key in ["channeling", "overshields", "aimGliding", "sliding", "infiniteAmmo", "selfShieldBreakForced"] {
                boolean(state.get(key), &format!("combatState.{key}"))?;
            }
            for key in ["frameArmor", "frameShield", "sprintMultiplier", "energyPool", "energySpent", "energySpendPerSecond",
                "selfShieldBreakAt", "selfShieldBreakInterval", "selfOverguardBreakAt", "selfOverguardBreakInterval",
                "selfShieldHitAt", "selfShieldHitInterval", "equipFromPrimaryAt", "equipFromPrimaryInterval",
                "abilityUseAt", "abilityUseInterval"] {
                if let Some(v) = state.get(key) { number(v, &format!("combatState.{key}"), 0.0, f64::MAX)?; }
            }
            if let Some(v) = state.get("abilityUses") { integer(v, "combatState.abilityUses", 0, 100000)?; }
            if let Some(route) = state.get("meleeRoute") {
                if !super::melee::ROUTES.contains(&text(route)) { return Err("meleeRoute 无效".into()); }
            }
            for key in ["meleeStance", "meleeCombo"] {
                if state.get(key).is_some_and(|v| !v.is_string()) { return Err(format!("{key} 必须为字符串")); }
            }
            if let Some(v) = state.get("meleeTargetDistance") { number(v, "meleeTargetDistance", 0.0, f64::MAX)?; }
            if state.get("zymosChildOverlap").is_some_and(|v| v != "off") {
                return Err("当前模拟支持邪莫斯的默认直接命中和寄生爆炸".into());
            }
        }
        let gauge_heads = boolean(state.and_then(|v| v.get("gaugeChargingHeadshots")), "combatState.gaugeChargingHeadshots")?;
        optional_distance("detonationDelaySeconds")?;
        if let Some(v) = scenario_value("detonationBatchSize") { integer(v, "detonationBatchSize", 1, 8)?; }
        let scenario = Scenario {
            aiming: boolean(scenario_value("aiming"), "aiming")?,
            cycle: boolean(scenario_value("incarnonCycle"), "incarnonCycle")?,
            heavy: boolean(scenario_value("heavyAttack"), "heavyAttack")?,
            cloaked: boolean(scenario_value("cloaked"), "cloaked")?,
            spread: boolean(scenario_value("modelSpread"), "modelSpread")?,
            combo: scenario_value("comboTier").map_or(Ok(0), |v| integer(v, "comboTier", 0, 11))?,
            distance: optional_distance("distance")?,
            nearest: optional_distance("nearestEnemyDistance")?,
            policy, gauge_heads, state,
        };
        let level = body.get("level").map_or(Ok(160), |v| integer(v, "level", 0, u64::MAX))?;
        let steel = boolean(body.get("steelPath"), "steelPath")?;
        let window = body.get("windowSeconds").map_or(Ok(30.0), |v| number(v, "windowSeconds", 0.1, 600.0))?;
        let enhancements = enhancements(data, body.get("enemyEnhancements").or_else(|| body.get("enhancements")),
            steel, &mut enhancement_notes)?;
        let forced = if let Some(values) = body.get("forcedProcs") {
            object(values, "forcedProcs")?.iter().map(|(key, value)| {
                number(value, &format!("forcedProcs[{key}]"), 0.0, f64::MAX).map(|v| (key.clone(), v))
            }).collect::<SimResult<_>>()?
        } else { Vec::new() };
        let slot_count = body.get("slotCount").map_or(Ok(8), |v| integer(v, "slotCount", 1, 8))? as usize;
        let mods = body.get("mods").map_or(Some(&[][..]), |v| v.as_array().map(Vec::as_slice))
            .ok_or("mods 必须为数组")?;
        if mods.len() > slot_count { return Err("mods 的条目数超出 slotCount".into()); }
        let (slots, polarity_source) = if get("slotPolarities").is_null() {
            (rows(&weapon["polarities"]), "export")
        } else {
            let slots = get("slotPolarities").as_array().ok_or("slotPolarities 必须为数组")?;
            if slots.iter().take(slot_count).any(|v| !v.is_null() && text(v).trim().is_empty()) {
                return Err("slotPolarities 需要有效的极性或 null".into());
            }
            (slots.as_slice(), "payload")
        };
        if slots.len() < slot_count { return Err("武器栏位极性数据不完整".into()); }
        let melee = matches!(text(&weapon["slot"]), "melee" | "arch_melee" | "space_melee" | "archmelee" | "spacemelee");
        let mut stance_polarity = if melee { weapon["polarities"][8].clone() } else { Value::Null };
        let mut exilus_polarity = weapon["polarities"][if melee { 9 } else { 8 }].clone();
        if !get("specialSlotPolarities").is_null() {
            let special = object(get("specialSlotPolarities"), "specialSlotPolarities")?;
            check_keys(special, if melee { &["exilus", "stance"] } else { &["exilus"] }, "specialSlotPolarities")?;
            for (key, polarity) in [("exilus", &mut exilus_polarity), ("stance", &mut stance_polarity)] {
                let Some(v) = special.get(key) else { continue; };
                let allowed = ["AP_ATTACK", "AP_DEFENSE", "AP_TACTIC", "AP_UMBRA", "AP_POWER",
                    "AP_WARD", "AP_PRECEPT", "AP_UNIVERSAL", "AP_ANY"];
                if !v.is_null() && !allowed.contains(&text(v)) { return Err(format!("{key} 极性无效")); }
                *polarity = v.clone();
            }
        }
        if ["kitgunParts", "externalBuffs"].iter().any(|key| {
            !get(key).is_null() && get(key).as_array().is_none_or(|v| !v.is_empty())
        }) || !get("innateElement").is_null() {
            return Err("当前模拟范围未支持此配装".into());
        }
        Ok(Self { body, melee, stance_polarity, weapon, mode, exported_mode_panel: false, target, damage_model, enhancements, enhancement_notes, level, steel,
            strip: strip_ratio(body, "armourStrip", "armorStrip")?,
            elemental_strip: strip_ratio(body, "elementalArmourStrip", "elementalArmorStrip")?,
            aim, window, forced, scenario, slot_count,
            slots: slots.iter().take(slot_count).map(polarity).collect(), polarity_source,
            exilus_polarity, choices: get("incarnonChoices") })
    }
}

fn enhancements(data: &EngineData, value: Option<&Value>, steel: bool, notes: &mut Vec<String>) -> SimResult<Value> {
    let mut selected = json!({"eximus": false, "archimedea": false, "empowered": false, "playerCount": null, "magneticStacks": 0});
    let Some(value) = value.filter(|v| !v.is_null()) else { return Ok(selected); };
    let raw = object(value, "enemyEnhancements")?;
    check_keys(raw, &["eximus", "archimedea", "empowered", "playerCount", "magneticStacks",
        "steelPath", "noOverguard", "shieldOverride"], "enemyEnhancements")?;
    for key in ["eximus", "archimedea", "empowered", "noOverguard", "steelPath"] {
        if key == "noOverguard" && !raw.contains_key(key) { continue; }
        let enabled = boolean(raw.get(key), &format!("enemyEnhancements.{key}"))?;
        if key == "steelPath" {
            if raw.contains_key(key) && enabled != steel { return Err("两处 steelPath 必须一致".into()); }
            if !enabled { continue; }
        }
        selected[key] = json!(enabled);
        if enabled && matches!(key, "eximus" | "archimedea" | "empowered")
            && data.enemy_overrides["wikiEnhancements"]["support"][key] != true {
            notes.push(format!("enemy-enhancement-{key}-manifest-only"));
        }
    }
    if let Some(v) = raw.get("shieldOverride").filter(|v| !v.is_null()) {
        selected["shieldOverride"] = json!(number(v, "enemyEnhancements.shieldOverride", 0.0, f64::MAX)?);
    }
    let players = raw.get("playerCount").filter(|v| !v.is_null())
        .map_or(Ok(1), |v| integer(v, "enemyEnhancements.playerCount", 1, 4))?;
    if selected["empowered"] == true { selected["playerCount"] = json!(players); }
    if let Some(v) = raw.get("magneticStacks") {
        selected["magneticStacks"] = json!(integer(v, "enemyEnhancements.magneticStacks", 0, 10)?);
    }
    Ok(selected)
}

pub(super) fn custom_target(value: &Value) -> SimResult<Value> {
    let raw = object(value, "defenseSnapshot")?;
    check_keys(raw, &["health", "armour", "armor", "shield", "healthType", "armourType", "armorType",
        "shieldType", "baseLevel", "factionResistance"], "defenseSnapshot")?;
    for (key, alias) in [("armour", "armor"), ("armourType", "armorType")] {
        if let Some((a, b)) = raw.get(key).zip(raw.get(alias)) {
            if a != b && !(a.is_number() && b.is_number() && num(a) == num(b)) {
                return Err(format!("defenseSnapshot.{key} 与 {alias} 冲突"));
            }
        }
    }
    let zero = json!(0);
    let amount = |key: &str, alias: &str, minimum| {
        number(raw.get(key).or_else(|| raw.get(alias)).unwrap_or(&zero),
            &format!("defenseSnapshot.{key}"), minimum, f64::MAX)
    };
    let kind = |key: &str, default: &str| -> SimResult<String> {
        let token = raw.get(key).map_or(default, text).trim();
        if token.strip_prefix("RK_").is_some_and(|part| !part.is_empty() &&
            part.bytes().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')) {
            Ok(token.to_owned())
        } else { Err(format!("defenseSnapshot.{key} 必须为 RK_ 防御类型")) }
    };
    Ok(json!({
        "path": "__custom_enemy__", "name": "Custom defense snapshot",
        "nameZh": "自定义防御", "nameEn": "Custom defense snapshot",
        "health": amount("health", "health", 0.000001)?, "shield": amount("shield", "shield", 0.0)?,
        "armour": amount("armour", "armor", 0.0)?, "healthType": kind("healthType", "")?,
        "shieldType": kind("shieldType", "")?, "armourType": kind("armourType", text(&value["armorType"]))?,
        "factionResistance": kind("factionResistance", "RK_GRINEER_FACTION")?,
        "baseLevel": integer(raw.get("baseLevel").unwrap_or(&zero), "defenseSnapshot.baseLevel", 0, 9999)?,
        "damageController": "custom",
        "hitProxies": [{"bone":"GAME_C1_HEAD", "part":"HEAD", "damageAtten":2.0, "criticalMultiplier":2.0}]
    }))
}
