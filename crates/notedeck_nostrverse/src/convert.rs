//! Convert protoverse Space AST to renderer room state.

use crate::room_state::{ObjectLocation, Room, RoomObject, RoomObjectType, RoomShape};
use glam::{Quat, Vec3};
use protoverse::{Attribute, Cell, CellId, CellType, Location, ObjectType, Shape, Space};

/// Convert a parsed protoverse Space into a Room and its objects.
pub fn convert_space(space: &Space) -> (Room, Vec<RoomObject>) {
    let room = extract_room(space, space.root);
    let mut objects = Vec::new();
    collect_objects(space, space.root, &mut objects);
    (room, objects)
}

fn extract_room(space: &Space, id: CellId) -> Room {
    let name = space.name(id).unwrap_or("Untitled Room").to_string();

    let shape = match space.shape(id) {
        Some(Shape::Rectangle) | Some(Shape::Square) => RoomShape::Rectangle,
        Some(Shape::Circle) => RoomShape::Circle,
        None => RoomShape::Rectangle,
    };

    let width = space.width(id).unwrap_or(20.0) as f32;
    let height = space.height(id).unwrap_or(15.0) as f32;
    let depth = space.depth(id).unwrap_or(10.0) as f32;

    Room {
        name,
        shape,
        width,
        height,
        depth,
    }
}

fn location_from_protoverse(loc: &Location) -> ObjectLocation {
    match loc {
        Location::Center => ObjectLocation::Center,
        Location::Floor => ObjectLocation::Floor,
        Location::Ceiling => ObjectLocation::Ceiling,
        Location::TopOf(id) => ObjectLocation::TopOf(id.clone()),
        Location::Near(id) => ObjectLocation::Near(id.clone()),
        Location::Custom(s) => ObjectLocation::Custom(s.clone()),
    }
}

fn location_to_protoverse(loc: &ObjectLocation) -> Location {
    match loc {
        ObjectLocation::Center => Location::Center,
        ObjectLocation::Floor => Location::Floor,
        ObjectLocation::Ceiling => Location::Ceiling,
        ObjectLocation::TopOf(id) => Location::TopOf(id.clone()),
        ObjectLocation::Near(id) => Location::Near(id.clone()),
        ObjectLocation::Custom(s) => Location::Custom(s.clone()),
    }
}

fn object_type_from_cell(obj_type: &ObjectType) -> RoomObjectType {
    match obj_type {
        ObjectType::Table => RoomObjectType::Table,
        ObjectType::Chair => RoomObjectType::Chair,
        ObjectType::Door => RoomObjectType::Door,
        ObjectType::Light => RoomObjectType::Light,
        ObjectType::Custom(s) if s == "prop" => RoomObjectType::Prop,
        ObjectType::Custom(s) => RoomObjectType::Custom(s.clone()),
    }
}

fn collect_objects(space: &Space, id: CellId, objects: &mut Vec<RoomObject>) {
    let cell = space.cell(id);

    if let CellType::Object(ref obj_type) = cell.cell_type {
        let obj_id = space.id_str(id).unwrap_or("").to_string();

        // Generate a fallback id if none specified
        let obj_id = if obj_id.is_empty() {
            format!("obj-{}", id.0)
        } else {
            obj_id
        };

        let name = space
            .name(id)
            .map(|s| s.to_string())
            .unwrap_or_else(|| cell.cell_type.to_string());

        let position = space
            .position(id)
            .map(|(x, y, z)| Vec3::new(x as f32, y as f32, z as f32))
            .unwrap_or(Vec3::ZERO);

        let model_url = space.model_url(id).map(|s| s.to_string());
        let location = space.location(id).map(location_from_protoverse);
        let rotation = space
            .rotation(id)
            .map(|(x, y, z)| {
                Quat::from_euler(
                    glam::EulerRot::YXZ,
                    (y as f32).to_radians(),
                    (x as f32).to_radians(),
                    (z as f32).to_radians(),
                )
            })
            .unwrap_or(Quat::IDENTITY);

        let mut obj = RoomObject::new(obj_id, name, position)
            .with_object_type(object_type_from_cell(obj_type));
        if let Some(url) = model_url {
            obj = obj.with_model_url(url);
        }
        if let Some(loc) = location {
            obj = obj.with_location(loc);
        }
        obj.rotation = rotation;
        objects.push(obj);
    }

    // Recurse into children
    for &child_id in space.children(id) {
        collect_objects(space, child_id, objects);
    }
}

