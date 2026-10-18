//! The figures in an explainer page, drawn in the review pane's Explainer
//! tab instead of shown as their source.
//!
//! A page's Markdown is drawn by `notedeck_ui::markdown`, and the figures
//! among its top-level elements, found once when it's parsed, by a
//! [`BlockHook`](notedeck_ui::markdown::BlockHook) that claims them. They are
//! what `explain` builds:
//!
//! - a ` ```figure ` fence, its caption after a last `---` line. A body that
//!   opens with `<svg` is a hand-drawn SVG, rasterised with resvg in the
//!   pane's light or dark theme (its `var(--…)` colours from the page's
//!   [`Palette`]); any other body is Markdown, drawn in the frame.
//! - a paragraph holding one image and nothing else: a screenshot, its
//!   `"title"` the caption, read from a `data:` URI or an absolute path on
//!   this host; a relative path says it can't show. One at a URL isn't a
//!   figure: the shared renderer draws it, as it does in any note. Only this
//!   pane reads local paths, since only its Markdown is the user's own.
//!
//! Every load runs on a thread of its own and lands in [`Figures`] as a
//! texture; a frame only looks it up.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, TryRecvError, channel};

use base64::Engine;
use egui::{ColorImage, TextureHandle, Vec2};
use md_stream::{LoneImage, MdElement, ParsedMarkdown, lone_image};
use resvg::usvg;

use crate::explain_palette::Palette;
use crate::review_images::{Decoded, MAX_IMAGE_BYTES, decode_image};
use crate::review_walk::SourceKey;

/// Longest edge a figure is rasterised at, in pixels.
const MAX_RASTER_EDGE: f32 = 2048.0;

/// A page's Markdown and the figures among its top-level elements.
pub(crate) struct Figured {
    pub md: ParsedMarkdown,
    /// In element order, one per element that is a figure.
    pub figures: Vec<Figure>,
}

impl Figured {
    /// Parse `text` and find its figures, numbering them from `next` on (and
    /// leaving `next` past the last), so every figure of a page has its own.
    pub(crate) fn parse(text: &str, next: &mut usize) -> Self {
        let md = ParsedMarkdown::parse(text);
        let figures = figures(&md, next);
        Self { md, figures }
    }

    /// The figure top-level element `element` is, if it's one.
    pub(crate) fn figure_at(&self, element: usize) -> Option<&Figure> {
        let at = self
            .figures
            .binary_search_by_key(&element, |f| f.element)
            .ok()?;
        self.figures.get(at)
    }
}

/// A figure: what it draws, and the caption under it.
pub(crate) struct Figure {
    /// The top-level element of its body it is.
    pub element: usize,
    /// Its number in the page: its load's key.
    pub index: usize,
    pub body: FigureBody,
    pub caption: Option<String>,
}

/// What a figure draws.
pub(crate) enum FigureBody {
    /// A hand-drawn `<svg>`, as written (its vars not yet substituted).
    Svg {
        source: String,
        /// Its `aria-label`, if it has one: its accessible label.
        label: Option<String>,
    },
    /// Markdown in the figure's frame.
    Markdown(Box<Figured>),
    /// An image, from a `data:` URI or a path.
    Image { alt: String, src: String },
}

/// The figures among `md`'s top-level elements, numbered from `next`.
fn figures(md: &ParsedMarkdown, next: &mut usize) -> Vec<Figure> {
    md.elements
        .iter()
        .enumerate()
        .filter_map(|(i, element)| figure_of(i, element, &md.source, next))
        .collect()
}

