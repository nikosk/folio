//! Lightweight TeX → Unicode for math spans. Not a typesetter: it maps
//! symbols to Unicode and turns ^/_ groups into superscript/subscript runs,
//! which covers the inline math found in most notes and READMEs.

use crate::doc::st;

/// Returns (text, extra style) runs; extra style is 0, SUP or SUB.
pub fn render(src: &str) -> Vec<(String, u16)> {
    let mut out = Vec::new();
    let chars: Vec<char> = src.chars().collect();
    emit(&chars, 0, &mut out);
    out
}

fn push(out: &mut Vec<(String, u16)>, s: &str, style: u16) {
    if s.is_empty() {
        return;
    }
    match out.last_mut() {
        Some((t, st)) if *st == style => t.push_str(s),
        _ => out.push((s.to_string(), style)),
    }
}

fn emit(c: &[char], style: u16, out: &mut Vec<(String, u16)>) {
    let mut i = 0;
    while i < c.len() {
        match c[i] {
            '\\' => {
                let (name, next) = command(c, i);
                i = next;
                match name.as_str() {
                    "frac" | "dfrac" | "tfrac" => {
                        let (a, n1) = group(c, i);
                        let (b, n2) = group(c, n1);
                        i = n2;
                        wrap(&a, style, out);
                        push(out, "/", style);
                        wrap(&b, style, out);
                    }
                    "sqrt" => {
                        let (a, n) = group(c, i);
                        i = n;
                        push(out, "√", style);
                        wrap(&a, style, out);
                    }
                    "text" | "mathrm" | "textrm" | "mathit" | "operatorname" | "mathbf" | "textbf" | "boldsymbol"
                    | "mathsf" | "mathtt" | "hat" | "bar" | "vec" | "overline" | "tilde" | "dot" => {
                        let (a, n) = group(c, i);
                        i = n;
                        emit(&a, style, out);
                        let accent = match name.as_str() {
                            "hat" => "\u{302}",
                            "bar" | "overline" => "\u{304}",
                            "vec" => "\u{20d7}",
                            "tilde" => "\u{303}",
                            "dot" => "\u{307}",
                            _ => "",
                        };
                        push(out, accent, style);
                    }
                    "mathbb" | "mathcal" => {
                        let (a, n) = group(c, i);
                        i = n;
                        let s: String = a.iter().map(|&ch| styled_letter(ch, name == "mathbb")).collect();
                        push(out, &s, style);
                    }
                    "left" | "right" | "big" | "Big" | "bigg" | "Bigg" | "displaystyle" | "limits" => {}
                    "," | ":" | ";" | " " | "quad" => push(out, " ", style),
                    "qquad" => push(out, "  ", style),
                    "\\" => push(out, "\n", style),
                    "{" | "}" | "$" | "%" | "&" | "#" | "_" => push(out, &name, style),
                    _ => push(out, symbol(&name).unwrap_or(&name), style),
                }
            }
            op @ ('^' | '_') => {
                let (g, n) = group(c, i + 1);
                i = n;
                let kind = if op == '^' { st::SUP } else { st::SUB };
                if style == 0 {
                    emit(&g, kind, out);
                } else {
                    // Already raised/lowered: nest using Unicode script characters.
                    let mut inner = Vec::new();
                    emit(&g, 0, &mut inner);
                    let s: String = inner.iter().map(|(t, _)| t.as_str()).collect();
                    match s.chars().map(|ch| script_char(ch, kind == st::SUP)).collect::<Option<String>>() {
                        Some(mapped) => push(out, &mapped, style),
                        None => push(out, &s, style),
                    }
                }
            }
            '{' | '}' => i += 1,
            '~' => {
                push(out, " ", style);
                i += 1;
            }
            ch => {
                let mut b = [0u8; 4];
                push(out, ch.encode_utf8(&mut b), style);
                i += 1;
            }
        }
    }
}

