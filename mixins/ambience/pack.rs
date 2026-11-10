use std::collections::HashMap;
use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::game::official_ambience as core;

pub const PROPERTIES: &str = "ambience.properties";
pub const REACTIVE: &str = "reactivemusic.yaml";
const MUSIC_DIR: &str = "music";
const SOURCE_EXTENSIONS: [&str; 3] = ["mp3", "ogg", "wav"];
const MAX_PROPERTIES: u64 = 256 * 1024;
const MAX_TRACK: usize = 64;
const CHECK_EVERY: Duration = Duration::from_secs(2);
const MANIFEST: &str = "manifest.json";
const CONVERT_VERSION: u32 = 2;
pub const FALLBACK_NAMESPACE: &str = "ambience_fb";

#[derive(Clone, Debug)]
pub enum Entry {
    File(PathBuf),
    Zipped { archive: PathBuf, name: String },
}

#[derive(Debug, Default)]
pub struct Pack {
    pub name: String,
    pub ns: String,
    pub enabled: bool,
    pub events: HashMap<String, String>,
    pub biomes: HashMap<String, String>,
    pub primary: HashMap<String, String>,
    pub secondary: HashMap<String, String>,
    pub audio: HashMap<String, Entry>,
    pub tracks: Vec<String>,
    pub reactive: Vec<ReactiveEntry>,
}

#[derive(Clone, Debug, Default)]
pub struct ReactiveEntry {
    pub events: Vec<String>,
    pub songs: Vec<String>,
}

impl Pack {
    pub fn event(&self, track: &str) -> String {
        format!("{}:{}", self.ns, track.to_ascii_lowercase())
    }

    pub fn events_json(&self, namespace: &str) -> String {
        let mut map = serde_json::Map::new();
        for track in &self.tracks {
            let lower = track.to_ascii_lowercase();
            let sound = serde_json::json!({
                "name": format!("{namespace}:{lower}"),
                "stream": true
            });
            map.insert(lower, serde_json::json!({ "replace": true, "sounds": [sound] }));
        }
        serde_json::Value::Object(map).to_string()
    }
}

struct Slot {
    checked: Option<Instant>,
    stamp: Option<Stamp>,
    loading: bool,
    fresh: bool,
    pack: Option<Arc<Pack>>,
    fallback: Option<Arc<Pack>>,
}

static SLOT: Mutex<Slot> = Mutex::new(Slot {
    checked: None,
    stamp: None,
    loading: false,
    fresh: false,
    pack: None,
    fallback: None,
});

type Manifest = HashMap<String, String>;
static PREPARED: Mutex<Option<String>> = Mutex::new(None);
static BUSY: AtomicBool = AtomicBool::new(false);

struct Progress {
    total: usize,
    done: usize,
    failed: Vec<(String, String)>,
}

static PROGRESS: Mutex<Progress> = Mutex::new(Progress {
    total: 0,
    done: 0,
    failed: Vec::new(),
});

pub fn packs_dir() -> PathBuf {
    crate::game::Game::game_dir().join("resourcepacks")
}

pub fn cache_root() -> PathBuf {
    crate::game::Game::game_dir().join(".horizon-cache").join(core::ID)
}

pub fn names() -> Vec<String> {
    let Ok(entries) = fs::read_dir(packs_dir()) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let path = entry.path();
            let name = entry.file_name().into_string().ok()?;
            let usable = if path.is_dir() {
                folder_file(&path, PROPERTIES).is_some() || folder_file(&path, REACTIVE).is_some()
            } else {
                path.extension().is_some_and(|ext| ext.eq_ignore_ascii_case("zip"))
                    && (zip_text(&path, PROPERTIES).is_some() || zip_text(&path, REACTIVE).is_some())
            };
            let stem = path.file_stem()?.to_str()?.to_owned();
            (usable && core::safe_name(&stem)).then_some(if path.is_dir() { name } else { stem })
        })
        .collect();
    names.sort();
    names.dedup();
    names
}

