fn ambience_page(game: &mut crate::game::Game<'_>, mod_id: &str, buttons: &mut Vec<String>) {
    use crate::game::official_ambience as m;
    if mod_id != m::ID {
        return;
    }
    buttons.extend(game.ambience_page_buttons());
}
