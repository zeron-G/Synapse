//! Lexer for the .bridge IDL format.

#[derive(Debug, Clone, PartialEq)]
pub enum Token {
    // Keywords
    Namespace,
    Struct,
    Enum,
    Channel,

    // Literals & identifiers
    Ident(String),
    IntLit(usize),

    // Punctuation
    LBrace,    // {
    RBrace,    // }
    LBracket,  // [
    RBracket,  // ]
    Colon,     // :
    Semicolon, // ;
    Comma,     // ,

    Eof,
}

#[derive(Debug, Clone)]
pub struct SpannedToken {
    pub token: Token,
    pub line: usize,
    pub col: usize,
}

pub struct Lexer<'a> {
    input: &'a [u8],
    pos: usize,
    line: usize,
    col: usize,
}

impl<'a> Lexer<'a> {
    pub fn new(input: &'a str) -> Self {
        Self {
            input: input.as_bytes(),
            pos: 0,
            line: 1,
            col: 1,
        }
    }

    pub fn tokenize(&mut self) -> Result<Vec<SpannedToken>, String> {
        let mut tokens = Vec::new();
        loop {
            let tok = self.next_token()?;
            let is_eof = tok.token == Token::Eof;
            tokens.push(tok);
            if is_eof {
                break;
            }
        }
        Ok(tokens)
    }

    fn next_token(&mut self) -> Result<SpannedToken, String> {
        self.skip_whitespace_and_comments();

        if self.pos >= self.input.len() {
            return Ok(SpannedToken {
                token: Token::Eof,
                line: self.line,
                col: self.col,
            });
        }

        let line = self.line;
        let col = self.col;
        let ch = self.input[self.pos] as char;

        let token = match ch {
            '{' => {
                self.advance();
                Token::LBrace
            }
            '}' => {
                self.advance();
                Token::RBrace
            }
            '[' => {
                self.advance();
                Token::LBracket
            }
            ']' => {
                self.advance();
                Token::RBracket
            }
            ':' => {
                self.advance();
                Token::Colon
            }
            ';' => {
                self.advance();
                Token::Semicolon
            }
            ',' => {
                self.advance();
                Token::Comma
            }
            '0'..='9' => self.lex_number()?,
            c if c.is_ascii_alphabetic() || c == '_' => self.lex_ident(),
            _ => return Err(format!("{}:{}: unexpected character '{}'", line, col, ch)),
        };

        Ok(SpannedToken { token, line, col })
    }

    fn advance(&mut self) {
        if self.pos < self.input.len() {
            if self.input[self.pos] == b'\n' {
                self.line += 1;
                self.col = 1;
            } else {
                self.col += 1;
            }
            self.pos += 1;
        }
    }

    fn peek(&self) -> Option<u8> {
        self.input.get(self.pos).copied()
    }

    fn skip_whitespace_and_comments(&mut self) {
        loop {
            // Skip whitespace
            while self.pos < self.input.len()
                && (self.input[self.pos] as char).is_ascii_whitespace()
            {
                self.advance();
            }
            // Skip line comments
            if self.pos + 1 < self.input.len()
                && self.input[self.pos] == b'/'
                && self.input[self.pos + 1] == b'/'
            {
                while self.pos < self.input.len() && self.input[self.pos] != b'\n' {
                    self.advance();
                }
                continue;
            }
            break;
        }
    }

    fn lex_number(&mut self) -> Result<Token, String> {
        let start = self.pos;
        while self.pos < self.input.len() && (self.input[self.pos] as char).is_ascii_digit() {
            self.advance();
        }
        let s = std::str::from_utf8(&self.input[start..self.pos]).unwrap();
        let n: usize = s
            .parse()
            .map_err(|_| format!("{}:{}: invalid integer '{}'", self.line, self.col, s))?;
        Ok(Token::IntLit(n))
    }

    fn lex_ident(&mut self) -> Token {
        let start = self.pos;
        while let Some(b) = self.peek() {
            if (b as char).is_ascii_alphanumeric() || b == b'_' {
                self.advance();
            } else {
                break;
            }
        }
        let word = std::str::from_utf8(&self.input[start..self.pos]).unwrap();
        match word {
            "namespace" => Token::Namespace,
            "struct" => Token::Struct,
            "enum" => Token::Enum,
            "channel" => Token::Channel,
            _ => Token::Ident(word.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic_tokens() {
        let mut lexer = Lexer::new("struct Foo { x: u32, }");
        let tokens = lexer.tokenize().unwrap();
        let kinds: Vec<_> = tokens.iter().map(|t| &t.token).collect();
        assert_eq!(
            kinds,
            vec![
                &Token::Struct,
                &Token::Ident("Foo".into()),
                &Token::LBrace,
                &Token::Ident("x".into()),
                &Token::Colon,
                &Token::Ident("u32".into()),
                &Token::Comma,
                &Token::RBrace,
                &Token::Eof,
            ]
        );
    }

    #[test]
    fn test_comments() {
        let mut lexer = Lexer::new("// comment\nstruct A {}");
        let tokens = lexer.tokenize().unwrap();
        assert_eq!(tokens[0].token, Token::Struct);
    }

    #[test]
    fn test_array_syntax() {
        let mut lexer = Lexer::new("[u8; 16]");
        let tokens = lexer.tokenize().unwrap();
        let kinds: Vec<_> = tokens.iter().map(|t| &t.token).collect();
        assert_eq!(
            kinds,
            vec![
                &Token::LBracket,
                &Token::Ident("u8".into()),
                &Token::Semicolon,
                &Token::IntLit(16),
                &Token::RBracket,
                &Token::Eof,
            ]
        );
    }

    #[test]
    fn test_namespace() {
        let mut lexer = Lexer::new("namespace game;");
        let tokens = lexer.tokenize().unwrap();
        assert_eq!(tokens[0].token, Token::Namespace);
        assert_eq!(tokens[1].token, Token::Ident("game".into()));
        assert_eq!(tokens[2].token, Token::Semicolon);
    }
}
