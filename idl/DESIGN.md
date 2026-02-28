# Synapse IDL (.bridge) Design

## Overview

The `.bridge` IDL (Interface Definition Language) defines cross-language data types for the Synapse shared memory bridge. A single `.bridge` file describes structs, enums, and channels that are compiled into matching Rust, Python, and C++ type definitions with identical memory layouts.

## IDL Syntax

### Namespace

Optional, must be first if present. Scopes generated code (C++ `namespace`, Rust/Python comments).

```
namespace game;
```

### Structs

Fixed-layout value types with named fields. Fields use C ABI alignment.

```
struct Vec3f {
    x: f32,
    y: f32,
    z: f32,
}
```

### Enums

Tagged unions with a `u32` discriminant. Variants may carry payload fields.

```
enum Command {
    MoveTo { target: Vec3f },
    Attack { target_id: u32 },
    Idle,
}
```

### Fixed Arrays

Inline fixed-size arrays using `[T; N]` syntax, including nested arrays.

```
struct Buffer {
    data: [u8; 256],
    matrix: [[f32; 4]; 4],
}
```

### Channels

Bind named directional data flows to types. Each entry maps a channel name to a struct or enum type.

```
channel game_bridge {
    host_to_client: GameState,
    client_to_host: Command,
}
```

### Comments

Line comments with `//`.

```
// This is a comment
struct Foo { x: u32, } // inline comment
```

## Primitive Types

| Type   | Size | Rust   | C++        | Python ctypes      |
|--------|------|--------|------------|--------------------|
| `u8`   | 1    | `u8`   | `uint8_t`  | `ctypes.c_uint8`   |
| `u16`  | 2    | `u16`  | `uint16_t` | `ctypes.c_uint16`  |
| `u32`  | 4    | `u32`  | `uint32_t` | `ctypes.c_uint32`  |
| `u64`  | 8    | `u64`  | `uint64_t` | `ctypes.c_uint64`  |
| `i8`   | 1    | `i8`   | `int8_t`   | `ctypes.c_int8`    |
| `i16`  | 2    | `i16`  | `int16_t`  | `ctypes.c_int16`   |
| `i32`  | 4    | `i32`  | `int32_t`  | `ctypes.c_int32`   |
| `i64`  | 8    | `i64`  | `int64_t`  | `ctypes.c_int64`   |
| `f32`  | 4    | `f32`  | `float`    | `ctypes.c_float`   |
| `f64`  | 8    | `f64`  | `double`   | `ctypes.c_double`  |
| `bool` | 1    | `bool` | `bool`     | `ctypes.c_bool`    |

## Layout Rules (C ABI)

1. **Field alignment**: Each field is placed at the next offset that is a multiple of its alignment
2. **Struct alignment**: Maximum alignment of all fields (minimum 1)
3. **Struct size**: Padded to a multiple of its alignment
4. **Array alignment**: Same as element alignment; size = element_size * count
5. **Enum layout**: `[u32 tag] [padding] [payload]`
   - Tag is always `u32` (4 bytes, align 4)
   - Payload offset is aligned to the maximum payload alignment
   - Payload size is the maximum of all variant payload sizes
   - Total alignment is `max(4, max_payload_align)`

## Compiler Pipeline

```
.bridge source
    |
    v
  Lexer (lexer.rs)     -- tokenization
    |
    v
  Parser (parser.rs)    -- AST construction
    |
    v
  Layout (layout.rs)    -- C ABI size/alignment computation
    |
    v
  Codegen               -- language-specific output
    |--- rust.rs         -- #[repr(C)] structs
    |--- python.rs       -- ctypes.Structure classes
    |--- cpp.rs          -- structs with static_assert
```

## Constraints

- Types must be defined before use (no forward references)
- All types resolve to fixed sizes at compile time (no heap allocation, no pointers)
- Generated layouts are identical across all three target languages