fn zip_names(archive: &Path) -> Vec<String> {
    let Ok(file) = fs::File::open(archive) else {
        return Vec::new();
    };
    let Ok(mut zip) = zip::ZipArchive::new(file) else {
        return Vec::new();
    };
    let mut names = Vec::new();
    for i in 0..zip.len() {
        let Ok(entry) = zip.by_index(i) else {
            continue;
        };
        if entry.is_dir() {
            continue;
        }
        if let Ok(name) = entry.name() {
            names.push(name.into_owned());
        }
    }
    names
}

fn zip_text(archive: &Path, wanted: &str) -> Option<String> {
    let file = fs::File::open(archive).ok()?;
    let mut zip = zip::ZipArchive::new(file).ok()?;
    for i in 0..zip.len() {
        let Ok(mut entry) = zip.by_index(i) else {
            continue;
        };
        let Ok(name) = entry.name().map(|name| name.into_owned()) else {
            continue;
        };
        if !name.to_ascii_lowercase().ends_with(wanted) {
            continue;
        }
        if entry.size() > MAX_PROPERTIES {
            return None;
        }
        let mut text = String::new();
        io::Read::read_to_string(&mut entry, &mut text).ok()?;
        return Some(text);
    }
    None
}

fn locate_folder(root: &Path) -> HashMap<String, Entry> {
    let mut audio = HashMap::new();
    let Ok(entries) = fs::read_dir(root.join(MUSIC_DIR)) else {
        return audio;
    };
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        let supported = path
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| SOURCE_EXTENSIONS.iter().any(|known| ext.eq_ignore_ascii_case(known)));
        if let (true, Some(stem)) = (supported, path.file_stem().and_then(|s| s.to_str())) {
            if valid_track(stem) {
                audio.insert(stem.to_ascii_lowercase(), Entry::File(path.clone()));
            }
        }
    }
    audio
}

fn locate_zip(archive: &Path) -> HashMap<String, Entry> {
    let mut audio = HashMap::new();
    for name in zip_names(archive) {
        let lower = name.to_ascii_lowercase();
        let Some(rest) = lower.rsplit_once("music/").map(|(_, rest)| rest.to_owned()) else {
            continue;
        };
        if let Some((stem, ext)) = rest.rsplit_once('.') {
            if SOURCE_EXTENSIONS.contains(&ext) && valid_track(stem) {
                audio.insert(
                    stem.to_owned(),
                    Entry::Zipped {
                        archive: archive.to_path_buf(),
                        name,
                    },
                );
            }
        }
    }
    audio
}

fn valid_list(value: &str) -> bool {
    let songs: Vec<&str> = value.split(',').map(str::trim).collect();
    !songs.is_empty() && songs.iter().all(|song| valid_track(song))
}

fn valid_track(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_TRACK
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

fn contained(root: &Path, path: &Path) -> bool {
    path.strip_prefix(root).is_ok_and(|rest| {
        rest.components().count() == 1
            && rest
                .to_str()
                .is_some_and(|text| !text.contains(['\\', ':', '/']))
    })
}

fn parse(pack: &mut Pack, text: &str) {
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with('!') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim().to_ascii_lowercase();
        let value = value.trim();
        if key == "enabled" {
            pack.enabled = value.eq_ignore_ascii_case("true");
            continue;
        }
        if !valid_list(value) {
            continue;
        }
        let value = value.to_owned();
        if let Some(event) = key.strip_prefix("event.") {
            let event = match event {
                "pumpkinhead" => "pumpkin",
                other => other,
            };
            pack.events.insert(event.to_owned(), value);
        } else if let Some(biome) = key.strip_prefix("biome.") {
            pack.biomes.insert(biome.replace('+', "_"), value);
        } else if let Some(tag) = key.strip_prefix("primarytag.") {
            pack.primary.insert(tag.to_owned(), value);
        } else if let Some(tag) = key.strip_prefix("secondarytag.") {
            pack.secondary.insert(tag.to_owned(), value);
        }
    }
}

fn folder_file(folder: &Path, wanted: &str) -> Option<PathBuf> {
    fs::read_dir(folder)
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.eq_ignore_ascii_case(wanted))
        })
}

