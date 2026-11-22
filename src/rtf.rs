//! Minimal RTF → plain text conversion for license files.
//!
//! Keeps paragraphs, line breaks, tabs and special characters (`\'hh` in the
//! document's Windows code page, `\uN` Unicode escapes); drops formatting and
//! non-text groups such as font/colour tables, pictures and document info.

/// Characters 0x80..=0x9F of Windows-1252; everything else maps 1:1 to Latin-1.
const CP1252_HIGH: [char; 32] = [
    '€', '\u{81}', '‚', 'ƒ', '„', '…', '†', '‡', 'ˆ', '‰', 'Š', '‹', 'Œ', '\u{8D}', 'Ž', '\u{8F}', '\u{90}', '‘', '’', '“',
    '”', '•', '–', '—', '˜', '™', 'š', '›', 'œ', '\u{9D}', 'ž', 'Ÿ',
];

/// Destinations whose contents are never visible text.
const SKIP_DESTINATIONS: &[&str] = &[
    "fonttbl", "colortbl", "stylesheet", "info", "pict", "object", "header", "footer", "headerl", "headerr", "headerf",
    "footerl", "footerr", "footerf", "listtable", "listoverridetable", "rsidtbl", "generator", "xmlnstbl", "themedata",
    "colorschememapping", "latentstyles", "datastore", "fldinst", "filetbl", "revtbl", "pgdsctbl", "mmathPr", "wgrffmtfilter",
];

fn cp1252(b: u8) -> char {
    if (0x80..=0x9F).contains(&b) {
        CP1252_HIGH[(b - 0x80) as usize]
    } else {
        b as char
    }
}

pub fn is_rtf(bytes: &[u8]) -> bool {
    bytes.starts_with(b"{\\rtf")
}

pub fn to_text(rtf: &str) -> String {
    let chars: Vec<char> = rtf.chars().collect();
    let mut out = String::new();
    // Per group: (skip contents, unicode fallback length \ucN)
    let mut stack: Vec<(bool, usize)> = vec![(false, 1)];
    let mut i = 0;
    let mut pending_skip = 0usize; // fallback chars to drop after \uN

    while i < chars.len() {
        let c = chars[i];
        let (skip, uc) = *stack.last().unwrap_or(&(false, 1));
        match c {
            '{' => {
                stack.push((skip, uc));
                i += 1;
                // "{\*\dest ..." marks an optional destination we don't understand.
                if chars.get(i) == Some(&'\\') && chars.get(i + 1) == Some(&'*') {
                    if let Some(top) = stack.last_mut() {
                        top.0 = true;
                    }
                }
            }
            '}' => {
                stack.pop();
                if stack.is_empty() {
                    break;
                }
                i += 1;
            }
            '\\' => {
                i += 1;
                let Some(&n) = chars.get(i) else { break };
                if n.is_ascii_alphabetic() {
                    let start = i;
                    while i < chars.len() && chars[i].is_ascii_alphabetic() {
                        i += 1;
                    }
                    let word: String = chars[start..i].iter().collect();
                    let num_start = i;
                    if i < chars.len() && (chars[i] == '-' || chars[i].is_ascii_digit()) {
                        i += 1;
                        while i < chars.len() && chars[i].is_ascii_digit() {
                            i += 1;
                        }
                    }
                    let num: Option<i32> = chars[num_start..i].iter().collect::<String>().parse().ok();
                    if chars.get(i) == Some(&' ') {
                        i += 1; // the delimiter space belongs to the control word
                    }

                    if SKIP_DESTINATIONS.contains(&word.as_str()) {
                        if let Some(top) = stack.last_mut() {
                            top.0 = true;
                        }
                        continue;
                    }
                    if skip {
                        continue;
                    }
                    match word.as_str() {
                        "par" | "line" | "sect" | "page" => out.push('\n'),
                        "row" => out.push('\n'),
                        "tab" | "cell" => out.push('\t'),
                        "emdash" => out.push('—'),
                        "endash" => out.push('–'),
                        "bullet" => out.push('•'),
                        "lquote" => out.push('‘'),
                        "rquote" => out.push('’'),
                        "ldblquote" => out.push('“'),
                        "rdblquote" => out.push('”'),
                        "uc" => {
                            if let (Some(top), Some(v)) = (stack.last_mut(), num) {
                                top.1 = v.max(0) as usize;
                            }
                        }
                        "u" => {
                            if let Some(v) = num {
                                let code = if v < 0 { (v + 65536) as u32 } else { v as u32 };
                                if let Some(ch) = char::from_u32(code) {
                                    out.push(ch);
                                }
                                pending_skip = uc;
                            }
                        }
                        _ => {}
                    }
                } else {
                    i += 1;
                    match n {
                        '\'' => {
                            let hex: String = chars.get(i..i + 2).map(|s| s.iter().collect()).unwrap_or_default();
                            i += 2;
                            if let Ok(b) = u8::from_str_radix(&hex, 16) {
                                if pending_skip > 0 {
                                    pending_skip -= 1;
                                } else if !skip {
                                    out.push(cp1252(b));
                                }
                            }
                        }
                        '\\' | '{' | '}' if !skip => {
                            if pending_skip > 0 {
                                pending_skip -= 1;
                            } else {
                                out.push(n);
                            }
                        }
                        '~' if !skip => out.push('\u{A0}'),
                        '_' if !skip => out.push('-'),
                        '\n' | '\r' if !skip => out.push('\n'), // "\<newline>" == \par
                        _ => {}
                    }
                }
            }
            '\r' | '\n' => i += 1, // raw newlines are not significant in RTF
            _ => {
                i += 1;
                if pending_skip > 0 {
                    pending_skip -= 1;
                } else if !skip {
                    out.push(c);
                }
            }
        }
    }

    // Tidy up: trim trailing spaces per line and collapse 3+ blank lines.
    let mut tidy = String::new();
    let mut blank = 0;
    for line in out.lines() {
        let l = line.trim_end();
        if l.is_empty() {
            blank += 1;
            if blank > 1 {
                continue;
            }
        } else {
            blank = 0;
        }
        tidy.push_str(l);
        tidy.push('\n');
    }
    tidy.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_wordpad_style_rtf() {
        let rtf = r"{\rtf1\ansi\ansicpg1252\deff0{\fonttbl{\f0\fnil\fcharset0 Calibri;}}{\colortbl ;\red255\green0\blue0;}
{\*\generator Riched20 10.0.19041}\viewkind4\uc1
\pard\sa200\sl276\slmult1\b\f0\fs22 MIT License\b0\par
Copyright \'a9 2026 Florin \endash  caf\'e9\par
Unicode: \u537?\u259? ok\tab Tab\par
}";
        let t = to_text(rtf);
        assert_eq!(t, "MIT License\nCopyright © 2026 Florin – café\nUnicode: șă ok\tTab");
    }
}