fn wrap(g: &[char], style: u16, out: &mut Vec<(String, u16)>) {
    let mut inner = Vec::new();
    emit(g, style, &mut inner);
    let text: String = inner.iter().map(|(t, _)| t.as_str()).collect();
    let simple = text.chars().all(char::is_alphanumeric) || text.chars().count() <= 1;
    if !simple {
        push(out, "(", style);
    }
    for (t, st) in inner {
        push(out, &t, st);
    }
    if !simple {
        push(out, ")", style);
    }
}

fn script_char(c: char, sup: bool) -> Option<char> {
    const SUP: [(char, char); 22] = [
        ('0', '⁰'), ('1', '¹'), ('2', '²'), ('3', '³'), ('4', '⁴'), ('5', '⁵'), ('6', '⁶'), ('7', '⁷'),
        ('8', '⁸'), ('9', '⁹'), ('+', '⁺'), ('-', '⁻'), ('=', '⁼'), ('(', '⁽'), (')', '⁾'), ('n', 'ⁿ'),
        ('i', 'ⁱ'), ('x', 'ˣ'), ('y', 'ʸ'), ('k', 'ᵏ'), ('T', 'ᵀ'), ('*', '*'),
    ];
    const SUB: [(char, char); 22] = [
        ('0', '₀'), ('1', '₁'), ('2', '₂'), ('3', '₃'), ('4', '₄'), ('5', '₅'), ('6', '₆'), ('7', '₇'),
        ('8', '₈'), ('9', '₉'), ('+', '₊'), ('-', '₋'), ('=', '₌'), ('(', '₍'), (')', '₎'), ('n', 'ₙ'),
        ('i', 'ᵢ'), ('x', 'ₓ'), ('k', 'ₖ'), ('j', 'ⱼ'), ('m', 'ₘ'), ('t', 'ₜ'),
    ];
    let table = if sup { &SUP } else { &SUB };
    table.iter().find(|(a, _)| *a == c).map(|(_, b)| *b)
}

fn command(c: &[char], i: usize) -> (String, usize) {
    let mut j = i + 1;
    if j < c.len() && !c[j].is_ascii_alphabetic() {
        return (c[j].to_string(), j + 1);
    }
    while j < c.len() && c[j].is_ascii_alphabetic() {
        j += 1;
    }
    (c[i + 1..j].iter().collect(), j)
}

/// Read a `{...}` group or a single token starting at `i` (skipping spaces).
fn group(c: &[char], mut i: usize) -> (Vec<char>, usize) {
    while i < c.len() && c[i] == ' ' {
        i += 1;
    }
    if i >= c.len() {
        return (Vec::new(), i);
    }
    if c[i] == '{' {
        let mut depth = 0;
        for j in i..c.len() {
            match c[j] {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        return (c[i + 1..j].to_vec(), j + 1);
                    }
                }
                _ => {}
            }
        }
        return (c[i + 1..].to_vec(), c.len());
    }
    if c[i] == '\\' {
        let (_, next) = command(c, i);
        return (c[i..next].to_vec(), next);
    }
    (vec![c[i]], i + 1)
}

fn styled_letter(ch: char, double: bool) -> char {
    let table: &[(char, char)] = if double {
        &[('R', 'ℝ'), ('N', 'ℕ'), ('Z', 'ℤ'), ('Q', 'ℚ'), ('C', 'ℂ'), ('P', 'ℙ'), ('E', '𝔼'), ('1', '𝟙')]
    } else {
        &[('L', 'ℒ'), ('O', '𝒪'), ('F', 'ℱ'), ('H', 'ℋ'), ('N', '𝒩'), ('P', '𝒫'), ('E', 'ℰ'), ('M', 'ℳ')]
    };
    table.iter().find(|(a, _)| *a == ch).map_or(ch, |(_, b)| *b)
}

