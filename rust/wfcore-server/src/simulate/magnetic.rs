//! Native M8 shield/overguard break bindings from the frozen WFSim export.
use super::{rows, text};
use serde_json::Value;
use crate::data::EngineData;

pub(super) fn weakness(data: &EngineData, path: &str) -> Option<f32> {
    data.rules.magnetic_weakness["weapons"][path].as_f64().map(|value| value as f32)
}

pub(super) fn proxy(target: &Value) -> Option<(f32, f32)> {
    let binding = match text(&target["path"]) {
        "/Lotus/Types/Enemies/Duviri/Avatars/DuviriMeleeAvatar" => Some((
            "/Lotus/Types/Enemies/Duviri/Avatars/DuviriMeleeTestDamageController",
            "/Lotus/Types/Enemies/Grineer/GrineerAvatars/HitProxies/GrineerHead")),
        "/Lotus/Types/Enemies/Corpus/Spaceman/AIWeek/RifleSpacemanAvatar" => Some((
            "/Lotus/Types/Enemies/Corpus/Spaceman/AIWeek/RifleSpacemanDamageController",
            "/Lotus/Types/Enemies/Corpus/Spaceman/Head")),
        "/Lotus/Types/Enemies/WF1999Infested/InfestedWarframeCapture/InfestedCaptureMutalistAvatar" => Some((
            "/Lotus/Types/Enemies/WF1999Infested/InfestedWarframeCapture/CaptureInfestedTargetScaledDamageController",
            "/Lotus/Types/Enemies/Corpus/Spaceman/Head")),
        _ => None,
    }.filter(|(controller, _)| {
        let exported = text(&target["damageController"]);
        exported == *controller || (exported.starts_with('/') && exported[1..].split('/').count() == 1
            && controller.rsplit('/').next() == Some(&exported[1..]))
    });
    let mut greatest = 0.0;
    let mut selected = None;
    for proxy in rows(&target["hitProxies"]) {
        let attenuation = proxy["damageAtten"].as_f64();
        let weakness = proxy["weakness"].as_f64().or_else(|| {
            binding.filter(|(_, proxy_type)| proxy["type"] == *proxy_type && attenuation == Some(2.0)).map(|_| 1.0)
        }).unwrap_or_default() as f32;
        if weakness > greatest {
            greatest = weakness;
            selected = attenuation.filter(|value| *value >= 0.0).map(|value| (weakness, value as f32));
        }
    }
    selected
}

pub(super) fn electricity_immune(target: &Value) -> bool {
    rows(&target["statusImmunities"]).iter().any(|value| value == "DT_ELECTRICITY" ||
        value == "Electricity" || value.as_f64() == Some(5.0))
}
