use std::sync::PoisonError;

use crate::game::Game;
use crate::game::official_ambience as core;
use crate::game::official_ambience_pack as pk;

fn next_pack(current: &Option<String>) -> String {
    let mut options = vec![String::new()];
    options.extend(pk::names());
    let index = current
        .as_ref()
        .and_then(|choice| options.iter().position(|option| option == choice))
        .unwrap_or(0);
    options[(index + 1) % options.len()].clone()
}

impl Game<'_> {
    pub(super) fn ambience_page_buttons(&self) -> Vec<String> {
        let mut labels = core::labels(&self.ambience_settings());
        let (busy, total, done, failed) = pk::progress();
        if busy {
            labels.push(format!("Converting tracks: {done} of {total}"));
        } else if total > 0 && !failed.is_empty() {
            labels.push(format!("Tracks not converted: {} (see /ambience tracks)", failed.len()));
        } else if total > 0 {
            labels.push(format!("Tracks converted: {done} of {total}"));
        }
        labels
    }

    pub(super) fn ambience_page_click(&mut self, button: usize) {
        let settings = self.ambience_settings();
        match button {
            0 => {
                if settings.enabled {
                    pk::purge(&self.options.resources);
                }
                self.native_set_setting(core::ID, core::KEYS[0], core::flag(!settings.enabled));
            }
            1 => {
                let next = next_pack(&settings.pack);
                self.native_set_setting(core::ID, core::KEYS[1], &next);
                pk::reset();
            }
            2 => self.native_set_setting(core::ID, core::KEYS[2], core::flag(!settings.menu_boss)),
            3 => self.native_set_setting(core::ID, core::KEYS[3], core::flag(!settings.stop_on_pause)),
            4 => {
                let next = core::next_step(settings.crossfade, &core::CROSSFADE_STEPS);
                self.native_set_setting(core::ID, core::KEYS[4], &next.to_string());
            }
            5 => {
                let next = core::next_step(settings.resume, &core::RESUME_STEPS);
                self.native_set_setting(core::ID, core::KEYS[5], &next.to_string());
            }
            6 => self.native_set_setting(core::ID, core::KEYS[6], core::flag(!settings.custom_rules)),
            7 => {
                let next = core::next_step(settings.volume, &core::VOLUME_STEPS);
                self.native_set_setting(core::ID, core::KEYS[7], &next.to_string());
            }
            8 => {
                let next = next_pack(&settings.fallback);
                self.native_set_setting(core::ID, core::KEYS[8], &next);
                pk::reset();
            }
            _ => return,
        }
        core::invalidate();
    }
}
