//! Protoverse: S-expression parser for spatial world descriptions
//!
//! Parses protoverse `.space` format — an s-expression language for
//! describing rooms, objects, and their attributes. Designed for
//! progressive LOD: text descriptions, 2D maps, and 3D rendering
//! can all be derived from the same source.
//!
//! # Example
//!
//! ```
//! use protoverse::{parse, serialize, describe};
//!
//! let input = r#"(room (name "My Room") (shape rectangle) (width 10) (depth 8)
//!   (group
//!     (table (name "desk") (material "wood"))
//!     (chair (name "office chair"))))"#;
//!
//! let space = parse(input).unwrap();
//! let description = describe(&space);
//! let roundtrip = serialize(&space);
//! ```

pub mod ast;
pub mod describe;
pub mod parser;
pub mod serializer;
pub mod tokenizer;

pub use ast::*;
pub use describe::{describe, describe_from};
pub use parser::parse;
pub use serializer::{serialize, serialize_from};

#[cfg(test)]
mod tests {
    use super::*;

    const SATOSHIS_CITADEL: &str = r#"(space (shape rectangle)
      (condition "clean")
      (condition "shiny")
      (material "solid gold")
      (name "Satoshi's Den")
      (width 10) (depth 10) (height 100)
      (group
         (table (id welcome-desk)
                (name "welcome desk")
                (material "marble")
                (condition "clean")
                (condition "new")
                (width 1) (depth 2) (height 1)
                (location center)
                (light (name "desk")))

         (chair (id welcome-desk-chair)
                (name "fancy"))

         (chair (name "throne") (material "invisible"))

         (light (location ceiling)
                (name "ceiling")
                (state off)
                (shape circle))))"#;

    const EXAMPLE_ROOM: &str = r#"(room (shape rectangle)
          (condition "clean")
          (material "gold")
          (name "Satoshi's Den")
          (width 10) (depth 10) (height 100)
          (group
            (table (id welcome-desk)
                   (name "welcome desk")
                   (material "marble")
                   (condition "new")
                   (width 1) (depth 2) (height 1)
                   (light (name "desk")))

            (chair (id welcome-desk-chair)
                   (name "fancy"))

            (light (location ceiling)
                   (name "ceiling")
                   (state off)
                   (shape circle))))"#;

    #[test]
    fn test_parse_satoshis_citadel() {
        let space = parse(SATOSHIS_CITADEL).unwrap();

        // Root is a space cell
        let root = space.cell(space.root);
        assert_eq!(root.cell_type, CellType::Space);
        assert_eq!(space.name(space.root), Some("Satoshi's Den"));

        // Root has 8 attributes
        let attrs = space.attrs(space.root);
        assert_eq!(attrs.len(), 8);

        // Root has one child (group)
        let root_children = space.children(space.root);
        assert_eq!(root_children.len(), 1);
        let group_id = root_children[0];
        let group = space.cell(group_id);
        assert_eq!(group.cell_type, CellType::Group);

        // Group has 4 children: table, chair, chair, light
        let group_children = space.children(group_id);
        assert_eq!(group_children.len(), 4);

        assert_eq!(
            space.cell(group_children[0]).cell_type,
            CellType::Object(ObjectType::Table)
        );
        assert_eq!(
            space.cell(group_children[1]).cell_type,
            CellType::Object(ObjectType::Chair)
        );
        assert_eq!(
            space.cell(group_children[2]).cell_type,
            CellType::Object(ObjectType::Chair)
        );
        assert_eq!(
            space.cell(group_children[3]).cell_type,
            CellType::Object(ObjectType::Light)
        );

        // Table has a child light
        let table_children = space.children(group_children[0]);
        assert_eq!(table_children.len(), 1);
        assert_eq!(
            space.cell(table_children[0]).cell_type,
            CellType::Object(ObjectType::Light)
        );
        assert_eq!(space.name(table_children[0]), Some("desk"));

        // Check object names
        assert_eq!(space.name(group_children[0]), Some("welcome desk"));
        assert_eq!(space.name(group_children[1]), Some("fancy"));
        assert_eq!(space.name(group_children[2]), Some("throne"));
        assert_eq!(space.name(group_children[3]), Some("ceiling"));
    }

    #[test]
    fn test_parse_example_room() {
        let space = parse(EXAMPLE_ROOM).unwrap();
        let root = space.cell(space.root);
        assert_eq!(root.cell_type, CellType::Room);
        assert_eq!(space.name(space.root), Some("Satoshi's Den"));
    }

    #[test]
    fn test_round_trip() {
        let space1 = parse(SATOSHIS_CITADEL).unwrap();
        let serialized = serialize(&space1);

        // Re-parse the serialized output
        let space2 = parse(&serialized).unwrap();

        // Same structure
        assert_eq!(space1.cells.len(), space2.cells.len());
        assert_eq!(space1.attributes.len(), space2.attributes.len());
        assert_eq!(space1.child_ids.len(), space2.child_ids.len());

        // Same root type
        assert_eq!(
            space1.cell(space1.root).cell_type,
            space2.cell(space2.root).cell_type
        );

        // Same name
        assert_eq!(space1.name(space1.root), space2.name(space2.root));

        // Same group children count
        let g1 = space1.children(space1.root)[0];
        let g2 = space2.children(space2.root)[0];
        assert_eq!(space1.children(g1).len(), space2.children(g2).len());
    }

    #[test]
    fn test_describe_satoshis_citadel() {
        let space = parse(SATOSHIS_CITADEL).unwrap();
        let desc = describe(&space);

        // Check the area description
        assert!(desc.contains("There is a(n)"));
        assert!(desc.contains("clean"));
        assert!(desc.contains("shiny"));
        assert!(desc.contains("rectangular"));
        assert!(desc.contains("space"));
        assert!(desc.contains("made of solid gold"));
        assert!(desc.contains("named Satoshi's Den"));

        // Check the group description
        assert!(desc.contains("It contains"));
        assert!(desc.contains("four"));
        assert!(desc.contains("objects:"));
        assert!(desc.contains("welcome desk table"));
        assert!(desc.contains("fancy chair"));
        assert!(desc.contains("throne chair"));
        assert!(desc.contains("ceiling light"));

        // Exact match against C reference output
        let expected = "There is a(n) clean and shiny rectangular space made of solid gold named Satoshi's Den.\nIt contains four objects: a welcome desk table, fancy chair, throne chair and ceiling light.\n";
        assert_eq!(desc, expected);
    }

    #[test]
    fn test_parse_real_space_file() {
        // Parse the actual .space file from the protoverse repo
        let path = "/home/jb55/src/c/protoverse/satoshis-citadel.space";
        if let Ok(content) = std::fs::read_to_string(path) {
            let space = parse(&content).unwrap();
            assert_eq!(space.cell(space.root).cell_type, CellType::Space);
            assert_eq!(space.name(space.root), Some("Satoshi's Den"));

            // Verify round-trip
            let serialized = serialize(&space);
            let space2 = parse(&serialized).unwrap();
            assert_eq!(space.cells.len(), space2.cells.len());
        }
    }

    #[test]
    fn test_parent_references() {
        let space = parse(SATOSHIS_CITADEL).unwrap();

        // Root has no parent
        assert_eq!(space.cell(space.root).parent, None);

        // Group's parent is root
        let group_id = space.children(space.root)[0];
        assert_eq!(space.cell(group_id).parent, Some(space.root));

        // Table's parent is group
        let table_id = space.children(group_id)[0];
        assert_eq!(space.cell(table_id).parent, Some(group_id));

        // Desk light's parent is table
        let light_id = space.children(table_id)[0];
        assert_eq!(space.cell(light_id).parent, Some(table_id));
    }

    #[test]
    fn test_attribute_details() {
        let space = parse(SATOSHIS_CITADEL).unwrap();

        // Check root shape
        let shape = space
            .find_attr(space.root, |a| matches!(a, Attribute::Shape(_)))
            .unwrap();
        assert_eq!(*shape, Attribute::Shape(Shape::Rectangle));

        // Check root dimensions
        let width = space
            .find_attr(space.root, |a| matches!(a, Attribute::Width(_)))
            .unwrap();
        assert_eq!(*width, Attribute::Width(10.0));

        // Check table material
        let table_id = space.children(space.children(space.root)[0])[0];
        let material = space
            .find_attr(table_id, |a| matches!(a, Attribute::Material(_)))
            .unwrap();
        assert_eq!(*material, Attribute::Material("marble".to_string()));

        // Check light state
        let light_id = space.children(space.children(space.root)[0])[3];
        let state = space
            .find_attr(light_id, |a| matches!(a, Attribute::State(_)))
            .unwrap();
        assert_eq!(*state, Attribute::State(CellState::Off));
    }
}