/// The figure `element` (top-level element `at`) is, if it is one: a
/// ` ```figure ` fence, or a paragraph holding one image and nothing else
/// whose source isn't a URL.
fn figure_of(at: usize, element: &MdElement, source: &str, next: &mut usize) -> Option<Figure> {
    match element {
        MdElement::CodeBlock(code) => {
            let lang = code.language.as_ref()?.resolve(source);
            if lang.split_whitespace().next() != Some("figure") {
                return None;
            }
            let Caption { body, caption } = split_caption(code.content.resolve(source));
            let index = take(next);
            let trimmed = body.trim();
            let body = if trimmed
                .get(..4)
                .is_some_and(|t| t.eq_ignore_ascii_case("<svg"))
            {
                FigureBody::Svg {
                    label: svg_label(trimmed),
                    source: trimmed.to_owned(),
                }
            } else {
                FigureBody::Markdown(Box::new(Figured::parse(body, next)))
            };
            Some(Figure {
                element: at,
                index,
                body,
                caption: caption.map(str::to_owned),
            })
        }
        MdElement::Paragraph(inlines) => {
            let LoneImage { alt, target } = lone_image(inlines, source)?;
            if matches!(image_source(target.src), ImageSource::Url(_)) {
                return None;
            }
            Some(Figure {
                element: at,
                index: take(next),
                body: FigureBody::Image {
                    alt: alt.to_owned(),
                    src: target.src.to_owned(),
                },
                caption: target.title.map(str::to_owned),
            })
        }
        _ => None,
    }
}

/// `*next`, moving it on.
fn take(next: &mut usize) -> usize {
    let n = *next;
    *next += 1;
    n
}

/// A block body split at its caption.
#[derive(Debug, PartialEq, Eq)]
struct Caption<'a> {
    body: &'a str,
    caption: Option<&'a str>,
}

/// `body` split at its last line holding only `---`: the caption after it,
/// trimmed (`None` when empty), as explain's `split_caption`.
fn split_caption(body: &str) -> Caption<'_> {
    let mut at = 0;
    let mut split = None;
    for line in body.split_inclusive('\n') {
        if line.trim_end_matches(['\n', '\r']) == "---" {
            split = Some(Caption {
                body: &body[..at],
                caption: Some(body[at + line.len()..].trim()).filter(|c| !c.is_empty()),
            });
        }
        at += line.len();
    }
    split.unwrap_or(Caption {
        body,
        caption: None,
    })
}

/// The `aria-label` on an SVG's opening tag.
fn svg_label(svg: &str) -> Option<String> {
    let tag = &svg[..svg.find('>')?];
    let at = tag.find("aria-label=")? + "aria-label=".len();
    let q = tag[at..]
        .chars()
        .next()
        .filter(|c| matches!(c, '"' | '\''))?;
    let value = &tag[at + 1..];
    Some(value[..value.find(q)?].to_owned())
}

/// `svg` with every `var(--name)` and `var(--name, fallback)` replaced for
/// the dark or light theme: a palette token's hex, `--t…` as brass, the font
/// vars as notedeck's bundled faces; an unknown var takes its fallback, else
/// ink. `currentColor` is ink too.
pub(crate) fn substitute_vars(svg: &str, palette: &Palette, dark: bool) -> String {
    let ink = palette.get("ink", dark).unwrap_or("#000000");
    let mut out = String::with_capacity(svg.len());
    let mut rest = svg;
    while let Some(at) = rest.find("var(") {
        out.push_str(&rest[..at]);
        let after = &rest[at + 4..];
        let Some(close) = matching_paren(after) else {
            out.push_str(&rest[at..]);
            rest = "";
            break;
        };
        let inner = &after[..close];
        let (name, fallback) = match inner.split_once(',') {
            Some((n, f)) => (n.trim(), Some(f.trim())),
            None => (inner.trim(), None),
        };
        let name = name.strip_prefix("--").unwrap_or(name);
        match var_value(name, palette, dark) {
            Some(value) => out.push_str(value),
            None => match fallback {
                Some(f) => out.push_str(&substitute_vars(f, palette, dark)),
                None => out.push_str(ink),
            },
        }
        rest = &after[close + 1..];
    }
    out.push_str(rest);
    out.replace("currentColor", ink)
}

/// The byte offset of the `)` closing an already-opened `(` in `s`.
fn matching_paren(s: &str) -> Option<usize> {
    let mut depth = 0usize;
    for (i, c) in s.char_indices() {
        match c {
            '(' => depth += 1,
            ')' if depth == 0 => return Some(i),
            ')' => depth -= 1,
            _ => {}
        }
    }
    None
}

