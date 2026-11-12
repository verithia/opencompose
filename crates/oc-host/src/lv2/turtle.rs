//! Small Turtle reader for LV2 bundle metadata.
//!
//! The reader covers the subset used by `manifest.ttl`: prefixes and a base,
//! predicate/object lists, blank node property lists, collections, and plain,
//! typed, language-tagged, numeric and boolean literals. Each document becomes
//! an in-memory triple graph; callers query that graph instead of re-parsing.

use std::collections::HashMap;

/// RDF `type` predicate.
pub const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
/// RDF collection `first` predicate.
pub const RDF_FIRST: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#first";
/// RDF collection `rest` predicate.
pub const RDF_REST: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#rest";
/// RDF empty-collection term.
pub const RDF_NIL: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#nil";
/// XSD string datatype.
pub const XSD_STRING: &str = "http://www.w3.org/2001/XMLSchema#string";
/// XSD integer datatype.
pub const XSD_INTEGER: &str = "http://www.w3.org/2001/XMLSchema#integer";
/// XSD decimal datatype.
pub const XSD_DECIMAL: &str = "http://www.w3.org/2001/XMLSchema#decimal";
/// XSD double datatype.
pub const XSD_DOUBLE: &str = "http://www.w3.org/2001/XMLSchema#double";
/// XSD boolean datatype.
pub const XSD_BOOLEAN: &str = "http://www.w3.org/2001/XMLSchema#boolean";

#[derive(Clone, Debug, PartialEq, Eq)]
/// One RDF term: IRI, blank node or literal.
pub enum Term {
    Iri(String),
    Blank(String),
    Literal {
        value: String,
        datatype: Option<String>,
        lang: Option<String>,
    },
}

impl Term {
    /// An IRI term.
    pub fn iri(value: impl Into<String>) -> Self {
        Self::Iri(value.into())
    }

    /// The IRI value when this is an IRI.
    pub fn as_iri(&self) -> Option<&str> {
        match self {
            Self::Iri(value) => Some(value),
            _ => None,
        }
    }

    /// IRI or literal text, if any.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::Iri(value) => Some(value),
            Self::Literal { value, .. } => Some(value),
            Self::Blank(_) => None,
        }
    }

    /// Literal value parsed as `f64`.
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Self::Literal { value, .. } => value.trim().parse().ok(),
            _ => None,
        }
    }

    /// Literal value parsed as `i64`.
    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Self::Literal { value, .. } => value.trim().parse().ok(),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
/// One RDF triple.
pub struct Triple {
    pub subject: Term,
    pub predicate: String,
    pub object: Term,
}

#[derive(Clone, Debug, Default)]
/// An in-memory triple graph.
pub struct Graph {
    triples: Vec<Triple>,
}

impl Graph {
    /// All triples in document order.
    pub fn triples(&self) -> &[Triple] {
        &self.triples
    }

    /// Objects for a subject and predicate.
    pub fn objects<'a>(
        &'a self,
        subject: &'a Term,
        predicate: &'a str,
    ) -> impl Iterator<Item = &'a Term> {
        self.triples
            .iter()
            .filter(move |triple| &triple.subject == subject && triple.predicate == predicate)
            .map(|triple| &triple.object)
    }

    /// First object for a subject and predicate.
    pub fn object<'a>(&'a self, subject: &'a Term, predicate: &'a str) -> Option<&'a Term> {
        self.objects(subject, predicate).next()
    }

    /// Whether the exact triple exists.
    pub fn has(&self, subject: &Term, predicate: &str, object: &Term) -> bool {
        self.triples.iter().any(|triple| {
            &triple.subject == subject && triple.predicate == predicate && &triple.object == object
        })
    }

    /// Whether a subject has the given `rdf:type`.
    pub fn has_type(&self, subject: &Term, type_uri: &str) -> bool {
        self.objects(subject, RDF_TYPE)
            .any(|object| object.as_iri() == Some(type_uri))
    }

    /// Distinct subjects with the given `rdf:type`.
    pub fn subjects_of_type(&self, type_uri: &str) -> Vec<&Term> {
        let mut subjects = Vec::new();
        for triple in &self.triples {
            if triple.predicate == RDF_TYPE
                && triple.object.as_iri() == Some(type_uri)
                && !subjects.contains(&&triple.subject)
            {
                subjects.push(&triple.subject);
            }
        }
        subjects
    }
}

