//! Conservative presentation parsing for products already classified as Base.
//! This does not resolve identity or authorize canonicalizing an apparel tag.
use unicode_normalization::UnicodeNormalization;

const MARKETING: &[&str] = &[
    "オリジナル3Dモデル",
    "オリジナル3Dアバター",
    "オリジナルモデル",
    "オリジナルアバター",
    "Original 3D Model",
    "Original 3D Avatar",
    "Original Avatar",
    "3Dモデル",
    "3Dアバター",
    "3D Model",
    "3D Avatar",
];
const PLATFORM: &[&str] = &[
    "for VRChat",
    "for VRC",
    "VRChat対応",
    "VRChat向け",
    "VRChat想定",
    "VRChat用",
    "VRChat",
    "VRC対応",
    "VRC",
];

fn closing(c: char) -> Option<char> {
    match c {
        '「' => Some('」'),
        '『' => Some('』'),
        '【' => Some('】'),
        '〖' => Some('〗'),
        '〈' => Some('〉'),
        '《' => Some('》'),
        '〔' => Some('〕'),
        '[' => Some(']'),
        '(' => Some(')'),
        '“' => Some('”'),
        '‘' => Some('’'),
        '"' => Some('"'),
        '\'' => Some('\''),
        _ => None,
    }
}

/// A complete opening group; bytes after its matching close remain unconsumed.
fn opening_group(text: &str) -> Option<(&str, &str)> {
    let first = text.chars().next()?;
    let close = closing(first)?;
    let mut depth = 1;
    for (index, c) in text.char_indices().skip(1) {
        if c == close {
            depth -= 1;
            if depth == 0 {
                return Some((
                    &text[first.len_utf8()..index],
                    &text[index + c.len_utf8()..],
                ));
            }
        } else if first != close && c == first {
            depth += 1;
        }
    }
    None
}

fn unquote(mut text: &str) -> &str {
    text = text.trim();
    while let Some((inside, rest)) = opening_group(text) {
        if !rest.trim().is_empty() {
            break;
        }
        text = inside.trim();
    }
    text
}

fn known_wrapper(text: &str) -> bool {
    text.split('/').all(|part| {
        MARKETING
            .iter()
            .chain(PLATFORM)
            .any(|word| part.trim().eq_ignore_ascii_case(word))
    })
}

fn separator(c: char) -> bool {
    c.is_whitespace() || matches!(c, '/' | ':' | '-' | '—' | '–' | '−' | '｜' | '|')
}

fn strip_version(text: &str) -> &str {
    // Inspect original character boundaries; never slice using offsets from a
    // Unicode case-folded copy, whose byte length can differ.
    for (index, _) in text.char_indices() {
        let retained = unquote(text[..index].trim_end_matches(separator));
        if !retained.chars().any(char::is_alphanumeric) || known_wrapper(retained) {
            continue;
        }
        let tail = &text[index..];
        for marker in ["version", "ver.", "ver", "v"] {
            let Some(prefix) = tail.get(..marker.len()) else {
                continue;
            };
            if !prefix.eq_ignore_ascii_case(marker) {
                continue;
            }
            if text[..index]
                .chars()
                .next_back()
                .is_some_and(|c| c.is_ascii_alphanumeric())
            {
                continue;
            }
            if marker == "v"
                && index > 0
                && !text[..index].chars().next_back().is_some_and(|c| {
                    c.is_whitespace() || matches!(c, '」' | '』' | '”' | ')' | ']')
                })
            {
                continue;
            }
            let version = tail[marker.len()..].trim();
            if version.chars().next().is_some_and(|c| c.is_ascii_digit())
                && version
                    .chars()
                    .all(|c| c.is_ascii_digit() || matches!(c, '.' | '-' | '_'))
            {
                return text[..index].trim_end();
            }
        }
    }
    text
}

