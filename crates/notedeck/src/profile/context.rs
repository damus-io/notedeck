use enostr::Pubkey;

pub enum ProfileContextSelection {
    CopyLink,
}

pub struct ProfileContext {
    pub profile: Pubkey,
    pub selection: ProfileContextSelection,
}

impl ProfileContextSelection {
    pub fn process(&self, ctx: &egui::Context, pk: &Pubkey) {
        let Some(npub) = pk.npub() else {
            return;
        };

        ctx.copy_text(format!("https://damus.io/{npub}"));
    }
}