/// Build a protoverse Space from Room and objects (reverse of convert_space).
///
/// Produces: (room (name ...) (shape ...) (width ...) (height ...) (depth ...)
///             (group <objects...>))
pub fn build_space(room: &Room, objects: &[RoomObject]) -> Space {
    let mut cells = Vec::new();
    let mut attributes = Vec::new();
    let mut child_ids = Vec::new();

    // Room attributes
    let room_attr_start = attributes.len() as u32;
    attributes.push(Attribute::Name(room.name.clone()));
    attributes.push(Attribute::Shape(match room.shape {
        RoomShape::Rectangle => Shape::Rectangle,
        RoomShape::Circle => Shape::Circle,
        RoomShape::Custom => Shape::Rectangle,
    }));
    attributes.push(Attribute::Width(room.width as f64));
    attributes.push(Attribute::Height(room.height as f64));
    attributes.push(Attribute::Depth(room.depth as f64));
    let room_attr_count = (attributes.len() as u32 - room_attr_start) as u16;

    // Room cell (index 0), child = group at index 1
    let room_child_start = child_ids.len() as u32;
    child_ids.push(CellId(1));
    cells.push(Cell {
        cell_type: CellType::Room,
        first_attr: room_attr_start,
        attr_count: room_attr_count,
        first_child: room_child_start,
        child_count: 1,
        parent: None,
    });

    // Group cell (index 1), children = objects at indices 2..
    let group_child_start = child_ids.len() as u32;
    for i in 0..objects.len() {
        child_ids.push(CellId(2 + i as u32));
    }
    cells.push(Cell {
        cell_type: CellType::Group,
        first_attr: attributes.len() as u32,
        attr_count: 0,
        first_child: group_child_start,
        child_count: objects.len() as u16,
        parent: Some(CellId(0)),
    });

    // Object cells (indices 2..)
    for obj in objects {
        let obj_attr_start = attributes.len() as u32;
        attributes.push(Attribute::Id(obj.id.clone()));
        attributes.push(Attribute::Name(obj.name.clone()));
        if let Some(url) = &obj.model_url {
            attributes.push(Attribute::ModelUrl(url.clone()));
        }
        if let Some(loc) = &obj.location {
            attributes.push(Attribute::Location(location_to_protoverse(loc)));
        }
        // When the object has a resolved location base, save the offset
        // from the base so that position remains relative to the location.
        let pos = match obj.location_base {
            Some(base) => obj.position - base,
            None => obj.position,
        };
        attributes.push(Attribute::Position(
            pos.x as f64,
            pos.y as f64,
            pos.z as f64,
        ));
        // Only emit rotation when non-identity to keep output clean
        if obj.rotation.angle_between(Quat::IDENTITY) > 1e-4 {
            let (y, x, z) = obj.rotation.to_euler(glam::EulerRot::YXZ);
            attributes.push(Attribute::Rotation(
                x.to_degrees() as f64,
                y.to_degrees() as f64,
                z.to_degrees() as f64,
            ));
        }
        let obj_attr_count = (attributes.len() as u32 - obj_attr_start) as u16;

        let obj_type = CellType::Object(match &obj.object_type {
            RoomObjectType::Table => ObjectType::Table,
            RoomObjectType::Chair => ObjectType::Chair,
            RoomObjectType::Door => ObjectType::Door,
            RoomObjectType::Light => ObjectType::Light,
            RoomObjectType::Prop => ObjectType::Custom("prop".to_string()),
            RoomObjectType::Custom(s) => ObjectType::Custom(s.clone()),
        });

        cells.push(Cell {
            cell_type: obj_type,
            first_attr: obj_attr_start,
            attr_count: obj_attr_count,
            first_child: child_ids.len() as u32,
            child_count: 0,
            parent: Some(CellId(1)),
        });
    }

    Space {
        cells,
        attributes,
        child_ids,
        root: CellId(0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use protoverse::parse;

    #[test]
    fn test_convert_simple_room() {
        let space = parse(
            r#"(room (name "Test Room") (shape rectangle) (width 10) (height 5) (depth 8)
              (group
                (table (id desk) (name "My Desk") (position 1 0 2))
                (chair (id chair1) (name "Office Chair"))))"#,
        )
        .unwrap();

        let (room, objects) = convert_space(&space);

        assert_eq!(room.name, "Test Room");
        assert_eq!(room.shape, RoomShape::Rectangle);
        assert_eq!(room.width, 10.0);
        assert_eq!(room.height, 5.0);
        assert_eq!(room.depth, 8.0);

        assert_eq!(objects.len(), 2);

        assert_eq!(objects[0].id, "desk");
        assert_eq!(objects[0].name, "My Desk");
        assert_eq!(objects[0].position, Vec3::new(1.0, 0.0, 2.0));
        assert!(matches!(objects[0].object_type, RoomObjectType::Table));

        assert_eq!(objects[1].id, "chair1");
        assert_eq!(objects[1].name, "Office Chair");
        assert_eq!(objects[1].position, Vec3::ZERO);
        assert!(matches!(objects[1].object_type, RoomObjectType::Chair));
    }

    #[test]
    fn test_convert_with_model_url() {
        let space = parse(
            r#"(room (name "Gallery")
              (group
                (table (id t1) (name "Display Table")
                       (model-url "/models/table.glb")
                       (position 0 0 0))))"#,
        )
        .unwrap();

        let (_, objects) = convert_space(&space);
        assert_eq!(objects.len(), 1);
        assert_eq!(objects[0].model_url.as_deref(), Some("/models/table.glb"));
    }

    #[test]
    fn test_convert_custom_object() {
        let space = parse(
            r#"(room (name "Test")
              (group
                (prop (id p1) (name "Water Bottle"))))"#,
        )
        .unwrap();

        let (_, objects) = convert_space(&space);
        assert_eq!(objects.len(), 1);
        assert_eq!(objects[0].id, "p1");
        assert_eq!(objects[0].name, "Water Bottle");
    }

    #[test]
    fn test_build_space_roundtrip() {
        let room = Room {
            name: "My Room".to_string(),
            shape: RoomShape::Rectangle,
            width: 15.0,
            height: 10.0,
            depth: 12.0,
        };
        let objects = vec![
            RoomObject::new(
                "desk".to_string(),
                "Office Desk".to_string(),
                Vec3::new(2.0, 0.0, 3.0),
            )
            .with_object_type(RoomObjectType::Table)
            .with_model_url("/models/desk.glb".to_string()),
            RoomObject::new("lamp".to_string(), "Floor Lamp".to_string(), Vec3::ZERO)
                .with_object_type(RoomObjectType::Light),
        ];

        let space = build_space(&room, &objects);

        // Serialize and re-parse
        let serialized = protoverse::serialize(&space);
        let reparsed = parse(&serialized).unwrap();

        // Convert back
        let (room2, objects2) = convert_space(&reparsed);

        assert_eq!(room2.name, "My Room");
        assert_eq!(room2.width, 15.0);
        assert_eq!(room2.height, 10.0);
        assert_eq!(room2.depth, 12.0);

        assert_eq!(objects2.len(), 2);
        assert_eq!(objects2[0].id, "desk");
        assert_eq!(objects2[0].name, "Office Desk");
        assert_eq!(objects2[0].model_url.as_deref(), Some("/models/desk.glb"));
        assert_eq!(objects2[0].position, Vec3::new(2.0, 0.0, 3.0));
        assert!(matches!(objects2[0].object_type, RoomObjectType::Table));

        assert_eq!(objects2[1].id, "lamp");
        assert_eq!(objects2[1].name, "Floor Lamp");
        assert!(matches!(objects2[1].object_type, RoomObjectType::Light));
    }

    #[test]
    fn test_convert_defaults() {
        let space = parse("(room)").unwrap();
        let (room, objects) = convert_space(&space);

        assert_eq!(room.name, "Untitled Room");
        assert_eq!(room.width, 20.0);
        assert_eq!(room.height, 15.0);
        assert_eq!(room.depth, 10.0);
        assert!(objects.is_empty());
    }

    #[test]
    fn test_convert_location_top_of() {
        let space = parse(
            r#"(room (group
                (table (id obj1) (name "Table") (position 0 0 0))
                (prop (id obj2) (name "Bottle") (location top-of obj1))))"#,
        )
        .unwrap();

        let (_, objects) = convert_space(&space);
        assert_eq!(objects.len(), 2);
        assert_eq!(objects[0].location, None);
        assert_eq!(
            objects[1].location,
            Some(ObjectLocation::TopOf("obj1".to_string()))
        );
    }

    #[test]
    fn test_build_space_always_emits_position() {
        let room = Room {
            name: "Test".to_string(),
            shape: RoomShape::Rectangle,
            width: 10.0,
            height: 10.0,
            depth: 10.0,
        };
        let objects = vec![RoomObject::new(
            "a".to_string(),
            "Thing".to_string(),
            Vec3::ZERO,
        )];

        let space = build_space(&room, &objects);
        let serialized = protoverse::serialize(&space);

        // Position should appear even for Vec3::ZERO
        assert!(serialized.contains("(position 0 0 0)"));
    }

    #[test]
    fn test_build_space_location_roundtrip() {
        let room = Room {
            name: "Test".to_string(),
            shape: RoomShape::Rectangle,
            width: 10.0,
            height: 10.0,
            depth: 10.0,
        };
        let objects = vec![
            RoomObject::new("obj1".to_string(), "Table".to_string(), Vec3::ZERO)
                .with_object_type(RoomObjectType::Table),
            RoomObject::new(
                "obj2".to_string(),
                "Bottle".to_string(),
                Vec3::new(0.0, 1.5, 0.0),
            )
            .with_location(ObjectLocation::TopOf("obj1".to_string())),
        ];

        let space = build_space(&room, &objects);
        let serialized = protoverse::serialize(&space);
        let reparsed = parse(&serialized).unwrap();
        let (_, objects2) = convert_space(&reparsed);

        assert_eq!(
            objects2[1].location,
            Some(ObjectLocation::TopOf("obj1".to_string()))
        );
        assert_eq!(objects2[1].position, Vec3::new(0.0, 1.5, 0.0));
    }
}
