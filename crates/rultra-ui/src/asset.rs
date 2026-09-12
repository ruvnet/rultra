//! Tests for the embedded console asset.
//!
//! The UI ships as one `include_str!`'d file. Nothing else validates it, so a
//! structural mistake would compile cleanly and blank the page at runtime.
//!
//! These checks walk a stack rather than counting tags. Counting is the trap:
//! `<section><div></section></div>` has a perfectly balanced count of every tag
//! and is still broken, and the failure mode is a blank region rather than an
//! error, so it survives a casual look at the page.

/// The console markup, as shipped.
pub const INDEX: &str = include_str!("../ui/index.html");

/// Elements that never have a closing tag.
#[cfg(test)]
const VOID: &[&str] = &[
    "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param", "source",
    "track", "wbr", "path", "circle", "rect", "polyline", "polygon", "line", "use", "stop",
];

/// A structural problem found in the markup.
#[cfg(test)]
#[derive(Debug, PartialEq, Eq)]
pub enum HtmlError {
    /// A closing tag that does not match the innermost open element.
    Misnested {
        /// What was closed.
        found: String,
        /// What was actually open.
        expected: String,
    },
    /// A closing tag with nothing open.
    Unopened(String),
    /// Elements left open at end of document.
    Unclosed(Vec<String>),
}

/// Walk the document, maintaining a stack of open elements.
///
/// Test-only: this validates the asset at build time, and has no job at runtime.
///
/// Skips `<script>` and `<style>` bodies wholesale: their contents are not
/// markup, and a `<` in a comparison or a CSS selector is not a tag.
#[cfg(test)]
pub fn check_nesting(html: &str) -> Result<usize, HtmlError> {
    let b = html.as_bytes();
    let mut stack: Vec<String> = Vec::new();
    let mut i = 0usize;
    let mut elements = 0usize;

    while i < b.len() {
        if b[i] != b'<' {
            i += 1;
            continue;
        }
        // Comments and doctype are not elements.
        if html[i..].starts_with("<!") {
            let end = html[i..].find('>').map(|e| i + e + 1).unwrap_or(b.len());
            i = end;
            continue;
        }
        let Some(close_rel) = html[i..].find('>') else {
            break;
        };
        let raw = &html[i + 1..i + close_rel];
        let closing = raw.starts_with('/');
        let name: String = raw
            .trim_start_matches('/')
            .split(|c: char| c.is_whitespace() || c == '/' || c == '>')
            .next()
            .unwrap_or("")
            .to_ascii_lowercase();
        let self_closing = raw.trim_end().ends_with('/');
        i += close_rel + 1;

        if name.is_empty() {
            continue;
        }

        if closing {
            match stack.pop() {
                None => return Err(HtmlError::Unopened(name)),
                Some(open) if open != name => {
                    return Err(HtmlError::Misnested {
                        found: name,
                        expected: open,
                    })
                }
                Some(_) => {}
            }
        } else {
            elements += 1;
            if VOID.contains(&name.as_str()) || self_closing {
                continue;
            }
            // Skip raw-text element bodies: their contents are not markup.
            if name == "script" || name == "style" {
                let close = format!("</{name}>");
                if let Some(rel) = html[i..].find(&close) {
                    i += rel + close.len();
                    continue;
                }
            }
            stack.push(name);
        }
    }

    if stack.is_empty() {
        Ok(elements)
    } else {
        Err(HtmlError::Unclosed(stack))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shipped_console_is_properly_nested() {
        if let Err(e) = check_nesting(INDEX) {
            panic!("console markup is structurally broken: {e:?}");
        }
    }

    /// The static shell is deliberately small — nearly the whole interface is
    /// built by script at runtime — so this only guards against the asset being
    /// truncated or emptied, not against it being thin. The measured size is
    /// around 28 elements; the floor is set well below that so ordinary edits
    /// do not trip it.
    #[test]
    fn the_shipped_console_is_not_empty_or_truncated() {
        let n = check_nesting(INDEX).expect("must parse");
        assert!(n >= 15, "console shell looks truncated: only {n} elements");
        assert!(INDEX.len() > 20_000, "console asset is suspiciously small");
        assert!(INDEX.trim_end().ends_with("</html>"), "asset is truncated");
    }

    /// The case that motivates a stack walk: tag COUNTS are balanced here and
    /// the document is still wrong.
    #[test]
    fn misnesting_is_caught_even_when_counts_balance() {
        let bad = "<section><div></section></div>";
        assert_eq!(
            check_nesting(bad),
            Err(HtmlError::Misnested {
                found: "section".into(),
                expected: "div".into()
            })
        );
    }

    #[test]
    fn an_unclosed_element_is_caught() {
        assert!(matches!(
            check_nesting("<div><p>hi</p>"),
            Err(HtmlError::Unclosed(_))
        ));
    }

    #[test]
    fn a_stray_closing_tag_is_caught() {
        assert_eq!(
            check_nesting("<p>hi</p></div>"),
            Err(HtmlError::Unopened("div".into()))
        );
    }

    #[test]
    fn void_elements_do_not_need_closing() {
        assert!(check_nesting("<div><br><img src=x><hr></div>").is_ok());
    }

    /// A `<` inside script or style is not markup, and treating it as such
    /// would make every comparison operator a parse error.
    #[test]
    fn script_and_style_bodies_are_not_parsed_as_markup() {
        assert!(check_nesting("<div><script>if(a<b){}</script></div>").is_ok());
        assert!(check_nesting("<div><style>a>b{color:red}</style></div>").is_ok());
    }

    /// Every element the JavaScript addresses by id must exist in the markup,
    /// or the console silently loses a piece of itself.
    #[test]
    fn every_id_the_script_addresses_exists() {
        for id in [
            "nav",
            "main",
            "live-dot",
            "foot-link",
            "foot-poll",
            "foot-chain",
        ] {
            assert!(
                INDEX.contains(&format!("id=\"{id}\"")),
                "the script addresses #{id} but the markup never defines it"
            );
        }
    }

    /// Each navigable section must have a matching render function, or a key
    /// press lands on a blank page.
    #[test]
    fn every_page_has_a_render_function() {
        for (page, func) in [
            ("overview", "renderOverview"),
            ("devices", "renderDevices"),
            ("loop", "renderLoop"),
            ("witness", "renderWitness"),
            ("hardware", "renderHardware"),
        ] {
            assert!(
                INDEX.contains(&format!("id:'{page}'")),
                "page {page} missing"
            );
            assert!(INDEX.contains(func), "no render function {func}");
        }
    }

    /// The console must never accept a token from the URL — it would land in
    /// server logs, history and Referer headers.
    #[test]
    fn the_console_never_reads_a_token_from_the_url() {
        assert!(
            !INDEX.contains("searchParams.get('token')") && !INDEX.contains("location.search"),
            "the console must not read a credential from the URL"
        );
    }
}