fn pack_text(name: &str) -> Option<(String, bool)> {
    let folder = packs_dir().join(name);
    if folder.is_dir() {
        if let Some(path) = folder_file(&folder, PROPERTIES) {
            return Some((fs::read_to_string(path).ok()?, false));
        }
        let path = folder_file(&folder, REACTIVE)?;
        return Some((fs::read_to_string(path).ok()?, true));
    }
    let archive = packs_dir().join(format!("{name}.zip"));
    if let Some(text) = zip_text(&archive, PROPERTIES) {
        return Some((text, false));
    }
    Some((zip_text(&archive, REACTIVE)?, true))
}

fn unquote(text: &str) -> String {
    text.trim().trim_matches('"').trim_matches('\'').trim().to_owned()
}

fn quoted_list(text: &str) -> Vec<String> {
    let inner = match (text.find('['), text.rfind(']')) {
        (Some(open), Some(close)) if open < close => &text[open + 1..close],
        _ => return Vec::new(),
    };
    inner
        .split(',')
        .map(unquote)
        .filter(|item| !item.is_empty())
        .collect()
}

pub fn parse_reactive(text: &str) -> Vec<ReactiveEntry> {
    let mut entries: Vec<ReactiveEntry> = Vec::new();
    let mut in_songs = false;
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(rest) = line.strip_prefix("- events:") {
            entries.push(ReactiveEntry {
                events: quoted_list(rest),
                songs: Vec::new(),
            });
            in_songs = false;
            continue;
        }
        let Some(entry) = entries.last_mut() else {
            continue;
        };
        if let Some(rest) = line.strip_prefix("songs:") {
            in_songs = true;
            entry.songs.extend(quoted_list(rest));
        } else if let Some(rest) = line.strip_prefix("- ") {
            if in_songs {
                let song = unquote(rest);
                if !song.is_empty() {
                    entry.songs.push(song);
                }
            }
        } else if line.contains(':') {
            in_songs = false;
        }
    }
    entries.retain(|entry| !entry.events.is_empty() && !entry.songs.is_empty());
    entries
}

