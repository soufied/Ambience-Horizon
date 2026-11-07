use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use horizon_client::sound_engine::{InstanceId, PlayResult, RECORDS, SoundEngine, SoundInstance, Sounds, Ticking};

use crate::game::official_ambience as core;

const COOLDOWN: Duration = Duration::from_secs(1);
const RETRY: Duration = Duration::from_secs(5);
pub const CHOOSE_EVERY: Duration = Duration::from_millis(250);

pub static PLAYER: Mutex<Player> = Mutex::new(Player::new());

#[derive(Clone, Copy, Debug, PartialEq)]
enum State {
    Active,
    Parked(Instant),
    Leaving,
}

struct Voice {
    event: String,
    id: InstanceId,
    gain: f32,
    state: State,
    heard: bool,
}

pub struct Frame {
    pub now: Instant,
    pub elapsed: Duration,
    pub volume: f32,
    pub fade: Duration,
    pub resume: Duration,
    pub scale: f32,
}

pub struct Player {
    voices: Vec<Voice>,
    cooldown_until: Option<Instant>,
    last_choice: Option<Instant>,
    pub chosen: Option<String>,
    pub owns: bool,
}

impl Player {
    pub const fn new() -> Self {
        Self {
            voices: Vec::new(),
            cooldown_until: None,
            last_choice: None,
            chosen: None,
            owns: false,
        }
    }

    pub fn due(&mut self, now: Instant) -> bool {
        if self
            .last_choice
            .is_none_or(|last| now.duration_since(last) >= CHOOSE_EVERY)
        {
            self.last_choice = Some(now);
            return true;
        }
        false
    }

    pub fn playing(&self) -> bool {
        self.voices.iter().any(|v| v.state == State::Active)
    }

    pub fn follow(&mut self, wanted: Option<&str>, frame: &Frame, sounds: &mut impl Sounds) {
        if let Some(active) = self.voices.iter_mut().find(|v| v.state == State::Active) {
            if Some(active.event.as_str()) == wanted {
                return;
            }
            active.state = if frame.resume.is_zero() {
                State::Leaving
            } else {
                State::Parked(frame.now)
            };
        }
        let Some(event) = wanted else {
            return;
        };
        if let Some(back) = self
            .voices
            .iter_mut()
            .find(|v| v.event == event && matches!(v.state, State::Parked(_)))
        {
            back.state = State::Active;
            return;
        }
        if self.cooldown_until.is_some_and(|until| frame.now < until) {
            return;
        }
        self.start(event, frame, sounds);
    }

    fn start(&mut self, event: &str, frame: &Frame, sounds: &mut impl Sounds) {
        let mut instance = SoundInstance::for_music(event, SoundEngine::unseeded());
        instance.volume = 0.0;
        instance.ticking = Ticking::Native;
        instance.source = RECORDS;
        instance.can_start_silent = true;
        let (id, result) = sounds.play_instance(instance, true);
        if matches!(result, PlayResult::NotStarted) {
            sounds.release(id);
            self.cooldown_until = Some(frame.now + RETRY);
            return;
        }
        self.voices.push(Voice {
            event: event.to_owned(),
            id,
            gain: 0.0,
            state: State::Active,
            heard: false,
        });
    }

    pub fn fade(&mut self, frame: &Frame, sounds: &mut impl Sounds) {
        let step = if frame.fade.is_zero() {
            1.0
        } else {
            frame.elapsed.as_secs_f32() / frame.fade.as_secs_f32()
        };
        for voice in &mut self.voices {
            let target = match voice.state {
                State::Active => frame.volume,
                State::Parked(_) | State::Leaving => 0.0,
            };
            voice.gain = if voice.gain < target {
                (voice.gain + step).min(target)
            } else {
                (voice.gain - step).max(target)
            };
            if let Some(instance) = sounds.engine().instance_mut(voice.id) {
                instance.volume = voice.gain * frame.scale;
            }
        }
    }

    pub fn prune(&mut self, frame: &Frame, sounds: &mut impl Sounds) {
        let mut kept = Vec::with_capacity(self.voices.len());
        for mut voice in std::mem::take(&mut self.voices) {
            let active = sounds.is_active(voice.id);
            voice.heard |= active;
            if voice.heard && !active {
                sounds.release(voice.id);
                if voice.state == State::Active {
                    self.cooldown_until = Some(frame.now + COOLDOWN);
                    crate::game::official_ambience_choose::advance_seed();
                }
                continue;
            }
            let done = match voice.state {
                State::Active => false,
                State::Parked(since) => frame.now.duration_since(since) >= frame.resume,
                State::Leaving => voice.gain <= 0.0,
            };
            if done {
                sounds.stop_instance(voice.id);
                sounds.release(voice.id);
                continue;
            }
            kept.push(voice);
        }
        self.voices = kept;
    }

    pub fn stop_all(&mut self, sounds: &mut impl Sounds) {
        for voice in self.voices.drain(..) {
            sounds.stop_instance(voice.id);
            sounds.release(voice.id);
        }
        self.cooldown_until = None;
        self.chosen = None;
    }
}

impl Default for Player {
    fn default() -> Self {
        Self::new()
    }
}

pub fn lock() -> std::sync::MutexGuard<'static, Player> {
    PLAYER.lock().unwrap_or_else(PoisonError::into_inner)
}

pub fn frame(elapsed: Duration, volume: f32, settings: &core::Settings) -> Frame {
    Frame {
        now: Instant::now(),
        elapsed,
        volume: volume.clamp(0.0, 1.0),
        fade: Duration::from_secs(u64::from(settings.crossfade)),
        resume: Duration::from_secs(u64::from(settings.resume)),
        scale: settings.volume as f32 / 100.0,
    }
}
