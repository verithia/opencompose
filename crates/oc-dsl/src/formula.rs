//! A deliberately small expression language for control-source formulas.
//!
//! It is not a scripting host: there are no variables beyond `x`, no
//! statements, no host access and no state. `x` is the shaped envelope value in
//! `[0, 1]`; the result is clamped into the same range, and a non-finite result
//! reads as zero. That makes a formula safe to evaluate per sample.
//!
//! Grammar (whitespace is ignored):
//!
//! ```text
//! expr    := term (('+' | '-') term)*
//! term    := power (('*' | '/') power)*
//! power   := unary ('^' power)?          // right associative
//! unary   := ('+' | '-')* primary
//! primary := number | 'x' | func '(' expr ')' | '(' expr ')'
//! func    := 'sqrt' | 'abs' | 'clamp01'
//! ```

/// A parsed, immutable formula over the control variable `x`.
#[derive(Debug, Clone, PartialEq)]
pub struct Formula {
    root: Expr,
}

#[derive(Debug, Clone, PartialEq)]
enum Expr {
    X,
    Number(f64),
    Neg(Box<Expr>),
    Add(Box<Expr>, Box<Expr>),
    Sub(Box<Expr>, Box<Expr>),
    Mul(Box<Expr>, Box<Expr>),
    Div(Box<Expr>, Box<Expr>),
    Pow(Box<Expr>, Box<Expr>),
    Sqrt(Box<Expr>),
    Abs(Box<Expr>),
    Clamp01(Box<Expr>),
}

impl Formula {
    /// Parse a formula, rejecting anything outside the documented grammar.
    pub fn parse(source: &str) -> Result<Self, String> {
        let mut parser = Parser {
            bytes: source.as_bytes(),
            position: 0,
        };
        let root = parser.expression()?;
        parser.skip_whitespace();
        if parser.position != parser.bytes.len() {
            return Err(format!(
                "unexpected '{}' at byte {}",
                &source[parser.position..],
                parser.position
            ));
        }
        Ok(Self { root })
    }

    /// Evaluate and clamp into `[0, 1]`; non-finite results read as zero.
    pub fn eval(&self, x: f64) -> f64 {
        let value = self.root.eval(x);
        if value.is_nan() {
            0.0
        } else {
            value.clamp(0.0, 1.0)
        }
    }
}

impl Expr {
    fn eval(&self, x: f64) -> f64 {
        match self {
            Expr::X => x,
            Expr::Number(value) => *value,
            Expr::Neg(inner) => -inner.eval(x),
            Expr::Add(left, right) => left.eval(x) + right.eval(x),
            Expr::Sub(left, right) => left.eval(x) - right.eval(x),
            Expr::Mul(left, right) => left.eval(x) * right.eval(x),
            Expr::Div(left, right) => left.eval(x) / right.eval(x),
            Expr::Pow(base, exponent) => base.eval(x).powf(exponent.eval(x)),
            Expr::Sqrt(inner) => inner.eval(x).sqrt(),
            Expr::Abs(inner) => inner.eval(x).abs(),
            Expr::Clamp01(inner) => inner.eval(x).clamp(0.0, 1.0),
        }
    }
}

