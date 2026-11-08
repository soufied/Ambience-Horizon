use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use horizon_client::sound_engine::{MUSIC, Sounds};

use crate::game::official_ambience as core;
use crate::game::official_ambience_pack as pk;
use crate::game::official_ambience_player as player;
use crate::game::official_ambience_rules as rules;

static TICKS: AtomicU64 = AtomicU64::new(0);

const DEEP_Y: f64 = 0.0;
const CAVE_Y: f64 = 50.0;
const HIGH_Y: f64 = 100.0;
const NIGHT_START: i64 = 13000;
const NIGHT_END: i64 = 23000;
const DAY_LENGTH: i64 = 24000;

const EVENT_ORDER: [&str; 25] = [
    "underwater",
    "dying",
    "fishing",
    "minecart",
    "boat",
    "horse",
    "pig",
    "pumpkin",
    "nether",
    "end",
    "bastion",
    "fortress",
    "stronghold",
    "mineshaft",
    "monument",
    "end_city",
    "ancient_city",
    "trial_chambers",
    "villagenight",
    "village",
    "deepunderground",
    "underground",
    "highup",
    "rain",
    "night",
];

const PRIMARY: [(&str, &[&str]); 7] = [
    ("spooky", &["dark_forest", "swamp"]),
    ("dead", &["desert", "badlands"]),
    ("lush", &["lush", "jungle"]),
    ("mushroom", &["mushroom"]),
    ("magical", &["cherry", "flower_forest", "meadow"]),
    ("ocean", &["ocean"]),
    ("river", &["river"]),
];

const SECONDARY: [(&str, &[&str]); 10] = [
    ("mesa", &["badlands", "mesa"]),
    ("forest", &["forest"]),
    ("plains", &["plains"]),
    ("moutain", &["mountain", "peak", "slope", "jagged", "stony"]),
    ("mountain", &["mountain", "peak", "slope", "jagged", "stony"]),
    ("hills", &["hill"]),
    ("swamp", &["swamp"]),
    ("sandy", &["desert", "beach"]),
    ("snowy", &["snowy", "frozen", "ice"]),
    ("beach", &["beach"]),
];

pub(super) fn biome_path<'a>(game: &'a crate::game::Game<'_>, eye: [f64; 3]) -> &'a str {
    let (x, y, z) = (eye[0].floor() as i32, eye[1].floor() as i32, eye[2].floor() as i32);
    match game.world.chunks.get(&(x >> 4, z >> 4)) {
        Some(chunk) => {
            let biomes = match &game.world_biomes {
                Some(b) => b,
                None => game.res.biomes(),
            };
            let name = biomes.list[chunk.biome(x, y, z) as usize].name.as_str();
            name.strip_prefix("minecraft:").unwrap_or(name)
        }
        None => "plains",
    }
}

fn night(day_time: i64) -> bool {
    (NIGHT_START..NIGHT_END).contains(&day_time.rem_euclid(DAY_LENGTH))
}

fn tagged<'a>(
    table: &[(&str, &[&str])],
    path: &str,
    tags: &'a HashMap<String, String>,
) -> Option<&'a String> {
    table.iter().find_map(|(tag, keywords)| {
        if keywords.iter().any(|keyword| path.contains(keyword)) {
            tags.get(*tag)
        } else {
            None
        }
    })
}

fn lookup(
    main: &pk::Pack,
    fallback: Option<&pk::Pack>,
    get: impl Fn(&pk::Pack) -> Option<&String>,
) -> Option<String> {
    get(main)
        .map(|track| main.event(pick_song(track)))
        .or_else(|| fallback.and_then(|pack| get(pack).map(|track| pack.event(pick_song(track)))))
}

fn forced(pack: &pk::Pack) -> Option<String> {
    let wanted = core::FORCE
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()?;
    pack.tracks
        .iter()
        .find(|track| track.eq_ignore_ascii_case(&wanted))
        .cloned()
}

static REACTIVE_SEED: AtomicUsize = AtomicUsize::new(0);
static STRUCTURE_CACHE: Mutex<Option<(String, [i32; 3], Vec<(String, bool)>, Instant)>> = Mutex::new(None);
const STRUCTURE_TTL: Duration = Duration::from_secs(1);

#[derive(Clone, Copy)]
enum Reach {
    Piece,
    Bounds,
}

