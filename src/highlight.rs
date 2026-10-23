//! Small, fast, table-driven syntax highlighter. It recognises comments,
//! strings, numbers, keywords, types and calls for common languages, which is
//! what makes code readable; it is deliberately not a full grammar engine
//! (no startup cost, no grammar files).

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Tok {
    Kw,
    Str,
    Com,
    Num,
    Ty,
    Fun,
    Add,
    Del,
    Meta,
}

struct Spec {
    kws: &'static [&'static str],
    line: &'static [&'static str],
    block: Option<(&'static str, &'static str)>,
    quotes: &'static [u8],
    caps_are_types: bool,
    ci: bool,
    vars: bool,
}

const C_LIKE: &[&str] = &[
    "auto", "break", "case", "char", "const", "continue", "default", "do", "double", "else", "enum", "extern",
    "float", "for", "goto", "if", "inline", "int", "long", "register", "return", "short", "signed", "sizeof",
    "static", "struct", "switch", "typedef", "union", "unsigned", "void", "volatile", "while", "bool", "true",
    "false", "NULL", "nullptr", "class", "namespace", "template", "typename", "public", "private", "protected",
    "virtual", "override", "new", "delete", "this", "using", "try", "catch", "throw", "constexpr", "noexcept",
    "include", "define", "ifdef", "ifndef", "endif", "pragma",
];
const RUST: &[&str] = &[
    "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum", "extern", "false", "fn",
    "for", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub", "ref", "return", "self",
    "Self", "static", "struct", "super", "trait", "true", "type", "unsafe", "use", "where", "while", "u8", "u16",
    "u32", "u64", "u128", "usize", "i8", "i16", "i32", "i64", "i128", "isize", "f32", "f64", "bool", "char", "str",
];
const JS: &[&str] = &[
    "async", "await", "break", "case", "catch", "class", "const", "continue", "debugger", "default", "delete",
    "do", "else", "export", "extends", "false", "finally", "for", "from", "function", "if", "import", "in",
    "instanceof", "let", "new", "null", "of", "return", "static", "super", "switch", "this", "throw", "true",
    "try", "typeof", "undefined", "var", "void", "while", "yield", "interface", "type", "enum", "implements",
    "readonly", "as", "string", "number", "boolean", "any", "unknown", "never",
];
const PY: &[&str] = &[
    "False", "None", "True", "and", "as", "assert", "async", "await", "break", "class", "continue", "def", "del",
    "elif", "else", "except", "finally", "for", "from", "global", "if", "import", "in", "is", "lambda",
    "nonlocal", "not", "or", "pass", "raise", "return", "try", "while", "with", "yield", "self", "match", "case",
];
const GO: &[&str] = &[
    "break", "case", "chan", "const", "continue", "default", "defer", "else", "fallthrough", "for", "func", "go",
    "goto", "if", "import", "interface", "map", "package", "range", "return", "select", "struct", "switch",
    "type", "var", "nil", "true", "false", "string", "int", "int64", "uint", "byte", "error", "bool", "float64",
];
const JAVA: &[&str] = &[
    "abstract", "boolean", "break", "byte", "case", "catch", "char", "class", "const", "continue", "default",
    "do", "double", "else", "enum", "extends", "final", "finally", "float", "for", "if", "implements", "import",
    "instanceof", "int", "interface", "long", "new", "null", "package", "private", "protected", "public",
    "return", "short", "static", "super", "switch", "this", "throw", "throws", "try", "void", "while", "true",
    "false", "val", "var", "fun", "when", "object", "override", "data", "sealed", "is", "in", "out",
];
const SH: &[&str] = &[
    "if", "then", "else", "elif", "fi", "for", "while", "until", "do", "done", "case", "esac", "in", "function",
    "return", "local", "export", "readonly", "echo", "exit", "source", "set", "unset", "cd", "sudo",
];
const SQL: &[&str] = &[
    "select", "from", "where", "insert", "into", "values", "update", "set", "delete", "create", "table", "drop",
    "alter", "index", "primary", "key", "foreign", "references", "join", "left", "right", "inner", "outer",
    "on", "group", "by", "order", "having", "limit", "offset", "as", "and", "or", "not", "null", "is", "in",
    "distinct", "union", "all", "case", "when", "then", "else", "end", "with", "default", "integer", "text",
    "varchar", "boolean", "count", "exists", "like", "between", "asc", "desc", "returning",
];
const RUBY: &[&str] = &[
    "def", "end", "class", "module", "if", "elsif", "else", "unless", "while", "until", "for", "in", "do",
    "return", "yield", "begin", "rescue", "ensure", "raise", "nil", "true", "false", "self", "require", "puts",
    "attr_accessor", "then", "case", "when", "lambda", "proc",
];
const LUA: &[&str] = &[
    "and", "break", "do", "else", "elseif", "end", "false", "for", "function", "if", "in", "local", "nil", "not",
    "or", "repeat", "return", "then", "true", "until", "while",
];
const DATA: &[&str] = &["true", "false", "null", "yes", "no", "on", "off"];

