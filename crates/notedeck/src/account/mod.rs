pub mod accounts;
pub mod cache;
pub mod mute;
pub mod relay;

pub const FALLBACK_PUBKEY: fn() -> enostr::Pubkey = || {
    enostr::Pubkey::new([
        170, 115, 48, 129, 228, 240, 247, 157, 212, 48, 35, 216, 152, 50, 101, 89, 63, 43, 65, 169,
        136, 103, 28, 252, 239, 63, 72, 155, 145, 173, 147, 254,
    ])
};