/// Parse one Turtle document. `base` is the document IRI used for relative IRIs.
pub fn parse(input: &str, base: Option<&str>) -> Result<Graph, String> {
    Parser::new(input, base).parse_document()
}

struct Parser<'a> {
    src: &'a str,
    bytes: &'a [u8],
    pos: usize,
    base: Option<String>,
    prefixes: HashMap<String, String>,
    graph: Graph,
    blank_counter: usize,
    labels: HashMap<String, String>,
}

impl<'a> Parser<'a> {
    fn new(src: &'a str, base: Option<&str>) -> Self {
        Self {
            src,
            bytes: src.as_bytes(),
            pos: 0,
            base: base.map(str::to_string),
            prefixes: HashMap::new(),
            graph: Graph::default(),
            blank_counter: 0,
            labels: HashMap::new(),
        }
    }

    fn parse_document(mut self) -> Result<Graph, String> {
        loop {
            self.skip_ws_comments();
            if self.pos >= self.bytes.len() {
                break;
            }
            if self.bytes[self.pos] == b'@' {
                self.parse_at_directive()?;
                continue;
            }
            if self.starts_with_keyword("prefix") || self.starts_with_keyword("base") {
                self.parse_sparql_directive()?;
                continue;
            }
            self.parse_statement()?;
        }
        Ok(self.graph)
    }

    fn parse_at_directive(&mut self) -> Result<(), String> {
        let start = self.pos;
        self.pos += 1;
        let word = self.read_token();
        if word.eq_ignore_ascii_case("prefix") {
            self.skip_ws_comments();
            let prefix = self.read_prefix_declaration()?;
            self.skip_ws_comments();
            let iri = self.parse_iri()?;
            self.prefixes.insert(prefix, iri);
            self.skip_ws_comments();
            self.expect_byte(b'.')?;
            Ok(())
        } else if word.eq_ignore_ascii_case("base") {
            self.skip_ws_comments();
            let iri = self.parse_iri()?;
            self.base = Some(iri);
            self.skip_ws_comments();
            self.expect_byte(b'.')?;
            Ok(())
        } else {
            Err(self.error_at(start, "unknown directive"))
        }
    }

    fn parse_sparql_directive(&mut self) -> Result<(), String> {
        let word = self.read_token();
        self.skip_ws_comments();
        if word.eq_ignore_ascii_case("prefix") {
            let prefix = self.read_prefix_declaration()?;
            self.skip_ws_comments();
            let iri = self.parse_iri()?;
            self.prefixes.insert(prefix, iri);
        } else {
            let iri = self.parse_iri()?;
            self.base = Some(iri);
        }
        self.skip_ws_comments();
        if self.peek_byte() == Some(b'.') {
            self.pos += 1;
        }
        Ok(())
    }

    fn read_prefix_declaration(&mut self) -> Result<String, String> {
        let start = self.pos;
        while self.pos < self.bytes.len() {
            match self.bytes[self.pos] {
                b':' => {
                    let prefix = self.src[start..self.pos].to_string();
                    self.pos += 1;
                    if !prefix.is_empty() && !prefix.chars().all(is_prefix_char) {
                        return Err(self.error_at(start, "invalid prefix name"));
                    }
                    return Ok(prefix);
                }
                byte if byte.is_ascii_whitespace() => break,
                _ => self.pos += 1,
            }
        }
        Err(self.error_at(start, "expected prefix declaration"))
    }

    fn parse_statement(&mut self) -> Result<(), String> {
        let subject = self.parse_subject()?;
        self.skip_ws_comments();
        self.parse_predicate_object_list(&subject, true)?;
        self.skip_ws_comments();
        self.expect_byte(b'.')?;
        Ok(())
    }

    fn parse_subject(&mut self) -> Result<Term, String> {
        self.skip_ws_comments();
        let term = self.parse_term(true)?;
        if matches!(term, Term::Literal { .. }) {
            return Err(self.error("literal is not a valid subject"));
        }
        Ok(term)
    }