/// What explain's `--name` is in the pane.
fn var_value<'a>(name: &str, palette: &'a Palette, dark: bool) -> Option<&'a str> {
    match name {
        "mono" => Some("Inconsolata, monospace"),
        "body" | "disp" => Some("DejaVu Sans, sans-serif"),
        "t" => palette.get("brass", dark),
        "t-soft" => palette.get("brass-soft", dark),
        "t-ink" => palette.get("brass-ink", dark),
        _ => palette.get(name, dark),
    }
}

/// A font database holding only notedeck's bundled Inconsolata (monospace)
/// and DejaVu Sans (sans-serif and serif): figures draw the same text on
/// every host.
pub(crate) fn figure_fonts() -> Arc<usvg::fontdb::Database> {
    let mut db = usvg::fontdb::Database::new();
    for face in [notedeck::fonts::INCONSOLATA, notedeck::fonts::DEJAVU_SANS] {
        db.load_font_source(usvg::fontdb::Source::Binary(Arc::new(face)));
    }
    db.set_monospace_family("Inconsolata");
    db.set_sans_serif_family("DejaVu Sans");
    db.set_serif_family("DejaVu Sans");
    Arc::new(db)
}

/// A figure rasterised or decoded, ready to upload.
pub(crate) struct Raster {
    pub image: ColorImage,
    /// Its intrinsic size in points: the SVG's, or the image file's pixels.
    pub size: Vec2,
}

/// `svg` (its vars already substituted) drawn at its intrinsic size times
/// `pixels_per_point`, its longest edge capped at [`MAX_RASTER_EDGE`].
#[profiling::function]
pub(crate) fn rasterize_svg(
    svg: &str,
    fonts: &Arc<usvg::fontdb::Database>,
    pixels_per_point: f32,
) -> Result<Raster, Failure> {
    let options = usvg::Options {
        font_family: "DejaVu Sans".to_owned(),
        fontdb: Arc::clone(fonts),
        ..Default::default()
    };
    let tree = usvg::Tree::from_str(svg, &options).map_err(|e| Failure::Svg(e.to_string()))?;
    let size = tree.size();
    let longest = size.width().max(size.height()) * pixels_per_point;
    let scale = pixels_per_point * (MAX_RASTER_EDGE / longest.max(1.0)).min(1.0);
    let w = (size.width() * scale).ceil() as u32;
    let h = (size.height() * scale).ceil() as u32;
    let mut pixmap = resvg::tiny_skia::Pixmap::new(w, h)
        .ok_or_else(|| Failure::Svg(format!("empty drawing ({w}×{h})")))?;
    resvg::render(
        &tree,
        resvg::tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    Ok(Raster {
        image: ColorImage::from_rgba_premultiplied([w as usize, h as usize], pixmap.data()),
        size: Vec2::new(size.width(), size.height()),
    })
}

/// Why a figure can't show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Failure {
    /// A relative path, or an absolute one this host hasn't got.
    NotOnHost,
    /// Bigger than [`MAX_IMAGE_BYTES`]; its size in bytes.
    TooLarge(u64),
    /// Read, but not an image notedeck decodes, or not read at all.
    Unreadable,
    /// An SVG resvg can't parse, and why.
    Svg(String),
}

impl Failure {
    /// The placeholder's words for it.
    pub(crate) fn reason(&self) -> String {
        match self {
            Self::NotOnHost => "not on this host".to_owned(),
            Self::TooLarge(bytes) => {
                format!("too large ({:.1} MB)", *bytes as f64 / (1024.0 * 1024.0))
            }
            Self::Unreadable => "unreadable".to_owned(),
            Self::Svg(e) => format!("SVG didn't parse: {e}"),
        }
    }
}