fn load(name: &str) -> Option<Pack> {
    let (text, reactive) = pack_text(name)?;
    let folder = packs_dir().join(name);
    let audio = if folder.is_dir() {
        locate_folder(&folder)
    } else {
        locate_zip(&packs_dir().join(format!("{name}.zip")))
    };
    let mut pack = Pack {
        name: name.to_owned(),
        enabled: true,
        audio,
        ns: core::NAMESPACE.to_owned(),
        ..Pack::default()
    };
    if reactive {
        pack.reactive = parse_reactive(&text);
    } else {
        parse(&mut pack, &text);
    }
    let mut tracks: Vec<String> = pack
        .events
        .values()
        .chain(pack.biomes.values())
        .chain(pack.primary.values())
        .chain(pack.secondary.values())
        .flat_map(|value| value.split(','))
        .chain(pack.reactive.iter().flat_map(|entry| entry.songs.iter().map(String::as_str)))
        .map(str::trim)
        .filter(|track| {
            valid_track(track) && pack.audio.contains_key(&track.to_ascii_lowercase())
        })
        .map(str::to_owned)
        .collect();
    tracks.sort();
    tracks.dedup();
    pack.tracks = tracks;
    Some(pack)
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Stamp {
    wanted: Option<String>,
    fallback: Option<String>,
    entries: Vec<(String, Option<SystemTime>, Option<SystemTime>, u64)>,
}

fn stamp(wanted: Option<&str>, fallback: Option<&str>) -> Stamp {
    let mut entries = Vec::new();
    if let Ok(dir) = fs::read_dir(packs_dir()) {
        for entry in dir.filter_map(Result::ok) {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            let meta = entry.metadata().ok();
            let size = meta.as_ref().map_or(0, fs::Metadata::len);
            let own = meta.and_then(|meta| meta.modified().ok());
            let description = if path.is_dir() {
                folder_file(&path, PROPERTIES)
                    .or_else(|| folder_file(&path, REACTIVE))
                    .and_then(|file| modified(&file))
            } else {
                None
            };
            entries.push((name, own, description, size));
        }
    }
    entries.sort();
    Stamp {
        wanted: wanted.map(str::to_owned),
        fallback: fallback.map(str::to_owned),
        entries,
    }
}

fn load_wanted(wanted: Option<&str>, fallback: Option<&str>) -> (Option<Pack>, Option<Pack>) {
    let names = names();
    let chosen = match wanted {
        Some(choice) if names.iter().any(|name| name.as_str() == choice) => Some(choice.to_owned()),
        _ => names.first().cloned(),
    };
    let main = chosen.as_deref().and_then(load);
    let second = fallback
        .filter(|choice| Some(*choice) != chosen.as_deref())
        .filter(|choice| names.iter().any(|name| name.as_str() == *choice))
        .and_then(load)
        .filter(|pack| pack.reactive.is_empty())
        .map(|mut pack| {
            pack.ns = FALLBACK_NAMESPACE.to_owned();
            pack
        });
    (main, second)
}

pub fn refresh(wanted: Option<&str>, fallback: Option<&str>, now: Instant) -> Option<Arc<Pack>> {
    let mut slot = SLOT.lock().unwrap_or_else(PoisonError::into_inner);
    let current = slot.pack.clone();
    if slot.loading || slot.checked.is_some_and(|at| now.duration_since(at) < CHECK_EVERY) {
        return current;
    }
    slot.checked = Some(now);
    let stamp = stamp(wanted, fallback);
    if slot.stamp.as_ref() == Some(&stamp) {
        return current;
    }
    slot.loading = true;
    drop(slot);
    let wanted = wanted.map(str::to_owned);
    let fallback = fallback.map(str::to_owned);
    let job = move || {
        let (main, second) = load_wanted(wanted.as_deref(), fallback.as_deref());
        let mut slot = SLOT.lock().unwrap_or_else(PoisonError::into_inner);
        slot.pack = main.map(Arc::new);
        slot.fallback = second.map(Arc::new);
        slot.stamp = Some(stamp);
        slot.loading = false;
        slot.fresh = true;
    };
    if std::thread::Builder::new()
        .name("ambience-pack".into())
        .spawn(job)
        .is_err()
    {
        SLOT.lock().unwrap_or_else(PoisonError::into_inner).loading = false;
    }
    current
}

pub fn cached() -> Option<Arc<Pack>> {
    SLOT.lock().unwrap_or_else(PoisonError::into_inner).pack.clone()
}

pub fn cached_fallback() -> Option<Arc<Pack>> {
    SLOT.lock().unwrap_or_else(PoisonError::into_inner).fallback.clone()
}

pub fn loaded() -> Vec<Arc<Pack>> {
    let slot = SLOT.lock().unwrap_or_else(PoisonError::into_inner);
    slot.pack.iter().chain(slot.fallback.iter()).cloned().collect()
}

pub fn take_fresh() -> bool {
    let mut slot = SLOT.lock().unwrap_or_else(PoisonError::into_inner);
    std::mem::take(&mut slot.fresh)
}

pub fn reset() {
    let mut slot = SLOT.lock().unwrap_or_else(PoisonError::into_inner);
    slot.stamp = None;
    slot.checked = None;
}

pub fn pack_cache(name: &str) -> PathBuf {
    cache_root().join("packs").join(name)
}

pub fn sounds_dir(name: &str) -> PathBuf {
    pack_cache(name).join("sounds")
}

pub fn sound_namespace(resources: &Path, name: &str) -> Option<String> {
    fs::create_dir_all(resources.join("assets")).ok()?;
    let from = std::path::absolute(resources.join("assets")).ok()?;
    let to = std::path::absolute(pack_cache(name)).ok()?;
    let from: Vec<_> = from.components().collect();
    let to: Vec<_> = to.components().collect();
    let common = from.iter().zip(&to).take_while(|(a, b)| a == b).count();
    let mut parts: Vec<String> = vec!["..".to_owned(); from.len() - common];
    for component in &to[common..] {
        parts.push(component.as_os_str().to_str()?.to_owned());
    }
    let namespace = parts.join("/");
    (!namespace.is_empty() && !namespace.contains(':')).then_some(namespace)
}

pub fn remove_legacy(resources: &Path) {
    let assets = resources.join("assets").join(core::NAMESPACE);
    let _ = fs::remove_dir_all(assets.join("sounds"));
    let _ = fs::remove_dir(&assets);
    let root = cache_root();
    let _ = fs::remove_dir_all(root.join("sounds"));
    let _ = fs::remove_file(root.join(MANIFEST));
    if let Ok(entries) = fs::read_dir(&root) {
        for path in entries.filter_map(|entry| entry.ok().map(|entry| entry.path())) {
            if path.extension().and_then(|ext| ext.to_str()) == Some("src") {
                let _ = fs::remove_file(path);
            }
        }
    }
}

pub fn ready(event: &str) -> bool {
    let Some((namespace, track)) = event.split_once(':') else {
        return true;
    };
    let pack = match namespace {
        core::NAMESPACE => cached(),
        FALLBACK_NAMESPACE => cached_fallback(),
        _ => return true,
    };
    if !valid_track(track) {
        return false;
    }
    pack.is_some_and(|pack| is_ready_file(&sounds_dir(&pack.name).join(format!("{track}.ogg"))))
}

pub fn main_event(track: &str) -> String {
    format!("{}:{}", core::NAMESPACE, track.to_ascii_lowercase())
}

fn is_ready_file(path: &Path) -> bool {
    fs::metadata(path).is_ok_and(|meta| meta.is_file() && meta.len() > 0)
}

pub fn prepare(packs: &[Arc<Pack>], force: bool) {
    let key = packs
        .iter()
        .map(|pack| pack.name.as_str())
        .collect::<Vec<_>>()
        .join("|");
    {
        let mut done = PREPARED.lock().unwrap_or_else(PoisonError::into_inner);
        let known = !force && done.as_deref() == Some(key.as_str());
        if known || BUSY.swap(true, Ordering::AcqRel) {
            return;
        }
        *done = Some(key);
    }
    let mut jobs: Vec<(String, String, Entry)> = Vec::new();
    for pack in packs {
        for track in &pack.tracks {
            let lower = track.to_ascii_lowercase();
            if !valid_track(&lower) {
                continue;
            }
            if let Some(entry) = pack.audio.get(&lower) {
                jobs.push((pack.name.clone(), lower, entry.clone()));
            }
        }
    }
    *PROGRESS.lock().unwrap_or_else(PoisonError::into_inner) = Progress {
        total: jobs.len(),
        done: 0,
        failed: Vec::new(),
    };
    let job = move || {
        let mut manifests: HashMap<String, Manifest> = HashMap::new();
        for (name, lower, entry) in &jobs {
            let cache = pack_cache(name);
            let sounds = cache.join("sounds");
            if fs::create_dir_all(&sounds).is_err() {
                PROGRESS
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .failed
                    .push((name.clone(), format!("cannot create {}", sounds.display())));
                continue;
            }
            let manifest = manifests
                .entry(name.clone())
                .or_insert_with(|| read_manifest(&cache));
            let result = convert(&sounds, &cache, lower, entry, manifest, force);
            let mut progress = PROGRESS.lock().unwrap_or_else(PoisonError::into_inner);
            match result {
                Ok(converted) => {
                    progress.done += 1;
                    drop(progress);
                    if converted {
                        write_manifest(&cache, manifest);
                    }
                }
                Err(error) => {
                    log::warn!("Ambience: {lower} was not converted: {error}");
                    progress.failed.push((lower.clone(), error));
                }
            }
        }
        BUSY.store(false, Ordering::Release);
    };
    if std::thread::Builder::new()
        .name("ambience-convert".into())
        .spawn(job)
        .is_err()
    {
        BUSY.store(false, Ordering::Release);
    }
}

pub fn progress() -> (bool, usize, usize, Vec<(String, String)>) {
    let progress = PROGRESS.lock().unwrap_or_else(PoisonError::into_inner);
    (
        BUSY.load(Ordering::Acquire),
        progress.total,
        progress.done,
        progress.failed.clone(),
    )
}

pub fn convert_again(packs: &[Arc<Pack>]) -> bool {
    if BUSY.load(Ordering::Acquire) {
        return false;
    }
    prepare(packs, true);
    true
}

fn modified(path: &Path) -> Option<SystemTime> {
    fs::metadata(path).ok()?.modified().ok()
}

fn time_key(path: &Path) -> u128 {
    modified(path)
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |since| since.as_nanos())
}

