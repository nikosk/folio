//! Spell checking through Enchant, the library GNOME apps use (it picks
//! Hunspell, Nuspell or Aspell dictionaries and keeps the personal word list
//! in `~/.config/enchant`).
//!
//! libenchant is opened with `dlopen` on a worker thread, so it is neither a
//! build dependency nor on the startup path; without it, spell checking is
//! simply unavailable. Results are cached per word.

use crate::doc::{st, Rich};
use std::collections::HashMap;
use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::sync::{Mutex, OnceLock};

/// Styles whose text is never checked: code, links, math, keys, badges and
/// the dimmed Markdown syntax of the source view.
const SKIP: u16 = st::CODE | st::KBD | st::LINK | st::MATH | st::BADGE | st::SYNTAX;

type CheckFn = unsafe extern "C" fn(*mut c_void, *const c_char, isize) -> c_int;
type SuggestFn = unsafe extern "C" fn(*mut c_void, *const c_char, isize, *mut usize) -> *mut *mut c_char;
type FreeListFn = unsafe extern "C" fn(*mut c_void, *mut *mut c_char);
type AddFn = unsafe extern "C" fn(*mut c_void, *const c_char, isize);

struct Speller {
    dict: *mut c_void,
    check: CheckFn,
    suggest: SuggestFn,
    free_list: FreeListFn,
    add: AddFn,
    add_session: AddFn,
    cache: HashMap<String, bool>,
}

// The dictionary is only used under the mutex below.
unsafe impl Send for Speller {}

static SPELLER: OnceLock<Option<Mutex<Speller>>> = OnceLock::new();

unsafe extern "C" {
    fn dlopen(file: *const c_char, mode: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, name: *const c_char) -> *mut c_void;
}

const RTLD_NOW: c_int = 2;

/// Dictionary tags to try, from the locale (`de_DE.UTF-8` → `de_DE`, `de`).
fn tags() -> Vec<String> {
    let lang = ["LC_ALL", "LC_MESSAGES", "LANG"]
        .iter()
        .filter_map(|v| std::env::var(v).ok())
        .find(|v| !v.is_empty() && v != "C" && v != "POSIX")
        .unwrap_or_default();
    let tag = lang.split(['.', '@']).next().unwrap_or("").to_string();
    let mut v = Vec::new();
    if !tag.is_empty() {
        if let Some((l, _)) = tag.split_once('_') {
            v.push(tag.clone());
            v.push(l.to_string());
        } else {
            v.push(tag);
        }
    }
    v.extend(["en_US".to_string(), "en".to_string()]);
    v
}

fn open() -> Option<Speller> {
    unsafe {
        let lib = dlopen(c"libenchant-2.so.2".as_ptr(), RTLD_NOW);
        if lib.is_null() {
            return None;
        }
        let sym = |name: &CStr| {
            let p = dlsym(lib, name.as_ptr());
            (!p.is_null()).then_some(p)
        };
        let init: unsafe extern "C" fn() -> *mut c_void = std::mem::transmute(sym(c"enchant_broker_init")?);
        let exists: unsafe extern "C" fn(*mut c_void, *const c_char) -> c_int =
            std::mem::transmute(sym(c"enchant_broker_dict_exists")?);
        let request: unsafe extern "C" fn(*mut c_void, *const c_char) -> *mut c_void =
            std::mem::transmute(sym(c"enchant_broker_request_dict")?);
        let mut s = Speller {
            dict: std::ptr::null_mut(),
            check: std::mem::transmute::<*mut c_void, CheckFn>(sym(c"enchant_dict_check")?),
            suggest: std::mem::transmute::<*mut c_void, SuggestFn>(sym(c"enchant_dict_suggest")?),
            free_list: std::mem::transmute::<*mut c_void, FreeListFn>(sym(c"enchant_dict_free_string_list")?),
            add: std::mem::transmute::<*mut c_void, AddFn>(sym(c"enchant_dict_add")?),
            add_session: std::mem::transmute::<*mut c_void, AddFn>(sym(c"enchant_dict_add_to_session")?),
            cache: HashMap::new(),
        };
        // The broker lives as long as the process.
        let broker = init();
        if broker.is_null() {
            return None;
        }
        for tag in tags() {
            let Ok(t) = CString::new(tag) else { continue };
            if exists(broker, t.as_ptr()) != 0 {
                s.dict = request(broker, t.as_ptr());
                if !s.dict.is_null() {
                    return Some(s);
                }
            }
        }
        None
    }
}

/// Start loading the dictionary in the background (once).
pub fn start() {
    if SPELLER.get().is_none() {
        std::thread::spawn(|| {
            let s = open().map(Mutex::new);
            let _ = SPELLER.set(s);
        });
    }
}

/// Whether loading has finished (successfully or not).
pub fn loaded() -> bool {
    SPELLER.get().is_some()
}

