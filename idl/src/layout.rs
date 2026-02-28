//! Layout computation for .bridge types using C ABI alignment rules.
//!
//! Rules:
//! - Each field is aligned to its natural alignment
//! - Struct alignment is the max alignment of its fields
//! - Struct size is padded to a multiple of its alignment
//! - Arrays: element alignment, total size = element_size * count
//! - Enums: tag (u32) + largest variant payload, padded to max alignment

use crate::ast::*;
use std::collections::HashMap;

/// Computed layout for a single field within a struct.
#[derive(Debug, Clone)]
pub struct FieldLayout {
    pub name: String,
    pub offset: usize,
    pub size: usize,
    pub align: usize,
    pub ty: Type,
}

/// Computed layout for a struct.
#[derive(Debug, Clone)]
pub struct StructLayout {
    pub name: String,
    pub fields: Vec<FieldLayout>,
    pub size: usize,
    pub align: usize,
}

/// Computed layout for an enum variant's payload.
#[derive(Debug, Clone)]
pub struct VariantLayout {
    pub name: String,
    pub fields: Vec<FieldLayout>,
    pub payload_size: usize,
    pub payload_align: usize,
}

/// Computed layout for an enum.
#[derive(Debug, Clone)]
pub struct EnumLayout {
    pub name: String,
    pub tag_size: usize,
    pub tag_align: usize,
    pub variants: Vec<VariantLayout>,
    pub max_payload_size: usize,
    pub max_payload_align: usize,
    pub size: usize,
    pub align: usize,
}

/// Computed layout for an entire schema.
#[derive(Debug, Clone)]
pub struct SchemaLayout {
    pub structs: HashMap<String, StructLayout>,
    pub enums: HashMap<String, EnumLayout>,
}

/// Compute layouts for all types in a schema.
pub fn compute_layout(schema: &Schema) -> Result<SchemaLayout, String> {
    let mut structs = HashMap::new();
    let mut enums = HashMap::new();

    // First pass: collect all type definitions so we can resolve references.
    // We process in order since forward references within the same file
    // require all earlier types to be laid out first.
    for item in &schema.items {
        match item {
            Item::Struct(def) => {
                let layout = compute_struct_layout(def, &structs, &enums)?;
                structs.insert(def.name.clone(), layout);
            }
            Item::Enum(def) => {
                let layout = compute_enum_layout(def, &structs, &enums)?;
                enums.insert(def.name.clone(), layout);
            }
            Item::Channel(_) => {} // Channels don't have their own layout
        }
    }

    Ok(SchemaLayout { structs, enums })
}

fn type_size_align(
    ty: &Type,
    structs: &HashMap<String, StructLayout>,
    enums: &HashMap<String, EnumLayout>,
) -> Result<(usize, usize), String> {
    match ty {
        Type::Primitive(p) => Ok((p.size(), p.align())),
        Type::Array(inner, count) => {
            let (elem_size, elem_align) = type_size_align(inner, structs, enums)?;
            Ok((elem_size * count, elem_align))
        }
        Type::Named(name) => {
            if let Some(s) = structs.get(name) {
                Ok((s.size, s.align))
            } else if let Some(e) = enums.get(name) {
                Ok((e.size, e.align))
            } else {
                Err(format!("unknown type '{}'", name))
            }
        }
    }
}

fn compute_struct_layout(
    def: &StructDef,
    structs: &HashMap<String, StructLayout>,
    enums: &HashMap<String, EnumLayout>,
) -> Result<StructLayout, String> {
    let mut fields = Vec::new();
    let mut offset = 0usize;
    let mut max_align = 1usize;

    for field in &def.fields {
        let (size, align) = type_size_align(&field.ty, structs, enums)?;
        // Align offset
        offset = align_up(offset, align);
        max_align = max_align.max(align);
        fields.push(FieldLayout {
            name: field.name.clone(),
            offset,
            size,
            align,
            ty: field.ty.clone(),
        });
        offset += size;
    }

    // Pad struct to alignment
    let total_size = align_up(offset, max_align);

    Ok(StructLayout {
        name: def.name.clone(),
        fields,
        size: total_size,
        align: max_align,
    })
}

fn compute_enum_layout(
    def: &EnumDef,
    structs: &HashMap<String, StructLayout>,
    enums: &HashMap<String, EnumLayout>,
) -> Result<EnumLayout, String> {
    let tag_size = 4usize; // u32 tag
    let tag_align = 4usize;

    let mut variants = Vec::new();
    let mut max_payload_size = 0usize;
    let mut max_payload_align = 1usize;

    for variant in &def.variants {
        let mut vfields = Vec::new();
        let mut voffset = 0usize;
        let mut valign = 1usize;

        for field in &variant.fields {
            let (size, align) = type_size_align(&field.ty, structs, enums)?;
            voffset = align_up(voffset, align);
            valign = valign.max(align);
            vfields.push(FieldLayout {
                name: field.name.clone(),
                offset: voffset,
                size,
                align,
                ty: field.ty.clone(),
            });
            voffset += size;
        }

        let payload_size = align_up(voffset, valign);
        max_payload_size = max_payload_size.max(payload_size);
        max_payload_align = max_payload_align.max(valign);

        variants.push(VariantLayout {
            name: variant.name.clone(),
            fields: vfields,
            payload_size,
            payload_align: valign,
        });
    }

    // Enum layout: [tag (4 bytes)] [padding to payload align] [payload (max_payload_size)]
    // Total alignment = max(tag_align, max_payload_align)
    let total_align = tag_align.max(max_payload_align);
    let payload_offset = align_up(tag_size, max_payload_align);
    let total_size = align_up(payload_offset + max_payload_size, total_align);

    Ok(EnumLayout {
        name: def.name.clone(),
        tag_size,
        tag_align,
        variants,
        max_payload_size,
        max_payload_align,
        size: total_size,
        align: total_align,
    })
}

