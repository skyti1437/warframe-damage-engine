pub(crate) mod corrections;

use indexmap::IndexMap;
use serde::Serialize;
use serde_json::Value;
use std::error::Error;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

pub const BUILD_LABEL: &str = "2026.08.13.13.24";
pub const SCHEMA_VERSION: u64 = 3;
pub type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;
pub type Records = IndexMap<String, Value>;

pub(crate) struct Rules {
    pub weapons: Value,
    pub metrics: Value,
    pub incarnon: Value,
    pub arcanes: Value,
    pub mods: Value,
    pub factions: Value,
    pub riven: Value,
    pub display: Value,
    pub magnetic_weakness: Value,
    pub fire_rate_spool: Value,
}

impl Rules {
    fn load(fingerprint: &mut Fingerprint) -> Result<Self> {
        let mut parse = |name: &str, raw: &str| {
            fingerprint.update(name, raw.as_bytes());
            serde_json::from_str::<Value>(raw)
        };
        Ok(Self {
            weapons: parse("weapon_rules", include_str!("../compat/weapon_rules.json"))?,
            metrics: parse("weapon_metrics", include_str!("../compat/weapon_metrics.json"))?,
            incarnon: parse("incarnon_details", include_str!("../compat/incarnon_details.json"))?,
            arcanes: parse("arcane_rules", include_str!("../compat/arcane_rules.json"))?,
            mods: parse("mod_rules", include_str!("../compat/mod_rules.json"))?,
            factions: parse("factions", include_str!("../compat/factions.json"))?,
            riven: parse("riven_rules", include_str!("../compat/riven_rules.json"))?,
            display: parse("display", include_str!("../compat/display.json"))?,
            magnetic_weakness: parse("magnetic_weakness", include_str!("../compat/magnetic_weakness.json"))?,
            fire_rate_spool: parse("fire_rate_spool", include_str!("../compat/fire_rate_spool.json"))?,
        })
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Health<'a> {
    ok: bool,
    build_label: &'static str,
    engine_fingerprint: &'a str,
    schema_version: u64,
}

/// 保留路径索引和完整战斗字段，供后续计算路由直接取用。
pub struct EngineData {
    pub weapons: Records,
    pub mods: Records,
    pub arcanes: Records,
    pub enemies: Records,
    pub warframes: Records,
    pub stances: Vec<Value>,
    pub utility_mods: Vec<Value>,
    pub exalted_weapons: Vec<Value>,
    pub riven_templates: Value,
    pub riven_disposition: Value,
    pub incarnon_defaults: Value,
    pub incarnon_strings: Value,
    pub enemy_overrides: Value,
    pub weapon_mechanics_overrides: Value,
    pub wiki_weapon_overrides: Value,
    pub mechanics: IndexMap<String, Value>,
    pub kitguns: Value,
    pub zaws: Value,
    pub modular_winners: Value,
    pub manifest: Value,
    pub source_lock: Value,
    pub ability_data: IndexMap<String, Value>,
    pub(crate) mod_art: Value,
    pub(crate) enemy_art: Value,
    pub(crate) root: PathBuf,
    pub(crate) dataset_root: PathBuf,
    pub(crate) rules: Rules,
    fingerprint: String,
}

impl EngineData {
    pub fn load(directory: &Path) -> Result<Self> {
        // 同时接受项目根目录和 data 目录。
        let mut root = directory.canonicalize()?;
        if root.join("export").is_dir() && !root.join("data").is_dir() {
            if let Some(parent) = root.parent() {
                root = parent.to_owned();
            }
        }
        let mut inputs = Inputs { root: &root, fingerprint: Fingerprint::new() };
        let source_lock = inputs.read("data/source-lock.json")?;
        require_build(&source_lock["activeBuild"], Path::new("data/source-lock.json"))?;
        let dataset_root = if let Some(snapshot) = source_lock["snapshotRoot"].as_str() {
            let path = root.join(snapshot).canonicalize()?;
            let snapshots = root.join("data/snapshots").canonicalize()?;
            if path == snapshots || !path.starts_with(snapshots) {
                return Err(invalid("snapshotRoot 必须位于 data/snapshots 下"));
            }
            path
        } else {
            root.clone()
        };
        let dataset = dataset_root.strip_prefix(&root)?;
        let export = dataset.join("data/export").join(BUILD_LABEL);
        let manifest = inputs.export(&export, "manifest", true)?;
        let mut weapons = inputs.records(&export, "weapons")?;
        for (path, weapon) in inputs.records(&export, "exalted_weapons")? {
            weapons.entry(path).or_insert(weapon);
        }
        let mods = inputs.records(&export, "mods")?;
        let arcanes = inputs.records(&export, "arcanes")?;
        let enemies = inputs.records(&export, "enemies")?;
        let warframes = inputs.records(&export, "warframes")?;
        let enemy_overrides = inputs.export(&export, "enemy_overrides", false)?;
        if !enemy_overrides["data"].is_object() {
            return Err(invalid("enemy_overrides.json 缺少 data 对象"));
        }
        let riven_templates = inputs.export(&export, "riven_templates", false)?;
        let kitguns = inputs.export(&export, "kitguns", false)?;
        let zaws = inputs.export(&export, "zaws", false)?;
        let modular_winners = inputs.export(&export, "modular_winners", false)?;
        let incarnon_defaults = inputs.read(export.join("incarnon_defaults.json"))?;
        let incarnon_strings = inputs.read(export.join("incarnon_strings.json"))?;
        if !incarnon_defaults["sampledAt"].is_string()
            || !incarnon_defaults["gameBuild"].is_string()
            || !incarnon_defaults["strategies"].is_object()
            || !incarnon_strings["data"].is_object()
        {
            return Err(invalid("灵化默认路线或本地化文本缺少有效字段"));
        }
        let stances = inputs.rows(dataset.join("data/stances.json"), "stances")?;
        let utility_mods = inputs.rows(dataset.join("data/utility_mods.json"), "mods")?;
        let exalted_weapons = inputs.rows(dataset.join("data/exalted_weapons.json"), "weapons")?;
        let weapon_mechanics_overrides = inputs.read("data/weapon_mechanics_overrides.json")?;
        let wiki_weapon_overrides = inputs.read("data/wiki_weapon_overrides.json")?;
        let mechanics = inputs.json_directory(Path::new("data/mechanics"))?;
        let mut ability_data = IndexMap::new();
        for name in ["ability_runtime_diff", "ability_runtime_fields", "overframe_abilitystats"] {
            ability_data.insert(name.to_owned(), inputs.read(format!("data/{name}.json"))?);
        }
        let mod_art = inputs.optional(Path::new("frontend/assets/mods/index.json"))?;
        let enemy_art = inputs.optional(&dataset.join("frontend/assets/enemies/index.json"))?;
        let rules = Rules::load(&mut inputs.fingerprint)?;
        let fingerprint = format!("{:016x}", inputs.fingerprint.0);
        let mut engine = Self {
            root, dataset_root, weapons, mods, arcanes, enemies, warframes, stances,
            utility_mods, exalted_weapons, riven_templates, riven_disposition: Value::Null,
            incarnon_defaults, incarnon_strings, enemy_overrides, weapon_mechanics_overrides,
            wiki_weapon_overrides, mechanics, kitguns, zaws, modular_winners, manifest,
            source_lock, ability_data, mod_art, enemy_art, rules, fingerprint,
        };
        corrections::apply(&mut engine)?;
        Ok(engine)
    }

