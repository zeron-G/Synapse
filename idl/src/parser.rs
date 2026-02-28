//! Parser for the .bridge IDL format.
//! Consumes a token stream from the lexer and produces an AST.

use crate::ast::*;
use crate::lexer::{SpannedToken, Token};

pub struct Parser {
    tokens: Vec<SpannedToken>,
    pos: usize,
}

impl Parser {
    pub fn new(tokens: Vec<SpannedToken>) -> Self {
        Self { tokens, pos: 0 }
    }

    pub fn parse(&mut self) -> Result<Schema, String> {
        let mut namespace = None;
        let mut items = Vec::new();

        // Optional namespace declaration (must be first if present)
        if self.peek_token() == &Token::Namespace {
            namespace = Some(self.parse_namespace()?);
        }

        while self.peek_token() != &Token::Eof {
            let item = match self.peek_token() {
                Token::Struct => Item::Struct(self.parse_struct()?),
                Token::Enum => Item::Enum(self.parse_enum()?),
                Token::Channel => Item::Channel(self.parse_channel()?),
                other => {
                    let sp = &self.tokens[self.pos];
                    return Err(format!(
                        "{}:{}: expected struct, enum, or channel, found {:?}",
                        sp.line, sp.col, other
                    ));
                }
            };
            items.push(item);
        }

        Ok(Schema { namespace, items })
    }

    fn peek_token(&self) -> &Token {
        &self.tokens[self.pos].token
    }

    fn expect(&mut self, expected: &Token) -> Result<(), String> {
        let sp = &self.tokens[self.pos];
        if &sp.token == expected {
            self.pos += 1;
            Ok(())
        } else {
            Err(format!(
                "{}:{}: expected {:?}, found {:?}",
                sp.line, sp.col, expected, sp.token
            ))
        }
    }

    fn expect_ident(&mut self) -> Result<String, String> {
        let sp = &self.tokens[self.pos];
        if let Token::Ident(name) = &sp.token {
            let name = name.clone();
            self.pos += 1;
            Ok(name)
        } else {
            Err(format!(
                "{}:{}: expected identifier, found {:?}",
                sp.line, sp.col, sp.token
            ))
        }
    }

    fn parse_namespace(&mut self) -> Result<String, String> {
        self.expect(&Token::Namespace)?;
        let name = self.expect_ident()?;
        self.expect(&Token::Semicolon)?;
        Ok(name)
    }

    fn parse_struct(&mut self) -> Result<StructDef, String> {
        self.expect(&Token::Struct)?;
        let name = self.expect_ident()?;
        self.expect(&Token::LBrace)?;
        let fields = self.parse_fields()?;
        self.expect(&Token::RBrace)?;
        Ok(StructDef { name, fields })
    }

    fn parse_enum(&mut self) -> Result<EnumDef, String> {
        self.expect(&Token::Enum)?;
        let name = self.expect_ident()?;
        self.expect(&Token::LBrace)?;
        let variants = self.parse_variants()?;
        self.expect(&Token::RBrace)?;
        Ok(EnumDef { name, variants })
    }

    fn parse_channel(&mut self) -> Result<ChannelDef, String> {
        self.expect(&Token::Channel)?;
        let name = self.expect_ident()?;
        self.expect(&Token::LBrace)?;
        let mut entries = Vec::new();
        while self.peek_token() != &Token::RBrace {
            let entry_name = self.expect_ident()?;
            self.expect(&Token::Colon)?;
            let ty = self.expect_ident()?;
            entries.push(ChannelEntry {
                name: entry_name,
                ty,
            });
            self.skip_comma();
        }
        self.expect(&Token::RBrace)?;
        Ok(ChannelDef { name, entries })
    }

    fn parse_fields(&mut self) -> Result<Vec<Field>, String> {
        let mut fields = Vec::new();
        while self.peek_token() != &Token::RBrace {
            let name = self.expect_ident()?;
            self.expect(&Token::Colon)?;
            let ty = self.parse_type()?;
            fields.push(Field { name, ty });
            self.skip_comma();
        }
        Ok(fields)
    }

    fn parse_variants(&mut self) -> Result<Vec<Variant>, String> {
        let mut variants = Vec::new();
        while self.peek_token() != &Token::RBrace {
            let name = self.expect_ident()?;
            let fields = if self.peek_token() == &Token::LBrace {
                self.expect(&Token::LBrace)?;
                let f = self.parse_fields()?;
                self.expect(&Token::RBrace)?;
                f
            } else {
                Vec::new()
            };
            variants.push(Variant { name, fields });
            self.skip_comma();
        }
        Ok(variants)
    }