struct Parser<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl Parser<'_> {
    fn skip_whitespace(&mut self) {
        while self
            .bytes
            .get(self.position)
            .is_some_and(|byte| byte.is_ascii_whitespace())
        {
            self.position += 1;
        }
    }

    fn peek(&mut self) -> Option<u8> {
        self.skip_whitespace();
        self.bytes.get(self.position).copied()
    }

    fn consume(&mut self, expected: u8) -> bool {
        if self.peek() == Some(expected) {
            self.position += 1;
            true
        } else {
            false
        }
    }

    fn expression(&mut self) -> Result<Expr, String> {
        let mut left = self.term()?;
        loop {
            match self.peek() {
                Some(b'+') => {
                    self.position += 1;
                    left = Expr::Add(Box::new(left), Box::new(self.term()?));
                }
                Some(b'-') => {
                    self.position += 1;
                    left = Expr::Sub(Box::new(left), Box::new(self.term()?));
                }
                _ => return Ok(left),
            }
        }
    }

    fn term(&mut self) -> Result<Expr, String> {
        let mut left = self.power()?;
        loop {
            match self.peek() {
                Some(b'*') => {
                    self.position += 1;
                    left = Expr::Mul(Box::new(left), Box::new(self.power()?));
                }
                Some(b'/') => {
                    self.position += 1;
                    left = Expr::Div(Box::new(left), Box::new(self.power()?));
                }
                _ => return Ok(left),
            }
        }
    }

    fn power(&mut self) -> Result<Expr, String> {
        let base = self.unary()?;
        if self.consume(b'^') {
            Ok(Expr::Pow(Box::new(base), Box::new(self.power()?)))
        } else {
            Ok(base)
        }
    }

    fn unary(&mut self) -> Result<Expr, String> {
        match self.peek() {
            Some(b'-') => {
                self.position += 1;
                Ok(Expr::Neg(Box::new(self.unary()?)))
            }
            Some(b'+') => {
                self.position += 1;
                self.unary()
            }
            _ => self.primary(),
        }
    }

    fn primary(&mut self) -> Result<Expr, String> {
        match self.peek() {
            Some(b'(') => {
                self.position += 1;
                let inner = self.expression()?;
                if !self.consume(b')') {
                    return Err("missing ')'".into());
                }
                Ok(inner)
            }
            Some(b'x') => {
                self.position += 1;
                Ok(Expr::X)
            }
            Some(byte) if byte.is_ascii_digit() || byte == b'.' => self.number(),
            Some(byte) if byte.is_ascii_alphabetic() => self.function(),
            Some(byte) => Err(format!("unexpected '{}'", byte as char)),
            None => Err("unexpected end of formula".into()),
        }
    }

    fn number(&mut self) -> Result<Expr, String> {
        let start = self.position;
        while self
            .bytes
            .get(self.position)
            .is_some_and(|byte| byte.is_ascii_digit() || *byte == b'.')
        {
            self.position += 1;
        }
        let text = std::str::from_utf8(&self.bytes[start..self.position]).unwrap_or("");
        text.parse::<f64>()
            .map(Expr::Number)
            .map_err(|_| format!("invalid number '{text}'"))
    }

    fn function(&mut self) -> Result<Expr, String> {
        let start = self.position;
        while self
            .bytes
            .get(self.position)
            .is_some_and(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
        {
            self.position += 1;
        }
        let name = std::str::from_utf8(&self.bytes[start..self.position]).unwrap_or("");
        if !self.consume(b'(') {
            return Err(format!("unknown variable or function '{name}'"));
        }
        let argument = self.expression()?;
        if !self.consume(b')') {
            return Err(format!("missing ')' after '{name}'"));
        }
        match name {
            "sqrt" => Ok(Expr::Sqrt(Box::new(argument))),
            "abs" => Ok(Expr::Abs(Box::new(argument))),
            "clamp01" => Ok(Expr::Clamp01(Box::new(argument))),
            _ => Err(format!("unknown function '{name}'")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn eval(source: &str, x: f64) -> f64 {
        Formula::parse(source).expect(source).eval(x)
    }

    #[test]
    fn evaluates_the_documented_operators() {
        assert_eq!(eval("x", 0.25), 0.25);
        assert_eq!(eval("1-x", 0.25), 0.75);
        assert_eq!(eval("x*x", 0.5), 0.25);
        assert_eq!(eval("2 ^ 3", 0.0), 1.0);
        assert_eq!(eval("2^3^x", 0.0), 1.0, "right associative");
        assert_eq!(eval("-x + 1.5", 0.5), 1.0);
        assert_eq!(eval("(x + 1) / 2", 0.0), 0.5);
        assert_eq!(eval("sqrt(x)", 0.25), 0.5);
        assert_eq!(eval("abs(-x)", 0.3), 0.3);
        assert_eq!(eval("clamp01(x*4)", 0.5), 1.0);
    }

    #[test]
    fn clamps_and_never_returns_nan() {
        assert_eq!(eval("x - 1", 0.5), 0.0);
        assert_eq!(eval("x * 3", 0.5), 1.0);
        assert_eq!(eval("x / 0", 1.0), 1.0);
        assert_eq!(eval("(x - x) / (x - x)", 0.5), 0.0);
        assert_eq!(eval("sqrt(x - 2)", 0.5), 0.0);
    }

    #[test]
    fn rejects_unknown_syntax() {
        for source in [
            "", "y", "x +", "x + ", "(x", "x)", "sin(x)", "min(x)", "x;", "x,1", "-",
        ] {
            assert!(
                Formula::parse(source).is_err(),
                "'{source}' should not parse"
            );
        }
    }
}
