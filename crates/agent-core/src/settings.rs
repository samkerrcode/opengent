//! App-wide, non-secret preferences in `settings.json` at the data root.
//!
//! One typed [`Settings`] struct with a sub-struct per Settings section. Every struct is
//! `#[serde(default)]`, so a file written by a build with fewer fields loads with defaults
//! for the new ones and adding a field needs no migration. Fields a newer build wrote are
//! kept on save, so downgrading doesn't lose them. A missing file means defaults; a corrupt
//! one is moved aside and defaults are used, so the app still starts.
//!
//! Adding a setting: add the field (with its default) to its section, check its range in
//! the section's `validate`, run `npm run gen:types`. Don't use `skip_serializing_if` on
//! fields: a partial update only accepts keys that appear in the serialized settings.
//! Secrets never go here; use `SecretRef` + `SecretStore`.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use ts_rs::TS;

use crate::config::{write_private, DataRoot};
use crate::{Error, Result};

pub const FILE_NAME: &str = "settings.json";
const VERSION: u16 = 1;

/// All app-wide preferences, one field per Settings section.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, TS)]
#[serde(default)]
#[ts(export)]
pub struct Settings {
    pub general: GeneralSettings,
    pub appearance: AppearanceSettings,
}

/// Settings → General. Its About group is read-only (`get_app_info`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(default)]
#[ts(export)]
pub struct GeneralSettings {
    /// Send the OS, time zone and each user message's local send time to models
    /// (`agent::environment`). Read when a run starts.
    pub share_environment: bool,
}

impl Default for GeneralSettings {
    fn default() -> Self {
        Self {
            share_environment: true,
        }
    }
}

/// Settings → Appearance.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, TS)]
#[serde(default)]
#[ts(export)]
pub struct AppearanceSettings {
    pub theme: Theme,
}

/// Light or dark UI. `System` follows the OS setting; the others override it for this app
/// only. The shell applies it to the window, so the webview's `prefers-color-scheme` follows.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum Theme {
    #[default]
    System,
    Light,
    Dark,
}

/// Semantic checks beyond what deserialization enforces (ranges, combinations). Runs on
/// every update and on load; a stored value that fails it counts as a corrupt file.
trait Validate {
    fn validate(&self) -> Result<()>;
}

impl Validate for Settings {
    fn validate(&self) -> Result<()> {
        self.general.validate()?;
        self.appearance.validate()
    }
}

impl Validate for GeneralSettings {
    fn validate(&self) -> Result<()> {
        Ok(())
    }
}

impl Validate for AppearanceSettings {
    fn validate(&self) -> Result<()> {
        Ok(())
    }
}

pub struct SettingsStore {
    path: PathBuf,
    state: Mutex<State>,
    /// See `load_read_only`.
    read_only: bool,
}

struct State {
    settings: Settings,
    /// The file as last read or written, including fields this build doesn't know.
    raw: Map<String, Value>,
}

impl SettingsStore {
    pub fn load(data_root: &DataRoot) -> Result<Self> {
        Self::load_with(data_root, false)
    }

    /// For a process that shares the data root with the app (the CLI): the same settings,
    /// but nothing is ever written — a corrupt file is left in place (defaults are used) and
    /// `update` fails.
    pub fn load_read_only(data_root: &DataRoot) -> Result<Self> {
        Self::load_with(data_root, true)
    }

