/// Integration tests for the synapse-idl compiler pipeline.
/// Tests the full flow: .bridge source -> parse -> layout -> codegen (all 3 languages).
use synapse_idl::ast::*;

const GAME_BRIDGE: &str = r#"
namespace game;

// 3D vector for positions and velocities
struct Vec3f {
    x: f32,
    y: f32,
    z: f32,
}

struct GameState {
    position: Vec3f,
    velocity: Vec3f,
    health: f32,
    frame_id: u64,
}

enum Command {
    MoveTo { target: Vec3f },
    Attack { target_id: u32 },
    Idle,
}

channel game_bridge {
    host_to_client: GameState,
    client_to_host: Command,
}
"#;

// ============================================================
// Full pipeline integration tests
// ============================================================

#[test]
fn test_full_pipeline_parse() {
    let schema = synapse_idl::parse(GAME_BRIDGE).unwrap();
    assert_eq!(schema.namespace, Some("game".to_string()));
    assert_eq!(schema.items.len(), 4);

    // Verify struct order
    assert!(matches!(&schema.items[0], Item::Struct(s) if s.name == "Vec3f"));
    assert!(matches!(&schema.items[1], Item::Struct(s) if s.name == "GameState"));
    assert!(matches!(&schema.items[2], Item::Enum(e) if e.name == "Command"));
    assert!(matches!(&schema.items[3], Item::Channel(c) if c.name == "game_bridge"));
}

#[test]
fn test_full_pipeline_layout() {
    let (schema, layout) = synapse_idl::compile(GAME_BRIDGE).unwrap();

    // Vec3f: 3 x f32 = 12 bytes, align 4
    let vec3f = &layout.structs["Vec3f"];
    assert_eq!(vec3f.size, 12);
    assert_eq!(vec3f.align, 4);

    // GameState: Vec3f(12) + Vec3f(12) + f32(4) + pad(4) + u64(8) = 40
    let gs = &layout.structs["GameState"];
    assert_eq!(gs.fields[0].offset, 0); // position
    assert_eq!(gs.fields[1].offset, 12); // velocity
    assert_eq!(gs.fields[2].offset, 24); // health
    assert_eq!(gs.fields[3].offset, 32); // frame_id (aligned to 8)
    assert_eq!(gs.size, 40);
    assert_eq!(gs.align, 8);

    // Command enum: tag(4) + payload(max of Vec3f=12, u32=4, 0) = 4+12 = 16
    let cmd = &layout.enums["Command"];
    assert_eq!(cmd.tag_size, 4);
    assert_eq!(cmd.max_payload_size, 12);
    assert_eq!(cmd.size, 16);
    assert_eq!(cmd.align, 4);

    // Verify channel references exist as types
    let channel = match &schema.items[3] {
        Item::Channel(c) => c,
        _ => panic!("expected channel"),
    };
    assert!(layout.structs.contains_key(&channel.entries[0].ty));
    assert!(layout.enums.contains_key(&channel.entries[1].ty));
}

#[test]
fn test_full_pipeline_rust_codegen() {
    let code = synapse_idl::generate_rust(GAME_BRIDGE).unwrap();

    // Struct generation
    assert!(code.contains("#[repr(C)]"));
    assert!(code.contains("pub struct Vec3f"));
    assert!(code.contains("pub x: f32"));
    assert!(code.contains("pub struct GameState"));
    assert!(code.contains("pub position: Vec3f"));
    assert!(code.contains("pub frame_id: u64"));

    // Enum generation
    assert!(code.contains("pub const MOVE_TO: u32 = 0"));
    assert!(code.contains("pub const ATTACK: u32 = 1"));
    assert!(code.contains("pub const IDLE: u32 = 2"));
    assert!(code.contains("pub struct CommandMoveToPayload"));
    assert!(code.contains("pub struct CommandAttackPayload"));
    assert!(code.contains("pub struct Command {"));
    assert!(code.contains("pub tag: u32"));

    // Namespace
    assert!(code.contains("Namespace: game"));

    // Channel
    assert!(code.contains("Channel: game_bridge"));
}