    fn parse_predicate_object_list(
        &mut self,
        subject: &Term,
        top_level: bool,
    ) -> Result<(), String> {
        loop {
            self.skip_ws_comments();
            if top_level && self.peek_byte() == Some(b'.') {
                return Ok(());
            }
            if !top_level && self.peek_byte() == Some(b']') {
                return Ok(());
            }
            let predicate = self.parse_verb()?;
            loop {
                self.skip_ws_comments();
                let object = self.parse_term(true)?;
                self.graph.triples.push(Triple {
                    subject: subject.clone(),
                    predicate: predicate.clone(),
                    object,
                });
                self.skip_ws_comments();
                if self.peek_byte() == Some(b',') {
                    self.pos += 1;
                    continue;
                }
                break;
            }
            self.skip_ws_comments();
            if self.peek_byte() == Some(b';') {
                while self.peek_byte() == Some(b';') {
                    self.pos += 1;
                    self.skip_ws_comments();
                }
                if self.peek_byte() == Some(b'.')
                    || (top_level && self.pos >= self.bytes.len())
                    || (!top_level && self.peek_byte() == Some(b']'))
                {
                    return Ok(());
                }
                continue;
            }
            return Ok(());
        }
    }

    fn parse_verb(&mut self) -> Result<String, String> {
        self.skip_ws_comments();
        if self.peek_byte() == Some(b'a')
            && self
                .bytes
                .get(self.pos + 1)
                .is_none_or(|byte| byte.is_ascii_whitespace() || *byte == b'<')
        {
            self.pos += 1;
            return Ok(RDF_TYPE.to_string());
        }
        match self.parse_term(false)? {
            Term::Iri(iri) => Ok(iri),
            _ => Err(self.error("predicate must be an IRI")),
        }
    }

    fn parse_term(&mut self, allow_literal: bool) -> Result<Term, String> {
        self.skip_ws_comments();
        let Some(byte) = self.peek_byte() else {
            return Err(self.error("unexpected end of input"));
        };
        match byte {
            b'<' => self.parse_iri().map(Term::Iri),
            b'_' if self.bytes.get(self.pos + 1) == Some(&b':') => self.parse_blank_label(),
            b'[' => self.parse_blank_property_list(),
            b'(' => self.parse_collection(),
            b'"' | b'\'' => self.parse_literal(),
            b'+' | b'-' | b'0'..=b'9' => self.parse_numeric_literal(),
            _ => {
                let start = self.pos;
                let token = self.read_token();
                if token == "true" || token == "false" {
                    if !allow_literal {
                        return Err(self.error_at(start, "literal is not a valid predicate"));
                    }
                    return Ok(Term::Literal {
                        value: token,
                        datatype: Some(XSD_BOOLEAN.to_string()),
                        lang: None,
                    });
                }
                if token.contains(':') {
                    return self.resolve_prefixed_name(&token).map(Term::Iri);
                }
                Err(self.error_at(start, "expected a term"))
            }
        }
    }

    fn parse_iri(&mut self) -> Result<String, String> {
        self.skip_ws_comments();
        self.expect_byte(b'<')?;
        let start = self.pos;
        while self.pos < self.bytes.len() {
            match self.bytes[self.pos] {
                b'>' => {
                    let raw = unescape(&self.src[start..self.pos])?;
                    self.pos += 1;
                    return Ok(self.resolve_reference(&raw));
                }
                b'\\' => self.pos += 2,
                _ => self.pos += 1,
            }
        }
        Err(self.error_at(start, "unterminated IRI"))
    }

    fn resolve_prefixed_name(&self, token: &str) -> Result<String, String> {
        let Some((prefix, local)) = token.split_once(':') else {
            return Err(self.error("expected a prefixed name"));
        };
        let base = self
            .prefixes
            .get(prefix)
            .ok_or_else(|| self.error(&format!("unknown prefix '{prefix}:'")))?;
        Ok(format!("{base}{}", unescape(local)?))
    }

    fn parse_blank_label(&mut self) -> Result<Term, String> {
        self.pos += 2;
        let start = self.pos;
        while self.pos < self.bytes.len() {
            let byte = self.bytes[self.pos];
            if byte.is_ascii_whitespace() || matches!(byte, b';' | b',' | b']' | b')' | b'.') {
                break;
            }
            self.pos += 1;
        }
        let label = self.src[start..self.pos].to_string();
        if label.is_empty() {
            return Err(self.error("blank node label is empty"));
        }
        if let Some(stored) = self.labels.get(&label) {
            return Ok(Term::Blank(stored.clone()));
        }
        let stored = self.fresh_blank();
        self.labels.insert(label, stored.clone());
        Ok(Term::Blank(stored))
    }

    fn parse_blank_property_list(&mut self) -> Result<Term, String> {
        self.expect_byte(b'[')?;
        let node = Term::Blank(self.fresh_blank());
        self.skip_ws_comments();
        if self.peek_byte() == Some(b']') {
            self.pos += 1;
            return Ok(node);
        }
        self.parse_predicate_object_list(&node, false)?;
        self.skip_ws_comments();
        self.expect_byte(b']')?;
        Ok(node)
    }