/// Where an image's `src` points, as the pane reads it.
#[derive(Debug, PartialEq, Eq)]
enum ImageSource<'a> {
    Data(&'a str),
    Path(&'a Path),
    Url(&'a str),
    /// A relative path: not anywhere this host can look.
    Elsewhere,
}

/// What `src` is.
fn image_source(src: &str) -> ImageSource<'_> {
    if src.starts_with("data:") {
        return ImageSource::Data(src);
    }
    if src.starts_with("https://") || src.starts_with("http://") {
        return ImageSource::Url(src);
    }
    let path = Path::new(src.strip_prefix("file://").unwrap_or(src));
    if path.is_absolute() {
        ImageSource::Path(path)
    } else {
        ImageSource::Elsewhere
    }
}

/// The theme an SVG is drawn in.
#[derive(Clone)]
struct SvgTheme {
    palette: Palette,
    dark: bool,
    pixels_per_point: f32,
    fonts: Arc<usvg::fontdb::Database>,
}

impl SvgTheme {
    fn rasterize(&self, svg: &str) -> Result<Raster, Failure> {
        let themed = substitute_vars(svg, &self.palette, self.dark);
        rasterize_svg(&themed, &self.fonts, self.pixels_per_point)
    }
}

/// A load a worker runs.
enum Job {
    Svg(String),
    Data(String),
    File(PathBuf),
}

impl Job {
    /// Run it: read, decode or rasterise. Worker thread only.
    fn run(self, theme: &SvgTheme) -> Result<Raster, Failure> {
        match self {
            Job::Svg(svg) => theme.rasterize(&svg),
            Job::Data(uri) => load_data_uri(&uri, theme),
            Job::File(path) => load_file(&path, MAX_IMAGE_BYTES, theme),
        }
    }
}

/// A `data:` URI's image: base64 or percent-encoded, an SVG rasterised.
fn load_data_uri(uri: &str, theme: &SvgTheme) -> Result<Raster, Failure> {
    let rest = uri.strip_prefix("data:").ok_or(Failure::Unreadable)?;
    let (meta, payload) = rest.split_once(',').ok_or(Failure::Unreadable)?;
    let mut parts = meta.split(';');
    let mime = parts.next().unwrap_or("").trim();
    let bytes = if parts.any(|p| p.trim() == "base64") {
        let clean: String = payload.chars().filter(|c| !c.is_whitespace()).collect();
        base64::engine::general_purpose::STANDARD
            .decode(clean)
            .map_err(|_| Failure::Unreadable)?
    } else {
        percent_decode(payload)
    };
    if bytes.len() as u64 > MAX_IMAGE_BYTES {
        return Err(Failure::TooLarge(bytes.len() as u64));
    }
    if mime.eq_ignore_ascii_case("image/svg+xml") {
        let svg = std::str::from_utf8(&bytes).map_err(|_| Failure::Unreadable)?;
        return theme.rasterize(svg);
    }
    decoded(&bytes)
}

/// `s` with its `%XX` escapes decoded.
fn percent_decode(s: &str) -> Vec<u8> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = bytes
            .get(i + 1..i + 3)
            .and_then(|h| std::str::from_utf8(h).ok())
            .and_then(|h| u8::from_str_radix(h, 16).ok());
        match (bytes[i], hex) {
            (b'%', Some(b)) => {
                out.push(b);
                i += 3;
            }
            (b, _) => {
                out.push(b);
                i += 1;
            }
        }
    }
    out
}

/// The image at `path`, read if it's at most `cap` bytes; an `.svg`
/// rasterised.
fn load_file(path: &Path, cap: u64, theme: &SvgTheme) -> Result<Raster, Failure> {
    let meta = std::fs::metadata(path).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => Failure::NotOnHost,
        _ => Failure::Unreadable,
    })?;
    if meta.len() > cap {
        return Err(Failure::TooLarge(meta.len()));
    }
    let bytes = std::fs::read(path).map_err(|_| Failure::Unreadable)?;
    if crate::review_images::has_extension(&path.to_string_lossy(), &["svg"]) {
        let svg = std::str::from_utf8(&bytes).map_err(|_| Failure::Unreadable)?;
        return theme.rasterize(svg);
    }
    decoded(&bytes)
}

/// `bytes` decoded as a raster image, one pixel to a point.
fn decoded(bytes: &[u8]) -> Result<Raster, Failure> {
    let Decoded {
        image,
        width,
        height,
    } = decode_image(bytes).ok_or(Failure::Unreadable)?;
    Ok(Raster {
        image,
        size: Vec2::new(width as f32, height as f32),
    })
}