    fn load_with(data_root: &DataRoot, read_only: bool) -> Result<Self> {
        let path = data_root.path().join(FILE_NAME);
        let (settings, raw) = match std::fs::read(&path) {
            Ok(bytes) => decode(&bytes).unwrap_or_else(|e| {
                let aside = (!read_only).then(|| set_aside(&path)).flatten();
                tracing::warn!(error = %e, kept = ?aside, "settings file is corrupt; using defaults");
                Default::default()
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Default::default(),
            Err(e) => return Err(e.into()),
        };
        Ok(Self {
            path,
            state: Mutex::new(State { settings, raw }),
            read_only,
        })
    }

    pub fn get(&self) -> Settings {
        self.state.lock().unwrap().settings.clone()
    }

    /// Applies a partial update — `{ section: { field: value } }`, each value replacing the
    /// stored one — validates and persists it, and returns the new settings. Unknown keys
    /// and invalid values are rejected; on any error nothing changes.
    pub fn update(&self, patch: &Value) -> Result<Settings> {
        if self.read_only {
            return Err(Error::InvalidInput(
                "settings can only be changed in the app".into(),
            ));
        }
        let mut state = self.state.lock().unwrap();
        let next: Settings = apply_patch(&state.settings, patch)?;
        if next == state.settings {
            return Ok(next);
        }
        let raw = encode(&next, &state.raw)?;
        self.persist(&raw)?;
        *state = State {
            settings: next.clone(),
            raw,
        };
        Ok(next)
    }

    fn persist(&self, doc: &Map<String, Value>) -> Result<()> {
        debug_assert!(!self.read_only);
        let tmp = self.path.with_extension("json.tmp");
        let bytes = serde_json::to_vec_pretty(doc).map_err(|e| Error::Internal(e.to_string()))?;
        write_private(&tmp, &bytes)?;
        std::fs::rename(&tmp, &self.path)?;
        Ok(())
    }
}

/// Moves a corrupt file to `settings.json.corrupt-<unix secs>` so it can be inspected.
fn set_aside(path: &Path) -> Option<PathBuf> {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let mut name = path.file_name()?.to_os_string();
    name.push(format!(".corrupt-{secs}"));
    let aside = path.with_file_name(name);
    std::fs::rename(path, &aside).ok().map(|()| aside)
}

fn decode<T: DeserializeOwned + Validate>(bytes: &[u8]) -> Result<(T, Map<String, Value>)> {
    let invalid = |e: serde_json::Error| Error::InvalidInput(e.to_string());
    let raw: Map<String, Value> = serde_json::from_slice(bytes).map_err(invalid)?;
    let value: T = serde_json::from_value(Value::Object(raw.clone())).map_err(invalid)?;
    value.validate()?;
    Ok((value, raw))
}

/// The file to write: `raw` (so unknown fields survive) overlaid with `value`, per section.
fn encode<T: Serialize>(value: &T, raw: &Map<String, Value>) -> Result<Map<String, Value>> {
    let mut doc = raw.clone();
    for (section, fields) in to_map(value)? {
        match (doc.get_mut(&section), fields) {
            (Some(Value::Object(target)), Value::Object(fields)) => target.extend(fields),
            (_, fields) => {
                doc.insert(section, fields);
            }
        }
    }
    doc.insert("version".into(), VERSION.into());
    Ok(doc)
}

fn apply_patch<T: Serialize + DeserializeOwned + Validate>(
    current: &T,
    patch: &Value,
) -> Result<T> {
    let invalid = |m: String| Error::InvalidInput(m);
    let Value::Object(patch) = patch else {
        return Err(invalid("settings update must be an object".into()));
    };
    let mut doc = to_map(current)?;
    for (section, fields) in patch {
        let Some(Value::Object(target)) = doc.get_mut(section) else {
            return Err(invalid(format!("unknown settings section `{section}`")));
        };
        let Value::Object(fields) = fields else {
            return Err(invalid(format!(
                "settings section `{section}` must be an object"
            )));
        };
        for (field, value) in fields {
            let slot = target
                .get_mut(field)
                .ok_or_else(|| invalid(format!("unknown setting `{section}.{field}`")))?;
            *slot = value.clone();
        }
    }
    let next: T = serde_json::from_value(Value::Object(doc))
        .map_err(|e| invalid(format!("invalid setting: {e}")))?;
    next.validate()?;
    Ok(next)
}

fn to_map<T: Serialize>(value: &T) -> Result<Map<String, Value>> {
    match serde_json::to_value(value) {
        Ok(Value::Object(map)) => Ok(map),
        Ok(_) => Err(Error::Internal(
            "settings must serialize to an object".into(),
        )),
        Err(e) => Err(Error::Internal(e.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::resolve_data_root;
    use serde_json::json;

    fn root() -> (DataRoot, PathBuf) {
        let tmp = std::env::temp_dir().join(format!("opengent-settings-{}", uuid::Uuid::new_v4()));
        (resolve_data_root(Some(&tmp)).unwrap(), tmp)
    }

    // A stand-in with real fields, so field-level behavior is tested before any section
    // has settings of its own.
    #[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
    #[serde(default)]
    struct Fake {
        appearance: FakeAppearance,
        tools: FakeTools,
    }

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    #[serde(default)]
    struct FakeAppearance {
        theme: String,
        font_size: u8,
    }

    impl Default for FakeAppearance {
        fn default() -> Self {
            Self {
                theme: "system".into(),
                font_size: 14,
            }
        }
    }

    #[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
    #[serde(default)]
    struct FakeTools {
        sandbox: bool,
    }

    impl Validate for Fake {
        fn validate(&self) -> Result<()> {
            if !(10..=24).contains(&self.appearance.font_size) {
                return Err(Error::InvalidInput("font size out of range".into()));
            }
            Ok(())
        }
    }

    #[test]
    fn missing_file_means_defaults() {
        let (root, tmp) = root();
        let store = SettingsStore::load(&root).unwrap();
        assert_eq!(store.get(), Settings::default());
        assert!(store.get().general.share_environment, "on by default");
        assert!(!tmp.join(FILE_NAME).exists(), "loading must not write");
        std::fs::remove_dir_all(tmp).unwrap();
    }

    #[test]
    fn corrupt_file_is_kept_aside_and_defaults_used() {
        let (root, tmp) = root();
        for bad in [&b"{ not json"[..], b"[1, 2]", br#"{"general": 7}"#] {
            std::fs::write(tmp.join(FILE_NAME), bad).unwrap();
            let store = SettingsStore::load(&root).unwrap();
            assert_eq!(store.get(), Settings::default());
            assert!(!tmp.join(FILE_NAME).exists());
            let kept: Vec<_> = std::fs::read_dir(&tmp)
                .unwrap()
                .filter_map(|e| e.ok())
                .filter(|e| {
                    e.file_name()
                        .to_string_lossy()
                        .starts_with("settings.json.corrupt-")
                })
                .collect();
            assert_eq!(std::fs::read(kept[0].path()).unwrap(), bad);
            std::fs::remove_file(kept[0].path()).unwrap();
        }
        std::fs::remove_dir_all(tmp).unwrap();
    }

    #[test]
    fn read_only_store_never_writes() {
        let (root, tmp) = root();
        std::fs::write(tmp.join(FILE_NAME), b"{ not json").unwrap();
        let store = SettingsStore::load_read_only(&root).unwrap();
        assert_eq!(store.get(), Settings::default());
        assert_eq!(std::fs::read(tmp.join(FILE_NAME)).unwrap(), b"{ not json");
        assert!(store
            .update(&json!({ "general": { "share_environment": false } }))
            .is_err());
        assert_eq!(std::fs::read(tmp.join(FILE_NAME)).unwrap(), b"{ not json");
        std::fs::remove_dir_all(tmp).unwrap();
    }

    #[test]
    fn store_round_trips_and_keeps_fields_from_newer_builds() {
        let (root, tmp) = root();
        let newer = json!({
            "version": 1,
            "general": { "share_environment": true, "future_flag": true },
            "future_section": { "x": 1 }
        });
        std::fs::write(tmp.join(FILE_NAME), newer.to_string()).unwrap();
        let store = SettingsStore::load(&root).unwrap();
        assert_eq!(store.get(), Settings::default());
        store.update(&json!({ "general": {} })).unwrap();
        let written = encode(&store.get(), &store.state.lock().unwrap().raw).unwrap();
        store.persist(&written).unwrap();
        let on_disk: Value =
            serde_json::from_slice(&std::fs::read(tmp.join(FILE_NAME)).unwrap()).unwrap();
        // Unknown fields survive; sections this build knows are written with their values.
        let mut expected = newer.clone();
        expected["appearance"] = json!({ "theme": "system" });
        assert_eq!(on_disk, expected);
        assert_eq!(SettingsStore::load(&root).unwrap().get(), store.get());
        assert!(store.update(&json!({ "general": { "nope": 1 } })).is_err());
        assert!(store.update(&json!({ "nope": {} })).is_err());
        assert!(store.update(&json!([])).is_err());
        std::fs::remove_dir_all(tmp).unwrap();
    }

    #[test]
    fn share_environment_can_be_turned_off_and_persists() {
        let (root, tmp) = root();
        let store = SettingsStore::load(&root).unwrap();
        let next = store
            .update(&json!({ "general": { "share_environment": false } }))
            .unwrap();
        assert!(!next.general.share_environment);
        assert!(
            !SettingsStore::load(&root)
                .unwrap()
                .get()
                .general
                .share_environment
        );
        assert!(store
            .update(&json!({ "general": { "share_environment": "no" } }))
            .is_err());
        std::fs::remove_dir_all(tmp).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn file_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let (root, tmp) = root();
        let store = SettingsStore::load(&root).unwrap();
        store
            .persist(&encode(&store.get(), &Map::new()).unwrap())
            .unwrap();
        let mode = std::fs::metadata(tmp.join(FILE_NAME))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
        std::fs::remove_dir_all(tmp).unwrap();
    }

    #[test]
    fn file_with_fewer_fields_loads_defaults_for_new_ones() {
        let (old, _) = decode::<Fake>(br#"{"version":1,"appearance":{"theme":"dark"}}"#).unwrap();
        assert_eq!(old.appearance.theme, "dark");
        assert_eq!(old.appearance.font_size, 14);
        assert_eq!(old.tools, FakeTools::default());
        let (empty, _) = decode::<Fake>(b"{}").unwrap();
        assert_eq!(empty, Fake::default());
    }

    #[test]
    fn stored_value_failing_validation_is_corrupt() {
        assert!(decode::<Fake>(br#"{"appearance":{"font_size":99}}"#).is_err());
        assert!(decode::<Fake>(br#"{"appearance":{"font_size":"big"}}"#).is_err());
    }

    #[test]
    fn patch_replaces_only_named_fields() {
        let next = apply_patch(
            &Fake::default(),
            &json!({ "appearance": { "theme": "dark" } }),
        )
        .unwrap();
        assert_eq!(next.appearance.theme, "dark");
        assert_eq!(next.appearance.font_size, 14);
        let next = apply_patch(&next, &json!({ "tools": { "sandbox": true } })).unwrap();
        assert_eq!(next.appearance.theme, "dark");
        assert!(next.tools.sandbox);
    }

    #[test]
    fn patch_rejects_unknown_keys_wrong_types_and_invalid_values() {
        let base = Fake::default();
        for bad in [
            json!(null),
            json!({ "nope": {} }),
            json!({ "appearance": "dark" }),
            json!({ "appearance": { "colour": "red" } }),
            json!({ "appearance": { "font_size": "big" } }),
            json!({ "appearance": { "font_size": 99 } }),
        ] {
            assert!(
                matches!(apply_patch(&base, &bad), Err(Error::InvalidInput(_))),
                "accepted {bad}"
            );
        }
    }

    #[test]
    fn encode_overlays_known_fields_on_the_raw_file() {
        let (value, raw) = decode::<Fake>(
            br#"{"version":1,"appearance":{"theme":"dark","accent":"teal"},"later":{"a":1}}"#,
        )
        .unwrap();
        let next = apply_patch(&value, &json!({ "appearance": { "theme": "light" } })).unwrap();
        let doc = Value::Object(encode(&next, &raw).unwrap());
        assert_eq!(
            doc,
            json!({
                "version": 1,
                "appearance": { "theme": "light", "font_size": 14, "accent": "teal" },
                "tools": { "sandbox": false },
                "later": { "a": 1 }
            })
        );
    }

    #[test]
    fn theme_defaults_to_system_and_accepts_only_known_values() {
        assert_eq!(Settings::default().appearance.theme, Theme::System);
        let base = Settings::default();
        for (value, theme) in [
            ("system", Theme::System),
            ("light", Theme::Light),
            ("dark", Theme::Dark),
        ] {
            let next = apply_patch(&base, &json!({ "appearance": { "theme": value } })).unwrap();
            assert_eq!(next.appearance.theme, theme);
        }
        for bad in [json!("Dark"), json!("sepia"), json!(1), json!(null)] {
            assert!(
                matches!(
                    apply_patch(&base, &json!({ "appearance": { "theme": bad } })),
                    Err(Error::InvalidInput(_))
                ),
                "accepted {bad}"
            );
        }
    }

    #[test]
    fn theme_persists_across_loads() {
        let (root, tmp) = root();
        let store = SettingsStore::load(&root).unwrap();
        store
            .update(&json!({ "appearance": { "theme": "dark" } }))
            .unwrap();
        let reloaded = SettingsStore::load(&root).unwrap().get();
        assert_eq!(reloaded.appearance.theme, Theme::Dark);
        std::fs::remove_dir_all(tmp).unwrap();
    }
}