pub(super) fn cleaned_title(title: &str) -> String {
    let normalized: String = title.nfkc().collect();
    let mut name = normalized.trim();
    loop {
        let previous = name;
        name = strip_version(name);
        name = unquote(name);
        // Complete trailing platform/model groups are structural wrappers too.
        // Never remove arbitrary prose, compatibility, or another quoted name.
        for (index, c) in name.char_indices() {
            if index > 0 && closing(c).is_some() {
                if let Some((inside, rest)) = opening_group(&name[index..]) {
                    if rest.trim().is_empty() && known_wrapper(inside) {
                        name = name[..index].trim_end_matches(separator);
                        break;
                    }
                }
            }
        }
        if let Some((inside, rest)) = opening_group(name) {
            if known_wrapper(inside) && !rest.trim().is_empty() {
                name = rest.trim_start_matches(separator);
            }
        }
        for wrapper in MARKETING.iter().chain(PLATFORM) {
            // Prefer the complete recognized phrase over a shorter overlapping
            // suffix ("Original 3D Avatar" must not become the name "Original").
            // Recheck after each removal in case another whole wrapper remains.
            if known_wrapper(name) {
                return name.to_owned();
            }
            if name
                .get(..wrapper.len())
                .is_some_and(|prefix| prefix.eq_ignore_ascii_case(wrapper))
            {
                let rest = &name[wrapper.len()..];
                let boundary = !wrapper.is_ascii()
                    || rest
                        .chars()
                        .next()
                        .is_none_or(|c| separator(c) || closing(c).is_some());
                if boundary && rest.chars().any(char::is_alphanumeric) {
                    name = rest.trim_start_matches(separator);
                }
            }
            if let Some(index) = name.len().checked_sub(wrapper.len()) {
                if name
                    .get(index..)
                    .is_some_and(|suffix| suffix.eq_ignore_ascii_case(wrapper))
                {
                    let before = &name[..index];
                    if before.chars().any(char::is_alphanumeric)
                        && before.chars().next_back().is_some_and(separator)
                    {
                        name = before.trim_end_matches(separator);
                    }
                }
            }
        }
        if name == previous {
            return name.to_owned();
        }
    }
}

/// A source containing only a platform/model wrapper is not a proper name.
pub(super) fn has_name(title: &str) -> bool {
    let (cleaned, _) = base_parts(title);
    let label = unquote(&cleaned);
    label.chars().any(char::is_alphanumeric) && !known_wrapper(label) && !label.contains('/')
}

fn japanese_name(text: &str) -> bool {
    let has_japanese = text.chars().any(|c| matches!(c as u32, 0x3040..=0x30ff | 0x3400..=0x4dbf | 0x4e00..=0x9fff | 0xf900..=0xfaff | 0x20000..=0x323af));
    has_japanese
        && !text.contains('/')
        && !text.contains('\n')
        && !text.chars().any(|c| {
            matches!(
                c,
                '(' | ')' | '[' | ']' | '「' | '」' | '『' | '』' | '“' | '”' | '"'
            )
        })
        && ![
            "対応",
            "専用",
            "衣装",
            "服装",
            "テクスチャ",
            "アクセサリー",
            "向け",
            "用コート",
        ]
        .iter()
        .any(|word| text.contains(word))
}

fn latin_name(text: &str) -> bool {
    let latin_letter = |c: char| {
        c.is_ascii_alphabetic()
            || c.is_alphabetic() && matches!(c as u32, 0x00c0..=0x024f | 0x1e00..=0x1eff)
    };
    if !text.chars().any(latin_letter)
        || !text.chars().all(|c| {
            latin_letter(c)
                || c.is_ascii_digit()
                || c.is_whitespace()
                || matches!(c, '-' | '\'' | '’' | '.' | '_')
        })
    {
        return false;
    }
    // Fully consumed text can still be a product qualifier, not a second name.
    !text
        .split(|c: char| !c.is_ascii_alphanumeric())
        .any(|word| {
            [
                "coat",
                "jacket",
                "dress",
                "outfit",
                "clothes",
                "clothing",
                "costume",
                "texture",
                "hair",
                "accessory",
                "for",
                "compatible",
                "avatar",
                "model",
                "unity",
                "vrchat",
                "quest",
                "pc",
                "fbx",
                "vrm",
                "package",
                "set",
                "bonus",
                "files",
                "with",
                "includes",
                "support",
                "free",
                "download",
                "version",
            ]
            .iter()
            .any(|blocked| word.eq_ignore_ascii_case(blocked))
        })
}

fn pair(original: &str, english: &str) -> Option<(String, Option<String>)> {
    let original = unquote(original);
    let english = unquote(english);
    if japanese_name(original) && latin_name(english) {
        Some((original.to_owned(), Some(english.to_owned())))
    } else {
        None
    }
}