/// A figure's load, keyed by its index and theme.
#[derive(Clone, Copy, Hash, PartialEq, Eq)]
struct FigureKey {
    index: usize,
    dark: bool,
}

/// Where a figure's load is.
enum FigureLoad {
    Pending(Receiver<Result<Raster, Failure>>),
    Ready { texture: TextureHandle, size: Vec2 },
    Failed(Failure),
}

/// One page's figure loads and textures, kept across frames and dropped with
/// the page (so a page key needn't be in each key).
pub(crate) struct Figures {
    /// What the page was parsed from: names its textures.
    page: SourceKey,
    /// The font database SVGs draw with, built on the first one.
    fonts: Option<Arc<usvg::fontdb::Database>>,
    loads: HashMap<FigureKey, FigureLoad>,
}

impl Figures {
    pub(crate) fn new(page: SourceKey) -> Self {
        Self {
            page,
            fonts: None,
            loads: HashMap::new(),
        }
    }

    /// Figure `index`'s load in the `dark` or light theme, started with
    /// `job` on a thread of its own the first time it's asked for, and
    /// uploaded once it lands.
    fn load(
        &mut self,
        ctx: &egui::Context,
        key: FigureKey,
        palette: &Palette,
        job: impl FnOnce() -> Job,
    ) -> &FigureLoad {
        let page = self.page;
        if !self.loads.contains_key(&key) {
            let fonts = Arc::clone(self.fonts.get_or_insert_with(figure_fonts));
            let theme = SvgTheme {
                palette: palette.clone(),
                dark: key.dark,
                pixels_per_point: ctx.pixels_per_point(),
                fonts,
            };
            let (tx, rx) = channel();
            let job = job();
            let repaint = ctx.clone();
            let spawned = std::thread::Builder::new()
                .name("explainer-figure".into())
                .spawn(move || {
                    let _ = tx.send(job.run(&theme));
                    repaint.request_repaint();
                });
            let load = match spawned {
                Ok(_) => FigureLoad::Pending(rx),
                Err(e) => {
                    tracing::warn!("spawning an explainer figure's load: {e}");
                    FigureLoad::Failed(Failure::Unreadable)
                }
            };
            self.loads.insert(key, load);
        }
        let load = self.loads.get_mut(&key).expect("inserted above");
        if let FigureLoad::Pending(rx) = load {
            match rx.try_recv() {
                Ok(Ok(Raster { image, size })) => {
                    let name = format!("explainer-figure:{page}:{}:{}", key.index, key.dark);
                    let texture =
                        notedeck::media::load_texture_checked(ctx, name, image, Default::default());
                    *load = FigureLoad::Ready { texture, size };
                }
                Ok(Err(failure)) => *load = FigureLoad::Failed(failure),
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => *load = FigureLoad::Failed(Failure::Unreadable),
            }
        }
        load
    }
}

/// What a figure's body resolves to this frame.
pub(crate) enum FigureView<'a> {
    /// Still loading.
    Pending,
    Ready {
        texture: &'a TextureHandle,
        size: Vec2,
    },
    Failed(Failure),
}

impl Figures {
    /// Figure `index`, an SVG, in the `dark` or light theme.
    pub(crate) fn svg(
        &mut self,
        ctx: &egui::Context,
        palette: &Palette,
        index: usize,
        dark: bool,
        source: &str,
    ) -> FigureView<'_> {
        let key = FigureKey { index, dark };
        view(self.load(ctx, key, palette, || Job::Svg(source.to_owned())))
    }

    /// Figure `index`, an image at `src`: a `data:` URI or a path.
    pub(crate) fn image(
        &mut self,
        ctx: &egui::Context,
        palette: &Palette,
        index: usize,
        dark: bool,
        src: &str,
    ) -> FigureView<'_> {
        let job = match image_source(src) {
            // A URL is never a figure ([`figure_of`]): it isn't one to read
            // off this host.
            ImageSource::Url(_) | ImageSource::Elsewhere => {
                return FigureView::Failed(Failure::NotOnHost);
            }
            ImageSource::Data(uri) => Job::Data(uri.to_owned()),
            ImageSource::Path(path) => Job::File(path.to_owned()),
        };
        // A raster image looks the same in both themes, but a data: or
        // path SVG doesn't, so every image is keyed by theme.
        let key = FigureKey { index, dark };
        view(self.load(ctx, key, palette, || job))
    }
}

