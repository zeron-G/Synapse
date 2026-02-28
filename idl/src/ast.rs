//! AST types for the .bridge IDL schema language.

/// A complete parsed .bridge schema file.
#[derive(Debug, Clone, PartialEq)]
pub struct Schema {
    pub namespace: Option<String>,
    pub items: Vec<Item>,
}

/// A top-level declaration.
#[derive(Debug, Clone, PartialEq)]
pub enum Item {
    Struct(StructDef),
    Enum(EnumDef),
    Channel(ChannelDef),
}

/// A struct definition: `struct Foo { field: Type, ... }`
#[derive(Debug, Clone, PartialEq)]
pub struct StructDef {
    pub name: String,
    pub fields: Vec<Field>,
}

/// A struct field: `name: Type`
#[derive(Debug, Clone, PartialEq)]
pub struct Field {
    pub name: String,
    pub ty: Type,
}

/// An enum definition with variants that can optionally carry fields.
#[derive(Debug, Clone, PartialEq)]
pub struct EnumDef {
    pub name: String,
    pub variants: Vec<Variant>,
}

/// An enum variant: `Name` or `Name { fields... }`
#[derive(Debug, Clone, PartialEq)]
pub struct Variant {
    pub name: String,
    pub fields: Vec<Field>,
}

/// A channel definition binding named directional channels to types.
#[derive(Debug, Clone, PartialEq)]
pub struct ChannelDef {
    pub name: String,
    pub entries: Vec<ChannelEntry>,
}

/// A single channel direction entry: `name: Type`
#[derive(Debug, Clone, PartialEq)]
pub struct ChannelEntry {
    pub name: String,
    pub ty: String,
}

/// A type reference in the IDL.
#[derive(Debug, Clone, PartialEq)]
pub enum Type {
    /// Primitive types: u8, u16, u32, u64, i8, i16, i32, i64, f32, f64, bool
    Primitive(PrimitiveType),
    /// Fixed-size array: `[T; N]`
    Array(Box<Type>, usize),
    /// Named type reference (struct or enum name)
    Named(String),
}

/// Primitive scalar types matching C ABI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrimitiveType {
    U8,
    U16,
    U32,
    U64,
    I8,
    I16,
    I32,
    I64,
    F32,
    F64,
    Bool,
}

impl PrimitiveType {
    /// Size in bytes.
    pub fn size(self) -> usize {
        match self {
            Self::U8 | Self::I8 | Self::Bool => 1,
            Self::U16 | Self::I16 => 2,
            Self::U32 | Self::I32 | Self::F32 => 4,
            Self::U64 | Self::I64 | Self::F64 => 8,
        }
    }

    /// Alignment in bytes (matches size for all primitives).
    pub fn align(self) -> usize {
        self.size()
    }
}
