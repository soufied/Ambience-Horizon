fn ambience_hook_music(game: &crate::game::Game<'_>, music: &mut Option<horizon_client::music::Music>) {
    game.ambience_music(music);
}