    fn parse_collection(&mut self) -> Result<Term, String> {
        self.expect_byte(b'(')?;
        self.skip_ws_comments();
        if self.peek_byte() == Some(b')') {
            self.pos += 1;
            return Ok(Term::Iri(RDF_NIL.to_string()));
        }
        let head = Term::Blank(self.fresh_blank());
        let mut tail = head.clone();
        loop {
            self.skip_ws_comments();
            if self.peek_byte() == Some(b')') {
                self.pos += 1;
                self.graph.triples.push(Triple {
                    subject: tail,
                    predicate: RDF_REST.to_string(),
                    object: Term::Iri(RDF_NIL.to_string()),
                });
                return Ok(head);
            }
            let item = self.parse_term(true)?;
            self.graph.triples.push(Triple {
                subject: tail.clone(),
                predicate: RDF_FIRST.to_string(),
                object: item,
            });
            let next = Term::Blank(self.fresh_blank());
            self.graph.triples.push(Triple {
                subject: tail,
                predicate: RDF_REST.to_string(),
                object: next.clone(),
            });
            tail = next;
        }
    }

    fn parse_literal(&mut self) -> Result<Term, String> {
        let quote = self.bytes[self.pos];
        self.pos += 1;
        let mut value = String::new();
        loop {
            if self.pos >= self.bytes.len() {
                return Err(self.error("unterminated literal"));
            }
            match self.bytes[self.pos] {
                byte if byte == quote => {
                    self.pos += 1;
                    break;
                }
                b'\\' => {
                    self.pos += 1;
                    let escaped = self.read_escape()?;
                    value.push_str(&escaped);
                }
                _ => {
                    let ch = self.src[self.pos..].chars().next().unwrap();
                    value.push(ch);
                    self.pos += ch.len_utf8();
                }
            }
        }
        let mut datatype = None;
        let mut lang = None;
        if self.peek_byte() == Some(b'@') {
            self.pos += 1;
            let start = self.pos;
            while self.pos < self.bytes.len() {
                let byte = self.bytes[self.pos];
                if byte.is_ascii_alphanumeric() || byte == b'-' {
                    self.pos += 1;
                } else {
                    break;
                }
            }
            if start == self.pos {
                return Err(self.error("empty language tag"));
            }
            lang = Some(self.src[start..self.pos].to_string());
        } else if self.bytes.get(self.pos..self.pos + 2) == Some(b"^^") {
            self.pos += 2;
            let term = self.parse_term(false)?;
            datatype = Some(match term {
                Term::Iri(iri) => iri,
                _ => return Err(self.error("datatype must be an IRI")),
            });
        }
        Ok(Term::Literal {
            value,
            datatype: datatype.or_else(|| lang.is_none().then(|| XSD_STRING.to_string())),
            lang,
        })
    }

    fn parse_numeric_literal(&mut self) -> Result<Term, String> {
        let start = self.pos;
        let token = self.read_token();
        if token.is_empty() {
            return Err(self.error_at(start, "expected a numeric literal"));
        }
        let datatype = if token.contains(['e', 'E']) {
            XSD_DOUBLE
        } else if token.contains('.') {
            XSD_DECIMAL
        } else {
            XSD_INTEGER
        };
        token
            .parse::<f64>()
            .map_err(|_| self.error_at(start, "invalid numeric literal"))?;
        Ok(Term::Literal {
            value: token,
            datatype: Some(datatype.to_string()),
            lang: None,
        })
    }

    fn read_escape(&mut self) -> Result<String, String> {
        let Some(byte) = self.peek_byte() else {
            return Err(self.error("trailing escape"));
        };
        self.pos += 1;
        match byte {
            b't' => Ok("\t".into()),
            b'b' => Ok("\u{8}".into()),
            b'n' => Ok("\n".into()),
            b'r' => Ok("\r".into()),
            b'f' => Ok("\u{c}".into()),
            b'"' => Ok("\"".into()),
            b'\'' => Ok("'".into()),
            b'\\' => Ok("\\".into()),
            b'u' => self.read_unicode_escape(4),
            b'U' => self.read_unicode_escape(8),
            _ => Err(self.error("unknown escape sequence")),
        }
    }