/// Whether a dictionary is ready to check words.
pub fn ready() -> bool {
    SPELLER.get().is_some_and(Option::is_some)
}

fn with<T>(f: impl FnOnce(&mut Speller) -> T) -> Option<T> {
    let s = SPELLER.get()?.as_ref()?;
    Some(f(&mut s.lock().unwrap_or_else(|e| e.into_inner())))
}

/// Whether `word` is spelled correctly (true when no dictionary is loaded).
pub fn check(word: &str) -> bool {
    with(|s| {
        if let Some(&ok) = s.cache.get(word) {
            return ok;
        }
        let ok = unsafe { (s.check)(s.dict, word.as_ptr().cast(), word.len() as isize) } <= 0;
        s.cache.insert(word.to_string(), ok);
        ok
    })
    .unwrap_or(true)
}

pub fn suggest(word: &str) -> Vec<String> {
    with(|s| unsafe {
        let mut n = 0usize;
        let list = (s.suggest)(s.dict, word.as_ptr().cast(), word.len() as isize, &mut n);
        if list.is_null() {
            return Vec::new();
        }
        let out = (0..n).map(|i| CStr::from_ptr(*list.add(i)).to_string_lossy().into_owned()).collect();
        (s.free_list)(s.dict, list);
        out
    })
    .unwrap_or_default()
}

/// Add to the personal dictionary, or with `session` only ignore it until quit.
pub fn accept(word: &str, session: bool) {
    with(|s| unsafe {
        let f = if session { s.add_session } else { s.add };
        f(s.dict, word.as_ptr().cast(), word.len() as isize);
        s.cache.insert(word.to_string(), true);
    });
}

/// Words to check in `text`: (start, end) byte ranges. Skips tokens that look
/// like code, paths, URLs or addresses (with `/ \ @ _ =`, digits, or a dot
/// inside), acronyms and camelCase.
pub fn words(text: &str) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let b = text.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i].is_ascii_whitespace() {
            i += 1;
            continue;
        }
        let start = i;
        while i < b.len() && !b[i].is_ascii_whitespace() {
            i += 1;
        }
        let tok = &text[start..i];
        if tok.contains(|c: char| c.is_ascii_digit() || "/\\@_=<>{}#|".contains(c)) || has_inner_dot(tok) {
            continue;
        }
        // Split into words at anything that isn't a letter or an apostrophe
        // between letters (don't, l'été).
        let mut ws: Option<usize> = None;
        let mut prev_letter = false;
        let mut chars = tok.char_indices().peekable();
        while let Some((k, c)) = chars.next() {
            let next_letter = chars.peek().is_some_and(|(_, n)| n.is_alphabetic());
            let inner = (c == '\'' || c == '’') && prev_letter && next_letter;
            if c.is_alphabetic() || inner {
                ws.get_or_insert(k);
            } else if let Some(s) = ws.take() {
                push_word(&mut out, tok, start, s, k);
            }
            prev_letter = c.is_alphabetic();
        }
        if let Some(s) = ws {
            push_word(&mut out, tok, start, s, tok.len());
        }
    }
    out
}

fn has_inner_dot(tok: &str) -> bool {
    let t = tok.trim_end_matches(|c: char| !c.is_alphanumeric());
    t.char_indices().any(|(k, c)| c == '.' && k > 0 && t[k + 1..].starts_with(char::is_alphanumeric))
}

fn push_word(out: &mut Vec<(usize, usize)>, tok: &str, base: usize, s: usize, e: usize) {
    let w = &tok[s..e];
    // One letter, an acronym (API), or camelCase (GtkWidget).
    if w.chars().count() < 2 || w.chars().skip(1).any(char::is_uppercase) {
        return;
    }
    out.push((base + s, base + e));
}

/// Misspelled words of rich text, as byte ranges of `r.text`.
pub fn typos(r: &Rich) -> Vec<(u32, u32)> {
    words(&r.text)
        .into_iter()
        .filter(|&(s, e)| !r.spans.iter().any(|sp| sp.style & SKIP != 0 && (sp.start as usize) < e && (sp.end as usize) > s))
        .filter(|&(s, e)| !check(&r.text[s..e]))
        .map(|(s, e)| (s as u32, e as u32))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ws(t: &str) -> Vec<&str> {
        words(t).into_iter().map(|(s, e)| &t[s..e]).collect()
    }

    #[test]
    fn splits_words_and_skips_code_like_tokens() {
        assert_eq!(ws("Hello, wörld! don't stop."), ["Hello", "wörld", "don't", "stop"]);
        assert_eq!(ws("see src/app.rs and foo_bar or x=1"), ["see", "and", "or"]);
        assert_eq!(ws("ask nk@example.com, the API, GtkWidget, 3rd"), ["ask", "the"]);
        assert_eq!(ws("well-known 'quoted' end."), ["well", "known", "quoted", "end"]);
    }
}
