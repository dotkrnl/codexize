// prompt_render.rs
//
// Tiny `{placeholder}` substitution helper for prompt templates loaded with
// `include_str!`. Templates live under `src/app/prompts/*.md`; their bodies
// are rendered into the final agent prompt by the prompt-builder functions
// in `src/app/prompts.rs`.
//
// Syntax:
//   - `{name}` is replaced with the value bound to `name` in `vars`.
//   - `{{` and `}}` emit literal `{` and `}` (used for braces inside Markdown
//     examples or TOML/Rust snippets in the prompt body).
//   - Any unbound `{name}`, unclosed `{`, or stray unescaped `}` is a hard
//     error (panic) so a typo in a template surfaces in tests rather than at
//     runtime.

pub(super) fn render(template: &str, vars: &[(&str, &str)]) -> String {
    let mut out = String::with_capacity(template.len());
    let bytes = template.as_bytes();
    let mut i = 0;
    let mut copy_start = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'{' => {
                out.push_str(&template[copy_start..i]);
                if i + 1 < bytes.len() && bytes[i + 1] == b'{' {
                    out.push('{');
                    i += 2;
                    copy_start = i;
                    continue;
                }
                let name_start = i + 1;
                let Some(rel_end) = bytes[name_start..].iter().position(|b| *b == b'}') else {
                    panic!(
                        "unclosed placeholder starting at byte {i} in prompt template (no matching `}}`)"
                    );
                };
                let name_end = name_start + rel_end;
                let name = &template[name_start..name_end];
                let Some((_, value)) = vars.iter().find(|(k, _)| *k == name) else {
                    panic!("unbound placeholder `{{{name}}}` in prompt template");
                };
                out.push_str(value);
                i = name_end + 1;
                copy_start = i;
            }
            b'}' => {
                out.push_str(&template[copy_start..i]);
                if i + 1 < bytes.len() && bytes[i + 1] == b'}' {
                    out.push('}');
                    i += 2;
                    copy_start = i;
                    continue;
                }
                panic!("unmatched closing brace at byte {i} in prompt template");
            }
            _ => {
                i += 1;
            }
        }
    }
    out.push_str(&template[copy_start..]);
    out
}

#[cfg(test)]
mod tests {
    use super::render;

    #[test]
    fn substitutes_named_placeholders() {
        let out = render("hello {name}!", &[("name", "world")]);
        assert_eq!(out, "hello world!");
    }

    #[test]
    fn replaces_every_occurrence_of_a_placeholder() {
        let out = render("{x}-{y}-{x}", &[("x", "A"), ("y", "B")]);
        assert_eq!(out, "A-B-A");
    }

    #[test]
    fn doubled_braces_emit_single_literal_braces() {
        let out = render("{{ {name} }}", &[("name", "ok")]);
        assert_eq!(out, "{ ok }");
    }

    #[test]
    fn doubled_braces_at_template_boundaries() {
        let out = render("{{a}}", &[]);
        assert_eq!(out, "{a}");
    }

    #[test]
    fn preserves_leading_and_trailing_whitespace_byte_for_byte() {
        let template = "\n  body {x}\n";
        let out = render(template, &[("x", "value")]);
        assert_eq!(out, "\n  body value\n");
    }

    #[test]
    fn copies_unicode_content_unchanged() {
        let out = render("ä·{name}·ø", &[("name", "★")]);
        assert_eq!(out, "ä·★·ø");
    }

    #[test]
    #[should_panic(expected = "unbound placeholder `{missing}`")]
    fn panics_on_unbound_placeholder() {
        let _ = render("hi {missing}", &[("present", "x")]);
    }

    #[test]
    #[should_panic(expected = "unclosed placeholder")]
    fn panics_on_unclosed_placeholder() {
        let _ = render("hi {oops", &[("oops", "x")]);
    }

    #[test]
    #[should_panic(expected = "unmatched closing brace")]
    fn panics_on_stray_closing_brace() {
        let _ = render("hi }oops", &[]);
    }
}