    pub fn health(&self) -> Health<'_> {
        Health {
            ok: true,
            build_label: BUILD_LABEL,
            engine_fingerprint: &self.fingerprint,
            schema_version: SCHEMA_VERSION,
        }
    }
}

pub fn locate_data_root(cwd: &Path, executable: &Path) -> Result<PathBuf> {
    cwd.ancestors()
        .chain(executable.parent().into_iter().flat_map(Path::ancestors))
        .find(|root| {
            let has_exports = |directory: &Path| directory.join("data/export").join(BUILD_LABEL).is_dir();
            if has_exports(root) {
                return true;
            }
            fs::read(root.join("data/source-lock.json")).ok().and_then(|bytes| {
                serde_json::from_slice::<Value>(bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(&bytes)).ok()
            }).is_some_and(|lock| lock["snapshotRoot"].as_str().is_some_and(|snapshot| {
                has_exports(&root.join(snapshot))
            }))
        })
        .map(Path::to_path_buf)
        .ok_or_else(|| invalid(format!("找不到 data/export/{BUILD_LABEL}，请指定 --data-dir")))
}

struct Inputs<'a> {
    root: &'a Path,
    fingerprint: Fingerprint,
}

impl Inputs<'_> {
    fn read(&mut self, name: impl AsRef<Path>) -> Result<Value> {
        let name = name.as_ref();
        let path = self.root.join(name);
        let bytes = fs::read(&path).map_err(|error| {
            io::Error::new(error.kind(), format!("读取 {} 失败：{error}", path.display()))
        })?;
        self.fingerprint.update(
            &name.to_string_lossy().replace('\\', "/"),
            &bytes,
        );
        let json = bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(&bytes);
        let document: Value = serde_json::from_slice(json)
            .map_err(|error| invalid(format!("解析 {} 失败：{error}", path.display())))?;
        if !document.is_object() {
            return Err(invalid(format!("{} 必须为 JSON 对象", path.display())));
        }
        Ok(document)
    }

    fn optional(&mut self, path: &Path) -> Result<Value> {
        if self.root.join(path).is_file() { self.read(path) } else { Ok(Value::Null) }
    }

    fn export(&mut self, base: &Path, name: &str, check_schema: bool) -> Result<Value> {
        let path = base.join(format!("{name}.json"));
        let value = self.read(&path)?;
        require_build(&value["buildLabel"], &path)?;
        if check_schema && value["schemaVersion"].as_u64() != Some(SCHEMA_VERSION) {
            return Err(invalid(format!("{} 的 schemaVersion 必须为 {SCHEMA_VERSION}", path.display())));
        }
        Ok(value)
    }

    fn records(&mut self, base: &Path, name: &str) -> Result<Records> {
        let mut document = self.export(base, name, true)?;
        let rows = take_rows(&mut document, "data", &base.join(format!("{name}.json")))?;
        let mut records = Records::with_capacity(rows.len());
        for row in rows {
            let path = row["path"].as_str().filter(|p| !p.is_empty())
                .ok_or_else(|| invalid(format!("{name}.json 含缺少 path 的记录")))?.to_owned();
            if records.insert(path.clone(), row).is_some() {
                return Err(invalid(format!("{name}.json 含重复路径 {path}")));
            }
        }
        Ok(records)
    }

    fn rows(&mut self, path: PathBuf, field: &str) -> Result<Vec<Value>> {
        let mut document = self.read(&path)?;
        require_build(&document["buildLabel"], &path)?;
        take_rows(&mut document, field, &path)
    }

    fn json_directory(&mut self, path: &Path) -> Result<IndexMap<String, Value>> {
        let mut files = fs::read_dir(self.root.join(path))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        files.sort_by_key(|entry| entry.file_name());
        let mut result = IndexMap::new();
        for entry in files {
            let name = PathBuf::from(entry.file_name());
            if name.extension().is_some_and(|ext| ext == "json") && entry.file_type()?.is_file() {
                result.insert(name.file_stem().unwrap().to_string_lossy().into_owned(), self.read(path.join(name))?);
            }
        }
        Ok(result)
    }
}

