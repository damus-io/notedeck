//! `explain`'s colour palette, ported from `explain/theme.py` (`palette`) and
//! `explain/color.py` (`oklch_to_hex`), so a hand-drawn figure's
//! `var(--moss)` reads in the review pane as it does on the built page.
//!
//! Every token is an OKLCH lightness and chroma per theme, at a hue the page's
//! front matter may move: `accent` moves brass, `ground` tints the neutrals,
//! and `hues:` moves any of the four roles.

use crate::review_walk::FrontMatter;

/// `[L, C]` per neutral token, light then dark. Their chroma is scaled by the
/// ground's tint.
const NEUTRAL: [NeutralToken; 9] = [
    NeutralToken::new("bg", [0.965, 0.006], [0.194, 0.018]),
    NeutralToken::new("surface", [1.000, 0.000], [0.229, 0.022]),
    NeutralToken::new("surface-2", [0.941, 0.011], [0.267, 0.026]),
    NeutralToken::new("surface-3", [0.916, 0.015], [0.309, 0.032]),
    NeutralToken::new("ink", [0.236, 0.022], [0.952, 0.012]),
    NeutralToken::new("ink-2", [0.430, 0.033], [0.772, 0.031]),
    NeutralToken::new("ink-3", [0.588, 0.035], [0.605, 0.034]),
    NeutralToken::new("rule", [0.880, 0.020], [0.317, 0.030]),
    NeutralToken::new("rule-strong", [0.800, 0.030], [0.389, 0.032]),
];

/// `[L, C]` per role suffix, light then dark; the role's chroma scales C.
const ROLE_SUFFIXES: [NeutralToken; 3] = [
    NeutralToken::new("", [0.570, 0.125], [0.745, 0.125]),
    NeutralToken::new("-ink", [0.455, 0.100], [0.848, 0.090]),
    NeutralToken::new("-soft", [0.928, 0.036], [0.295, 0.038]),
];

/// The four roles: name, chroma scale, default hue.
const ROLES: [Role; 4] = [
    Role {
        name: "brass",
        chroma: 1.0,
        hue: 67.0,
    },
    Role {
        name: "slate",
        chroma: 0.45,
        hue: 239.0,
    },
    Role {
        name: "moss",
        chroma: 0.58,
        hue: 141.0,
    },
    Role {
        name: "alert",
        chroma: 1.25,
        hue: 32.0,
    },
];

/// Hues a page may name instead of giving degrees.
const NAMED_HUES: [NamedHue; 16] = [
    NamedHue::new("rose", 5.0),
    NamedHue::new("red", 25.0),
    NamedHue::new("rust", 40.0),
    NamedHue::new("amber", 67.0),
    NamedHue::new("brass", 67.0),
    NamedHue::new("gold", 85.0),
    NamedHue::new("olive", 110.0),
    NamedHue::new("green", 141.0),
    NamedHue::new("moss", 141.0),
    NamedHue::new("teal", 185.0),
    NamedHue::new("cyan", 210.0),
    NamedHue::new("slate", 239.0),
    NamedHue::new("blue", 255.0),
    NamedHue::new("indigo", 275.0),
    NamedHue::new("violet", 300.0),
    NamedHue::new("plum", 330.0),
];

/// The named grounds: hue and tint.
const GROUNDS: [Ground; 4] = [
    Ground {
        name: "cool",
        hue: 274.0,
        tint: 1.0,
    },
    Ground {
        name: "warm",
        hue: 92.0,
        tint: 0.75,
    },
    Ground {
        name: "neutral",
        hue: 270.0,
        tint: 0.15,
    },
    Ground {
        name: "paper",
        hue: 80.0,
        tint: 0.45,
    },
];

/// A token's `[L, C]` in each theme.
struct NeutralToken {
    name: &'static str,
    light: [f64; 2],
    dark: [f64; 2],
}

impl NeutralToken {
    const fn new(name: &'static str, light: [f64; 2], dark: [f64; 2]) -> Self {
        Self { name, light, dark }
    }
}

struct Role {
    name: &'static str,
    chroma: f64,
    hue: f64,
}