const STRUCTURES: [(&str, Reach); 9] = [
    ("bastion", Reach::Piece),
    ("fortress", Reach::Piece),
    ("stronghold", Reach::Piece),
    ("mineshaft", Reach::Piece),
    ("monument", Reach::Piece),
    ("end_city", Reach::Piece),
    ("ancient_city", Reach::Piece),
    ("trial_chambers", Reach::Piece),
    ("village", Reach::Bounds),
];

pub(super) fn advance_seed() {
    let next = reactive_seed()
        .wrapping_mul(6_364_136_223_846_793_005)
        .wrapping_add(1_442_695_040_888_963_407)
        >> 17;
    REACTIVE_SEED.store(next.max(1), Ordering::Relaxed);
}

fn cached_reaching(game: &crate::game::Game<'_>, dimension: &str, pos: [i32; 3]) -> Vec<(String, bool)> {
    let mut cache = STRUCTURE_CACHE.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some((cached_dim, cached_pos, reaching, at)) = cache.as_ref() {
        if cached_pos == &pos && cached_dim == dimension && at.elapsed() < STRUCTURE_TTL {
            return reaching.clone();
        }
    }
    let reaching = game.structures_reaching(pos);
    *cache = Some((dimension.to_owned(), pos, reaching.clone(), Instant::now()));
    reaching
}

fn reactive_seed() -> usize {
    let current = REACTIVE_SEED.load(Ordering::Relaxed);
    if current != 0 {
        return current;
    }
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.subsec_nanos() as usize)
        .unwrap_or(1)
        .max(1);
    REACTIVE_SEED.store(nanos, Ordering::Relaxed);
    nanos
}

