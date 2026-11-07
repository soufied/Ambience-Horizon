fn ambience_commands_hook(commands: &mut Vec<&'static str>) {
    commands.push(crate::game::official_ambience::COMMAND);
}
