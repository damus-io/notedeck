use crate::ast::*;
use crate::tokenizer::{tokenize, Token};
use std::fmt;

#[derive(Debug)]
pub struct ParseError {
    pub msg: String,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "parse error: {}", self.msg)
    }
}

impl std::error::Error for ParseError {}

/// Parse an s-expression string into a Space.
pub fn parse(input: &str) -> Result<Space, ParseError> {
    let tokens = tokenize(input).map_err(|e| ParseError {
        msg: format!("tokenization failed: {}", e),
    })?;

    let mut parser = Parser {
        tokens,
        pos: 0,
        cells: Vec::new(),
        attributes: Vec::new(),
        child_ids: Vec::new(),
    };

    let root = parser.parse_cell().ok_or_else(|| ParseError {
        msg: "failed to parse root cell".into(),
    })?;

    Ok(Space {
        cells: parser.cells,
        attributes: parser.attributes,
        child_ids: parser.child_ids,
        root,
    })
}

struct Parser<'a> {
    tokens: Vec<Token<'a>>,
    pos: usize,
    cells: Vec<Cell>,
    attributes: Vec<Attribute>,
    child_ids: Vec<CellId>,
}

#[derive(Clone)]
struct Checkpoint {
    pos: usize,
    cells_len: usize,
    attrs_len: usize,
    child_ids_len: usize,
}

impl<'a> Parser<'a> {
    fn checkpoint(&self) -> Checkpoint {
        Checkpoint {
            pos: self.pos,
            cells_len: self.cells.len(),
            attrs_len: self.attributes.len(),
            child_ids_len: self.child_ids.len(),
        }
    }

    fn restore(&mut self, cp: Checkpoint) {
        self.pos = cp.pos;
        self.cells.truncate(cp.cells_len);
        self.attributes.truncate(cp.attrs_len);
        self.child_ids.truncate(cp.child_ids_len);
    }

