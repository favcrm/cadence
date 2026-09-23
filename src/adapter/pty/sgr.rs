//! Reading a styled pane capture (`tmux capture-pane -e`): the text
//! with its escape sequences removed, and the same text with its
//! SGR-dim cells removed.
//!
//! A TUI can render text in its input line that the user never typed —
//! Claude Code's prompt suggestion is dim (`ESC[2m`) ghost text in an
//! empty box. A plain capture (`-p`) cannot tell it from a draft; the
//! attributes can. Only the dim bit is tracked: everything else a
//! capture carries (colours, bold, OSC 8 hyperlinks, charset shifts) is
//! dropped.
//!
//! tmux carries attribute state across the lines of one capture — a
//! row can start dim without repeating `ESC[2m` — so dim state is
//! tracked over the whole capture, never reset per line.

/// A styled capture reduced to plain text. `plain` is the capture with
/// every escape sequence and control character (except `\n` and `\t`)
/// removed; `undimmed` is `plain` minus the cells drawn dim. Both keep
/// the capture's line structure, so row `i` of one is row `i` of the
/// other.
pub struct Frame {
    pub plain: String,
    pub undimmed: String,
}

/// The plain text of a styled capture — see [`Frame::plain`].
pub fn strip(styled: &str) -> String {
    parse(styled).plain
}

/// Split a styled capture into its plain and undimmed text.
pub fn parse(styled: &str) -> Frame {
    let mut plain = String::with_capacity(styled.len());
    let mut undimmed = String::with_capacity(styled.len());
    let mut dim = false;
    let mut chars = styled.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\x1b' => match chars.next() {
                // CSI: parameter and intermediate bytes, then one final
                // byte (0x40–0x7E). Only `m` (SGR) changes the dim bit.
                Some('[') => {
                    let mut params = String::new();
                    let mut last = None;
                    for p in chars.by_ref() {
                        if ('\x40'..='\x7e').contains(&p) {
                            last = Some(p);
                            break;
                        }
                        params.push(p);
                    }
                    if last == Some('m') {
                        dim = apply_sgr(dim, &params);
                    }
                }
                // OSC (an OSC 8 hyperlink): runs to BEL or ST (`ESC \`).
                Some(']') => {
                    while let Some(p) = chars.next() {
                        if p == '\x07' {
                            break;
                        }
                        if p == '\x1b' {
                            chars.next_if_eq(&'\\');
                            break;
                        }
                    }
                }
                // A charset pick (`ESC ( B`) is three characters long;
                // any other escape is two.
                Some('(') | Some(')') => {
                    chars.next();
                }
                _ => {}
            },
            '\n' | '\t' => {
                plain.push(c);
                undimmed.push(c);
            }
            c if c.is_control() => {}
            c => {
                plain.push(c);
                if !dim {
                    undimmed.push(c);
                }
            }
        }
    }
    Frame { plain, undimmed }
}

/// Apply one SGR parameter list to the dim bit. `0` (or an empty list,
/// `ESC[m`) resets everything; `2` sets dim; `22` (normal intensity)
/// clears it. Extended colours take arguments that must not be read as
/// attributes — `38;5;2` is palette colour 2, not dim — so `38`/`48`/`58`
/// consume their `5;n` or `2;r;g;b` tail. A colon sub-parameter group
/// (`38:5:2`, `4:3`) is self-contained and never sets dim.
fn apply_sgr(mut dim: bool, params: &str) -> bool {
    if params.is_empty() {
        return false;
    }
    let mut it = params.split(';');
    while let Some(p) = it.next() {
        if p.contains(':') {
            continue;
        }
        match p.parse::<u32>().unwrap_or(0) {
            0 => dim = false,
            2 => dim = true,
            22 => dim = false,
            38 | 48 | 58 => match it.next() {
                Some("5") => {
                    it.next();
                }
                Some("2") => {
                    it.nth(2);
                }
                _ => {}
            },
            _ => {}
        }
    }
    dim
}

#[cfg(test)]
mod tests {
    use super::*;

    fn undimmed(s: &str) -> String {
        parse(s).undimmed
    }

    #[test]
    fn strip_removes_sgr_osc_and_charset_shifts() {
        let s = "\x1b[38;5;246mhi\x1b[39m \x1b]8;id=x;https://e.x/\x1b\\link\x1b]8;;\x1b\\ \
                 \x0eq\x0f \x1b]0;title\x07end\x1b(B";
        assert_eq!(strip(s), "hi link q end");
    }

    #[test]
    fn dim_cells_drop_from_undimmed_only() {
        let f = parse("❯\u{a0}\x1b[2msuggestion\x1b[0m");
        assert_eq!(f.plain, "❯\u{a0}suggestion");
        assert_eq!(f.undimmed, "❯\u{a0}");
    }

    #[test]
    fn normal_intensity_and_resets_end_dim() {
        assert_eq!(undimmed("\x1b[2mghost\x1b[22mtyped"), "typed");
        assert_eq!(undimmed("\x1b[2mghost\x1b[mtyped"), "typed");
        assert_eq!(undimmed("\x1b[2mghost\x1b[0;1mtyped"), "typed");
        // Bold on / other attributes off leave dim alone.
        assert_eq!(undimmed("\x1b[2mghost\x1b[1;39mstill"), "");
    }

    #[test]
    fn extended_colour_arguments_are_not_attributes() {
        // Palette/RGB components of 2 are colours, not dim.
        assert_eq!(undimmed("\x1b[38;5;2mgreen"), "green");
        assert_eq!(undimmed("\x1b[48;2;2;2;2mrgb"), "rgb");
        assert_eq!(undimmed("\x1b[38:5:2mcolon"), "colon");
        // Dim alongside a colour, in either order, is still dim.
        assert_eq!(undimmed("\x1b[2;38;5;246mghost\x1b[0mx"), "x");
        assert_eq!(undimmed("\x1b[38;5;246;2mghost\x1b[0mx"), "x");
    }

    #[test]
    fn dim_state_carries_across_lines() {
        let f = parse("a\x1b[2mb\nc\x1b[0md\n");
        assert_eq!(f.undimmed, "a\nd\n");
        assert_eq!(f.plain.lines().count(), f.undimmed.lines().count());
    }
}