#[test]
fn test_full_pipeline_python_codegen() {
    let code = synapse_idl::generate_python(GAME_BRIDGE).unwrap();

    assert!(code.contains("import ctypes"));
    assert!(code.contains("class Vec3f(ctypes.Structure)"));
    assert!(code.contains("(\"x\", ctypes.c_float)"));
    assert!(code.contains("class GameState(ctypes.Structure)"));
    assert!(code.contains("(\"position\", Vec3f)"));
    assert!(code.contains("(\"frame_id\", ctypes.c_uint64)"));

    // Enum
    assert!(code.contains("COMMAND_MOVE_TO = 0"));
    assert!(code.contains("COMMAND_ATTACK = 1"));
    assert!(code.contains("COMMAND_IDLE = 2"));
    assert!(code.contains("class CommandMoveToPayload(ctypes.Structure)"));
    assert!(code.contains("class Command(ctypes.Structure)"));
}

#[test]
fn test_full_pipeline_cpp_codegen() {
    let code = synapse_idl::generate_cpp(GAME_BRIDGE).unwrap();

    assert!(code.contains("#pragma once"));
    assert!(code.contains("#include <cstdint>"));
    assert!(code.contains("namespace game {"));

    // Struct
    assert!(code.contains("struct Vec3f {"));
    assert!(code.contains("float x;"));
    assert!(code.contains("static_assert(sizeof(Vec3f) == 12"));
    assert!(code.contains("struct GameState {"));
    assert!(code.contains("static_assert(sizeof(GameState) == 40"));

    // Enum
    assert!(code.contains("enum class CommandTag : uint32_t"));
    assert!(code.contains("MoveTo = 0"));
    assert!(code.contains("Attack = 1"));
    assert!(code.contains("Idle = 2"));
    assert!(code.contains("struct CommandMoveToPayload"));
    assert!(code.contains("struct CommandAttackPayload"));
    assert!(code.contains("static_assert(sizeof(Command) == 16"));

    assert!(code.contains("} // namespace"));
}

// ============================================================
// Layout consistency: all 3 codegen targets report same sizes
// ============================================================

#[test]
fn test_layout_consistency_across_languages() {
    let (schema, layout) = synapse_idl::compile(GAME_BRIDGE).unwrap();
    let rust = synapse_idl::codegen::rust::generate(&schema, &layout);
    let cpp = synapse_idl::codegen::cpp::generate(&schema, &layout);
    let python = synapse_idl::codegen::python::generate(&schema, &layout);

    // All three should reference the same struct sizes
    for (name, sl) in &layout.structs {
        assert!(
            rust.contains(&format!("Size: {} bytes", sl.size)),
            "Rust missing size for {}",
            name
        );
        assert!(
            cpp.contains(&format!("sizeof({}) == {}", name, sl.size)),
            "C++ missing sizeof for {}",
            name
        );
        assert!(
            python.contains(&format!("size={}", sl.size)),
            "Python missing size for {}",
            name
        );
    }
}

// ============================================================
// Edge cases
// ============================================================

#[test]
fn test_empty_struct() {
    let input = "struct Empty {}";
    let (_, layout) = synapse_idl::compile(input).unwrap();
    let s = &layout.structs["Empty"];
    // Empty struct: size 0 is valid in our layout (C compilers may differ)
    assert_eq!(s.size, 0);
    assert_eq!(s.fields.len(), 0);

    // All codegen should still work
    synapse_idl::generate_rust(input).unwrap();
    synapse_idl::generate_python(input).unwrap();
    synapse_idl::generate_cpp(input).unwrap();
}

#[test]
fn test_enum_no_payloads() {
    let input = "enum Direction { North, South, East, West, }";
    let (_, layout) = synapse_idl::compile(input).unwrap();
    let e = &layout.enums["Direction"];
    assert_eq!(e.tag_size, 4);
    assert_eq!(e.max_payload_size, 0);
    assert_eq!(e.size, 4); // Just the tag
    assert_eq!(e.variants.len(), 4);
}

#[test]
fn test_enum_all_payloads() {
    let input = "enum AllPayloads { A { x: u32 }, B { y: u64 }, }";
    let (_, layout) = synapse_idl::compile(input).unwrap();
    let e = &layout.enums["AllPayloads"];
    // max payload is u64 (8 bytes), tag 4 + pad 4 + payload 8 = 16
    assert_eq!(e.size, 16);
    assert_eq!(e.align, 8);
}