    fn peek(&self) -> Option<&Token<'a>> {
        self.tokens.get(self.pos)
    }

    fn eat_open(&mut self) -> bool {
        if matches!(self.peek(), Some(Token::Open)) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn eat_close(&mut self) -> bool {
        if matches!(self.peek(), Some(Token::Close)) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn eat_symbol_match(&mut self, expected: &str) -> bool {
        if let Some(Token::Symbol(s)) = self.peek() {
            if *s == expected {
                self.pos += 1;
                return true;
            }
        }
        false
    }

    fn eat_symbol(&mut self) -> Option<&'a str> {
        if let Some(Token::Symbol(s)) = self.peek() {
            let s = *s;
            self.pos += 1;
            Some(s)
        } else {
            None
        }
    }

    fn eat_string(&mut self) -> Option<&'a str> {
        if let Some(Token::Str(s)) = self.peek() {
            let s = *s;
            self.pos += 1;
            Some(s)
        } else {
            None
        }
    }

    fn eat_number(&mut self) -> Option<f64> {
        if let Some(Token::Number(s)) = self.peek() {
            if let Ok(n) = s.parse::<f64>() {
                self.pos += 1;
                return Some(n);
            }
        }
        None
    }

    fn push_cell(&mut self, cell: Cell) -> CellId {
        let id = CellId(self.cells.len() as u32);
        self.cells.push(cell);
        id
    }

    // --- Attribute parsing ---

    fn try_parse_attribute(&mut self) -> Option<Attribute> {
        let cp = self.checkpoint();

        if !self.eat_open() {
            return None;
        }

        let sym = match self.eat_symbol() {
            Some(s) => s,
            None => {
                self.restore(cp);
                return None;
            }
        };

        let result = match sym {
            "shape" => self.eat_symbol().and_then(|s| {
                let shape = match s {
                    "rectangle" => Shape::Rectangle,
                    "circle" => Shape::Circle,
                    "square" => Shape::Square,
                    _ => return None,
                };
                Some(Attribute::Shape(shape))
            }),
            "id" => self.eat_symbol().map(|s| Attribute::Id(s.to_string())),
            "name" => self.eat_string().map(|s| Attribute::Name(s.to_string())),
            "material" => self
                .eat_string()
                .map(|s| Attribute::Material(s.to_string())),
            "condition" => self
                .eat_string()
                .map(|s| Attribute::Condition(s.to_string())),
            "location" => self.eat_symbol().and_then(|s| {
                let loc = match s {
                    "center" => Location::Center,
                    "floor" => Location::Floor,
                    "ceiling" => Location::Ceiling,
                    "top-of" => {
                        let id = self.eat_symbol()?;
                        Location::TopOf(id.to_string())
                    }
                    "near" => {
                        let id = self.eat_symbol()?;
                        Location::Near(id.to_string())
                    }
                    other => Location::Custom(other.to_string()),
                };
                Some(Attribute::Location(loc))
            }),
            "state" => self.eat_symbol().and_then(|s| {
                let state = match s {
                    "on" => CellState::On,
                    "off" => CellState::Off,
                    "sleeping" => CellState::Sleeping,
                    _ => return None,
                };
                Some(Attribute::State(state))
            }),
            "type" => self.eat_symbol().map(|s| Attribute::Type(s.to_string())),
            "width" => self.eat_number().map(Attribute::Width),
            "height" => self.eat_number().map(Attribute::Height),
            "depth" => self.eat_number().map(Attribute::Depth),
            "position" => {
                let x = self.eat_number();
                let y = self.eat_number();
                let z = self.eat_number();
                match (x, y, z) {
                    (Some(x), Some(y), Some(z)) => Some(Attribute::Position(x, y, z)),
                    _ => None,
                }
            }
            "rotation" => {
                let x = self.eat_number();
                let y = self.eat_number();
                let z = self.eat_number();
                match (x, y, z) {
                    (Some(x), Some(y), Some(z)) => Some(Attribute::Rotation(x, y, z)),
                    _ => None,
                }
            }
            "model-url" => self
                .eat_string()
                .map(|s| Attribute::ModelUrl(s.to_string())),
            _ => None,
        };

        match result {
            Some(attr) => {
                if self.eat_close() {
                    Some(attr)
                } else {
                    self.restore(cp);
                    None
                }
            }
            None => {
                self.restore(cp);
                None
            }
        }
    }

    /// Parse zero or more attributes, returning the count.
    /// Attributes are pushed contiguously into self.attributes.
    fn parse_attributes(&mut self) -> u16 {
        let mut count = 0u16;
        while let Some(attr) = self.try_parse_attribute() {
            self.attributes.push(attr);
            count += 1;
        }
        count
    }

    // --- Cell parsing ---

    /// Parse attributes and an optional child cell (for room/space/object).
    fn parse_cell_attrs(&mut self, cell_type: CellType) -> Option<CellId> {
        let first_attr = self.attributes.len() as u32;
        let attr_count = self.parse_attributes();

        // Parse optional child cell — recursion may push to child_ids
        let opt_child = self.parse_cell();

        // Capture first_child AFTER recursion so nested children don't interleave
        let first_child = self.child_ids.len() as u32;
        let child_count;
        if let Some(child_id) = opt_child {
            self.child_ids.push(child_id);
            child_count = 1u16;
        } else {
            child_count = 0;
        }

        let id = self.push_cell(Cell {
            cell_type,
            first_attr,
            attr_count,
            first_child,
            child_count,
            parent: None,
        });

        // Set parent on children
        for i in 0..child_count {
            let child_id = self.child_ids[(first_child + i as u32) as usize];
            self.cells[child_id.0 as usize].parent = Some(id);
        }

        Some(id)
    }

    fn try_parse_named_cell(&mut self, name: &str, cell_type: CellType) -> Option<CellId> {
        let cp = self.checkpoint();

        if !self.eat_symbol_match(name) {
            self.restore(cp);
            return None;
        }

        match self.parse_cell_attrs(cell_type) {
            Some(id) => Some(id),
            None => {
                self.restore(cp);
                None
            }
        }
    }

    fn try_parse_room(&mut self) -> Option<CellId> {
        self.try_parse_named_cell("room", CellType::Room)
    }

    fn try_parse_space(&mut self) -> Option<CellId> {
        self.try_parse_named_cell("space", CellType::Space)
    }

    fn try_parse_group(&mut self) -> Option<CellId> {
        let cp = self.checkpoint();

        if !self.eat_symbol_match("group") {
            self.restore(cp);
            return None;
        }

        // Collect children — each parse_cell may recursively push to child_ids,
        // so we collect CellIds first and append ours after recursion completes
        let mut collected = Vec::new();
        while let Some(child_id) = self.parse_cell() {
            collected.push(child_id);
        }

        if collected.is_empty() {
            self.restore(cp);
            return None;
        }

        // Now append our children contiguously
        let first_child = self.child_ids.len() as u32;
        let child_count = collected.len() as u16;
        self.child_ids.extend_from_slice(&collected);

        let id = self.push_cell(Cell {
            cell_type: CellType::Group,
            first_attr: 0,
            attr_count: 0,
            first_child,
            child_count,
            parent: None,
        });

        // Set parent on children
        for i in 0..child_count {
            let child_id = self.child_ids[(first_child + i as u32) as usize];
            self.cells[child_id.0 as usize].parent = Some(id);
        }

        Some(id)
    }

    fn try_parse_object(&mut self) -> Option<CellId> {
        let cp = self.checkpoint();

        let sym = self.eat_symbol()?;

        let obj_type = match sym {
            "table" => ObjectType::Table,
            "chair" => ObjectType::Chair,
            "door" => ObjectType::Door,
            "light" => ObjectType::Light,
            _ => ObjectType::Custom(sym.to_string()),
        };

        match self.parse_cell_attrs(CellType::Object(obj_type)) {
            Some(id) => Some(id),
            None => {
                self.restore(cp);
                None
            }
        }
    }

    fn parse_cell(&mut self) -> Option<CellId> {
        let cp = self.checkpoint();

        if !self.eat_open() {
            return None;
        }

        // Try each cell type
        let id = self
            .try_parse_group()
            .or_else(|| self.try_parse_room())
            .or_else(|| self.try_parse_space())
            .or_else(|| self.try_parse_object());

        match id {
            Some(id) => {
                if self.eat_close() {
                    Some(id)
                } else {
                    self.restore(cp);
                    None
                }
            }
            None => {
                self.restore(cp);
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_simple_room() {
        let space = parse("(room (name \"Test Room\") (width 10))").unwrap();
        assert_eq!(space.cells.len(), 1);
        let root = space.cell(space.root);
        assert_eq!(root.cell_type, CellType::Room);
        assert_eq!(root.attr_count, 2);
        assert_eq!(space.name(space.root), Some("Test Room"));
    }

    #[test]
    fn test_parse_object_with_child() {
        let input = "(table (name \"desk\") (light (name \"lamp\")))";
        let space = parse(input).unwrap();
        // light is cell 0, table is cell 1
        assert_eq!(space.cells.len(), 2);
        let root = space.cell(space.root);
        assert_eq!(root.cell_type, CellType::Object(ObjectType::Table));
        assert_eq!(root.child_count, 1);

        let children = space.children(space.root);
        let child = space.cell(children[0]);
        assert_eq!(child.cell_type, CellType::Object(ObjectType::Light));
        assert_eq!(space.name(children[0]), Some("lamp"));
    }

    #[test]
    fn test_parse_group() {
        let input = "(room (group (table (name \"t1\")) (chair (name \"c1\"))))";
        let space = parse(input).unwrap();
        // table=0, chair=1, group=2, room=3
        assert_eq!(space.cells.len(), 4);
        let root = space.cell(space.root);
        assert_eq!(root.cell_type, CellType::Room);

        // room has one child (group)
        let room_children = space.children(space.root);
        assert_eq!(room_children.len(), 1);
        let group = space.cell(room_children[0]);
        assert_eq!(group.cell_type, CellType::Group);

        // group has two children
        let group_children = space.children(room_children[0]);
        assert_eq!(group_children.len(), 2);
        assert_eq!(
            space.cell(group_children[0]).cell_type,
            CellType::Object(ObjectType::Table)
        );
        assert_eq!(
            space.cell(group_children[1]).cell_type,
            CellType::Object(ObjectType::Chair)
        );
    }

    #[test]
    fn test_parse_location_variants() {
        // Simple locations
        let space = parse("(table (location center))").unwrap();
        assert_eq!(space.location(space.root), Some(&Location::Center));

        let space = parse("(table (location floor))").unwrap();
        assert_eq!(space.location(space.root), Some(&Location::Floor));

        let space = parse("(table (location ceiling))").unwrap();
        assert_eq!(space.location(space.root), Some(&Location::Ceiling));

        // Relational locations
        let space = parse("(prop (location top-of obj1))").unwrap();
        assert_eq!(
            space.location(space.root),
            Some(&Location::TopOf("obj1".to_string()))
        );

        let space = parse("(chair (location near desk))").unwrap();
        assert_eq!(
            space.location(space.root),
            Some(&Location::Near("desk".to_string()))
        );

        // Custom/unknown location
        let space = parse("(light (location somewhere))").unwrap();
        assert_eq!(
            space.location(space.root),
            Some(&Location::Custom("somewhere".to_string()))
        );
    }

    #[test]
    fn test_location_roundtrip() {
        use crate::serializer::serialize;

        let input = r#"(room (group (table (id obj1) (position 0 0 0)) (prop (id obj2) (location top-of obj1))))"#;
        let space1 = parse(input).unwrap();
        let serialized = serialize(&space1);
        let space2 = parse(&serialized).unwrap();

        // Find obj2 in both
        let group1 = space1.children(space1.root)[0];
        let obj2_1 = space1.children(group1)[1];
        assert_eq!(
            space1.location(obj2_1),
            Some(&Location::TopOf("obj1".to_string()))
        );

        let group2 = space2.children(space2.root)[0];
        let obj2_2 = space2.children(group2)[1];
        assert_eq!(
            space2.location(obj2_2),
            Some(&Location::TopOf("obj1".to_string()))
        );
    }
}
