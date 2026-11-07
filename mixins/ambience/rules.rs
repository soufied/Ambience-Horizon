use std::sync::{Mutex, PoisonError};
use std::time::SystemTime;

use crate::game::official_ambience_pack as pk;

type Cache = Option<(Option<SystemTime>, Vec<pk::ReactiveEntry>)>;
static CACHE: Mutex<Cache> = Mutex::new(None);

pub fn path() -> std::path::PathBuf {
    crate::game::Game::game_dir()
        .join("ambience")
        .join("rules.yaml")
}

pub fn with_entries<R>(with: impl FnOnce(&[pk::ReactiveEntry]) -> R) -> R {
    let path = path();
    let modified = std::fs::metadata(&path)
        .and_then(|meta| meta.modified())
        .ok();
    let mut cache = CACHE.lock().unwrap_or_else(PoisonError::into_inner);
    let fresh = !matches!(cache.as_ref(), Some((seen, _)) if *seen == modified);
    if fresh {
        let entries = modified
            .and_then(|_| std::fs::read_to_string(&path).ok())
            .map(|text| pk::parse_reactive(&text))
            .unwrap_or_default();
        *cache = Some((modified, entries));
    }
    match cache.as_ref() {
        Some((_, entries)) => with(entries),
        None => with(&[]),
    }
}
