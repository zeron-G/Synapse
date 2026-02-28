pub mod ast;
pub mod codegen;
pub mod layout;
pub mod lexer;
pub mod parser;

use ast::Schema;
use layout::SchemaLayout;
use lexer::Lexer;
use parser::Parser;

/// Parse a .bridge IDL source string into a Schema AST.
pub fn parse(input: &str) -> Result<Schema, String> {
    let mut lexer = Lexer::new(input);
    let tokens = lexer.tokenize()?;
    let mut parser = Parser::new(tokens);
    parser.parse()
}

/// Parse and compute layouts for a .bridge IDL source string.
pub fn compile(input: &str) -> Result<(Schema, SchemaLayout), String> {
    let schema = parse(input)?;
    let layout = layout::compute_layout(&schema)?;
    Ok((schema, layout))
}

/// Generate Rust code from a .bridge IDL source string.
pub fn generate_rust(input: &str) -> Result<String, String> {
    let (schema, layout) = compile(input)?;
    Ok(codegen::rust::generate(&schema, &layout))
}

/// Generate Python ctypes code from a .bridge IDL source string.
pub fn generate_python(input: &str) -> Result<String, String> {
    let (schema, layout) = compile(input)?;
    Ok(codegen::python::generate(&schema, &layout))
}

/// Generate C++ code from a .bridge IDL source string.
pub fn generate_cpp(input: &str) -> Result<String, String> {
    let (schema, layout) = compile(input)?;
    Ok(codegen::cpp::generate(&schema, &layout))
}
