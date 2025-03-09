use std::collections::HashMap;

use nostrdb::Note;

pub struct Blur<'a> {
    pub blurhash: &'a str,
    pub dimensions: Option<(u32, u32)>, // width and height in pixels
}

pub fn imeta_blurhashes<'a>(note: &'a Note) -> HashMap<&'a str, Blur<'a>> {
    let mut blurs = HashMap::new();

    for tag in note.tags() {
        let mut tag_iter = tag.into_iter();
        if tag_iter
            .next()
            .and_then(|s| s.str())
            .filter(|s| *s == "imeta")
            .is_none()
        {
            continue;
        }

        let Some((url, blur)) = find_blur(tag_iter) else {
            continue;
        };

        blurs.insert(url, blur);
    }

    blurs
}

fn find_blur(tag_iter: nostrdb::TagIter) -> Option<(&str, Blur)> {
    let mut url = None;
    let mut blurhash = None;
    let mut dims = None;

    for tag_elem in tag_iter {
        let Some(s) = tag_elem.str() else { continue };
        let mut split = s.split_whitespace();

        let Some(first) = split.next() else { continue };
        let Some(second) = split.next() else { continue };

        match first {
            "url" => url = Some(second),
            "blurhash" => blurhash = Some(second),
            "dim" => dims = Some(second),
            _ => {}
        }

        if url.is_some() && blurhash.is_some() && dims.is_some() {
            break;
        }
    }

    let url = url?;
    let blurhash = blurhash?;

    let dimensions = dims.and_then(|d| {
        let mut split = d.split('x');
        let width = split.next()?.parse::<u32>().ok()?;
        let height = split.next()?.parse::<u32>().ok()?;

        Some((width, height))
    });

    Some((
        url,
        Blur {
            blurhash,
            dimensions,
        },
    ))
}
