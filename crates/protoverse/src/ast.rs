use std::fmt;

/// Index into Space.cells
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct CellId(pub u32);

/// The parsed space — a flat arena of cells and attributes.
///
/// Cells and attributes are stored contiguously. Each cell references
/// its attributes via a range into `attributes`, and its children
/// via a range into `child_ids` (which itself stores CellIds).
pub struct Space {
    /// All cells, indexed by CellId
    pub cells: Vec<Cell>,
    /// All attributes, contiguous per cell
    pub attributes: Vec<Attribute>,
    /// Flat child reference array — cells index into this
    pub child_ids: Vec<CellId>,
    /// Root cell of the space
    pub root: CellId,
}

pub struct Cell {
    pub cell_type: CellType,
    /// Index of first attribute in Space.attributes
    pub first_attr: u32,
    /// Number of attributes
    pub attr_count: u16,
    /// Index of first child reference in Space.child_ids
    pub first_child: u32,
    /// Number of children
    pub child_count: u16,
    /// Parent cell
    pub parent: Option<CellId>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum CellType {
    Room,
    Space,
    Group,
    Object(ObjectType),
}

#[derive(Clone, Debug, PartialEq)]
pub enum ObjectType {
    Table,
    Chair,
    Door,
    Light,
    Custom(String),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Attribute {
    Id(String),
    Type(String),
    Name(String),
    Material(String),
    Condition(String),
    Shape(Shape),
    Width(f64),
    Depth(f64),
    Height(f64),
    Location(String),
    State(CellState),
    Position(f64, f64, f64),
    ModelUrl(String),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Shape {
    Rectangle,
    Circle,
    Square,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum CellState {
    On,
    Off,
    Sleeping,
}

// --- Display implementations ---

impl fmt::Display for ObjectType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ObjectType::Table => write!(f, "table"),
            ObjectType::Chair => write!(f, "chair"),
            ObjectType::Door => write!(f, "door"),
            ObjectType::Light => write!(f, "light"),
            ObjectType::Custom(s) => write!(f, "{}", s),
        }
    }
}

impl fmt::Display for CellType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CellType::Room => write!(f, "room"),
            CellType::Space => write!(f, "space"),
            CellType::Group => write!(f, "group"),
            CellType::Object(o) => write!(f, "{}", o),
        }
    }
}

impl fmt::Display for Shape {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Shape::Rectangle => write!(f, "rectangle"),
            Shape::Circle => write!(f, "circle"),
            Shape::Square => write!(f, "square"),
        }
    }
}

impl fmt::Display for CellState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CellState::On => write!(f, "on"),
            CellState::Off => write!(f, "off"),
            CellState::Sleeping => write!(f, "sleeping"),
        }
    }
}

// --- Space accessor methods ---

impl Space {
    pub fn cell(&self, id: CellId) -> &Cell {
        &self.cells[id.0 as usize]
    }

    pub fn children(&self, id: CellId) -> &[CellId] {
        let cell = self.cell(id);
        let start = cell.first_child as usize;
        let end = start + cell.child_count as usize;
        &self.child_ids[start..end]
    }

    pub fn attrs(&self, id: CellId) -> &[Attribute] {
        let cell = self.cell(id);
        let start = cell.first_attr as usize;
        let end = start + cell.attr_count as usize;
        &self.attributes[start..end]
    }

    pub fn name(&self, id: CellId) -> Option<&str> {
        self.attrs(id).iter().find_map(|a| match a {
            Attribute::Name(s) => Some(s.as_str()),
            _ => None,
        })
    }

    pub fn find_attr<F>(&self, id: CellId, pred: F) -> Option<&Attribute>
    where
        F: Fn(&Attribute) -> bool,
    {
        self.attrs(id).iter().find(|a| pred(a))
    }

    pub fn id_str(&self, id: CellId) -> Option<&str> {
        self.attrs(id).iter().find_map(|a| match a {
            Attribute::Id(s) => Some(s.as_str()),
            _ => None,
        })
    }

    pub fn position(&self, id: CellId) -> Option<(f64, f64, f64)> {
        self.attrs(id).iter().find_map(|a| match a {
            Attribute::Position(x, y, z) => Some((*x, *y, *z)),
            _ => None,
        })
    }

    pub fn model_url(&self, id: CellId) -> Option<&str> {
        self.attrs(id).iter().find_map(|a| match a {
            Attribute::ModelUrl(s) => Some(s.as_str()),
            _ => None,
        })
    }

    pub fn width(&self, id: CellId) -> Option<f64> {
        self.attrs(id).iter().find_map(|a| match a {
            Attribute::Width(n) => Some(*n),
            _ => None,
        })
    }

    pub fn height(&self, id: CellId) -> Option<f64> {
        self.attrs(id).iter().find_map(|a| match a {
            Attribute::Height(n) => Some(*n),
            _ => None,
        })
    }

    pub fn depth(&self, id: CellId) -> Option<f64> {
        self.attrs(id).iter().find_map(|a| match a {
            Attribute::Depth(n) => Some(*n),
            _ => None,
        })
    }

    pub fn shape(&self, id: CellId) -> Option<&Shape> {
        self.attrs(id).iter().find_map(|a| match a {
            Attribute::Shape(s) => Some(s),
            _ => None,
        })
    }
}