fn spec(lang: &str) -> Option<Spec> {
    let s = |kws, line, block, quotes, caps_are_types| Spec { kws, line, block, quotes, caps_are_types, ci: false, vars: false };
    let cblock = Some(("/*", "*/"));
    Some(match lang {
        "rust" | "rs" => s(RUST, &["//"], cblock, b"\"", true),
        "c" | "h" | "cpp" | "c++" | "cc" | "hpp" | "cxx" | "objc" | "cs" | "csharp" | "zig" | "swift" | "dart" => {
            s(C_LIKE, &["//"], cblock, b"\"'", true)
        }
        "js" | "javascript" | "jsx" | "ts" | "typescript" | "tsx" | "mjs" | "cjs" => s(JS, &["//"], cblock, b"\"'`", true),
        "py" | "python" | "python3" => s(PY, &["#"], None, b"\"'", true),
        "go" | "golang" => s(GO, &["//"], cblock, b"\"'`", true),
        "java" | "kotlin" | "kt" | "scala" | "groovy" => s(JAVA, &["//"], cblock, b"\"'", true),
        "sh" | "bash" | "shell" | "zsh" | "fish" | "console" | "shellsession" => Spec { vars: true, ..s(SH, &["#"], None, b"\"'", false) },
        "sql" | "psql" | "mysql" | "sqlite" => Spec { ci: true, ..s(SQL, &["--"], cblock, b"'\"", false) },
        "rb" | "ruby" => s(RUBY, &["#"], None, b"\"'", true),
        "lua" => s(LUA, &["--"], Some(("--[[", "]]")), b"\"'", false),
        "json" | "jsonc" | "json5" => s(DATA, &["//"], cblock, b"\"", false),
        "yaml" | "yml" | "toml" | "ini" | "conf" | "cfg" | "dockerfile" | "makefile" | "make" | "nix" | "r" | "perl" | "pl" => {
            s(DATA, &["#"], None, b"\"'", false)
        }
        "css" | "scss" | "less" => s(&[], &["//"], cblock, b"\"'", false),
        "html" | "xml" | "svg" | "vue" | "svelte" => s(&[], &[], Some(("<!--", "-->")), b"\"'", false),
        "haskell" | "hs" | "elm" => s(&["module", "import", "where", "let", "in", "case", "of", "if", "then", "else", "data", "type", "class", "instance", "deriving"], &["--"], Some(("{-", "-}")), b"\"", true),
        "php" => Spec { vars: true, ..s(C_LIKE, &["//", "#"], cblock, b"\"'", true) },
        _ => return None,
    })
}

