//! JS: live/frameworks/tag-strategy.mjs + script-src.mjs. The generic
//! marker-wrapped `<script src>` block: build, insert, remove, and the CSP
//! meta patch/revert.

use crate::util::{base64_decode, base64_encode, encode_uri_component};
use once_cell::sync::Lazy;
use regex::{Regex, RegexBuilder};

pub const MARKER_OPEN_TEXT: &str = "impeccable-live-start";
pub const MARKER_CLOSE_TEXT: &str = "impeccable-live-end";
pub const CSP_MARKER_ATTR: &str = "data-impeccable-csp-original";
/// JS: TAG_PATCH_MARKERS
pub const TAG_PATCH_MARKERS: [&str; 2] = [MARKER_OPEN_TEXT, CSP_MARKER_ATTR];

/// JS: buildLiveScriptSrc(port, token)
pub fn build_live_script_src(port: i64, token: Option<&str>) -> String {
    let base = format!("http://localhost:{}/live.js", port);
    match token {
        Some(t) if !t.is_empty() => format!("{}?token={}", base, encode_uri_component(t)),
        _ => base,
    }
}

fn comment_open(syntax: &str) -> &'static str {
    if syntax == "jsx" {
        "{/*"
    } else {
        "<!--"
    }
}
fn comment_close(syntax: &str) -> &'static str {
    if syntax == "jsx" {
        "*/}"
    } else {
        "-->"
    }
}

/// JS: buildTagBlock(syntax, port, token, scriptAttrs)
pub fn build_tag_block(syntax: &str, port: i64, token: Option<&str>, script_attrs: &str) -> String {
    let open = comment_open(syntax);
    let close = comment_close(syntax);
    format!(
        "{o} {m1} {c}\n<script {a}src=\"{src}\"></script>\n{o} {m2} {c}\n",
        o = open,
        c = close,
        m1 = MARKER_OPEN_TEXT,
        m2 = MARKER_CLOSE_TEXT,
        a = script_attrs,
        src = build_live_script_src(port, token)
    )
}

fn detect_line_ending(content: &str) -> &'static str {
    if content.contains("\r\n") {
        "\r\n"
    } else if content.contains('\r') {
        "\r"
    } else {
        "\n"
    }
}

fn normalize_line_endings(content: &str, le: &str) -> String {
    if le == "\n" {
        content.to_string()
    } else {
        content.replace('\n', le)
    }
}

fn read_line_ending_at(content: &str, index: usize) -> &'static str {
    let b = content.as_bytes();
    if b.get(index) == Some(&b'\r') && b.get(index + 1) == Some(&b'\n') {
        "\r\n"
    } else if b.get(index) == Some(&b'\n') {
        "\n"
    } else if b.get(index) == Some(&b'\r') {
        "\r"
    } else {
        ""
    }
}

/// JS: insertTag(content, config, port, token, scriptAttrs)
pub fn insert_tag(
    content: &str,
    comment_syntax: &str,
    insert_before: Option<&str>,
    insert_after: Option<&str>,
    port: i64,
    token: Option<&str>,
    script_attrs: &str,
) -> String {
    let le = detect_line_ending(content);
    let block = normalize_line_endings(
        &build_tag_block(comment_syntax, port, token, script_attrs),
        le,
    );
    // JS: `if (config.insertBefore)`: truthy string
    if let Some(anchor) = insert_before.filter(|a| !a.is_empty()) {
        return match content.rfind(anchor) {
            None => content.to_string(),
            Some(idx) => format!("{}{}{}", &content[..idx], block, &content[idx..]),
        };
    }
    let anchor = insert_after.unwrap_or("");
    let Some(idx) = content.find(anchor) else {
        return content.to_string();
    };
    let after = idx + anchor.len();
    let existing_nl = read_line_ending_at(content, after);
    let nl = if existing_nl.is_empty() {
        le
    } else {
        existing_nl
    };
    let prefix = format!("{}{}", &content[..after], nl);
    let rest = &content[after + existing_nl.len()..];
    format!("{}{}{}", prefix, block, rest)
}

