use std::sync::{Arc, Mutex, PoisonError};

pub const ID: &str = "ambience";
pub const NAME: &str = "Ambience";
pub const VERSION: &str = "0.1.2";
pub const DESCRIPTION: &str = "Plays Ambience and ReactiveMusic packs in Horizon, matched to place, biome, and situation.";
pub const NAMESPACE: &str = "ambience";
pub const COMMAND: &str = "ambience";

pub const KEYS: [&str; 9] = [
    "enabled",
    "pack",
    "menu_boss",
    "stop_on_pause",
    "crossfade",
    "resume",
    "custom_rules",
    "volume",
    "fallback",
];
pub const CROSSFADE_STEPS: [u32; 5] = [0, 2, 4, 8, 15];
pub const RESUME_STEPS: [u32; 5] = [0, 5, 10, 30, 60];
pub const VOLUME_STEPS: [u32; 4] = [25, 50, 75, 100];
const DEFAULT_VOLUME: u32 = 100;
const DEFAULT_CROSSFADE: u32 = 2;
const DEFAULT_RESUME: u32 = 5;

const MAX_NAME: usize = 64;

pub static CACHE: Mutex<Option<Arc<Settings>>> = Mutex::new(None);
pub static FORCE: Mutex<Option<String>> = Mutex::new(None);
pub static LAST: Mutex<Option<Last>> = Mutex::new(None);

#[derive(Clone, Debug)]
pub struct Last {
    pub event: String,
    pub reason: &'static str,
    pub volume: f32,
    pub tick: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    pub enabled: bool,
    pub pack: Option<String>,
    pub menu_boss: bool,
    pub stop_on_pause: bool,
    pub crossfade: u32,
    pub resume: u32,
    pub custom_rules: bool,
    pub volume: u32,
    pub fallback: Option<String>,
}

pub fn safe_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_NAME
        && !name.starts_with('.')
        && !name.contains("..")
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ' '))
}

impl Settings {
    pub fn read(get: impl Fn(&str) -> Option<String>) -> Self {
        let raw = |key: &str| get(key).map(|value| value.trim().to_owned());
        Self {
            enabled: raw(KEYS[0]).as_deref() != Some("off"),
            pack: raw(KEYS[1]).filter(|name| safe_name(name)),
            menu_boss: raw(KEYS[2]).as_deref() != Some("off"),
            stop_on_pause: raw(KEYS[3]).as_deref() == Some("on"),
            crossfade: raw(KEYS[4])
                .and_then(|value| value.parse().ok())
                .unwrap_or(DEFAULT_CROSSFADE)
                .min(30),
            resume: raw(KEYS[5])
                .and_then(|value| value.parse().ok())
                .unwrap_or(DEFAULT_RESUME)
                .min(600),
            custom_rules: raw(KEYS[6]).as_deref() != Some("off"),
            volume: raw(KEYS[7])
                .and_then(|value| value.parse().ok())
                .unwrap_or(DEFAULT_VOLUME)
                .min(100),
            fallback: raw(KEYS[8]).filter(|name| safe_name(name)),
        }
    }
}

pub fn flag(on: bool) -> &'static str {
    if on { "on" } else { "off" }
}

pub fn labels(settings: &Settings) -> Vec<String> {
    vec![
        format!("Ambience: {}", flag(settings.enabled)),
        format!("Pack: {}", settings.pack.as_deref().unwrap_or("auto")),
        format!("Pack menu and boss music: {}", flag(settings.menu_boss)),
        format!("Stop music in the pause menu (off = pause and resume where it stopped): {}", flag(settings.stop_on_pause)),
        format!("Crossfade between tracks: {} s", settings.crossfade),
        format!("Resume the last track within: {} s (0 = off)", settings.resume),
        format!("Custom rules (ambience/rules.yaml): {}", flag(settings.custom_rules)),
        format!("Volume: {}%", settings.volume),
        format!(
            "Fallback pack (for situations the main pack lacks): {}",
            settings.fallback.as_deref().unwrap_or("off")
        ),
    ]
}

pub fn next_step(current: u32, steps: &[u32]) -> u32 {
    steps
        .iter()
        .position(|step| *step == current)
        .map_or(steps[0], |index| steps[(index + 1) % steps.len()])
}

pub fn invalidate() {
    *CACHE.lock().unwrap_or_else(PoisonError::into_inner) = None;
}