#[test]
fn test_deeply_nested_structs() {
    let input = r#"
        struct A { x: u32, }
        struct B { a: A, y: u8, }
        struct C { b: B, z: u64, }
    "#;
    let (_, layout) = synapse_idl::compile(input).unwrap();

    let a = &layout.structs["A"];
    assert_eq!(a.size, 4);

    let b = &layout.structs["B"];
    // A(4) + u8(1) + pad(3) = 8
    assert_eq!(b.size, 8);
    assert_eq!(b.align, 4);

    let c = &layout.structs["C"];
    // B(8) + u64(8) = 16
    assert_eq!(c.fields[0].offset, 0);
    assert_eq!(c.fields[1].offset, 8);
    assert_eq!(c.size, 16);
    assert_eq!(c.align, 8);
}

#[test]
fn test_all_primitive_types() {
    let input = r#"
        struct AllPrimitives {
            a: u8, b: u16, c: u32, d: u64,
            e: i8, f: i16, g: i32, h: i64,
            i: f32, j: f64, k: bool,
        }
    "#;
    let (_, layout) = synapse_idl::compile(input).unwrap();
    let s = &layout.structs["AllPrimitives"];

    // Verify all fields exist
    assert_eq!(s.fields.len(), 11);
    // Largest alignment is 8 (u64, i64, f64)
    assert_eq!(s.align, 8);
}

#[test]
fn test_array_of_structs() {
    let input = r#"
        struct Vec2 { x: f32, y: f32, }
        struct Polygon { vertices: [Vec2; 4], }
    "#;
    let (_, layout) = synapse_idl::compile(input).unwrap();
    let p = &layout.structs["Polygon"];
    // Vec2 is 8 bytes, 4 of them = 32 bytes
    assert_eq!(p.fields[0].size, 32);
    assert_eq!(p.size, 32);
}

#[test]
fn test_nested_arrays() {
    let input = "struct Matrix { data: [[f32; 4]; 4], }";
    let (_, layout) = synapse_idl::compile(input).unwrap();
    let m = &layout.structs["Matrix"];
    // [f32; 4] = 16 bytes, [[f32;4]; 4] = 64 bytes
    assert_eq!(m.fields[0].size, 64);
    assert_eq!(m.size, 64);
    assert_eq!(m.align, 4);
}

#[test]
fn test_enum_with_struct_payload() {
    let input = r#"
        struct Vec3f { x: f32, y: f32, z: f32, }
        enum Event {
            Move { pos: Vec3f, speed: f32 },
            Stop,
        }
    "#;
    let (_, layout) = synapse_idl::compile(input).unwrap();
    let e = &layout.enums["Event"];
    // Move payload: Vec3f(12) + f32(4) = 16
    assert_eq!(e.variants[0].payload_size, 16);
    assert_eq!(e.max_payload_size, 16);
    // tag(4) + payload(16) = 20
    assert_eq!(e.size, 20);
    assert_eq!(e.align, 4);
}

#[test]
fn test_multiple_channels() {
    let input = r#"
        struct A { x: u32, }
        struct B { y: u32, }
        channel ch1 { send: A, recv: B, }
        channel ch2 { send: B, recv: A, }
    "#;
    let schema = synapse_idl::parse(input).unwrap();
    let channels: Vec<_> = schema
        .items
        .iter()
        .filter_map(|i| match i {
            Item::Channel(c) => Some(c),
            _ => None,
        })
        .collect();
    assert_eq!(channels.len(), 2);
    assert_eq!(channels[0].name, "ch1");
    assert_eq!(channels[1].name, "ch2");
}

#[test]
fn test_no_namespace() {
    let input = "struct A { x: u32, }";
    let schema = synapse_idl::parse(input).unwrap();
    assert_eq!(schema.namespace, None);

    // C++ should not emit namespace block
    let cpp = synapse_idl::generate_cpp(input).unwrap();
    assert!(!cpp.contains("namespace"));
}

#[test]
fn test_comments_preserved_in_source() {
    let input = r#"
        // This is a comment
        struct A {
            // Field comment
            x: u32,
        }
    "#;
    // Comments are ignored by the lexer, parsing should succeed
    let schema = synapse_idl::parse(input).unwrap();
    assert_eq!(schema.items.len(), 1);
}