fn require_build(value: &Value, path: &Path) -> Result<()> {
    if value.as_str() != Some(BUILD_LABEL) {
        return Err(invalid(format!("{} 的导出版本必须为 {BUILD_LABEL}，实际为 {value}", path.display())));
    }
    Ok(())
}

fn take_rows(document: &mut Value, field: &str, path: &Path) -> Result<Vec<Value>> {
    match document[field].take() {
        Value::Array(rows) if rows.iter().all(Value::is_object) => Ok(rows),
        _ => Err(invalid(format!("{} 缺少有效的 {field} 数组", path.display()))),
    }
}

pub(crate) fn invalid(message: impl Into<String>) -> Box<dyn Error + Send + Sync> {
    io::Error::new(io::ErrorKind::InvalidData, message.into()).into()
}

/// FNV-1a 只用于区分已加载的数据和规则版本。
struct Fingerprint(u64);

impl Fingerprint {
    fn new() -> Self {
        let mut fingerprint = Self(0xcbf29ce484222325);
        fingerprint.update("wfcore-server", b"phase2a");
        fingerprint
    }

    fn update(&mut self, name: &str, bytes: &[u8]) {
        for part in [name.as_bytes(), bytes] {
            for byte in (part.len() as u64).to_le_bytes().iter().chain(part) {
                self.0 ^= u64::from(*byte);
                self.0 = self.0.wrapping_mul(0x100000001b3);
            }
        }
    }
}