    fn parse_type(&mut self) -> Result<Type, String> {
        // Check for array: [T; N]
        if self.peek_token() == &Token::LBracket {
            self.pos += 1;
            let inner = self.parse_type()?;
            self.expect(&Token::Semicolon)?;
            let sp = &self.tokens[self.pos];
            let len = if let Token::IntLit(n) = &sp.token {
                let n = *n;
                self.pos += 1;
                n
            } else {
                return Err(format!(
                    "{}:{}: expected array length, found {:?}",
                    sp.line, sp.col, sp.token
                ));
            };
            self.expect(&Token::RBracket)?;
            return Ok(Type::Array(Box::new(inner), len));
        }

        let name = self.expect_ident()?;
        match name.as_str() {
            "u8" => Ok(Type::Primitive(PrimitiveType::U8)),
            "u16" => Ok(Type::Primitive(PrimitiveType::U16)),
            "u32" => Ok(Type::Primitive(PrimitiveType::U32)),
            "u64" => Ok(Type::Primitive(PrimitiveType::U64)),
            "i8" => Ok(Type::Primitive(PrimitiveType::I8)),
            "i16" => Ok(Type::Primitive(PrimitiveType::I16)),
            "i32" => Ok(Type::Primitive(PrimitiveType::I32)),
            "i64" => Ok(Type::Primitive(PrimitiveType::I64)),
            "f32" => Ok(Type::Primitive(PrimitiveType::F32)),
            "f64" => Ok(Type::Primitive(PrimitiveType::F64)),
            "bool" => Ok(Type::Primitive(PrimitiveType::Bool)),
            _ => Ok(Type::Named(name)),
        }
    }

    fn skip_comma(&mut self) {
        if self.peek_token() == &Token::Comma {
            self.pos += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::Lexer;

    fn parse_str(input: &str) -> Schema {
        let mut lexer = Lexer::new(input);
        let tokens = lexer.tokenize().unwrap();
        let mut parser = Parser::new(tokens);
        parser.parse().unwrap()
    }

    #[test]
    fn test_parse_struct() {
        let schema = parse_str("struct Vec3f { x: f32, y: f32, z: f32, }");
        assert_eq!(schema.items.len(), 1);
        if let Item::Struct(s) = &schema.items[0] {
            assert_eq!(s.name, "Vec3f");
            assert_eq!(s.fields.len(), 3);
            assert_eq!(s.fields[0].name, "x");
            assert_eq!(s.fields[0].ty, Type::Primitive(PrimitiveType::F32));
        } else {
            panic!("expected struct");
        }
    }

    #[test]
    fn test_parse_enum() {
        let schema = parse_str("enum Command { MoveTo { target_id: u32 }, Idle, }");
        if let Item::Enum(e) = &schema.items[0] {
            assert_eq!(e.name, "Command");
            assert_eq!(e.variants.len(), 2);
            assert_eq!(e.variants[0].name, "MoveTo");
            assert_eq!(e.variants[0].fields.len(), 1);
            assert_eq!(e.variants[1].name, "Idle");
            assert!(e.variants[1].fields.is_empty());
        } else {
            panic!("expected enum");
        }
    }

    #[test]
    fn test_parse_channel() {
        let schema = parse_str(
            "channel game_bridge { host_to_client: GameState, client_to_host: Command, }",
        );
        if let Item::Channel(ch) = &schema.items[0] {
            assert_eq!(ch.name, "game_bridge");
            assert_eq!(ch.entries.len(), 2);
            assert_eq!(ch.entries[0].name, "host_to_client");
            assert_eq!(ch.entries[0].ty, "GameState");
        } else {
            panic!("expected channel");
        }
    }

    #[test]
    fn test_parse_namespace() {
        let schema = parse_str("namespace game;\nstruct A { x: u8, }");
        assert_eq!(schema.namespace, Some("game".to_string()));
    }

    #[test]
    fn test_parse_array_field() {
        let schema = parse_str("struct Buf { data: [u8; 64], }");
        if let Item::Struct(s) = &schema.items[0] {
            assert_eq!(
                s.fields[0].ty,
                Type::Array(Box::new(Type::Primitive(PrimitiveType::U8)), 64)
            );
        } else {
            panic!("expected struct");
        }
    }

    #[test]
    fn test_full_example() {
        let input = r#"
            namespace game;
            struct Vec3f { x: f32, y: f32, z: f32, }
            struct GameState { position: Vec3f, velocity: Vec3f, health: f32, frame_id: u64, }
            enum Command { MoveTo { target: Vec3f }, Attack { target_id: u32 }, Idle, }
            channel game_bridge { host_to_client: GameState, client_to_host: Command, }
        "#;
        let schema = parse_str(input);
        assert_eq!(schema.namespace, Some("game".into()));
        assert_eq!(schema.items.len(), 4);
    }
}