fn pick(songs: &[String]) -> Option<String> {
    if songs.is_empty() {
        None
    } else {
        songs.get(reactive_seed() % songs.len()).cloned()
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct Activity {
    minecart: bool,
    boat: bool,
    horse: bool,
    pig: bool,
    fishing: bool,
    dying: bool,
    pumpkin: bool,
}

struct Situation<'a> {
    title: bool,
    underwater: bool,
    activity: Activity,
    y: f64,
    night: bool,
    rain: bool,
    dimension: &'a str,
    biome: &'a str,
    sunrise: bool,
    sunset: bool,
    storm: bool,
    village: bool,
    boss: bool,
}

fn keyword(biome: &str, words: &[&str]) -> bool {
    words.iter().any(|word| biome.contains(*word))
}

const BIOME_TAG_WORDS: [(&str, &[&str]); 12] = [
    ("hot", &["desert", "savanna", "badlands"]),
    ("cold", &["snowy", "frozen", "taiga", "ice"]),
    ("mountain", &["mountain", "peak", "slope", "jagged", "stony"]),
    ("ocean", &["ocean"]),
    ("river", &["river"]),
    ("forest", &["forest"]),
    ("jungle", &["jungle"]),
    ("swamp", &["swamp"]),
    ("desert", &["desert"]),
    ("plains", &["plains"]),
    ("beach", &["beach"]),
    ("mushroom", &["mushroom"]),
];

fn biome_tag_active(name: &str, path: &str) -> bool {
    let wanted = name.strip_prefix("is_").unwrap_or(name);
    let path = path.to_ascii_lowercase();
    PRIMARY
        .iter()
        .chain(SECONDARY.iter())
        .chain(BIOME_TAG_WORDS.iter())
        .any(|(tag, words)| *tag == wanted && words.iter().any(|word| path.contains(*word)))
}

fn event_active(event: &str, situation: &Situation) -> bool {
    if situation.title {
        return event == "MAIN_MENU";
    }
    let activity = situation.activity;
    match event {
        "GENERIC" => true,
        "UNDERWATER" => situation.underwater,
        "UNDERGROUND" => situation.y < CAVE_Y,
        "DEEP_UNDERGROUND" => situation.y < DEEP_Y,
        "HIGH_UP" => situation.y >= HIGH_Y,
        "DAY" => !situation.night,
        "NIGHT" => situation.night,
        "SUNRISE" => situation.sunrise,
        "SUNSET" => situation.sunset,
        "RAIN" => situation.rain,
        "STORM" => situation.storm,
        "SNOW" => keyword(situation.biome, &["snowy", "frozen", "ice"]),
        "MOUNTAIN" => keyword(situation.biome, &["mountain", "peak", "slope", "jagged", "stony"]),
        "BEACH" => keyword(situation.biome, &["beach"]),
        "FOREST" => keyword(situation.biome, &["forest"]),
        "OVERWORLD" => situation.dimension == "minecraft:overworld",
        "NETHER" => situation.dimension == "minecraft:the_nether",
        "END" => situation.dimension == "minecraft:the_end",
        "MINECART" => activity.minecart,
        "BOAT" => activity.boat,
        "HORSE" => activity.horse,
        "PIG" => activity.pig,
        "FISHING" => activity.fishing,
        "DYING" => activity.dying,
        "VILLAGE" => situation.village,
        "BOSS" => situation.boss,
        _ => false,
    }
}

fn term_active(term: &str, situation: &Situation) -> bool {
    let term: String = term
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect::<String>()
        .to_ascii_lowercase();
    if let Some(name) = term.strip_prefix("biome=") {
        return !name.is_empty() && situation.biome.to_ascii_lowercase().contains(name);
    }
    if let Some(name) = term.strip_prefix("biometag=") {
        return biome_tag_active(name, situation.biome);
    }
    if let Some(name) = term.strip_prefix("dim=") {
        return !name.is_empty() && situation.dimension.to_ascii_lowercase().contains(name);
    }
    if term.starts_with("block=") {
        return false;
    }
    event_active(&term.to_ascii_uppercase(), situation)
}

fn condition_active(events: &[String], situation: &Situation) -> bool {
    !events.is_empty()
        && events
            .iter()
            .all(|condition| condition.split("||").any(|term| term_active(term, situation)))
}

fn pick_song(value: &str) -> &str {
    let songs: Vec<&str> = value
        .split(',')
        .map(str::trim)
        .filter(|song| !song.is_empty())
        .collect();
    if songs.is_empty() {
        value
    } else {
        songs[reactive_seed() % songs.len()]
    }
}

impl crate::game::Game<'_> {
    fn activity(&self) -> Activity {
        let riding = self.riding;
        let mount = self.mount_index().map(|i| self.mobs[i].species());
        Activity {
            minecart: self.carts.iter().any(|c| Some(c.id) == riding),
            boat: self.boats.iter().any(|b| Some(b.id) == riding),
            horse: mount.is_some_and(horizon_client::mobs::equine::is_equine),
            pig: mount == Some(horizon_client::mobs::Species::Pig),
            fishing: self.hook.is_some(),
            dying: self.screen == crate::game::Screen::Dead,
            pumpkin: self
                .armor[3]
                .as_ref()
                .is_some_and(|s| s.item.as_ref() == "minecraft:carved_pumpkin"),
        }
    }

    pub(super) fn ambience_settings(&self) -> Arc<core::Settings> {
        let mut cache = core::CACHE.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(settings) = cache.as_ref() {
            return Arc::clone(settings);
        }
        let settings = Arc::new(core::Settings::read(|key| self.native_setting(core::ID, key)));
        *cache = Some(Arc::clone(&settings));
        settings
    }

    pub(super) fn ambience_choose(&self, pack: &pk::Pack) -> Option<(String, &'static str)> {
        if !pack.reactive.is_empty() {
            return self.reactive_choose(pack);
        }
        if let Some(track) = forced(pack) {
            return Some((pack.event(&track), "forced with /ambience play"));
        }
        if let Some(choice) = self.custom_choose() {
            return Some(choice);
        }
        let fallback = pk::cached_fallback();
        let fb = fallback.as_deref();
        if self.title.is_some() {
            return lookup(pack, fb, |p| p.events.get("mainmenu")).map(|event| (event, "title screen"));
        }
        let eye = self.eye_position()?;
        let y = eye[1];
        let pos = [
            eye[0].floor() as i32,
            eye[1].floor() as i32,
            eye[2].floor() as i32,
        ];
        let path = biome_path(self, eye);
        let dimension = self.dim.id();

        if self.boss_music() {
            if let Some(event) = lookup(pack, fb, |p| p.events.get("boss")) {
                return Some((event, "boss fight"));
            }
        }
        let activity = self.activity();
        let reaching = cached_reaching(self, dimension, pos);
        let structures: Vec<&str> = reaching
            .iter()
            .filter(|(_, piece)| *piece)
            .map(|(name, _)| name.as_str())
            .collect();
        let inside = |word: &str| structures.iter().any(|name| name.contains(word));
        let in_bounds = |word: &str| reaching.iter().any(|(name, _)| name.contains(word));
        for event in EVENT_ORDER {
            if lookup(pack, fb, |p| p.events.get(event)).is_none() {
                continue;
            }
            let active = match event {
                "underwater" => self.ambience.was_underwater(),
                "deepunderground" => y < DEEP_Y,
                "underground" => y < CAVE_Y,
                "highup" => y >= HIGH_Y,
                "dying" => activity.dying,
                "fishing" => activity.fishing,
                "minecart" => activity.minecart,
                "boat" => activity.boat,
                "horse" => activity.horse,
                "pig" => activity.pig,
                "pumpkin" => activity.pumpkin,
                "rain" => self.weather_levels(1.0).0 > 0.2,
                "night" => night(self.world.day_time),
                "villagenight" => in_bounds("village") && night(self.world.day_time),
                "nether" => dimension == "minecraft:the_nether",
                "end" => dimension == "minecraft:the_end",
                _ => STRUCTURES.iter().any(|(name, reach)| {
                    *name == event
                        && match reach {
                            Reach::Piece => inside(*name),
                            Reach::Bounds => in_bounds(*name),
                        }
                }),
            };
            if active {
                if let Some(choice) = lookup(pack, fb, |p| p.events.get(event)) {
                    return Some((choice, event));
                }
            }
        }
        if let Some(event) = lookup(pack, fb, |p| p.biomes.get(path)) {
            return Some((event, "biome"));
        }
        let dimension_tag = match dimension {
            "minecraft:the_nether" => Some("nether"),
            "minecraft:the_end" => Some("end"),
            _ => None,
        };
        if let Some(event) = lookup(pack, fb, |p| dimension_tag.and_then(|tag| p.primary.get(tag))) {
            return Some((event, "dimension"));
        }
        if let Some(event) = lookup(pack, fb, |p| tagged(&PRIMARY, path, &p.primary)) {
            return Some((event, "primary biome tag"));
        }
        if let Some(event) = lookup(pack, fb, |p| tagged(&SECONDARY, path, &p.secondary)) {
            return Some((event, "secondary biome tag"));
        }
        lookup(pack, fb, |p| p.events.get("generic")).map(|event| (event, "generic fallback"))
    }

    pub(super) fn stop_ambience_music(&mut self) {
        player::lock().stop_all(&mut self.audio);
    }

    fn situation(&self) -> Option<Situation<'_>> {
        if self.title.is_some() {
            return Some(Situation {
                title: true,
                underwater: false,
                activity: Activity::default(),
                y: 0.0,
                night: false,
                rain: false,
                dimension: "",
                biome: "",
                sunrise: false,
                sunset: false,
                storm: false,
                village: false,
                boss: false,
            });
        }
        let eye = self.eye_position()?;
        let time = self.world.day_time.rem_euclid(DAY_LENGTH);
        let pos = [
            eye[0].floor() as i32,
            eye[1].floor() as i32,
            eye[2].floor() as i32,
        ];
        let dimension = self.dim.id();
        let village = cached_reaching(self, dimension, pos)
            .iter()
            .any(|(name, _)| name.contains("village"));
        Some(Situation {
            title: false,
            underwater: self.ambience.was_underwater(),
            activity: self.activity(),
            y: eye[1],
            night: night(self.world.day_time),
            rain: self.weather_levels(1.0).0 > 0.2,
            dimension,
            biome: biome_path(self, eye),
            sunrise: time >= 23000 || time < 1000,
            sunset: (12000..13000).contains(&time),
            storm: self.weather_levels(1.0).1 > 0.2,
            village,
            boss: self.boss_music(),
        })
    }

    fn custom_choose(&self) -> Option<(String, &'static str)> {
        if !self.ambience_settings().custom_rules {
            return None;
        }
        rules::with_entries(|entries| {
            if entries.is_empty() {
                return None;
            }
            let situation = self.situation()?;
            for entry in entries {
                if condition_active(&entry.events, &situation) {
                    if let Some(song) = pick(&entry.songs) {
                        let event = if song.contains(':') {
                            song
                        } else {
                            pk::main_event(&song)
                        };
                        return Some((event, "custom rules (ambience/rules.yaml)"));
                    }
                }
            }
            None
        })
    }

    fn reactive_choose(&self, pack: &pk::Pack) -> Option<(String, &'static str)> {
        if let Some(track) = forced(pack) {
            return Some((pack.event(&track), "forced with /ambience play"));
        }
        let situation = self.situation()?;
        for entry in &pack.reactive {
            if condition_active(&entry.events, &situation) {
                if let Some(song) = pick(&entry.songs) {
                    return Some((
                        pack.event(&song),
                        "ReactiveMusic entry (first match in file order)",
                    ));
                }
            }
        }
        None
    }

    pub(super) fn ambience_frame(&mut self, elapsed: Duration) {
        self.ambience_sync();
        let settings = self.ambience_settings();
        let now = Instant::now();
        let pack = pk::cached().filter(|pack| pack.enabled);
        let paused = self.pause && settings.stop_on_pause;
        let held_back = !settings.menu_boss && (self.title.is_some() || self.boss_music());
        let may_play = settings.enabled
            && pack.is_some()
            && !paused
            && !held_back
            && self.win.is_none();
        let mut ours = player::lock();
        let Some(pack) = pack.filter(|_| may_play) else {
            ours.owns = false;
            ours.stop_all(&mut self.audio);
            return;
        };
        let (_, volume) = self.situational_music();
        let frame = player::frame(elapsed, volume, &settings);
        if !self.pause && ours.due(now) {
            match self.ambience_choose(&pack) {
                Some((event, _)) if !pk::ready(&event) => {}
                Some((event, reason)) => {
                    *core::LAST.lock().unwrap_or_else(PoisonError::into_inner) =
                        Some(core::Last {
                            event: event.clone(),
                            reason,
                            volume,
                            tick: TICKS.load(Ordering::Relaxed),
                        });
                    ours.chosen = Some(event);
                }
                None => ours.chosen = None,
            }
        }
        let wanted = ours.chosen.clone();
        ours.prune(&frame, &mut self.audio);
        ours.follow(wanted.as_deref(), &frame, &mut self.audio);
        ours.fade(&frame, &mut self.audio);
        ours.owns = wanted.is_some() || ours.playing();
        if ours.owns {
            self.hold_music();
        }
    }

    fn hold_music(&mut self) {
        if self.music.current_instance().is_some() {
            self.music.stop_playing(None, &mut self.audio);
        }
        if self.audio.engine.category_gain(MUSIC) != 1.0 {
            self.audio.update_category_volume(MUSIC, 1.0);
        }
    }

    pub(super) fn ambience_sync(&mut self) {
        let settings = self.ambience_settings();
        if !settings.enabled {
            return;
        }
        TICKS.fetch_add(1, Ordering::Relaxed);
        static LEGACY_CHECKED: AtomicBool = AtomicBool::new(false);
        if !LEGACY_CHECKED.swap(true, Ordering::Relaxed) {
            pk::remove_legacy(&self.options.resources);
        }
        let Some(pack) = pk::refresh(
            settings.pack.as_deref(),
            settings.fallback.as_deref(),
            Instant::now(),
        ) else {
            return;
        };
        if !pack.enabled {
            return;
        }
        let fresh = pk::take_fresh();
        let missing = pack
            .tracks
            .first()
            .is_some_and(|track| !self.audio.engine.registry.contains(&pack.event(track)));
        if fresh || missing {
            let packs = pk::loaded();
            for loaded in &packs {
                let Some(namespace) = pk::sound_namespace(&self.options.resources, &loaded.name) else {
                    log::warn!(
                        "Ambience: the converted tracks of {} cannot be reached from the resources folder",
                        loaded.name
                    );
                    return;
                };
                let json = loaded.events_json(&namespace);
                self.audio
                    .engine
                    .registry
                    .add_file(&loaded.ns, json.as_bytes(), &|_: &str, _: &str| true);
            }
            pk::prepare(&packs, false);
        }
    }

    pub(super) fn ambience_music(&self, music: &mut Option<horizon_client::music::Music>) {
        if music
            .as_ref()
            .is_some_and(|vanilla| vanilla.sound == "minecraft:music.credits")
        {
            return;
        }
        if player::lock().owns {
            *music = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generic_is_not_an_early_event() {
        assert!(!EVENT_ORDER.contains(&"generic"));
    }
}