fn fingerprint(entry: &Entry) -> Option<String> {
    match entry {
        Entry::File(path) => {
            let size = fs::metadata(path).ok()?.len();
            Some(format!(
                "v{CONVERT_VERSION}|file|{}|{size}|{}",
                path.display(),
                time_key(path)
            ))
        }
        Entry::Zipped { archive, name } => {
            let size = fs::metadata(archive).ok()?.len();
            Some(format!(
                "v{CONVERT_VERSION}|zip|{}|{name}|{size}|{}",
                archive.display(),
                time_key(archive)
            ))
        }
    }
}

fn read_manifest(cache: &Path) -> Manifest {
    fs::read_to_string(cache.join(MANIFEST))
        .ok()
        .and_then(|text| serde_json::from_str::<Manifest>(&text).ok())
        .unwrap_or_default()
}

fn write_manifest(cache: &Path, manifest: &Manifest) {
    let Ok(text) = serde_json::to_string(manifest) else {
        return;
    };
    let partial = cache.join("manifest.partial");
    if fs::write(&partial, text).is_ok() {
        let _ = fs::rename(&partial, cache.join(MANIFEST));
    }
}

fn up_to_date(sounds: &Path, manifest: &Manifest, lower: &str, entry: &Entry, force: bool) -> bool {
    !force
        && fingerprint(entry).is_some_and(|print| manifest.get(lower) == Some(&print))
        && is_ready_file(&sounds.join(format!("{lower}.ogg")))
}