static REMOVE_HTML_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"([ \t]*)<!--\s*impeccable-live-start\s*-->[\s\S]*?<!--\s*impeccable-live-end\s*-->([ \t]*(?:\r\n|\n|\r|$)?)").unwrap()
});
static REMOVE_JSX_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"([ \t]*)\{/\*\s*impeccable-live-start\s*\*/\}[\s\S]*?\{/\*\s*impeccable-live-end\s*\*/\}([ \t]*(?:\r\n|\n|\r|$)?)").unwrap()
});

/// JS: removeTag(content). Matches either comment style; indent-preserving.
pub fn remove_tag(content: &str) -> String {
    let mut content = content.to_string();
    for pat in [&*REMOVE_HTML_RE, &*REMOVE_JSX_RE] {
        let mut changed = false;
        loop {
            let next = pat
                .replacen(&content, 1, |caps: &regex::Captures| {
                    let indent = caps.get(1).map(|m| m.as_str()).unwrap_or("");
                    let trailing = caps.get(2).map(|m| m.as_str()).unwrap_or("");
                    if trailing.contains('\r') || trailing.contains('\n') {
                        indent.to_string()
                    } else if !indent.is_empty() {
                        indent.to_string()
                    } else {
                        trailing.to_string()
                    }
                })
                .into_owned();
            if next == content {
                break;
            }
            content = next;
            changed = true;
        }
        if changed {
            return content;
        }
    }
    content
}

struct MetaTag {
    start: usize,
    end: usize,
    full: String,
    attrs: String,
}

