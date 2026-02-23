use crate::ast::*;
use std::fmt::Write;

/// Serialize a Space back to s-expression format.
pub fn serialize(space: &Space) -> String {
    serialize_from(space, space.root)
}

/// Serialize a subtree starting from a specific cell.
pub fn serialize_from(space: &Space, root: CellId) -> String {
    let mut out = String::new();
    write_cell(space, root, 0, &mut out);
    out
}

fn format_number(n: f64) -> String {
    if n == n.floor() && n.abs() < i64::MAX as f64 {
        format!("{}", n as i64)
    } else {
        format!("{}", n)
    }
}

fn write_cell(space: &Space, id: CellId, indent: usize, out: &mut String) {
    let cell = space.cell(id);
    let pad = "  ".repeat(indent);
    let inner_pad = "  ".repeat(indent + 1);

    out.push('(');
    out.push_str(&cell.cell_type.to_string());

    // Attributes
    let attrs = space.attrs(id);
    for attr in attrs {
        let _ = write!(out, "\n{}", inner_pad);
        write_attr(attr, out);
    }

    // Children
    let children = space.children(id);
    for &child_id in children {
        let _ = write!(out, "\n{}", inner_pad);
        write_cell(space, child_id, indent + 1, out);
    }

    // Closing paren on same line if no attrs/children, else on new line
    if !attrs.is_empty() || !children.is_empty() {
        // For readability, close on the last line
        out.push(')');
    } else {
        out.push(')');
    }

    let _ = pad; // used above via inner_pad derivation
}

fn write_attr(attr: &Attribute, out: &mut String) {
    match attr {
        Attribute::Shape(s) => {
            let _ = write!(out, "(shape {})", s);
        }
        Attribute::Id(s) => {
            let _ = write!(out, "(id {})", s);
        }
        Attribute::Name(s) => {
            let _ = write!(out, "(name \"{}\")", s);
        }
        Attribute::Material(s) => {
            let _ = write!(out, "(material \"{}\")", s);
        }
        Attribute::Condition(s) => {
            let _ = write!(out, "(condition \"{}\")", s);
        }
        Attribute::Location(loc) => {
            let _ = write!(out, "(location {})", loc);
        }
        Attribute::State(s) => {
            let _ = write!(out, "(state {})", s);
        }
        Attribute::Type(s) => {
            let _ = write!(out, "(type {})", s);
        }
        Attribute::Width(n) => {
            let _ = write!(out, "(width {})", format_number(*n));
        }
        Attribute::Height(n) => {
            let _ = write!(out, "(height {})", format_number(*n));
        }
        Attribute::Depth(n) => {
            let _ = write!(out, "(depth {})", format_number(*n));
        }
        Attribute::Position(x, y, z) => {
            let _ = write!(
                out,
                "(position {} {} {})",
                format_number(*x),
                format_number(*y),
                format_number(*z)
            );
        }
        Attribute::ModelUrl(s) => {
            let _ = write!(out, "(model-url \"{}\")", s);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::parse;

    #[test]
    fn test_serialize_simple() {
        let space = parse("(room (name \"Test\") (width 10))").unwrap();
        let output = serialize(&space);
        assert!(output.contains("(room"));
        assert!(output.contains("(name \"Test\")"));
        assert!(output.contains("(width 10)"));
    }
}