struct NamedHue {
    name: &'static str,
    hue: f64,
}

impl NamedHue {
    const fn new(name: &'static str, hue: f64) -> Self {
        Self { name, hue }
    }
}

struct Ground {
    name: &'static str,
    hue: f64,
    tint: f64,
}

/// One palette token: its name (no `--`) and its hex in each theme.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Token {
    pub name: String,
    pub light: String,
    pub dark: String,
}

/// A page's palette: every token, light and dark.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Palette {
    tokens: Vec<Token>,
}

impl Default for Palette {
    /// The palette of a page with no `accent`, `ground` or `hues`.
    fn default() -> Self {
        Self::of(&FrontMatter::default())
    }
}

impl Palette {
    /// The palette `front`'s `accent`, `ground` and `hues` pick. A value
    /// explain would warn about falls back to the default, as there.
    pub(crate) fn of(front: &FrontMatter) -> Self {
        let cool = &GROUNDS[0];
        let (mut ground_hue, mut tint) = (cool.hue, cool.tint);
        if let Some(ground) = front.ground.as_deref() {
            if let Some(named) = GROUNDS.iter().find(|g| g.name == ground) {
                ground_hue = named.hue;
                tint = named.tint;
            } else {
                ground_hue = hue(Some(ground), ground_hue);
                tint = 0.8;
            }
        }
        let mut hues: [f64; 4] = ROLES.map(|r| r.hue);
        hues[0] = hue(front.accent.as_deref(), hues[0]);
        for over in &front.hues {
            if let Some(i) = ROLES.iter().position(|r| r.name == over.role) {
                hues[i] = hue(Some(&over.value), hues[i]);
            }
        }
        let mut tokens = Vec::with_capacity(NEUTRAL.len() + ROLES.len() * ROLE_SUFFIXES.len());
        for t in &NEUTRAL {
            tokens.push(Token {
                name: t.name.to_owned(),
                light: oklch_to_hex(t.light[0], t.light[1] * tint, ground_hue),
                dark: oklch_to_hex(t.dark[0], t.dark[1] * tint, ground_hue),
            });
        }
        for (role, h) in ROLES.iter().zip(hues) {
            for s in &ROLE_SUFFIXES {
                let k = role.chroma;
                tokens.push(Token {
                    name: format!("{}{}", role.name, s.name),
                    light: oklch_to_hex(s.light[0], s.light[1] * k, h),
                    dark: oklch_to_hex(s.dark[0], s.dark[1] * k, h),
                });
            }
        }
        Self { tokens }
    }

    /// Token `name`'s hex (`#RRGGBB`) in the dark or light theme.
    pub(crate) fn get(&self, name: &str, dark: bool) -> Option<&str> {
        let token = self.tokens.iter().find(|t| t.name == name)?;
        Some(if dark { &token.dark } else { &token.light })
    }
}

/// `value` as a hue: degrees (wrapped into `[0, 360)`) or a named hue;
/// `fallback` when it's empty or neither.
fn hue(value: Option<&str>, fallback: f64) -> f64 {
    let Some(value) = value.filter(|v| !v.is_empty()) else {
        return fallback;
    };
    if is_degrees(value)
        && let Ok(d) = value.parse::<f64>()
    {
        return ((d % 360.0) + 360.0) % 360.0;
    }
    NAMED_HUES
        .iter()
        .find(|n| n.name == value)
        .map_or(fallback, |n| n.hue)
}

/// Whether `s` is `-?[0-9]+(\.[0-9]+)?`, as explain's degrees.
fn is_degrees(s: &str) -> bool {
    let s = s.strip_prefix('-').unwrap_or(s);
    let (int, frac) = match s.split_once('.') {
        Some((i, f)) => (i, Some(f)),
        None => (s, None),
    };
    let digits = |p: &str| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit());
    digits(int) && frac.is_none_or(digits)
}

