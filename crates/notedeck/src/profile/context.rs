use enostr::Pubkey;

pub enum ProfileContextSelection {
    AddProfileColumn,
    CopyLink,
    ViewAs,
    MuteUser,
}

pub struct ProfileContext {
    pub profile: Pubkey,
    pub selection: ProfileContextSelection,
}

impl ProfileContextSelection {
    pub fn process(&self, ctx: &egui::Context, pk: &Pubkey) {
        match self {
            ProfileContextSelection::CopyLink => {
                let Some(npub) = pk.npub() else {
                    return;
                };

                ctx.copy_text(format!("https://damus.io/{npub}"));
            }
            ProfileContextSelection::ViewAs
            | ProfileContextSelection::AddProfileColumn
            | ProfileContextSelection::MuteUser => {
                // handled separately in profile.rs
            }
        }
    }
}