    fn read_unicode_escape(&mut self, digits: usize) -> Result<String, String> {
        if self.pos + digits > self.bytes.len() {
            return Err(self.error("truncated unicode escape"));
        }
        let text = &self.src[self.pos..self.pos + digits];
        let code =
            u32::from_str_radix(text, 16).map_err(|_| self.error("invalid unicode escape"))?;
        self.pos += digits;
        char::from_u32(code)
            .map(|ch| ch.to_string())
            .ok_or_else(|| self.error("invalid unicode scalar"))
    }

    fn resolve_reference(&self, reference: &str) -> String {
        match &self.base {
            Some(base) => resolve_iri(base, reference),
            None => reference.to_string(),
        }
    }

    fn read_token(&mut self) -> String {
        let start = self.pos;
        while self.pos < self.bytes.len() {
            let byte = self.bytes[self.pos];
            if byte.is_ascii_whitespace() || matches!(byte, b';' | b',' | b'(' | b')' | b'[' | b']')
            {
                break;
            }
            // A dot ends a token only when it is the last character before a
            // delimiter; dots inside a name (e.g. `lv2:minorVersion`) are data.
            self.pos += 1;
        }
        let mut end = self.pos;
        while end > start && self.bytes[end - 1] == b'.' {
            end -= 1;
        }
        if end != self.pos {
            self.pos = end;
        }
        self.src[start..end].to_string()
    }

    fn starts_with_keyword(&self, keyword: &str) -> bool {
        let rest = &self.src[self.pos..];
        let Some(head) = rest.get(..keyword.len()) else {
            return false;
        };
        head.eq_ignore_ascii_case(keyword)
            && rest
                .get(keyword.len()..)
                .is_some_and(|tail| tail.starts_with(char::is_whitespace))
    }

    fn skip_ws_comments(&mut self) {
        loop {
            while self.pos < self.bytes.len() && self.bytes[self.pos].is_ascii_whitespace() {
                self.pos += 1;
            }
            if self.peek_byte() == Some(b'#') {
                while self.pos < self.bytes.len() && self.bytes[self.pos] != b'\n' {
                    self.pos += 1;
                }
                continue;
            }
            break;
        }
    }

    fn peek_byte(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn expect_byte(&mut self, expected: u8) -> Result<(), String> {
        if self.peek_byte() == Some(expected) {
            self.pos += 1;
            Ok(())
        } else {
            Err(self.error(&format!("expected '{}'", expected as char)))
        }
    }

    fn fresh_blank(&mut self) -> String {
        self.blank_counter += 1;
        format!("b{}", self.blank_counter)
    }

    fn error(&self, message: &str) -> String {
        self.error_at(self.pos, message)
    }

    fn error_at(&self, position: usize, message: &str) -> String {
        let line = self.src[..position.min(self.src.len())]
            .bytes()
            .filter(|byte| *byte == b'\n')
            .count()
            + 1;
        format!("Turtle error on line {line}: {message}")
    }
}

fn is_prefix_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.')
}

fn unescape(text: &str) -> Result<String, String> {
    if !text.contains('\\') {
        return Ok(text.to_string());
    }
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            out.push(ch);
            continue;
        }
        let Some(escaped) = chars.next() else {
            return Err("trailing escape in IRI".into());
        };
        match escaped {
            'u' | 'U' => {
                let digits = if escaped == 'u' { 4 } else { 8 };
                let mut code = String::new();
                for _ in 0..digits {
                    code.push(chars.next().ok_or("truncated unicode escape")?);
                }
                let value = u32::from_str_radix(&code, 16).map_err(|_| "invalid unicode escape")?;
                out.push(char::from_u32(value).ok_or("invalid unicode scalar")?);
            }
            other => out.push(other),
        }
    }
    Ok(out)
}

fn has_scheme(uri: &str) -> bool {
    let mut chars = uri.chars();
    match chars.next() {
        Some(first) if first.is_ascii_alphabetic() => {}
        _ => return false,
    }
    for ch in chars {
        match ch {
            ':' => return true,
            ch if ch.is_ascii_alphanumeric() || matches!(ch, '+' | '-' | '.') => {}
            _ => return false,
        }
    }
    false
}

