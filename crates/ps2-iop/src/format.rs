//! `printf`-style formatting for the IOP's `printf` and `sprintf`, over
//! 32-bit guest arguments.
//!
//! Conversions: `d i u x X o c s p %`, with the flags `- 0 + space #`, a
//! width and precision (`*` takes an argument), and the length modifiers
//! `h l` (no effect: every integer is 32 bits on the IOP).

/// Formats `format`, taking each argument from `next` and strings through
/// `string` (a guest address to its bytes, without the terminator).
#[allow(clippy::too_many_lines, reason = "one parser, top to bottom")]
pub(crate) fn format(
    format: &[u8],
    mut next: impl FnMut() -> u32,
    string: impl Fn(u32) -> Vec<u8>,
) -> Vec<u8> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < format.len() {
        let c = format[i];
        i += 1;
        if c != b'%' {
            out.push(c);
            continue;
        }
        let (mut left, mut zero, mut plus, mut space, mut alt) =
            (false, false, false, false, false);
        while let Some(&f) = format.get(i) {
            match f {
                b'-' => left = true,
                b'0' => zero = true,
                b'+' => plus = true,
                b' ' => space = true,
                b'#' => alt = true,
                _ => break,
            }
            i += 1;
        }
        let mut number = |i: &mut usize| -> Option<usize> {
            if format.get(*i) == Some(&b'*') {
                *i += 1;
                return Some(next() as usize);
            }
            let start = *i;
            while format.get(*i).is_some_and(u8::is_ascii_digit) {
                *i += 1;
            }
            std::str::from_utf8(&format[start..*i]).ok()?.parse().ok()
        };
        let width = number(&mut i).unwrap_or(0);
        let precision = if format.get(i) == Some(&b'.') {
            i += 1;
            Some(number(&mut i).unwrap_or(0))
        } else {
            None
        };
        while matches!(
            format.get(i),
            Some(b'h' | b'l' | b'L' | b'q' | b'z' | b'j' | b't')
        ) {
            i += 1;
        }
        let Some(&conversion) = format.get(i) else {
            break;
        };
        i += 1;
        let (prefix, mut body): (&[u8], Vec<u8>) = match conversion {
            b'd' | b'i' => {
                let v = next().cast_signed();
                let sign: &[u8] = if v < 0 {
                    b"-"
                } else if plus {
                    b"+"
                } else if space {
                    b" "
                } else {
                    b""
                };
                (sign, v.unsigned_abs().to_string().into_bytes())
            }
            b'u' => (b"", next().to_string().into_bytes()),
            b'x' => {
                let v = next();
                (
                    if alt && v != 0 { b"0x" } else { b"" },
                    format!("{v:x}").into_bytes(),
                )
            }
            b'X' => {
                let v = next();
                (
                    if alt && v != 0 { b"0X" } else { b"" },
                    format!("{v:X}").into_bytes(),
                )
            }
            b'o' => (
                if alt { b"0" } else { b"" },
                format!("{:o}", next()).into_bytes(),
            ),
            b'p' => (b"0x", format!("{:x}", next()).into_bytes()),
            b'c' => (b"", vec![next() as u8]),
            b's' => {
                let mut s = string(next());
                if let Some(p) = precision {
                    s.truncate(p);
                }
                (b"", s)
            }
            b'%' => (b"", b"%".to_vec()),
            other => (b"", vec![b'%', other]),
        };
        // Integer precision: at least that many digits.
        if let Some(p) = precision
            && matches!(conversion, b'd' | b'i' | b'u' | b'x' | b'X' | b'o')
            && body.len() < p
        {
            let mut padded = vec![b'0'; p - body.len()];
            padded.extend_from_slice(&body);
            body = padded;
        }
        let len = prefix.len() + body.len();
        let pad = width.saturating_sub(len);
        let zero = zero && !left && precision.is_none() && conversion != b's' && conversion != b'c';
        if !left && !zero {
            out.extend(std::iter::repeat_n(b' ', pad));
        }
        out.extend_from_slice(prefix);
        if zero {
            out.extend(std::iter::repeat_n(b'0', pad));
        }
        out.extend_from_slice(&body);
        if left {
            out.extend(std::iter::repeat_n(b' ', pad));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::format;

    fn f(fmt: &str, args: &[u32]) -> String {
        let mut it = args.iter().copied();
        let bytes = format(
            fmt.as_bytes(),
            || it.next().unwrap_or(0),
            |_| b"str".to_vec(),
        );
        String::from_utf8(bytes).unwrap_or_default()
    }

    #[test]
    fn conversions() {
        assert_eq!(
            f(
                "%d %x %s %c",
                &[(-42i32).cast_unsigned(), 0xbeef, 0, u32::from(b'z')]
            ),
            "-42 beef str z"
        );
        assert_eq!(
            f("[%5d|%-5d|%05d]", &[42, 42, (-42i32).cast_unsigned()]),
            "[   42|42   |-0042]"
        );
        assert_eq!(
            f("%08X %#x %.3d %%", &[0xabc, 0x10, 7]),
            "00000ABC 0x10 007 %"
        );
        assert_eq!(f("%*d %.2s %lu", &[4, 9, 0, 12]), "   9 st 12");
    }
}