/// Returns (start, end, token) byte ranges; unlisted text is plain.
pub fn highlight(lang: &str, text: &str) -> Vec<(u32, u32, Tok)> {
    let mut out = Vec::new();
    match lang {
        "diff" | "patch" => {
            let mut at = 0;
            for line in text.split_inclusive('\n') {
                let tok = match line.as_bytes().first() {
                    Some(b'+') if !line.starts_with("+++") => Some(Tok::Add),
                    Some(b'-') if !line.starts_with("---") => Some(Tok::Del),
                    Some(b'@') => Some(Tok::Meta),
                    _ if line.starts_with("diff ") || line.starts_with("+++") || line.starts_with("---") => Some(Tok::Kw),
                    _ => None,
                };
                if let Some(t) = tok {
                    out.push((at as u32, (at + line.trim_end_matches('\n').len()) as u32, t));
                }
                at += line.len();
            }
            return out;
        }
        _ => {}
    }
    let Some(sp) = spec(lang) else { return out };
    let markup = matches!(lang, "html" | "xml" | "svg" | "vue" | "svelte");
    let css = matches!(lang, "css" | "scss" | "less");
    let kv = matches!(lang, "yaml" | "yml" | "toml" | "ini" | "json" | "jsonc" | "json5");
    let rust = matches!(lang, "rust" | "rs");
    let b = text.as_bytes();
    let n = b.len();
    let mut i = 0;
    let mut line_start = true;
    let mut in_tag = false;
    let ident = |c: u8| c.is_ascii_alphanumeric() || c == b'_';
    while i < n {
        let c = b[i];
        if c == b'\n' {
            line_start = true;
            in_tag = false;
            i += 1;
            continue;
        }
        let rest = &text[i..];
        if let Some((open, close)) = sp.block
            && rest.starts_with(open) {
                let end = rest[open.len()..].find(close).map_or(n, |e| i + open.len() + e + close.len());
                out.push((i as u32, end as u32, Tok::Com));
                i = end;
                continue;
            }
        if sp.line.iter().any(|l| rest.starts_with(l)) && !(c == b'#' && i > 0 && ident(b[i - 1])) {
            let end = rest.find('\n').map_or(n, |e| i + e);
            out.push((i as u32, end as u32, Tok::Com));
            i = end;
            continue;
        }
        if markup {
            if c == b'<' {
                let mut j = i + 1;
                while j < n && (ident(b[j]) || b[j] == b'/' || b[j] == b'-' || b[j] == b'!' || b[j] == b':') {
                    j += 1;
                }
                out.push((i as u32, j as u32, Tok::Kw));
                in_tag = true;
                i = j;
                continue;
            }
            if c == b'>' {
                in_tag = false;
            }
            if !in_tag && c != b'"' && c != b'\'' {
                i += 1;
                continue;
            }
        }
        if sp.quotes.contains(&c) && !(rust && c == b'\'' && is_lifetime(b, i)) {
            let mut j = i + 1;
            while j < n && b[j] != c {
                if b[j] == b'\\' {
                    j += 1;
                } else if b[j] == b'\n' && c != b'`' && !markup {
                    break;
                }
                j += 1;
            }
            let end = (j + 1).min(n);
            let tok = if kv && text[end..].trim_start().starts_with(':') { Tok::Ty } else { Tok::Str };
            out.push((i as u32, end as u32, tok));
            i = end;
            line_start = false;
            continue;
        }
        if sp.vars && c == b'$' {
            let mut j = i + 1;
            if j < n && b[j] == b'{' {
                while j < n && b[j] != b'}' {
                    j += 1;
                }
                j += 1;
            } else {
                while j < n && ident(b[j]) {
                    j += 1;
                }
            }
            out.push((i as u32, j.min(n) as u32, Tok::Ty));
            i = j.min(n);
            continue;
        }
        if c.is_ascii_digit() && (i == 0 || !ident(b[i - 1])) {
            let mut j = i;
            while j < n && (b[j].is_ascii_alphanumeric() || b[j] == b'.' || b[j] == b'_') {
                j += 1;
            }
            out.push((i as u32, j as u32, Tok::Num));
            i = j;
            line_start = false;
            continue;
        }
        if ident(c) || (css && c == b'-') {
            let mut j = i;
            while j < n && (ident(b[j]) || ((css || markup || lang == "yaml" || lang == "yml") && b[j] == b'-')) {
                j += 1;
            }
            let word = &text[i..j];
            let after = text[j..].trim_start_matches([' ', '\t']);
            let key = (kv || css) && after.starts_with([':', '=']) && (line_start || css);
            let tok = if markup || key {
                Some(Tok::Ty)
            } else if is_kw(&sp, word) {
                Some(Tok::Kw)
            } else if after.starts_with('(') || (rust && after.starts_with('!')) {
                Some(Tok::Fun)
            } else if sp.caps_are_types && word.as_bytes()[0].is_ascii_uppercase() {
                Some(Tok::Ty)
            } else {
                None
            };
            if let Some(t) = tok {
                out.push((i as u32, j as u32, t));
            }
            i = j;
            line_start = false;
            continue;
        }
        if !c.is_ascii_whitespace() {
            line_start = false;
        }
        i += 1;
        while i < n && !text.is_char_boundary(i) {
            i += 1;
        }
    }
    out
}

fn is_kw(sp: &Spec, w: &str) -> bool {
    if sp.ci {
        sp.kws.iter().any(|k| k.eq_ignore_ascii_case(w))
    } else {
        sp.kws.contains(&w)
    }
}

/// Rust lifetimes ('a) vs char literals ('a').
fn is_lifetime(b: &[u8], i: usize) -> bool {
    !(b.get(i + 2) == Some(&b'\'') || b.get(i + 1) == Some(&b'\\'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rust_tokens() {
        let src = "fn main() { let s = \"hi\"; // c\n}";
        let toks: Vec<_> = highlight("rust", src).into_iter().map(|(s, e, t)| (&src[s as usize..e as usize], t)).collect();
        assert!(toks.contains(&("fn", Tok::Kw)));
        assert!(toks.contains(&("main", Tok::Fun)));
        assert!(toks.contains(&("\"hi\"", Tok::Str)));
        assert!(toks.contains(&("// c", Tok::Com)));
    }
}
