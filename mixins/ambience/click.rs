fn ambience_click(game: &mut crate::game::Game<'_>, mod_id: &str, button: usize) {
    use crate::game::official_ambience as m;
    if mod_id != m::ID {
        return;
    }
    game.ambience_page_click(button);
}
