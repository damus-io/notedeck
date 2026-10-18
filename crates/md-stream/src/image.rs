//! Images: an `![alt](target)`'s target split into its source and title, and
//! the paragraph that holds one image and nothing else.

use crate::{InlineElement, Span};

/// An image's link target, split.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageTarget<'a> {
    /// Where the image is.
    pub src: &'a str,
    /// The `"title"` after it: a screenshot's caption.
    pub title: Option<&'a str>,
}

/// `![alt](target)`'s target (as the parser gives it, title and all) split
/// into its source and its optional `"title"` or `'title'`. A source in
/// `<…>` loses them.
pub fn split_image_target(target: &str) -> ImageTarget<'_> {
    let target = target.trim();
    let mut out = ImageTarget {
        src: target,
        title: None,
    };
    for q in ['"', '\''] {
        let Some(inner) = target.strip_suffix(q) else {
            continue;
        };
        let Some(open) = inner.rfind(q) else {
            continue;
        };
        let before = &inner[..open];
        if !before.ends_with(char::is_whitespace) {
            continue;
        }
        out = ImageTarget {
            src: before.trim_end(),
            title: Some(&inner[open + 1..]),
        };
        break;
    }
    if let Some(bare) = out.src.strip_prefix('<').and_then(|s| s.strip_suffix('>')) {
        out.src = bare;
    }
    out
}

/// The one image a paragraph holds, read out of its source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LoneImage<'a> {
    pub alt: &'a str,
    /// Its target, split ([`split_image_target`]).
    pub target: ImageTarget<'a>,
}

/// The image `inlines` (a paragraph's, parsed from `source`) holds, when it
/// holds one image and nothing else but whitespace and line breaks: a
/// screenshot standing on its own, rather than one in the middle of a
/// sentence.
pub fn lone_image<'a>(inlines: &[InlineElement], source: &'a str) -> Option<LoneImage<'a>> {
    let mut image: Option<(Span, Span)> = None;
    for inline in inlines {
        match inline {
            InlineElement::Image { alt, url } if image.is_none() => image = Some((*alt, *url)),
            InlineElement::Text(t) if t.resolve(source).trim().is_empty() => {}
            InlineElement::LineBreak => {}
            _ => return None,
        }
    }
    let (alt, url) = image?;
    Some(LoneImage {
        alt: alt.resolve(source),
        target: split_image_target(url.resolve(source)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{MdElement, ParsedMarkdown};

    #[test]
    fn split_image_target_takes_the_title_off() {
        let t = |s| split_image_target(s);
        assert_eq!(
            t("/a/b.jpg \"Deep in a chapter.\""),
            ImageTarget {
                src: "/a/b.jpg",
                title: Some("Deep in a chapter.")
            }
        );
        assert_eq!(
            t(" shot.png 'one' "),
            ImageTarget {
                src: "shot.png",
                title: Some("one")
            }
        );
        assert_eq!(
            t("<a b.png> \"t\""),
            ImageTarget {
                src: "a b.png",
                title: Some("t")
            }
        );
        // A quote glued to the path is the path's.
        assert_eq!(
            t("/a/b\"c\""),
            ImageTarget {
                src: "/a/b\"c\"",
                title: None
            }
        );
        assert_eq!(
            t("https://x/y.png"),
            ImageTarget {
                src: "https://x/y.png",
                title: None
            }
        );
    }

    /// A paragraph of one image is one, title split off; an image in a
    /// sentence, or two together, isn't.
    #[test]
    fn lone_image_stands_alone() {
        let md = ParsedMarkdown::parse(
            "![A shot](https://x/y.png \"Its caption.\")\n\n\
             See ![inline](x.png) here.\n\n\
             ![a](a.png) ![b](b.png)\n",
        );
        let found: Vec<_> = md
            .elements
            .iter()
            .map(|e| match e {
                MdElement::Paragraph(inlines) => lone_image(inlines, &md.source),
                _ => None,
            })
            .collect();
        assert_eq!(
            found,
            [
                Some(LoneImage {
                    alt: "A shot",
                    target: ImageTarget {
                        src: "https://x/y.png",
                        title: Some("Its caption.")
                    }
                }),
                None,
                None,
            ]
        );
    }
}
