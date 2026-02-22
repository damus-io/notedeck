//! Convert protoverse Space AST to renderer room state.

use crate::room_state::{Room, RoomObject, RoomShape};
use glam::Vec3;
use protoverse::{CellId, CellType, Shape, Space};

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

fn collect_objects(space: &Space, id: CellId, objects: &mut Vec<RoomObject>) {
    let cell = space.cell(id);

    if let CellType::Object(_) = &cell.cell_type {
        let obj_id = space.id_str(id).unwrap_or_else(|| "").to_string();

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

        let mut obj = RoomObject::new(obj_id, name, position);
        if let Some(url) = model_url {
            obj = obj.with_model_url(url);
        }
        objects.push(obj);
    }

    // Recurse into children
    for &child_id in space.children(id) {
        collect_objects(space, child_id, objects);
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

        assert_eq!(objects[1].id, "chair1");
        assert_eq!(objects[1].name, "Office Chair");
        assert_eq!(objects[1].position, Vec3::ZERO);
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
    fn test_convert_defaults() {
        let space = parse("(room)").unwrap();
        let (room, objects) = convert_space(&space);

        assert_eq!(room.name, "Untitled Room");
        assert_eq!(room.width, 20.0);
        assert_eq!(room.height, 15.0);
        assert_eq!(room.depth, 10.0);
        assert!(objects.is_empty());
    }
}