static META_TAG_RE: Lazy<Regex> = Lazy::new(|| {
    RegexBuilder::new(r"<meta\s+([^>]*?)/?>")
        .case_insensitive(true)
        .dot_matches_new_line(true)
        .build()
        .unwrap()
});
static HTTP_EQUIV_RE: Lazy<Regex> = Lazy::new(|| {
    RegexBuilder::new(r#"(?:http-equiv|httpEquiv)\s*=\s*(['"])Content-Security-Policy(['"])"#)
        .case_insensitive(true)
        .build()
        .unwrap()
});

fn find_csp_meta_tags(content: &str) -> Vec<MetaTag> {
    let mut out = Vec::new();
    for m in META_TAG_RE.captures_iter(content) {
        let whole = m.get(0).unwrap();
        let attrs = m.get(1).map(|a| a.as_str()).unwrap_or("");
        // JS backreference \2: same quote both sides.
        let mut ok = false;
        for c in HTTP_EQUIV_RE.captures_iter(attrs) {
            if c.get(1).map(|q| q.as_str()) == c.get(2).map(|q| q.as_str()) {
                ok = true;
                break;
            }
        }
        if !ok {
            continue;
        }
        out.push(MetaTag {
            start: whole.start(),
            end: whole.end(),
            full: whole.as_str().to_string(),
            attrs: attrs.to_string(),
        });
    }
    out
}

struct Attr {
    quote: String,
    value: String,
    full: String,
}

fn get_attr(attrs: &str, name: &str) -> Option<Attr> {
    // JS: new RegExp(`\\b${name}\\s*=\\s*(['"])([\\s\\S]*?)\\1`, 'i')
    // Emulate the backreference by trying each quote.
    let mut best: Option<(usize, Attr)> = None;
    for q in ["'", "\""] {
        let re = RegexBuilder::new(&format!(
            "\\b{}\\s*=\\s*{}([\\s\\S]*?){}",
            crate::util::escape_regex(name),
            q,
            q
        ))
        .case_insensitive(true)
        .build()
        .ok()?;
        if let Some(c) = re.captures(attrs) {
            let whole = c.get(0).unwrap();
            let attr = Attr {
                quote: q.to_string(),
                value: c.get(1).map(|v| v.as_str()).unwrap_or("").to_string(),
                full: whole.as_str().to_string(),
            };
            match &best {
                Some((s, _)) if *s <= whole.start() => {}
                _ => best = Some((whole.start(), attr)),
            }
        }
    }
    // JS: `\bname\s*=\s*(['"])` — the earliest match position wins; when both
    // quotes match at the same start (impossible) keep the first.
    best.map(|(_, a)| a)
}

/// Length of the HTML character reference at the start of `s` (which starts
/// with `&`), if one starts there. A numeric reference may omit its `;`, as
/// browsers read it; a named one counts only with its `;`.
fn reference_len(s: &str) -> Option<usize> {
    let b = s.as_bytes();
    if b.get(1) == Some(&b'#') {
        let hex = matches!(b.get(2), Some(b'x' | b'X'));
        let start = if hex { 3 } else { 2 };
        let mut i = start;
        while i < b.len()
            && (if hex {
                b[i].is_ascii_hexdigit()
            } else {
                b[i].is_ascii_digit()
            })
        {
            i += 1;
        }
        if i == start {
            return None;
        }
        if b.get(i) == Some(&b';') {
            i += 1;
        }
        Some(i)
    } else {
        let mut i = 1;
        while i < b.len() && b[i].is_ascii_alphanumeric() {
            i += 1;
        }
        if i == 1 || b.get(i) != Some(&b';') {
            return None;
        }
        Some(i + 1)
    }
}

/// The named references that decode to ASCII, the only characters a CSP is
/// made of. Any other `&name;` is kept as one opaque unit: it may be a real
/// reference (`&copy;`) or literal text (`&bogus;`), and telling them apart
/// takes the full HTML table. Treating it as text inside a source means an
/// insertion never lands between its name and its `;`.
const ASCII_NAMED_REFERENCES: [(&str, char); 29] = [
    ("amp", '&'),
    ("quot", '"'),
    ("apos", '\''),
    ("lt", '<'),
    ("gt", '>'),
    ("semi", ';'),
    ("colon", ':'),
    ("comma", ','),
    ("period", '.'),
    ("sol", '/'),
    ("bsol", '\\'),
    ("Tab", '\t'),
    ("NewLine", '\n'),
    ("ast", '*'),
    ("plus", '+'),
    ("lowbar", '_'),
    ("equals", '='),
    ("quest", '?'),
    ("num", '#'),
    ("percnt", '%'),
    ("excl", '!'),
    ("commat", '@'),
    ("dollar", '$'),
    ("lpar", '('),
    ("rpar", ')'),
    ("lsqb", '['),
    ("rsqb", ']'),
    ("lcub", '{'),
    ("rcub", '}'),
];

/// `tail` starts with `&`. Returns the decoded char and the reference length
/// for numeric references and the names in `ASCII_NAMED_REFERENCES`.
fn decode_reference(tail: &str) -> Option<(char, usize)> {
    let len = reference_len(tail)?;
    let body = tail[1..len].trim_end_matches(';');
    let ch = if let Some(num) = body.strip_prefix('#') {
        let code = match num.strip_prefix(['x', 'X']) {
            Some(hex) => u32::from_str_radix(hex, 16).ok(),
            None => num.parse::<u32>().ok(),
        };
        code.filter(|c| *c != 0)
            .and_then(char::from_u32)
            .unwrap_or('\u{FFFD}')
    } else {
        ASCII_NAMED_REFERENCES
            .iter()
            .find(|(name, _)| *name == body)?
            .1
    };
    Some((ch, len))
}

/// One decoded character of a raw attribute value and the raw bytes it came
/// from (a single character, or a whole character reference).
struct Unit {
    ch: char,
    start: usize,
    end: usize,
    /// An `&name;` outside `ASCII_NAMED_REFERENCES`, read as its raw text.
    opaque: bool,
}

fn decode_units(raw: &str) -> Vec<Unit> {
    let mut out = Vec::with_capacity(raw.len());
    let mut i = 0;
    while i < raw.len() {
        let tail = &raw[i..];
        let (ch, len, opaque) = if tail.starts_with('&') {
            match decode_reference(tail) {
                Some((ch, len)) => (ch, len, false),
                None => match reference_len(tail) {
                    Some(len) if tail.as_bytes()[1] != b'#' => ('\u{FFFD}', len, true),
                    _ => ('&', 1, false),
                },
            }
        } else {
            let c = tail.chars().next().unwrap();
            (c, c.len_utf8(), false)
        };
        out.push(Unit {
            ch,
            start: i,
            end: i + len,
            opaque,
        });
        i += len;
    }
    out
}

/// The value a browser hands the CSP parser, for the references
/// `decode_reference` knows; anything else passes through unchanged.
#[cfg(test)]
fn decode_html_attr_value(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for u in decode_units(value) {
        if u.opaque {
            out.push_str(&value[u.start..u.end]);
        } else {
            out.push(u.ch);
        }
    }
    out
}

/// Encode a source for an attribute delimited by `quote`, so the quotes in
/// `'self'` and `'wasm-unsafe-eval'` cannot end a single-quoted value.
fn encode_html_attr_value(value: &str, quote: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' if quote == "\"" => out.push_str("&quot;"),
            '\'' if quote == "'" => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

/// Append `source` to `directive` in the raw attribute value `raw`, or add the
/// directive as `; <directive> 'self' <source>` when it is missing. Directive
/// names, separators and sources are read on the decoded value, the way the
/// browser reads them, but the edit is made on the raw text: the existing text
/// and its character references are copied through byte for byte, and only
/// the inserted text is encoded for `quote`. Whitespace is CSP's ASCII
/// whitespace, so a decoded `&#160;` does not separate a name from a source.
fn append_origin_to_directive(raw: &str, directive: &str, source: &str, quote: &str) -> String {
    let encoded = encode_html_attr_value(source, quote);
    let units = decode_units(raw);
    let name: Vec<char> = directive.chars().collect();
    for seg in units.split(|u| u.ch == ';') {
        let body: Vec<&Unit> = seg
            .iter()
            .skip_while(|u| u.ch.is_ascii_whitespace())
            .collect();
        let named = body.len() > name.len()
            && body
                .iter()
                .zip(&name)
                .all(|(u, c)| u.ch.eq_ignore_ascii_case(c))
            && body[name.len()].ch.is_ascii_whitespace();
        if !named {
            continue;
        }
        let rest: String = body[name.len()..]
            .iter()
            .map(|u| if u.opaque { '\u{FFFD}' } else { u.ch })
            .collect();
        if rest
            .split(|c: char| c.is_ascii_whitespace())
            .any(|t| t == source)
        {
            return raw.to_string();
        }
        // After the last source, or after the name when there is none.
        let at = body
            .iter()
            .rev()
            .find(|u| !u.ch.is_ascii_whitespace())
            .map(|u| u.end)
            .unwrap_or(raw.len());
        return format!("{} {}{}", &raw[..at], encoded, &raw[at..]);
    }
    // JS: csp.trim().replace(/;?\s*$/, ''), on decoded characters.
    let first = units.iter().position(|u| !u.ch.is_ascii_whitespace());
    let last = units.iter().rposition(|u| !u.ch.is_ascii_whitespace());
    let base = match (first, last) {
        (Some(f), Some(l)) => {
            let end = if units[l].ch == ';' {
                units[l].start
            } else {
                units[l].end
            };
            &raw[units[f].start..end.max(units[f].start)]
        }
        _ => "",
    };
    // A trailing `&#99` or `&amp` would take the new `;` as its terminator
    // and swallow the separator; a space ends the reference first.
    let tail = base.rfind('&').map(|i| &base[i + 1..]).unwrap_or(" ");
    let gap = if !tail.is_empty() && tail.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'#') {
        " "
    } else {
        ""
    };
    format!(
        "{}{}; {} {} {}",
        base,
        gap,
        directive,
        encode_html_attr_value("'self'", quote),
        encoded
    )
}

/// JS: patchCspMeta(content, port)
pub fn patch_csp_meta(content: &str, port: i64) -> String {
    let tags = find_csp_meta_tags(content);
    if tags.is_empty() {
        return content.to_string();
    }
    let origin = format!("http://localhost:{}", port);
    let mut result = content.to_string();
    for tag in tags.iter().rev() {
        let attrs = &tag.attrs;
        if get_attr(attrs, CSP_MARKER_ATTR).is_some() {
            continue;
        }
        let Some(content_attr) = get_attr(attrs, "content") else {
            continue;
        };
        // The value is patched in place on its raw text (character references
        // kept, only the inserted sources encoded for the quote style), and
        // the marker keeps the raw original so revert restores its bytes.
        let original = content_attr.value.clone();
        let q = content_attr.quote.as_str();
        let mut patched = original.clone();
        patched = append_origin_to_directive(&patched, "script-src", &origin, q);
        // The detector the overlay loads from that origin is a WebAssembly
        // module (WASM-BUNDLE.md in the detector repo); a script-src that names the origin
        // but not 'wasm-unsafe-eval' still refuses to compile it. Not in the
        // JS patchCspMeta: goldens live-inject-vite-csp-meta,
        // live-inject-csp-meta-no-connect-src and live-inject-next-jsx carry
        // the pre-wasm form (see tests/oracle/DELTAS.md in the public repo).
        patched = append_origin_to_directive(&patched, "script-src", "'wasm-unsafe-eval'", q);
        patched = append_origin_to_directive(&patched, "connect-src", &origin, q);
        patched = append_origin_to_directive(&patched, "img-src", "blob:", q);
        if patched == original {
            continue;
        }
        let new_content_attr = format!("content={q}{patched}{q}");
        let marker = format!(
            "{}=\"{}\"",
            CSP_MARKER_ATTR,
            base64_encode(original.as_bytes())
        );
        let trailing_ws_len = attrs.len() - attrs.trim_end_matches([' ', '\t']).len();
        let trailing_ws = &attrs[attrs.len() - trailing_ws_len..];
        let attrs_body = &attrs[..attrs.len() - trailing_ws_len];
        let new_attrs = format!(
            "{} {}{}",
            attrs_body.replacen(&content_attr.full, &new_content_attr, 1),
            marker,
            trailing_ws
        );
        let new_tag = tag.full.replacen(attrs.as_str(), &new_attrs, 1);
        result = format!("{}{}{}", &result[..tag.start], new_tag, &result[tag.end..]);
    }
    result
}

/// JS: revertCspMeta(content)
pub fn revert_csp_meta(content: &str) -> String {
    let tags = find_csp_meta_tags(content);
    if tags.is_empty() {
        return content.to_string();
    }
    let mut result = content.to_string();
    for tag in tags.iter().rev() {
        let Some(orig_attr) = get_attr(&tag.attrs, CSP_MARKER_ATTR) else {
            continue;
        };
        let Some(content_attr) = get_attr(&tag.attrs, "content") else {
            continue;
        };
        let original_value = String::from_utf8_lossy(&base64_decode(&orig_attr.value)).into_owned();
        let new_content_attr = format!(
            "content={}{}{}",
            content_attr.quote, original_value, content_attr.quote
        );
        let mut new_attrs = tag.attrs.replacen(&content_attr.full, &new_content_attr, 1);
        // Remove the marker literally. The JS built a regex from the raw
        // base64, where `+` acts as a quantifier, so a marker whose value held
        // a `+` was never matched and stayed behind. Patch writes the marker
        // as ` <marker>`: drop exactly that one space so the attributes come
        // back byte for byte.
        if let Some(pos) = new_attrs.find(&orig_attr.full) {
            let start = if new_attrs[..pos].ends_with(' ') {
                pos - 1
            } else {
                pos
            };
            new_attrs = format!(
                "{}{}",
                &new_attrs[..start],
                &new_attrs[pos + orig_attr.full.len()..]
            );
        }
        let new_tag = tag.full.replacen(tag.attrs.as_str(), &new_attrs, 1);
        result = format!("{}{}{}", &result[..tag.start], new_tag, &result[tag.end..]);
    }
    result
}

/// JS: unpatchTagFile(content)
pub fn unpatch_tag_file(content: &str) -> String {
    revert_csp_meta(&remove_tag(content))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csp_meta_patch_adds_wasm_unsafe_eval_to_script_src() {
        let html = "<html><head><meta http-equiv=\"Content-Security-Policy\" content=\"default-src 'self'; script-src 'self'; img-src 'self'\"></head></html>";
        let patched = patch_csp_meta(html, 8412);
        assert!(
            patched.contains("script-src 'self' http://localhost:8412 'wasm-unsafe-eval'"),
            "{patched}"
        );
        assert!(patched.contains("connect-src 'self' http://localhost:8412"));
        assert!(patched.contains("img-src 'self' blob:"));
        // idempotent
        assert_eq!(patch_csp_meta(&patched, 8412), patched);
        // reverts to the original
        assert_eq!(revert_csp_meta(&patched), html);
    }

    #[test]
    fn csp_meta_patch_creates_script_src_when_missing() {
        let html = "<meta http-equiv=\"Content-Security-Policy\" content=\"default-src 'self'\">";
        let patched = patch_csp_meta(html, 8412);
        assert!(
            patched.contains("script-src 'self' http://localhost:8412 'wasm-unsafe-eval'"),
            "{patched}"
        );
    }

    /// The `content` value of the first CSP meta, decoded the way a browser
    /// reads it. Panics when the tag or the attribute cannot be found.
    fn browser_policy(html: &str) -> String {
        let tag = find_csp_meta_tags(html)
            .into_iter()
            .next()
            .expect("csp meta");
        let attr = get_attr(&tag.attrs, "content").expect("content attr");
        decode_html_attr_value(&attr.value)
    }

    fn marker_value(html: &str) -> String {
        let tag = find_csp_meta_tags(html)
            .into_iter()
            .next()
            .expect("csp meta");
        get_attr(&tag.attrs, CSP_MARKER_ATTR).expect("marker").value
    }

    #[test]
    fn single_quoted_content_escapes_inserted_quotes() {
        let html = "<meta http-equiv=\"Content-Security-Policy\" content='script-src https://example.com/a~' />";
        let patched = patch_csp_meta(html, 9797);
        assert!(
            patched.contains("content='script-src https://example.com/a~ http://localhost:9797 &#39;wasm-unsafe-eval&#39;; connect-src &#39;self&#39; http://localhost:9797; img-src &#39;self&#39; blob:'"),
            "{patched}"
        );
        assert_eq!(
            browser_policy(&patched),
            "script-src https://example.com/a~ http://localhost:9797 'wasm-unsafe-eval'; connect-src 'self' http://localhost:9797; img-src 'self' blob:"
        );
        assert_eq!(revert_csp_meta(&patched), html);
    }

    #[test]
    fn entity_encoded_semicolons_are_not_directive_separators() {
        let html = "<meta http-equiv=\"Content-Security-Policy\" content='script-src &#39;self&#39; https://a.example; object-src &#x27;none&#x27;' />";
        let patched = patch_csp_meta(html, 9797);
        assert_eq!(
            browser_policy(&patched),
            "script-src 'self' https://a.example http://localhost:9797 'wasm-unsafe-eval'; object-src 'none'; connect-src 'self' http://localhost:9797; img-src 'self' blob:"
        );
        assert_eq!(revert_csp_meta(&patched), html);
    }

    #[test]
    fn double_quoted_content_with_entities_round_trips() {
        let html = "<meta http-equiv='Content-Security-Policy' content=\"script-src &#39;self&#39; https://a.example/?a=1&amp;b=&quot;2&quot;\">";
        let patched = patch_csp_meta(html, 8412);
        assert_eq!(
            browser_policy(&patched),
            "script-src 'self' https://a.example/?a=1&b=\"2\" http://localhost:8412 'wasm-unsafe-eval'; connect-src 'self' http://localhost:8412; img-src 'self' blob:"
        );
        // The existing text keeps its own references; only the inserted
        // sources are new, and raw `'` is fine inside double quotes.
        assert!(
            patched.contains("content=\"script-src &#39;self&#39; https://a.example/?a=1&amp;b=&quot;2&quot; http://localhost:8412 'wasm-unsafe-eval';"),
            "{patched}"
        );
        assert_eq!(revert_csp_meta(&patched), html);
    }

    #[test]
    fn already_present_entity_encoded_source_is_not_duplicated() {
        let html = "<meta http-equiv=\"Content-Security-Policy\" content='script-src http://localhost:9797 &#39;wasm-unsafe-eval&#39;; connect-src http://localhost:9797; img-src blob:'>";
        assert_eq!(patch_csp_meta(html, 9797), html);
    }

    #[test]
    fn revert_removes_marker_whose_base64_holds_plus_or_slash() {
        for (html, needle) in [
            (
                "<meta http-equiv=\"Content-Security-Policy\" content=\"script-src https://example.com/a~\" />",
                '+',
            ),
            ("<meta http-equiv=\"Content-Security-Policy\" content=\"img-src ???\">", '/'),
        ] {
            let patched = patch_csp_meta(html, 9797);
            let marker = marker_value(&patched);
            assert!(marker.contains(needle), "{marker} lacks {needle}");
            let reverted = revert_csp_meta(&patched);
            assert!(!reverted.contains(CSP_MARKER_ATTR), "{reverted}");
            assert_eq!(reverted, html);
        }
    }

    #[test]
    fn decode_leaves_unknown_and_bare_ampersands_alone() {
        assert_eq!(
            decode_html_attr_value("a & b &copy; &#xZZ; &#"),
            "a & b &copy; &#xZZ; &#"
        );
        // Numeric references without `;`, and NUL as U+FFFD, as browsers read them.
        assert_eq!(decode_html_attr_value("sr&#99 &#0;"), "src \u{FFFD}");
        assert_eq!(
            decode_html_attr_value("&#39;&#x27;&#X27;&apos;&quot;&amp;&lt;&gt;"),
            "''''\"&<>"
        );
    }

    #[test]
    fn existing_references_are_copied_through_unchanged() {
        // `&#99` (no `;`) is `c` to a browser, and `&gt;` must stay escaped so
        // the tag reader still finds the end of the tag after the patch.
        let html = "<meta http-equiv=\"Content-Security-Policy\" content=\"object-sr&#99 'none'; img-src https://example.com/a&gt;b &copy; x; default-src *\">";
        let patched = patch_csp_meta(html, 9797);
        assert!(patched.contains("object-sr&#99 'none'; img-src https://example.com/a&gt;b &copy; x blob:; default-src *; script-src 'self' http://localhost:9797 'wasm-unsafe-eval'; connect-src 'self' http://localhost:9797\""), "{patched}");
        assert!(patched.contains(CSP_MARKER_ATTR));
        assert_eq!(revert_csp_meta(&patched), html);
        // Re-inject after remove patches the same way again.
        assert_eq!(patch_csp_meta(&revert_csp_meta(&patched), 9797), patched);
    }

    #[test]
    fn trailing_reference_semicolon_is_not_stripped() {
        let html =
            "<meta http-equiv=\"Content-Security-Policy\" content='default-src &#39;none&#39;'>";
        let patched = patch_csp_meta(html, 9797);
        assert_eq!(
            browser_policy(&patched),
            "default-src 'none'; script-src 'self' http://localhost:9797 'wasm-unsafe-eval'; connect-src 'self' http://localhost:9797; img-src 'self' blob:"
        );
    }

    #[test]
    fn directive_names_and_boundaries_are_read_decoded() {
        for (csp, expect) in [
            (
                "script-src&#32;'self'",
                "script-src&#32;'self' http://localhost:9797 'wasm-unsafe-eval'; connect-src 'self' http://localhost:9797; img-src 'self' blob:",
            ),
            (
                "script-sr&#99; 'self'",
                "script-sr&#99; 'self' http://localhost:9797 'wasm-unsafe-eval'; connect-src 'self' http://localhost:9797; img-src 'self' blob:",
            ),
            (
                "img-src 'self'&#59; script-src 'self'",
                "img-src 'self' blob:&#59; script-src 'self' http://localhost:9797 'wasm-unsafe-eval'; connect-src 'self' http://localhost:9797",
            ),
        ] {
            let html = format!("<meta http-equiv=\"Content-Security-Policy\" content=\"{csp}\">");
            let patched = patch_csp_meta(&html, 9797);
            assert!(patched.contains(&format!("content=\"{expect}\"")), "{patched}");
            assert_eq!(revert_csp_meta(&patched), html);
        }
    }

    #[test]
    fn trailing_unterminated_reference_keeps_the_new_separator() {
        for (csp, base) in [
            (
                "default-src https://a.example/&#99",
                "default-src https://a.example/&#99 ",
            ),
            (
                "default-src https://a.example/?a&amp",
                "default-src https://a.example/?a&amp ",
            ),
        ] {
            let html = format!("<meta http-equiv=\"Content-Security-Policy\" content=\"{csp}\">");
            let patched = patch_csp_meta(&html, 9797);
            assert!(
                patched.contains(&format!(
                    "content=\"{base}; script-src 'self' http://localhost:9797 'wasm-unsafe-eval';"
                )),
                "{patched}"
            );
            assert_eq!(revert_csp_meta(&patched), html);
        }
    }

    #[test]
    fn only_ascii_whitespace_and_exact_names_match_a_directive() {
        // `&#160;` is not CSP whitespace and `&hyphen;` is U+2010, so neither
        // value names script-src; the patch adds a real one.
        for csp in [
            "default-src 'self'; script-src&#160;'self'",
            "default-src 'self'; script&hyphen;src 'self'",
        ] {
            let html = format!("<meta http-equiv=\"Content-Security-Policy\" content=\"{csp}\">");
            let patched = patch_csp_meta(&html, 9797);
            assert!(
                patched.contains(&format!(
                    "content=\"{csp}; script-src 'self' http://localhost:9797 'wasm-unsafe-eval';"
                )),
                "{patched}"
            );
            assert_eq!(revert_csp_meta(&patched), html);
        }
    }

    #[test]
    fn patch_then_revert_is_byte_identical() {
        let cases = [
            "<meta http-equiv=\"Content-Security-Policy\" content=\"default-src 'self'; script-src 'self'; img-src 'self'\">",
            "<meta http-equiv=\"Content-Security-Policy\" content='script-src https://example.com/a~' />",
            "<meta http-equiv=\"Content-Security-Policy\" content='script-src &#39;self&#39; https://a.example; object-src &#x27;none&#x27;' />",
            "<meta http-equiv=\"Content-Security-Policy\" content=\"script-src https://example.com/a~\" />",
            "<meta\n  http-equiv=\"Content-Security-Policy\"\n  content='default-src &apos;self&apos;; connect-src ws:;'\n/>",
            "<meta http-equiv=\"Content-Security-Policy\" content=\"img-src ???; script-src https://x.example/?q=a&amp;r=&quot;s&quot;\"\t>",
            "<meta http-equiv=\"Content-Security-Policy\" content=\"object-sr&#99 'none'; img-src https://e.example/a&gt;b; default-src *\">",
            "<meta http-equiv='Content-Security-Policy' content='default-src &#39;none&#39;'><meta http-equiv=\"Content-Security-Policy\" content=\"script-src 'self'\">",
        ];
        for csp in cases {
            let html =
                format!("<!DOCTYPE html>\r\n<html><head>{csp}</head><body>\r\n</body></html>\r\n");
            let patched = patch_csp_meta(&html, 9797);
            assert_ne!(patched, html, "nothing patched: {csp}");
            assert!(!browser_policy(&patched).is_empty());
            assert_eq!(revert_csp_meta(&patched), html, "revert: {csp}");
            // The full inject/remove cycle a live session runs.
            let injected = patch_csp_meta(
                &insert_tag(&html, "html", Some("</body>"), None, 9797, Some("t"), ""),
                9797,
            );
            assert_eq!(unpatch_tag_file(&injected), html, "unpatch: {csp}");
        }
    }
}