fn convert(
    sounds: &Path,
    cache: &Path,
    lower: &str,
    entry: &Entry,
    manifest: &mut Manifest,
    force: bool,
) -> Result<bool, String> {
    if !valid_track(lower) {
        return Err(format!("unsafe track name {lower:?}"));
    }
    if up_to_date(sounds, manifest, lower, entry, force) {
        return Ok(false);
    }
    let print = fingerprint(entry).ok_or_else(|| "the source cannot be read".to_owned())?;
    let target = sounds.join(format!("{lower}.ogg"));
    let partial = sounds.join(format!("{lower}.partial"));
    if !contained(sounds, &target) || !contained(sounds, &partial) {
        return Err(format!("output path for {lower:?} leaves the cache"));
    }
    if let Err(error) = encode_entry(entry, cache, lower, &partial).and_then(|()| verify(&partial)) {
        let _ = fs::remove_file(&partial);
        return Err(error);
    }
    fs::rename(&partial, &target).map_err(|e| e.to_string())?;
    manifest.insert(lower.to_owned(), print);
    Ok(true)
}

fn encode_entry(entry: &Entry, cache: &Path, lower: &str, partial: &Path) -> Result<(), String> {
    match entry {
        Entry::File(path) => encode_file(path, partial),
        Entry::Zipped { archive, name } => {
            let extracted = cache.join(format!("{lower}.src"));
            if !contained(cache, &extracted) {
                return Err(format!("source path for {lower:?} leaves the cache"));
            }
            if !extract(archive, name, &extracted) {
                return Err(format!("could not read {name} from {}", archive.display()));
            }
            encode_file(&extracted, partial)
        }
    }
}

fn encode_file(source: &Path, partial: &Path) -> Result<(), String> {
    let loudness: &[&str] = &["-af", "loudnorm=I=-16:TP=-1.5:LRA=11"];
    let primary: Vec<&str> = loudness.iter().copied().chain(["-c:a", "libvorbis", "-q:a", "5"]).collect();
    let fallback: Vec<&str> = loudness
        .iter()
        .copied()
        .chain(["-strict", "-2", "-c:a", "vorbis", "-q:a", "5"])
        .collect();
    encode(source, partial, &primary).or_else(|_| encode(source, partial, &fallback))
}

