/// Removes terminal escape sequences (CSI, OSC, DCS/SOS/PM/APC strings, two-byte ESC
/// sequences and their C1 single-char forms) from `text`. Everything else is kept.
#[must_use]
pub fn strip_escapes(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '\u{1b}' => match chars.peek().copied() {
                Some('[') => {
                    chars.next();
                    skip_csi(&mut chars);
                }
                Some(']' | 'P' | 'X' | '^' | '_') => {
                    chars.next();
                    skip_string(&mut chars);
                }
                Some(_) => skip_esc(&mut chars),
                None => {}
            },
            '\u{9b}' => skip_csi(&mut chars),
            '\u{9d}' | '\u{90}' | '\u{98}' | '\u{9e}' | '\u{9f}' => skip_string(&mut chars),
            _ => out.push(ch),
        }
    }
    out
}

type Chars<'a> = std::iter::Peekable<std::str::Chars<'a>>;

/// After `ESC [`: parameter and intermediate bytes, then one final byte (0x40..=0x7e).
/// Any other character aborts the sequence and is left for the caller.
fn skip_csi(chars: &mut Chars<'_>) {
    while let Some(&ch) = chars.peek() {
        match ch {
            '\u{20}'..='\u{3f}' => {
                chars.next();
            }
            '\u{40}'..='\u{7e}' => {
                chars.next();
                return;
            }
            _ => return,
        }
    }
}

/// After an OSC/DCS/SOS/PM/APC introducer: consume up to BEL, ST (`ESC \` or U+009C).
/// An unterminated string runs to the end of the input; a stray ESC ends it unconsumed.
fn skip_string(chars: &mut Chars<'_>) {
    while let Some(&ch) = chars.peek() {
        match ch {
            '\u{7}' | '\u{9c}' => {
                chars.next();
                return;
            }
            '\u{1b}' => {
                chars.next();
                if chars.peek() == Some(&'\\') {
                    chars.next();
                } else {
                    // Not an ST: leave the byte after ESC to be re-read as a new sequence.
                    skip_esc_from_start(chars);
                }
                return;
            }
            _ => {
                chars.next();
            }
        }
    }
}

/// A stray `ESC` already consumed inside a string: treat what follows as a fresh ESC.
fn skip_esc_from_start(chars: &mut Chars<'_>) {
    match chars.peek().copied() {
        Some('[') => {
            chars.next();
            skip_csi(chars);
        }
        Some(']' | 'P' | 'X' | '^' | '_') => {
            chars.next();
            skip_string(chars);
        }
        Some(_) => skip_esc(chars),
        None => {}
    }
}

/// After `ESC`: intermediates (0x20..=0x2f) then one final byte (0x30..=0x7e).
fn skip_esc(chars: &mut Chars<'_>) {
    while let Some(&ch) = chars.peek() {
        match ch {
            '\u{20}'..='\u{2f}' => {
                chars.next();
            }
            '\u{30}'..='\u{7e}' => {
                chars.next();
                return;
            }
            _ => return,
        }
    }
}

/// Makes text safe to type into a PTY: drops ESC and every other C0 control (plus DEL and
/// the C1 range) except `\n` and `\t`, so the text cannot inject terminal sequences.
#[must_use]
pub fn sanitize_send_text(text: &str) -> String {
    text.chars()
        .filter(|&ch| !ch.is_control() || ch == '\n' || ch == '\t')
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_escapes_removes_csi_osc() {
        assert_eq!(strip_escapes("\u{1b}[31mred\u{1b}[0m"), "red");
        assert_eq!(strip_escapes("a\u{1b}[2K\u{1b}[1;32;40mb"), "ab");
        assert_eq!(strip_escapes("\u{1b}]0;title\u{7}after"), "after");
        assert_eq!(
            strip_escapes("\u{1b}]8;;http://x\u{1b}\\link\u{1b}]8;;\u{1b}\\"),
            "link"
        );
        assert_eq!(strip_escapes("\u{1b}Pq data\u{1b}\\ok"), "ok");
        assert_eq!(strip_escapes("x\u{1b}(By\u{1b}=z"), "xyz");
        assert_eq!(strip_escapes("\u{9b}31mred\u{9d}t\u{7}!"), "red!");
    }

    #[test]
    fn strip_escapes_keeps_plain_text_and_whitespace() {
        let text = "line1\nline2\ttab \u{e7}\u{1f600}";
        assert_eq!(strip_escapes(text), text);
    }

    #[test]
    fn strip_escapes_handles_truncated_and_nested_sequences() {
        assert_eq!(strip_escapes("a\u{1b}"), "a");
        assert_eq!(strip_escapes("a\u{1b}[31"), "a");
        assert_eq!(strip_escapes("a\u{1b}]0;never ends"), "a");
        assert_eq!(strip_escapes("\u{1b}]0;t\u{1b}[31mred"), "red");
        assert_eq!(strip_escapes("\u{1b}[3\u{1}1m"), "\u{1}1m");
    }

    #[test]
    fn sanitize_send_text_drops_esc_keeps_newline() {
        assert_eq!(
            sanitize_send_text("ls\n\u{1b}[201~rm -rf\tx\r\u{7}\u{0}\u{7f}\u{9b}"),
            "ls\n[201~rm -rf\tx"
        );
        assert_eq!(sanitize_send_text("ol\u{e1} \u{1f600}"), "ol\u{e1} \u{1f600}");
    }
}