fn align_up(offset: usize, align: usize) -> usize {
    (offset + align - 1) & !(align - 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::Lexer;
    use crate::parser::Parser;

    fn layout_from(input: &str) -> SchemaLayout {
        let mut lexer = Lexer::new(input);
        let tokens = lexer.tokenize().unwrap();
        let mut parser = Parser::new(tokens);
        let schema = parser.parse().unwrap();
        compute_layout(&schema).unwrap()
    }

    #[test]
    fn test_vec3f_layout() {
        let layout = layout_from("struct Vec3f { x: f32, y: f32, z: f32, }");
        let s = &layout.structs["Vec3f"];
        assert_eq!(s.size, 12); // 3 * 4 bytes, align 4
        assert_eq!(s.align, 4);
        assert_eq!(s.fields[0].offset, 0);
        assert_eq!(s.fields[1].offset, 4);
        assert_eq!(s.fields[2].offset, 8);
    }

    #[test]
    fn test_mixed_alignment() {
        // u8 (1) + padding(3) + u32 (4) = 8, align 4
        let layout = layout_from("struct Mixed { a: u8, b: u32, }");
        let s = &layout.structs["Mixed"];
        assert_eq!(s.fields[0].offset, 0);
        assert_eq!(s.fields[1].offset, 4);
        assert_eq!(s.size, 8);
        assert_eq!(s.align, 4);
    }

    #[test]
    fn test_trailing_padding() {
        // u32 (4) + u8 (1) + padding(3) = 8 (padded to align 4)
        let layout = layout_from("struct Padded { a: u32, b: u8, }");
        let s = &layout.structs["Padded"];
        assert_eq!(s.fields[0].offset, 0);
        assert_eq!(s.fields[1].offset, 4);
        assert_eq!(s.size, 8); // padded to align(4)
        assert_eq!(s.align, 4);
    }

    #[test]
    fn test_u64_alignment() {
        // u8 (1) + pad(7) + u64 (8) = 16
        let layout = layout_from("struct Big { a: u8, b: u64, }");
        let s = &layout.structs["Big"];
        assert_eq!(s.fields[0].offset, 0);
        assert_eq!(s.fields[1].offset, 8);
        assert_eq!(s.size, 16);
        assert_eq!(s.align, 8);
    }

    #[test]
    fn test_nested_struct() {
        let layout = layout_from(
            "struct Vec3f { x: f32, y: f32, z: f32, }\n\
             struct GameState { pos: Vec3f, frame: u64, }",
        );
        let gs = &layout.structs["GameState"];
        // Vec3f is 12 bytes, align 4. Then frame: u64 at align 8
        // pos @ 0 (12 bytes), pad to 16, frame @ 16 (8 bytes) = 24
        assert_eq!(gs.fields[0].offset, 0);
        assert_eq!(gs.fields[1].offset, 16);
        assert_eq!(gs.size, 24);
        assert_eq!(gs.align, 8);
    }

    #[test]
    fn test_array_layout() {
        let layout = layout_from("struct Buf { data: [u8; 16], count: u32, }");
        let s = &layout.structs["Buf"];
        assert_eq!(s.fields[0].offset, 0);
        assert_eq!(s.fields[0].size, 16);
        assert_eq!(s.fields[1].offset, 16); // u8 array has align 1, u32 at 16 is already aligned
        assert_eq!(s.size, 20);
        assert_eq!(s.align, 4);
    }

    #[test]
    fn test_enum_layout() {
        let layout = layout_from("enum Simple { A { val: u32 }, B, }");
        let e = &layout.enums["Simple"];
        // tag: u32 (4), payload: u32 (4), total: 8
        assert_eq!(e.tag_size, 4);
        assert_eq!(e.max_payload_size, 4);
        assert_eq!(e.size, 8);
        assert_eq!(e.align, 4);
    }

    #[test]
    fn test_enum_with_u64_payload() {
        let layout = layout_from("enum Big { A { val: u64 }, B, }");
        let e = &layout.enums["Big"];
        // tag: u32 (4), pad to align 8, payload: u64 (8), total: 16
        assert_eq!(e.size, 16);
        assert_eq!(e.align, 8);
    }
}