/// RFC 3986 reference resolution, sufficient for bundle-relative IRIs.
pub fn resolve_iri(base: &str, reference: &str) -> String {
    if has_scheme(reference) {
        return strip_dot_segments_uri(reference);
    }
    let base = base.split('#').next().unwrap_or(base);
    if reference.is_empty() {
        return base.to_string();
    }
    if let Some(fragment) = reference.strip_prefix('#') {
        return format!("{base}#{fragment}");
    }
    let (scheme, authority, path) = split_base(base);
    if let Some(rest) = reference.strip_prefix("//") {
        return format!("{scheme}://{rest}");
    }
    let merged = if reference.starts_with('/') {
        reference.to_string()
    } else if path.is_empty() {
        format!("/{reference}")
    } else {
        let cut = path.rfind('/').map_or(0, |index| index + 1);
        format!("{}{reference}", &path[..cut])
    };
    let mut result = String::new();
    result.push_str(scheme);
    result.push(':');
    if let Some(authority) = authority {
        result.push_str("//");
        result.push_str(authority);
        result.push_str(&normalize_path(&merged));
    } else {
        result.push_str(&normalize_path(&merged));
    }
    result
}

fn split_base(base: &str) -> (&str, Option<&str>, &str) {
    let Some(colon) = base.find(':') else {
        return ("", None, base);
    };
    let scheme = &base[..colon];
    let rest = &base[colon + 1..];
    if let Some(without) = rest.strip_prefix("//") {
        let end = without.find('/').unwrap_or(without.len());
        let authority = &without[..end];
        let path = &without[end..];
        (scheme, Some(authority), path)
    } else {
        (scheme, None, rest)
    }
}

fn strip_dot_segments_uri(uri: &str) -> String {
    let (scheme, authority, path) = split_base(uri);
    if path.is_empty() {
        return uri.to_string();
    }
    let mut result = String::from(scheme);
    result.push(':');
    if let Some(authority) = authority {
        result.push_str("//");
        result.push_str(authority);
    }
    result.push_str(&normalize_path(path));
    if let Some((_, query)) = uri.split_once('?') {
        result.push('?');
        result.push_str(query);
    }
    result
}

fn normalize_path(path: &str) -> String {
    let absolute = path.starts_with('/');
    let trailing = path.ends_with('/');
    let mut segments: Vec<&str> = Vec::new();
    for segment in path.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                segments.pop();
            }
            other => segments.push(other),
        }
    }
    let mut result = String::new();
    if absolute {
        result.push('/');
    }
    result.push_str(&segments.join("/"));
    if trailing && !result.ends_with('/') {
        result.push('/');
    }
    result
}

/// Convert a `file:` IRI to a filesystem path.
pub fn file_uri_to_path(uri: &str) -> Option<std::path::PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    #[cfg(not(windows))]
    let rest = match rest.strip_prefix("localhost") {
        Some(path) if path.starts_with('/') => path,
        _ => rest,
    };
    #[cfg(windows)]
    let rest = match rest.strip_prefix("localhost/") {
        // A drive-qualified localhost URI names a local disk; otherwise retain
        // localhost as the server of a UNC share, like every other authority.
        Some(path) if path.as_bytes().get(1) == Some(&b':') => path,
        _ => rest,
    };
    let decoded = percent_decode(rest)?;
    #[cfg(windows)]
    let decoded = if let Some(drive) = decoded
        .strip_prefix('/')
        .filter(|value| value.as_bytes().get(1) == Some(&b':'))
    {
        drive.to_string()
    } else if !decoded.starts_with(['/', '\\']) && decoded.as_bytes().get(1) != Some(&b':') {
        // A nonlocal URI authority is a UNC server, not a relative directory.
        format!("//{decoded}")
    } else {
        decoded
    };
    Some(std::path::PathBuf::from(decoded))
}

/// Encode a filesystem path as a file URI.
pub fn path_to_file_uri(path: &std::path::Path) -> String {
    let text = path.to_string_lossy();
    #[cfg(windows)]
    let text = {
        // URI reference resolution recognizes '/', never Windows '\\'. Strip
        // the filesystem-only verbatim prefix while retaining drive/UNC identity.
        let slashes = text.replace('\\', "/");
        if let Some(unc) = slashes.strip_prefix("//?/UNC/") {
            format!("//{unc}")
        } else {
            slashes.strip_prefix("//?/").unwrap_or(&slashes).to_string()
        }
    };
    let mut encoded = String::with_capacity(text.len() + 8);
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' | b':' => {
                encoded.push(byte as char)
            }
            other => encoded.push_str(&format!("%{other:02X}")),
        }
    }
    #[cfg(windows)]
    if encoded.starts_with("//") {
        return format!("file:{encoded}");
    }
    if encoded.starts_with('/') {
        format!("file://{encoded}")
    } else {
        format!("file:///{encoded}")
    }
}