/// OKLCH to linear sRGB.
fn to_rgb(l: f64, c: f64, h: f64) -> [f64; 3] {
    let h = h * std::f64::consts::PI / 180.0;
    let (a, b) = (c * h.cos(), c * h.sin());
    let l_ = (l + 0.3963377774 * a + 0.2158037573 * b).powi(3);
    let m_ = (l - 0.1055613458 * a - 0.0638541728 * b).powi(3);
    let s_ = (l - 0.0894841775 * a - 1.291485548 * b).powi(3);
    [
        4.0767416621 * l_ - 3.3077115913 * m_ + 0.2309699292 * s_,
        -1.2684380046 * l_ + 2.6097574011 * m_ - 0.3413193965 * s_,
        -0.0041960863 * l_ - 0.7034186147 * m_ + 1.707614701 * s_,
    ]
}

/// Linear to gamma-encoded sRGB.
fn gamma(c: f64) -> f64 {
    if c <= 0.0031308 {
        12.92 * c
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    }
}

/// An OKLCH colour as `#RRGGBB`. Out of gamut it loses chroma, not
/// lightness, so contrast holds.
fn oklch_to_hex(l: f64, c: f64, h: f64) -> String {
    let out = |rgb: &[f64; 3]| rgb.iter().any(|v| *v < -1e-4 || *v > 1.0 + 1e-4);
    let mut rgb = to_rgb(l, c, h);
    let mut c = c;
    while out(&rgb) && c > 0.0 {
        rgb = to_rgb(l, c, h);
        c -= 0.002;
    }
    // Math.round: halves go up.
    let byte = |v: f64| (gamma(v.clamp(0.0, 1.0)).clamp(0.0, 1.0) * 255.0 + 0.5).floor() as u8;
    format!(
        "#{:02X}{:02X}{:02X}",
        byte(rgb[0]),
        byte(rgb[1]),
        byte(rgb[2])
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::review_walk::HueOverride;

    /// The hex `explain.theme.palette` printed for the same front matter.
    #[test]
    fn matches_explain_palette() {
        let default = Palette::default();
        for (name, light, dark) in [
            ("bg", "#F2F3F8", "#12141D"),
            ("ink", "#1B1E29", "#ECEFF7"),
            ("ink-2", "#4A4F62", "#AEB4C9"),
            ("brass", "#A76603", "#E19C4E"),
            ("brass-ink", "#7B4A01", "#F5C28D"),
            ("brass-soft", "#F8E3CE", "#392917"),
            ("moss", "#60825A", "#94B88D"),
            ("moss-ink", "#455F40", "#BBD6B6"),
            ("alert", "#C24C37", "#FE836C"),
            ("alert-ink", "#8F3526", "#FEBAAC"),
            ("slate-soft", "#DEE9F1", "#252E34"),
        ] {
            assert_eq!(default.get(name, false), Some(light), "{name} light");
            assert_eq!(default.get(name, true), Some(dark), "{name} dark");
        }

        let front = FrontMatter {
            accent: Some("rust".into()),
            ground: Some("warm".into()),
            hues: vec![HueOverride {
                role: "moss".into(),
                value: "150".into(),
            }],
            ..Default::default()
        };
        let moved = Palette::of(&front);
        for (name, light, dark) in [
            ("bg", "#F4F3F0", "#17150E"),
            ("ink", "#211E15", "#F1EFE8"),
            ("brass", "#B45A3A", "#EF906E"),
            ("brass-soft", "#FEE0D6", "#3D261E"),
            ("moss", "#588461", "#8CBA94"),
            ("moss-ink", "#3F6045", "#B6D7BB"),
            ("alert", "#C24C37", "#FE836C"),
        ] {
            assert_eq!(moved.get(name, false), Some(light), "{name} light");
            assert_eq!(moved.get(name, true), Some(dark), "{name} dark");
        }
        assert_eq!(default.get("nope", false), None);
    }

    #[test]
    fn hues_read_as_explain_reads_them() {
        assert_eq!(hue(Some("400"), 1.0), 40.0);
        assert_eq!(hue(Some("-30.5"), 1.0), 329.5);
        assert_eq!(hue(Some("teal"), 1.0), 185.0);
        assert_eq!(hue(Some("1e3"), 1.0), 1.0);
        assert_eq!(hue(Some(""), 1.0), 1.0);
        assert_eq!(hue(None, 1.0), 1.0);
    }
}