fn kana_form(text: &str) -> Option<String> {
    let result: String = text
        .nfkc()
        .filter(|c| !c.is_whitespace())
        .map(|c| {
            if ('ァ'..='ヶ').contains(&c) {
                char::from_u32(c as u32 - 0x60).unwrap()
            } else {
                c
            }
        })
        .collect();
    (!result.is_empty()
        && result
            .chars()
            .all(|c| ('ぁ'..='ゖ').contains(&c) || c == 'ー'))
    .then_some(result)
}

fn reading_pair(left: &str, right: &str) -> Option<(String, Option<String>)> {
    let left = unquote(left);
    let right = unquote(right);
    if !japanese_name(left) || !japanese_name(right) {
        return None;
    }
    if left == right {
        return Some((left.to_owned(), None));
    }
    for (original, kana) in [(left, right), (right, left)] {
        let has_han=original.chars().any(|c| matches!(c as u32,0x3400..=0x4dbf|0x4e00..=0x9fff|0xf900..=0xfaff|0x20000..=0x323af));
        if !has_han {
            continue;
        }
        if let Some(expected) = kana_form(kana) {
            if super::phonetic::reading(original)
                .as_deref()
                .and_then(kana_form)
                == Some(expected)
            {
                return Some((original.to_owned(), None));
            }
        }
    }
    None
}

