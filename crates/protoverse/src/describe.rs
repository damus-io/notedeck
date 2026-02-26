use crate::ast::*;

/// Generate a natural language description of a space.
pub fn describe(space: &Space) -> String {
    describe_from(space, space.root, 10)
}

/// Generate a description starting from a specific cell with depth limit.
pub fn describe_from(space: &Space, root: CellId, max_depth: usize) -> String {
    let mut buf = String::new();
    describe_cells(space, root, max_depth, 0, &mut buf);
    buf
}

fn describe_cells(space: &Space, id: CellId, max_depth: usize, depth: usize, buf: &mut String) {
    if depth > max_depth {
        return;
    }

    if !describe_cell(space, id, buf) {
        return;
    }

    buf.push_str(".\n");

    let children = space.children(id);
    if children.is_empty() {
        return;
    }

    let cell = space.cell(id);
    if matches!(cell.cell_type, CellType::Room | CellType::Space) {
        push_word(buf, "It contains");
    }

    // Recurse into first child (matches C behavior)
    describe_cells(space, children[0], max_depth, depth + 1, buf);
}

fn describe_cell(space: &Space, id: CellId, buf: &mut String) -> bool {
    let cell = space.cell(id);
    match &cell.cell_type {
        CellType::Room => describe_area(space, id, "room", buf),
        CellType::Space => describe_area(space, id, "space", buf),
        CellType::Group => describe_group(space, id, buf),
        CellType::Tilemap => false,
        CellType::Object(_) => false, // unimplemented in C reference
    }
}

fn describe_area(space: &Space, id: CellId, area_name: &str, buf: &mut String) -> bool {
    buf.push_str("There is a(n)");

    push_adjectives(space, id, buf);
    push_shape(space, id, buf);
    push_word(buf, area_name);
    push_made_of(space, id, buf);
    push_named(space, id, buf);

    true
}

fn describe_group(space: &Space, id: CellId, buf: &mut String) -> bool {
    let children = space.children(id);
    let nobjs = children.len();

    describe_amount(nobjs, buf);
    push_word(buf, "object");

    if nobjs > 1 {
        buf.push_str("s:");
    } else {
        buf.push(':');
    }

    push_word(buf, "a");

    for (i, &child_id) in children.iter().enumerate() {
        if i > 0 {
            if i == nobjs - 1 {
                push_word(buf, "and");
            } else {
                buf.push(',');
            }
        }
        describe_object_name(space, child_id, buf);
    }

    true
}

fn describe_object_name(space: &Space, id: CellId, buf: &mut String) {
    if let Some(name) = space.name(id) {
        push_word(buf, name);
    }

    let cell = space.cell(id);
    let type_str = match &cell.cell_type {
        CellType::Object(obj) => obj.to_string(),
        other => other.to_string(),
    };
    push_word(buf, &type_str);
}

fn describe_amount(n: usize, buf: &mut String) {
    let word = match n {
        1 => "a single",
        2 => "a couple",
        3 => "three",
        4 => "four",
        5 => "five",
        _ => "many",
    };
    push_word(buf, word);
}

// --- Helper functions ---

/// Push a word with automatic space separation.
/// Adds a space before the word if the previous character is not whitespace.
fn push_word(buf: &mut String, word: &str) {
    if let Some(last) = buf.as_bytes().last() {
        if !last.is_ascii_whitespace() {
            buf.push(' ');
        }
    }
    buf.push_str(word);
}

fn push_adjectives(space: &Space, id: CellId, buf: &mut String) {
    let attrs = space.attrs(id);
    let conditions: Vec<&str> = attrs
        .iter()
        .filter_map(|a| match a {
            Attribute::Condition(s) => Some(s.as_str()),
            _ => None,
        })
        .collect();

    let adj_count = conditions.len();

    for (i, cond) in conditions.iter().enumerate() {
        if i > 0 {
            if i == adj_count - 1 {
                push_word(buf, "and");
            } else {
                buf.push(',');
            }
        }
        push_word(buf, cond);
    }
}

fn push_shape(space: &Space, id: CellId, buf: &mut String) {
    let shape = space.attrs(id).iter().find_map(|a| match a {
        Attribute::Shape(s) => Some(s),
        _ => None,
    });

    if let Some(shape) = shape {
        let adj = match shape {
            Shape::Rectangle => "rectangular",
            Shape::Circle => "circular",
            Shape::Square => "square",
        };
        push_word(buf, adj);
    }
}

fn push_made_of(space: &Space, id: CellId, buf: &mut String) {
    let material = space.attrs(id).iter().find_map(|a| match a {
        Attribute::Material(s) => Some(s.as_str()),
        _ => None,
    });

    if let Some(mat) = material {
        push_word(buf, "made of");
        push_word(buf, mat);
    }
}

fn push_named(space: &Space, id: CellId, buf: &mut String) {
    if let Some(name) = space.name(id) {
        push_word(buf, "named");
        push_word(buf, name);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::parse;

    #[test]
    fn test_describe_simple_room() {
        let space =
            parse("(room (shape rectangle) (name \"Test Room\") (material \"wood\"))").unwrap();
        let desc = describe(&space);
        assert!(desc.contains("There is a(n)"));
        assert!(desc.contains("rectangular"));
        assert!(desc.contains("room"));
        assert!(desc.contains("made of wood"));
        assert!(desc.contains("named Test Room"));
    }
}