// ============================================================
// Error handling tests
// ============================================================

#[test]
fn test_error_undefined_type() {
    let input = "struct A { x: NonExistent, }";
    let result = synapse_idl::compile(input);
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("unknown type"));
}

#[test]
fn test_error_forward_reference() {
    // B references A which hasn't been defined yet
    let input = r#"
        struct B { a: A, }
        struct A { x: u32, }
    "#;
    let result = synapse_idl::compile(input);
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("unknown type 'A'"));
}

#[test]
fn test_error_invalid_syntax_missing_brace() {
    let input = "struct A { x: u32, ";
    let result = synapse_idl::parse(input);
    assert!(result.is_err());
}

#[test]
fn test_error_invalid_syntax_missing_colon() {
    let input = "struct A { x u32, }";
    let result = synapse_idl::parse(input);
    assert!(result.is_err());
}

#[test]
fn test_error_unexpected_token() {
    let input = "42 struct A {}";
    let result = synapse_idl::parse(input);
    assert!(result.is_err());
}

#[test]
fn test_error_invalid_character() {
    let input = "struct A { x: u32; @ }";
    let result = synapse_idl::parse(input);
    assert!(result.is_err());
}

#[test]
fn test_error_empty_input() {
    let schema = synapse_idl::parse("").unwrap();
    assert_eq!(schema.namespace, None);
    assert!(schema.items.is_empty());
}

#[test]
fn test_error_only_comments() {
    let schema = synapse_idl::parse("// just a comment\n// another comment").unwrap();
    assert!(schema.items.is_empty());
}

// ============================================================
// Alignment padding edge cases
// ============================================================

#[test]
fn test_padding_u8_u16_u32_u64() {
    let input = "struct Padded { a: u8, b: u16, c: u32, d: u64, }";
    let (_, layout) = synapse_idl::compile(input).unwrap();
    let s = &layout.structs["Padded"];
    assert_eq!(s.fields[0].offset, 0); // u8
    assert_eq!(s.fields[1].offset, 2); // u16 at align 2
    assert_eq!(s.fields[2].offset, 4); // u32 at align 4
    assert_eq!(s.fields[3].offset, 8); // u64 at align 8
    assert_eq!(s.size, 16);
    assert_eq!(s.align, 8);
}

#[test]
fn test_padding_reverse_order() {
    let input = "struct Rev { d: u64, c: u32, b: u16, a: u8, }";
    let (_, layout) = synapse_idl::compile(input).unwrap();
    let s = &layout.structs["Rev"];
    assert_eq!(s.fields[0].offset, 0); // u64
    assert_eq!(s.fields[1].offset, 8); // u32
    assert_eq!(s.fields[2].offset, 12); // u16
    assert_eq!(s.fields[3].offset, 14); // u8
                                        // Total = 15, padded to align 8 = 16
    assert_eq!(s.size, 16);
    assert_eq!(s.align, 8);
}

#[test]
fn test_bool_alignment() {
    let input = "struct BoolTest { flag: bool, value: u32, }";
    let (_, layout) = synapse_idl::compile(input).unwrap();
    let s = &layout.structs["BoolTest"];
    assert_eq!(s.fields[0].offset, 0); // bool
    assert_eq!(s.fields[1].offset, 4); // u32 at align 4
    assert_eq!(s.size, 8);
}

#[test]
fn test_struct_of_single_u8() {
    let input = "struct Tiny { x: u8, }";
    let (_, layout) = synapse_idl::compile(input).unwrap();
    let s = &layout.structs["Tiny"];
    assert_eq!(s.size, 1);
    assert_eq!(s.align, 1);
}

#[test]
fn test_array_alignment_propagation() {
    // Array of u32 should have align 4
    let input = "struct ArrAlign { data: [u32; 3], b: u8, }";
    let (_, layout) = synapse_idl::compile(input).unwrap();
    let s = &layout.structs["ArrAlign"];
    assert_eq!(s.fields[0].offset, 0); // [u32; 3] = 12 bytes
    assert_eq!(s.fields[0].size, 12);
    assert_eq!(s.fields[1].offset, 12); // u8 at 12, no padding needed
    assert_eq!(s.size, 16); // 13 padded to align 4
    assert_eq!(s.align, 4);
}
