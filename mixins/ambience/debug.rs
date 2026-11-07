use std::collections::BTreeMap;
use std::process::Command;
use std::sync::PoisonError;

use crate::game::official_ambience as core;
use crate::game::official_ambience_pack as pk;

const USAGE: &str = "usage: /ambience [status | packs | pack <name|auto> | tracks | play <track|off> | stop | on | off | reload | convert | selftest]";
const MAX_TRACK_LINES: usize = 40;

impl crate::game::Game<'_> {
    pub(super) fn ambience_command(&mut self, line: &str) -> bool {
        let mut words = line.split_whitespace();
        if words.next() != Some(core::COMMAND) {
            return false;
        }
        let args: Vec<&str> = words.collect();
        match args.as_slice() {
            ["status"] => self.ambience_status(),
            ["packs"] => self.ambience_packs(),
            ["pack", name] => self.ambience_set_pack(name),
            ["tracks"] => self.ambience_tracks(),
            ["play", "off"] | ["stop"] => self.ambience_force(None),
            ["play", track] => self.ambience_force(Some(*track)),
            ["on"] => self.ambience_switch(true),
            ["off"] => self.ambience_switch(false),
            ["reload"] => self.ambience_reload(),
            ["convert"] => self.ambience_convert(),
            ["selftest"] => self.ambience_selftest(),
            _ => self.ambience_say(USAGE),
        }
        true
    }

    fn ambience_say(&mut self, text: impl AsRef<str>) {
        let line = format!("[Ambience] {}", text.as_ref());
        self.system_message(horizon_client::text::Text::literal(line));
    }

    fn ambience_status(&mut self) {
        let settings = self.ambience_settings();
        self.ambience_say(format!(
            "enabled: {}, pack setting: {}",
            core::flag(settings.enabled),
            settings.pack.as_deref().unwrap_or("auto")
        ));

        let found = pk::names();
        if found.is_empty() {
            let dir = pk::packs_dir();
            self.ambience_say(format!(
                "no pack found in {} (a folder or .zip with ambience.properties)",
                dir.display()
            ));
        } else {
            self.ambience_say(format!("packs found: {}", found.join(", ")));
        }

        let ffmpeg = Command::new("ffmpeg")
            .arg("-version")
            .output()
            .map(|out| out.status.success())
            .unwrap_or(false);
        self.ambience_say(format!(
            "ffmpeg (only for mp3/wav tracks): {}, vorbis encoder: {}",
            if ffmpeg { "found" } else { "not found" },
            super::official_ambience_pack::vorbis_encoder()
        ));

        let Some(pack) = pk::cached() else {
            self.ambience_say("pack not loaded yet (it loads on the first tick while enabled)");
            return;
        };
        let (running, total, done, failed) = pk::progress();
        self.ambience_say(format!(
            "conversion: {}, {} of {} done, {} failed",
            if running { "running" } else { "idle" },
            done,
            total,
            failed.len()
        ));
        for (track, error) in failed.iter().take(3) {
            self.ambience_say(format!("failed {track}: {error}"));
        }
        if !pack.reactive.is_empty() {
            self.ambience_say(format!(
                "format: ReactiveMusic ({} entries)",
                pack.reactive.len()
            ));
        }
        let sounds = pk::sounds_dir(&pack.name);
        let converted = pack
            .tracks
            .iter()
            .filter(|track| sounds.join(format!("{}.ogg", track.to_ascii_lowercase())).is_file())
            .count();
        self.ambience_say(format!(
            "loaded pack: {} (enabled={}), {} tracks, {} converted to ogg",
            pack.name,
            pack.enabled,
            pack.tracks.len(),
            converted
        ));

        match self.title.is_some() {
            true => self.ambience_say("situation: title screen"),
            false => match self.eye_position() {
                Some(eye) => {
                    let biome = super::official_ambience_choose::biome_path(self, eye).to_owned();
                    let line = format!(
                        "situation: dimension {}, biome {}, eye at {:.1} {:.1} {:.1}, underwater {}, boss {}, raining {}",
                        self.dim.id(),
                        biome,
                        eye[0],
                        eye[1],
                        eye[2],
                        self.ambience.was_underwater(),
                        self.boss_music(),
                        self.weather_levels(1.0).0 > 0.2,
                    );
                    self.ambience_say(line);
                    let pos = [
                        eye[0].floor() as i32,
                        eye[1].floor() as i32,
                        eye[2].floor() as i32,
                    ];
                    self.ambience_structure_steps(pos);
                }
                None => self.ambience_say("situation: no player in the world"),
            },
        }

        match self.ambience_choose(&pack) {
            Some((event, reason)) => {
                let registered = self.audio.engine.registry.contains(&event);
                let file = pk::ready(&event);
                self.ambience_say(format!(
                    "would play: {event} ({reason}); in sound engine: {}, ogg present: {}",
                    yes_no(registered),
                    yes_no(file)
                ));
            }
            None => self.ambience_say("would play: nothing (no track matches this situation)"),
        }

        let forced = core::FORCE.lock().unwrap_or_else(PoisonError::into_inner).clone();
        if let Some(track) = forced {
            self.ambience_say(format!("forced track: {track} (/ambience play off to stop)"));
        }

        let last = core::LAST.lock().unwrap_or_else(PoisonError::into_inner).clone();
        match last {
            Some(last) => self.ambience_say(format!(
                "last set on the music hook: {} ({}), vanilla volume {:.2}, tick {}",
                last.event, last.reason, last.volume, last.tick
            )),
            None => self.ambience_say(
                "the music hook has not set a track yet (menu/boss gating or no match)",
            ),
        }

        let ticks = self.music.next_song_delay();
        self.ambience_say(format!(
            "music manager: next track in about {} s (the music hook starts a track after this delay)",
            ticks.max(0) / 20
        ));
        let (vanilla, volume) = self.situational_music();
        let vanilla = vanilla
            .map(|music| music.sound)
            .unwrap_or_else(|| "none".to_owned());
        self.ambience_say(format!("vanilla would play: {vanilla} at volume {volume:.2}"));
    }

    fn ambience_packs(&mut self) {
        let settings = self.ambience_settings();
        let names = pk::names();
        if names.is_empty() {
            self.ambience_say("no packs found");
            return;
        }
        let active = match settings.pack.as_deref() {
            Some(choice) => choice.to_owned(),
            None => names.first().cloned().unwrap_or_default(),
        };
        let list: Vec<String> = names
            .iter()
            .map(|name| {
                if *name == active {
                    format!("*{name}")
                } else {
                    name.clone()
                }
            })
            .collect();
        self.ambience_say(format!("packs (* active): {}", list.join(", ")));
    }

    fn ambience_set_pack(&mut self, name: &str) {
        let value = if name == "auto" {
            String::new()
        } else if pk::names().iter().any(|known| known == name) {
            name.to_owned()
        } else {
            self.ambience_say(format!("no pack named {name}; see /ambience packs"));
            return;
        };
        self.native_set_setting(core::ID, core::KEYS[1], &value);
        core::invalidate();
        pk::reset();
        self.ambience_say(format!(
            "pack set to {}",
            if value.is_empty() { "auto" } else { name }
        ));
    }

    fn ambience_structure_steps(&mut self, pos: [i32; 3]) {
        let refs: Option<Vec<(String, i32, i32)>> = self
            .world
            .chunks
            .get(&(pos[0] >> 4, pos[2] >> 4))
            .map(|chunk| {
                chunk
                    .structure_refs
                    .iter()
                    .map(|(name, (x, z))| (name.to_string(), *x, *z))
                    .collect()
            });
        let Some(refs) = refs else {
            self.ambience_say("structures: this chunk is not loaded yet");
            return;
        };
        if refs.is_empty() {
            self.ambience_say("structures: this chunk has no structure references");
            return;
        }
        let listed: Vec<String> = refs
            .iter()
            .map(|(name, x, z)| format!("{name} at chunk {x},{z}"))
            .collect();
        self.ambience_say(format!("structure references here: {}", listed.join("; ")));
        let Some(locator) = self.end.locator() else {
            self.ambience_say("structures: no structure locator for this world, so bounds cannot be checked");
            return;
        };
        let mut lines = Vec::new();
        for (name, sx, sz) in &refs {
            let starts = locator.starts_at(*sx, *sz);
            let matching: Vec<_> = starts.iter().filter(|start| start.name == *name).collect();
            let inside = matching.iter().any(|start| start.bounding_box.is_inside(pos));
            lines.push(format!(
                "  {name}: {} start(s) at chunk {sx},{sz}; your position inside its bounds: {}",
                matching.len(),
                if inside { "yes" } else { "no" }
            ));
        }
        for line in lines {
            self.ambience_say(line);
        }
    }

    fn ambience_selftest(&mut self) {
        let mut checks: Vec<(bool, String)> = Vec::new();
        let pack = pk::cached();
        match &pack {
            Some(pack) => {
                checks.push((true, format!("pack: {} ({} tracks)", pack.name, pack.tracks.len())));
                let sounds = pk::sounds_dir(&pack.name);
                let ready = pack
                    .tracks
                    .iter()
                    .filter(|track| pk::ready(&pack.event(track)))
                    .count();
                let missing = pack.tracks.len() - ready;
                checks.push((
                    missing == 0,
                    if missing == 0 {
                        format!("converted: {ready} of {}", pack.tracks.len())
                    } else {
                        format!("{missing} tracks not converted (see /ambience tracks)")
                    },
                ));
                let writable = std::fs::create_dir_all(&sounds).is_ok();
                checks.push((writable, format!("cache folder: {}", sounds.display())));
                let linked = pk::sound_namespace(&self.options.resources, &pack.name).is_some();
                checks.push((linked, "sound events can reach the cache".to_owned()));
                let registered = pack
                    .tracks
                    .first()
                    .is_some_and(|track| self.audio.engine.registry.contains(&pack.event(track)));
                checks.push((registered, "sound events registered in the engine".to_owned()));
            }
            None => checks.push((false, "no pack loaded (put one in resourcepacks/)".to_owned())),
        }
        let tool = |name: &str| {
            Command::new(name)
                .arg("-version")
                .output()
                .is_ok_and(|output| output.status.success())
        };
        checks.push((tool("ffmpeg"), "ffmpeg".to_owned()));
        let probe = tool("ffprobe");
        checks.push((
            probe,
            if probe {
                "ffprobe (converted tracks are verified)".to_owned()
            } else {
                "ffprobe not found: converted tracks are not verified".to_owned()
            },
        ));
        for (ok, text) in checks {
            self.ambience_say(format!("{} {text}", if ok { "ok  " } else { "FAIL" }));
        }
    }

    fn ambience_tracks(&mut self) {
        let Some(pack) = pk::cached() else {
            self.ambience_say("pack not loaded yet (it loads on the first tick while enabled)");
            return;
        };
        let sounds = pk::sounds_dir(&pack.name);

        let mut uses: BTreeMap<String, Vec<String>> = BTreeMap::new();
        let groups = [
            ("event", &pack.events),
            ("biome", &pack.biomes),
            ("primarytag", &pack.primary),
            ("secondarytag", &pack.secondary),
        ];
        for (prefix, map) in groups {
            for (key, track) in map {
                uses.entry(track.to_ascii_lowercase())
                    .or_default()
                    .push(format!("{prefix}.{key}"));
            }
        }

        for entry in &pack.reactive {
            for song in &entry.songs {
                uses.entry(song.to_ascii_lowercase())
                    .or_default()
                    .push(format!("reactive.{}", entry.events.join("+")));
            }
        }
        self.ambience_say(format!("tracks in {} ({} shown max):", pack.name, MAX_TRACK_LINES));
        for track in pack.tracks.iter().take(MAX_TRACK_LINES) {
            let lower = track.to_ascii_lowercase();
            let ogg = if sounds.join(format!("{lower}.ogg")).is_file() {
                "ogg ok"
            } else {
                "no ogg"
            };
            let keys = uses
                .get(&lower)
                .map(|keys| keys.join(", "))
                .unwrap_or_default();
            self.ambience_say(format!("{track} [{ogg}] <- {keys}"));
        }
        if pack.tracks.len() > MAX_TRACK_LINES {
            self.ambience_say(format!("... and {} more", pack.tracks.len() - MAX_TRACK_LINES));
        }
    }

    fn ambience_force(&mut self, track: Option<&str>) {
        let Some(track) = track else {
            *core::FORCE.lock().unwrap_or_else(PoisonError::into_inner) = None;
            self.stop_ambience_music();
            self.ambience_say("forced track stopped; the situation's music returns shortly");
            return;
        };
        let Some(pack) = pk::cached() else {
            self.ambience_say("pack not loaded yet; enable Ambience and wait a second");
            return;
        };
        match pack
            .tracks
            .iter()
            .find(|known| known.eq_ignore_ascii_case(track))
        {
            Some(name) => {
                let name = name.clone();
                *core::FORCE.lock().unwrap_or_else(PoisonError::into_inner) = Some(name.clone());
                self.ambience_say(format!(
                    "playing {name} now (/ambience play off to stop forcing)"
                ));
            }
            None => self.ambience_say(format!(
                "no track named {track} in {}; see /ambience tracks",
                pack.name
            )),
        }
    }

    fn ambience_switch(&mut self, on: bool) {
        if self.ambience_settings().enabled != on {
            self.ambience_page_click(0);
        }
        self.ambience_say(format!("enabled: {}", core::flag(on)));
    }

    fn ambience_convert(&mut self) {
        if pk::cached().is_none() {
            self.ambience_say("pack not loaded yet; enable Ambience and wait a second");
            return;
        }
        let packs = pk::loaded();
        if pk::convert_again(&packs) {
            let tracks: usize = packs.iter().map(|pack| pack.tracks.len()).sum();
            self.ambience_say(format!("converting {tracks} tracks again; see /ambience status"));
        } else {
            self.ambience_say("a conversion is still running; see /ambience status");
        }
    }

    fn ambience_reload(&mut self) {
        core::invalidate();
        pk::reset();
        self.ambience_say("pack reloads on the next tick");
    }
}

fn yes_no(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}
