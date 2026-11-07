fn ambience_command_hook(game: &mut crate::game::Game<'_>, command: &str, ran: &mut bool) {
    if game.ambience_command(command) {
        *ran = true;
    }
}
