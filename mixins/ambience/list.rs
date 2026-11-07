fn ambience_listed(mods: &mut Vec<crate::mod_api::NativeMod>) {
    use crate::game::official_ambience as m;
    mods.push(crate::mod_api::NativeMod {
        id: m::ID.into(),
        title: format!("{} {}", m::NAME, m::VERSION),
        description: m::DESCRIPTION.into(),
        keys: Vec::new(),
    });
}