fn symbol(name: &str) -> Option<&'static str> {
    Some(match name {
        "alpha" => "α", "beta" => "β", "gamma" => "γ", "delta" => "δ", "epsilon" => "ϵ", "varepsilon" => "ε",
        "zeta" => "ζ", "eta" => "η", "theta" => "θ", "vartheta" => "ϑ", "iota" => "ι", "kappa" => "κ",
        "lambda" => "λ", "mu" => "μ", "nu" => "ν", "xi" => "ξ", "pi" => "π", "rho" => "ρ", "sigma" => "σ",
        "tau" => "τ", "upsilon" => "υ", "phi" => "ϕ", "varphi" => "φ", "chi" => "χ", "psi" => "ψ", "omega" => "ω",
        "Gamma" => "Γ", "Delta" => "Δ", "Theta" => "Θ", "Lambda" => "Λ", "Xi" => "Ξ", "Pi" => "Π",
        "Sigma" => "Σ", "Upsilon" => "Υ", "Phi" => "Φ", "Psi" => "Ψ", "Omega" => "Ω",
        "sum" => "∑", "prod" => "∏", "int" => "∫", "iint" => "∬", "oint" => "∮", "infty" => "∞",
        "partial" => "∂", "nabla" => "∇", "pm" => "±", "mp" => "∓", "times" => "×", "div" => "÷",
        "cdot" => "·", "cdots" => "⋯", "ldots" | "dots" => "…", "vdots" => "⋮", "ddots" => "⋱",
        "leq" | "le" => "≤", "geq" | "ge" => "≥", "neq" | "ne" => "≠", "approx" => "≈", "equiv" => "≡",
        "sim" => "∼", "simeq" => "≃", "cong" => "≅", "propto" => "∝", "ll" => "≪", "gg" => "≫",
        "in" => "∈", "notin" => "∉", "ni" => "∋", "subset" => "⊂", "subseteq" => "⊆", "supset" => "⊃",
        "supseteq" => "⊇", "cup" => "∪", "cap" => "∩", "emptyset" | "varnothing" => "∅", "setminus" => "∖",
        "forall" => "∀", "exists" => "∃", "neg" | "lnot" => "¬", "land" | "wedge" => "∧", "lor" | "vee" => "∨",
        "to" | "rightarrow" => "→", "leftarrow" | "gets" => "←", "Rightarrow" | "implies" => "⇒",
        "Leftarrow" => "⇐", "leftrightarrow" => "↔", "Leftrightarrow" | "iff" => "⇔", "mapsto" => "↦",
        "uparrow" => "↑", "downarrow" => "↓", "langle" => "⟨", "rangle" => "⟩", "lceil" => "⌈",
        "rceil" => "⌉", "lfloor" => "⌊", "rfloor" => "⌋", "circ" => "∘", "bullet" => "•", "star" => "⋆",
        "ast" => "∗", "oplus" => "⊕", "otimes" => "⊗", "perp" => "⊥", "parallel" => "∥", "angle" => "∠",
        "degree" => "°", "prime" => "′", "hbar" => "ℏ", "ell" => "ℓ", "Re" => "ℜ", "Im" => "ℑ",
        "aleph" => "ℵ", "therefore" => "∴", "because" => "∵", "mid" => "∣", "vert" => "|", "Vert" => "‖",
        "lim" => "lim", "log" => "log", "ln" => "ln", "exp" => "exp", "sin" => "sin", "cos" => "cos",
        "tan" => "tan", "max" => "max", "min" => "min", "sup" => "sup", "inf" => "inf", "det" => "det",
        "lbrace" => "{", "rbrace" => "}",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flat(s: &str) -> String {
        render(s).into_iter().map(|(t, st)| match st {
            st::SUP => format!("^[{t}]"),
            st::SUB => format!("_[{t}]"),
            _ => t,
        }).collect()
    }

    #[test]
    fn basics() {
        assert_eq!(flat(r"\alpha + \beta"), "α + β");
        assert_eq!(flat("x^2 + y_{ij}"), "x^[2] + y_[ij]");
        assert_eq!(flat(r"\frac{a+b}{2}"), "(a+b)/2");
        assert_eq!(flat(r"\sqrt{\pi}"), "√π");
        assert_eq!(flat("e^{-x^2}"), "e^[-x²]");
        assert_eq!(flat(r"\mathbb{R}^n"), "ℝ^[n]");
        assert_eq!(flat(r"e^{i\pi}"), "e^[iπ]");
    }
}