fn verify(path: &Path) -> Result<(), String> {
    let output = match Command::new("ffprobe")
        .args(["-v", "error", "-show_entries", "format=duration", "-of", "csv=p=0"])
        .arg(path)
        .output()
    {
        Ok(output) => output,
        Err(_) => return Ok(()),
    };
    let duration = String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse::<f64>()
        .unwrap_or(0.0);
    if output.status.success() && duration > 0.0 {
        Ok(())
    } else {
        Err("the converted file is damaged (ffprobe)".to_owned())
    }
}
fn encode(source: &Path, partial: &Path, codec: &[&str]) -> Result<(), String> {
    let mut args: Vec<OsString> = ["-nostdin", "-y", "-loglevel", "error", "-threads", "1", "-i"]
        .iter()
        .map(|arg| OsString::from(*arg))
        .collect();
    args.push(source.as_os_str().to_owned());
    args.extend(["-vn"].iter().map(|arg| OsString::from(*arg)));
    args.extend(codec.iter().map(|arg| OsString::from(*arg)));
    args.extend(["-threads", "1", "-f", "ogg"].iter().map(|arg| OsString::from(*arg)));
    args.push(partial.as_os_str().to_owned());
    match ffmpeg(&args) {
        Ok(out) if out.status.success() => Ok(()),
        Ok(out) => Err(last_line(&String::from_utf8_lossy(&out.stderr))),
        Err(error) => Err(format!("ffmpeg did not start: {error}")),
    }
}

fn ffmpeg(args: &[OsString]) -> io::Result<Output> {
    if cfg!(unix) {
        match Command::new("nice").args(["-n", "19", "ffmpeg"]).args(args).output() {
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            other => return other,
        }
    }
    Command::new("ffmpeg").args(args).output()
}

pub fn vorbis_encoder() -> &'static str {
    let encoders = Command::new("ffmpeg")
        .args(["-hide_banner", "-encoders"])
        .output()
        .map(|out| String::from_utf8_lossy(&out.stdout).into_owned())
        .unwrap_or_default();
    if encoders.lines().any(|line| line.split_whitespace().nth(1) == Some("libvorbis")) {
        "libvorbis"
    } else if encoders.lines().any(|line| line.split_whitespace().nth(1) == Some("vorbis")) {
        "built-in vorbis (experimental)"
    } else {
        "none"
    }
}

fn last_line(text: &str) -> String {
    let line = text
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("ffmpeg failed without output");
    line.trim().chars().take(200).collect()
}

pub fn purge(resources: &Path) {
    let assets = resources.join("assets").join(core::NAMESPACE);
    if assets.file_name().is_some_and(|name| name == core::NAMESPACE) {
        let _ = fs::remove_dir_all(&assets);
    }
    let cache = cache_root();
    if cache.file_name().is_some_and(|name| name == core::ID) {
        let _ = fs::remove_dir_all(&cache);
    }
    *PREPARED.lock().unwrap_or_else(PoisonError::into_inner) = None;
    reset();
}

fn extract(archive: &Path, name: &str, to: &Path) -> bool {
    let Ok(file) = fs::File::open(archive) else {
        return false;
    };
    let Ok(mut zip) = zip::ZipArchive::new(file) else {
        return false;
    };
    for i in 0..zip.len() {
        let Ok(mut entry) = zip.by_index(i) else {
            continue;
        };
        if entry.name().ok().as_deref() != Some(name) {
            continue;
        }
        let Ok(mut out) = fs::File::create(to) else {
            return false;
        };
        let ok = io::copy(&mut entry, &mut out).is_ok();
        if !ok {
            let _ = fs::remove_file(to);
        }
        return ok;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refuses_traversal_names_for_every_separator_style() {
        for name in [
            "..\\..\\victim",
            "..\\x",
            "music\\x",
            "C:x",
            "C:\\x",
            "a/b",
            "a.b",
            "a:b",
            "",
        ] {
            assert!(!valid_track(name), "{name:?} must be rejected");
        }
    }

    #[test]
    fn accepts_plain_track_names() {
        for name in ["rain", "cave_1", "ocean-wave"] {
            assert!(valid_track(name), "{name:?} must be accepted");
        }
    }

    #[test]
    fn contained_refuses_windows_style_escape() {
        let root = Path::new("cache").join("sounds");
        assert!(!contained(&root, &root.join("..\\..\\victim.src")));
        assert!(!contained(&root, &root.join("..").join("victim.src")));
        assert!(!contained(&root, &root.join("sub").join("x.ogg")));
        assert!(contained(&root, &root.join("rain.ogg")));
    }
}