fn percent_decode(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let hex = text.get(index + 1..index + 3)?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            index += 3;
        } else {
            out.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(out).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn graph(input: &str) -> Graph {
        parse(input, Some("file:///tmp/bundle/manifest.ttl")).unwrap()
    }

    #[test]
    fn prefixed_names_predicate_lists_and_comments() {
        let graph = graph(
            r#"
            @prefix lv2: <http://lv2plug.in/ns/lv2core#> .
            @prefix doap: <http://usefulinc.com/ns/doap#> .
            # a comment
            <http://example.org/gain>
                a lv2:Plugin ;
                doap:name "Gain", "Gain Unit" ;
                lv2:optionalFeature lv2:hardRTCapable .
            "#,
        );
        let subject = Term::iri("http://example.org/gain");
        assert!(graph.has_type(&subject, "http://lv2plug.in/ns/lv2core#Plugin"));
        let names: Vec<_> = graph
            .objects(&subject, "http://usefulinc.com/ns/doap#name")
            .filter_map(Term::as_str)
            .collect();
        assert_eq!(names, ["Gain", "Gain Unit"]);
        assert!(graph.has(
            &subject,
            "http://lv2plug.in/ns/lv2core#optionalFeature",
            &Term::iri("http://lv2plug.in/ns/lv2core#hardRTCapable")
        ));
    }

    #[test]
    fn blank_property_lists_and_typed_literals() {
        let graph = graph(
            r#"
            @prefix lv2: <http://lv2plug.in/ns/lv2core#> .
            <urn:plugin> lv2:port [
                lv2:index 4 ;
                lv2:symbol "gain" ;
                lv2:minimum 0.0 ;
                lv2:maximum "4"^^<http://www.w3.org/2001/XMLSchema#float> ;
                lv2:default 1
            ] .
            "#,
        );
        let subject = Term::iri("urn:plugin");
        let port = graph
            .object(&subject, "http://lv2plug.in/ns/lv2core#port")
            .expect("port");
        let Term::Blank(_) = port else {
            panic!("expected a blank node")
        };
        assert_eq!(
            graph
                .object(port, "http://lv2plug.in/ns/lv2core#index")
                .and_then(Term::as_i64),
            Some(4)
        );
        assert_eq!(
            graph
                .object(port, "http://lv2plug.in/ns/lv2core#maximum")
                .and_then(Term::as_f64),
            Some(4.0)
        );
        assert_eq!(
            graph
                .object(port, "http://lv2plug.in/ns/lv2core#default")
                .and_then(Term::as_f64),
            Some(1.0)
        );
    }

    #[test]
    fn relative_iris_resolve_against_the_manifest() {
        let graph = graph(
            r#"
            @prefix lv2: <http://lv2plug.in/ns/lv2core#> .
            <gain> a lv2:Plugin ; lv2:binary <../build/gain.dylib> .
            "#,
        );
        assert_eq!(
            graph
                .object(
                    &Term::iri("file:///tmp/bundle/gain"),
                    "http://lv2plug.in/ns/lv2core#binary"
                )
                .and_then(Term::as_str),
            Some("file:///tmp/build/gain.dylib")
        );
    }

    #[test]
    fn absolute_path_and_base_directives() {
        let graph = parse(
            r#"
            @base <file:///opt/plugins/demo/manifest.ttl> .
            @prefix lv2: <http://lv2plug.in/ns/lv2core#> .
            <plugin> a lv2:Plugin ;
                lv2:binary </opt/plugins/demo/libdemo.so> .
            "#,
            None,
        )
        .unwrap();
        assert_eq!(
            graph
                .object(
                    &Term::iri("file:///opt/plugins/demo/plugin"),
                    "http://lv2plug.in/ns/lv2core#binary"
                )
                .and_then(Term::as_str),
            Some("file:///opt/plugins/demo/libdemo.so")
        );
    }

    #[test]
    fn collections_and_boolean_literals() {
        let graph = graph(
            r#"
            @prefix lv2: <http://lv2plug.in/ns/lv2core#> .
            <urn:p> lv2:requiredFeature ( <urn:a> <urn:b> ) .
            <urn:p> lv2:hardRTCapable true .
            "#,
        );
        let subject = Term::iri("urn:p");
        let list = graph
            .object(&subject, "http://lv2plug.in/ns/lv2core#requiredFeature")
            .expect("list");
        let first = graph
            .object(list, RDF_FIRST)
            .and_then(Term::as_str)
            .unwrap();
        assert_eq!(first, "urn:a");
        let rest = graph.object(list, RDF_REST).expect("rest");
        assert_eq!(
            graph.object(rest, RDF_FIRST).and_then(Term::as_str),
            Some("urn:b")
        );
        assert_eq!(
            graph
                .object(&subject, "http://lv2plug.in/ns/lv2core#hardRTCapable")
                .and_then(Term::as_str),
            Some("true")
        );
    }

    #[test]
    fn escaped_literals_and_language_tags() {
        let graph = graph(
            r#"
            @prefix doap: <http://usefulinc.com/ns/doap#> .
            <urn:p> doap:name "A \"quoted\" name\n"@en .
            "#,
        );
        let subject = Term::iri("urn:p");
        let name = graph
            .object(&subject, "http://usefulinc.com/ns/doap#name")
            .expect("name");
        assert_eq!(
            name,
            &Term::Literal {
                value: "A \"quoted\" name\n".into(),
                datatype: None,
                lang: Some("en".into())
            }
        );
    }

    #[test]
    fn malformed_documents_report_an_error() {
        for source in [
            "@prefix lv2: <http://lv2plug.in/ns/lv2core#>",
            "<urn:p> <urn:q> \"unterminated .",
            "<urn:p> <urn:q> <urn:r>",
            "@prefix missing <urn:x> .",
        ] {
            assert!(parse(source, None).is_err(), "{source}");
        }
    }

    #[test]
    fn file_uri_round_trip_handles_spaces() {
        let path = std::path::Path::new("/tmp/A B/c.dylib");
        let uri = path_to_file_uri(path);
        assert_eq!(uri, "file:///tmp/A%20B/c.dylib");
        assert_eq!(file_uri_to_path(&uri).as_deref(), Some(path));
    }

    #[cfg(windows)]
    #[test]
    fn windows_file_uri_preserves_drive_paths_and_relative_binaries() {
        let manifest = std::path::Path::new(r"C:\Audio Projects\Piano Ω.lv2\manifest.ttl");
        let uri = path_to_file_uri(manifest);
        assert_eq!(
            uri,
            "file:///C:/Audio%20Projects/Piano%20%CE%A9.lv2/manifest.ttl"
        );
        assert_eq!(file_uri_to_path(&uri).as_deref(), Some(manifest));
        let binary = resolve_iri(&uri, "plugin.dll");
        assert_eq!(
            file_uri_to_path(&binary).as_deref(),
            Some(std::path::Path::new(
                r"C:\Audio Projects\Piano Ω.lv2\plugin.dll"
            ))
        );
        assert_eq!(
            file_uri_to_path("file://localhost/C:/Audio%20Projects/plugin.dll").as_deref(),
            Some(std::path::Path::new(r"C:\Audio Projects\plugin.dll"))
        );
        assert_eq!(
            path_to_file_uri(std::path::Path::new(r"\\?\C:\Audio Projects\manifest.ttl")),
            "file:///C:/Audio%20Projects/manifest.ttl"
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_file_uri_preserves_unc_authority_through_relative_resolution() {
        let manifest = std::path::Path::new(r"\\server\Shared Audio\Piano.lv2\manifest.ttl");
        let uri = path_to_file_uri(manifest);
        assert_eq!(uri, "file://server/Shared%20Audio/Piano.lv2/manifest.ttl");
        assert_eq!(file_uri_to_path(&uri).as_deref(), Some(manifest));
        let binary = resolve_iri(&uri, "../plugin.dll");
        assert_eq!(
            file_uri_to_path(&binary).as_deref(),
            Some(std::path::Path::new(r"\\server\Shared Audio\plugin.dll"))
        );
        assert_eq!(
            path_to_file_uri(std::path::Path::new(
                r"\\?\UNC\server\Shared Audio\Piano.lv2\manifest.ttl"
            )),
            uri
        );
        let local_server = std::path::Path::new(r"\\localhost\share\plugin.dll");
        assert_eq!(
            file_uri_to_path(&path_to_file_uri(local_server)).as_deref(),
            Some(local_server)
        );
        // A server whose name starts with localhost keeps its full authority.
        assert_eq!(
            file_uri_to_path("file://localhost-a/share/plugin.dll").as_deref(),
            Some(std::path::Path::new(r"\\localhost-a\share\plugin.dll"))
        );
    }
}