/// Parse only presentation of a known Base product. An extracted Latin spelling
/// is source-title evidence, not verified creator English or an identity match.
/// Ambiguous or unsupported text remains intact rather than losing letters.
pub(super) fn base_parts(title: &str) -> (String, Option<String>) {
    let name = cleaned_title(title);
    if name.contains('/') {
        // Slash is structural only when each discarded segment is a complete
        // known wrapper, and the remainder is one name or one validated pair.
        let segments: Vec<_> = name.split('/').map(cleaned_title).collect();
        let retained: Vec<_> = segments
            .iter()
            .filter(|part| !known_wrapper(part))
            .collect();
        if retained.len() == 1 && retained.len() < segments.len() && !retained[0].is_empty() {
            return base_parts(retained[0]);
        }
        if retained.len() == 2 {
            let left = retained[0];
            let right = retained[1];
            if let Some(parts) = pair(left, right)
                .or_else(|| pair(right, left))
                .or_else(|| reading_pair(left, right))
            {
                return parts;
            }
        }
    }
    // Japanese name followed by a complete parenthesized Latin spelling.
    for (index, c) in name.char_indices() {
        if c == '(' {
            if let Some((inside, rest)) = opening_group(&name[index..]) {
                if rest.trim().is_empty() {
                    if let Some(parts) = pair(&name[..index], inside) {
                        return parts;
                    }
                }
            }
        }
    }
    // A quoted Japanese name may be followed only by a Latin name (not arbitrary
    // trailing product prose); the whole input must be consumed by the pair.
    if let Some((inside, rest)) = opening_group(&name) {
        let rest = rest.trim();
        if let Some(parts) = pair(inside, rest) {
            return parts;
        }
    }
    for (index, c) in name.char_indices() {
        if matches!(c, '-' | '—' | '–' | '−') {
            let rest = name[index + c.len_utf8()..].trim();
            let english = rest.strip_suffix(c).unwrap_or(rest).trim();
            if let Some(parts) = pair(&name[..index], english) {
                return parts;
            }
        }
    }
    // A complete space-separated bilingual title is common on creator pages.
    for (index, c) in name.char_indices() {
        if c.is_whitespace() {
            if let Some(parts) = pair(&name[..index], &name[index..])
                .or_else(|| pair(&name[index..], &name[..index]))
            {
                return parts;
            }
        }
    }
    (name, None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trailing_structural_wrappers_and_plain_bilingual_names_are_consumed() {
        for title in [
            "ソラネコ Soraneko / オリジナル3Dモデル",
            "Original 3D Avatar ソラネコ Soraneko for VRChat",
            "Soraneko ソラネコ / Original 3D Avatar",
            "「ソラネコ」 Soraneko 【オリジナル3Dモデル】",
            "〖VRChat対応〗 ソラネコ (Soraneko) 〔3D Avatar〕",
        ] {
            assert_eq!(
                base_parts(title),
                ("ソラネコ".into(), Some("Soraneko".into())),
                "{title}"
            );
        }
        for title in [
            "ソラネコ 【別モデル】",
            "ソラネコ (対応衣装)",
            "ソラネコ (Soraneko) bonus files",
        ] {
            assert_eq!(base_parts(title), (title.into(), None), "{title}");
        }
    }

    #[test]
    fn slash_wrappers_and_reading_pairs_do_not_invent_multi_avatar_names() {
        for title in [
            "VRChat対応 / オリジナル3Dモデル「星空猫」 / VRChat",
            "オリジナル3Dモデル / 星空猫 / VRChat対応",
            "星空猫 / 星空猫",
            "VRChat対応 星空猫 / VRChat",
        ] {
            assert_eq!(base_parts(title), ("星空猫".into(), None), "{title}");
            assert!(has_name(title));
        }
        for title in [
            "Hoshizoraneko / 星空猫",
            "Original 3D Model / 星空猫 / Hoshizoraneko / VRChat",
        ] {
            assert_eq!(
                base_parts(title),
                ("星空猫".into(), Some("Hoshizoraneko".into()))
            );
            assert!(has_name(title));
        }
        for title in [
            "星猫 / 海猫",
            "オリジナル3Dモデル「星猫」 / オリジナル3Dモデル「海猫」",
            "VRChat / 3D Avatar",
            "星空猫 / 別人",
        ] {
            assert!(!has_name(title), "{title}");
        }
        #[cfg(target_os = "windows")]
        for title in [
            "星空猫 / ホシゾラネコ",
            "ほしぞらねこ / 星空猫 / VRChat対応",
        ] {
            assert_eq!(base_parts(title), ("星空猫".into(), None));
            assert!(has_name(title));
        }
    }

    #[test]
    fn fully_consumed_bilingual_base_titles_have_one_pair() {
        for title in [
            "オリジナル３Ｄモデル「ソラネコ / Soraneko」Ver.1.2",
            "【VRChat対応】オリジナル3Dモデル ソラネコ（Soraneko）",
            "【VRChat対応 / オリジナル3Dモデル】ソラネコ（Soraneko）",
            "ソラネコ -Soraneko- / オリジナル3Dモデル",
            "Original 3D Model “ソラネコ” Soraneko v1.2",
            "ソラネコ — Soraneko",
            "ソラネコ / “Soraneko”",
        ] {
            assert_eq!(
                base_parts(title),
                ("ソラネコ".into(), Some("Soraneko".into())),
                "{title}"
            );
        }
        assert_eq!(
            base_parts("ソラネコ -Sora-Neko-"),
            ("ソラネコ".into(), Some("Sora-Neko".into()))
        );
        assert_eq!(
            base_parts("ソラネコ / Sóraneko"),
            ("ソラネコ".into(), Some("Sóraneko".into()))
        );
    }

    #[test]
    fn ambiguous_products_and_unsupported_letters_are_preserved() {
        for label in [
            "Original 3D Avatar",
            "Original 3D Model",
            "3D Avatar",
            "オリジナル3Dアバター",
        ] {
            assert_eq!(base_parts(label), (label.to_owned(), None));
            assert!(!has_name(label));
        }
        assert_eq!(
            base_parts("【VRChat対応】 Original 3D Avatar"),
            ("Original 3D Avatar".into(), None)
        );
        assert!(!has_name("【VRChat対応】 Original 3D Avatar"));
        for title in [
            "「真冬」対応コート",
            "「真冬」 Jacket",
            "真冬 / Jacket",
            "真冬 / マヌカ",
            "真冬 / Mafuyu / マヌカ",
            "ソラネコ (PC Quest)",
            "ソラネコ / Сора",
            "ソラネコ / Σόρα",
            "ソラネコ / Soraneko))))",
            "そら🧪ねこ",
            "未知漢字",
            "ソラネコ / Soraneko bonus files",
        ] {
            let (original, english) = base_parts(title);
            assert_eq!(original, title, "{title}");
            assert!(english.is_none(), "{title}");
        }
        assert_eq!(
            base_parts("Original 3D Modeler"),
            ("Original 3D Modeler".into(), None)
        );
        assert_eq!(base_parts("Silver2"), ("Silver2".into(), None));
        assert_eq!(base_parts("Version2"), ("Version2".into(), None));
        assert_eq!(base_parts("“V2”"), ("V2".into(), None));
        assert_eq!(base_parts("Original 3D Model V2"), ("V2".into(), None));
        assert_eq!(
            base_parts("【オリジナル3Dモデル】「未知漢字」"),
            ("未知漢字".into(), None)
        );
        assert_eq!(
            base_parts("İİİİソラネコ Ver.1.2"),
            ("İİİİソラネコ".into(), None)
        );
    }
}