fn view(load: &FigureLoad) -> FigureView<'_> {
    match load {
        FigureLoad::Pending(_) => FigureView::Pending,
        FigureLoad::Ready { texture, size } => FigureView::Ready {
            texture,
            size: *size,
        },
        FigureLoad::Failed(f) => FigureView::Failed(f.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn theme(dark: bool) -> SvgTheme {
        SvgTheme {
            palette: Palette::default(),
            dark,
            pixels_per_point: 1.0,
            fonts: figure_fonts(),
        }
    }

    #[test]
    fn split_caption_takes_the_last_rule() {
        assert_eq!(
            split_caption("a\n---\nb\n---\n  cap \n"),
            Caption {
                body: "a\n---\nb\n",
                caption: Some("cap")
            }
        );
        assert_eq!(
            split_caption("a\n---\n\n"),
            Caption {
                body: "a\n",
                caption: None
            }
        );
        assert_eq!(
            split_caption("a --- b\n"),
            Caption {
                body: "a --- b\n",
                caption: None
            }
        );
    }

    /// The figures among a body's elements: an SVG fence (its caption and
    /// label), a Markdown fence (its own image numbered after it), a lone
    /// image (its title the caption). A flow fence, an image mid-sentence and
    /// a lone image at a URL aren't: the shared renderer draws them.
    #[test]
    fn figures_are_found_by_element() {
        let text = "\
Intro.

```figure
<svg viewBox=\"0 0 10 10\" aria-label=\"a box\">
  <g/>

  <g/>
</svg>
---
The caption.
```

```flow
a -> b
```

See ![inline](x.png) here.

```figure id=f
Some **markdown**.

![inner](/i.png)
```

![A shot](/abs/shot.jpg \"Its caption.\")

![Hosted](https://example.com/shot.png \"On the web.\")

Outro.
";
        let mut next = 0;
        let doc = Figured::parse(text, &mut next);
        assert_eq!(next, 4);
        let shape: Vec<String> = doc
            .figures
            .iter()
            .map(|f| match &f.body {
                FigureBody::Svg { source, label } => format!(
                    "@{} svg #{} {:?} {:?} blank={}",
                    f.element,
                    f.index,
                    label,
                    f.caption,
                    source.contains("\n\n")
                ),
                FigureBody::Markdown(inner) => format!(
                    "@{} md-fig #{} {:?} inner={:?}",
                    f.element,
                    f.index,
                    f.caption,
                    inner.figures.iter().map(|f| f.element).collect::<Vec<_>>()
                ),
                FigureBody::Image { alt, src } => {
                    format!(
                        "@{} img #{} {alt} {src} {:?}",
                        f.element, f.index, f.caption
                    )
                }
            })
            .collect();
        assert_eq!(
            shape,
            [
                "@1 svg #0 Some(\"a box\") Some(\"The caption.\") blank=true",
                "@4 md-fig #1 None inner=[1]",
                "@5 img #3 A shot /abs/shot.jpg Some(\"Its caption.\")",
            ]
        );
        assert_eq!(doc.md.elements.len(), 8);
        assert!(doc.figure_at(5).is_some());
        assert!(doc.figure_at(6).is_none(), "the https image");
    }

    #[test]
    fn substitute_vars_in_light_and_dark() {
        let p = Palette::default();
        let svg = "<g fill=\"var(--moss-ink)\" stroke=\"var(--t)\" font-family=\"var(--mono)\">\
                   <text fill=\"var(--nope, var(--alert))\" stroke=\"var(--gone)\" color=\"currentColor\"/>";
        assert_eq!(
            substitute_vars(svg, &p, false),
            "<g fill=\"#455F40\" stroke=\"#A76603\" font-family=\"Inconsolata, monospace\">\
             <text fill=\"#C24C37\" stroke=\"#1B1E29\" color=\"#1B1E29\"/>"
        );
        assert_eq!(
            substitute_vars(svg, &p, true),
            "<g fill=\"#BBD6B6\" stroke=\"#E19C4E\" font-family=\"Inconsolata, monospace\">\
             <text fill=\"#FE836C\" stroke=\"#ECEFF7\" color=\"#ECEFF7\"/>"
        );
        assert_eq!(substitute_vars("var(--ink", &p, false), "var(--ink");
    }

    /// An SVG of only text draws pixels, with the bundled faces.
    #[test]
    fn rasterize_svg_draws_text() {
        let svg = "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 120 30\">\
                   <text x=\"2\" y=\"20\" font-family=\"JetBrains Mono, monospace\" font-size=\"14\">hello</text></svg>";
        let raster = theme(false).rasterize(svg).expect("rasterised");
        assert_eq!(raster.size, Vec2::new(120.0, 30.0));
        assert_eq!(raster.image.size, [120, 30]);
        assert!(raster.image.pixels.iter().any(|p| p.a() > 0));

        let big = rasterize_svg(
            "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"4000\" height=\"100\"/>",
            &figure_fonts(),
            2.0,
        )
        .expect("rasterised");
        assert_eq!(big.image.size, [2048, 52]);
    }

    /// Each reason a figure can't show.
    #[test]
    fn placeholder_reasons() {
        let theme = theme(true);
        assert_eq!(image_source("shot.png"), ImageSource::Elsewhere);
        assert_eq!(
            image_source("file:///a/b.png"),
            ImageSource::Path(Path::new("/a/b.png"))
        );
        assert_eq!(
            load_file(
                Path::new("/nowhere/at/all/shot.jpg"),
                MAX_IMAGE_BYTES,
                &theme
            )
            .err(),
            Some(Failure::NotOnHost)
        );
        let dir = tempfile::tempdir().expect("tempdir");
        let junk = dir.path().join("junk.png");
        std::fs::write(&junk, b"not a png at all").expect("write");
        assert_eq!(
            load_file(&junk, MAX_IMAGE_BYTES, &theme).err(),
            Some(Failure::Unreadable)
        );
        assert_eq!(
            load_file(&junk, 4, &theme).err(),
            Some(Failure::TooLarge(16))
        );
        let bad = dir.path().join("bad.svg");
        std::fs::write(&bad, "<svg").expect("write");
        assert!(matches!(
            load_file(&bad, MAX_IMAGE_BYTES, &theme),
            Err(Failure::Svg(_))
        ));
        assert_eq!(Failure::NotOnHost.reason(), "not on this host");
        assert_eq!(Failure::TooLarge(3 << 20).reason(), "too large (3.0 MB)");
        assert_eq!(Failure::Unreadable.reason(), "unreadable");
    }

    /// A data: URI's SVG (base64 or percent-encoded) draws, its vars themed.
    #[test]
    fn data_uris_load() {
        let svg = "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"4\" height=\"2\"><rect width=\"4\" height=\"2\" fill=\"var(--ink)\"/></svg>";
        let b64 = base64::engine::general_purpose::STANDARD.encode(svg);
        let uri = format!("data:image/svg+xml;base64,{b64}");
        let dark = load_data_uri(&uri, &theme(true)).expect("loaded");
        assert_eq!(dark.image.size, [4, 2]);
        // Dark ink is near white.
        assert!(dark.image.pixels[0].r() > 200);
        let plain = format!(
            "data:image/svg+xml,{}",
            svg.replace('#', "%23").replace('"', "%22")
        );
        let light = load_data_uri(&plain, &theme(false)).expect("loaded");
        assert!(light.image.pixels[0].r() < 60);
        assert_eq!(
            load_data_uri("data:image/png;base64,!!!", &theme(false)).err(),
            Some(Failure::Unreadable)
        );
    }
}
