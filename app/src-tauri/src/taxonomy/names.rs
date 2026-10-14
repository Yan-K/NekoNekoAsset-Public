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


/// Base presentation is separate from exact tag cleanup: descriptors in a Base
/// title must never make a similarly worded outfit an automatic avatar match.
pub(super) fn structured_base_parts(title: &str) -> super::AvatarLabelParts {
    let decoded = decode_entities(title);
    let source: String = decoded.nfkc().collect::<String>().split_whitespace().collect::<Vec<_>>().join(" ");
    let mut qualifiers = Vec::new();
    let mut name = base_edges(&source, &mut qualifiers);
    // A complete inner name remains useful when an outer marketing bracket is
    // malformed. Surrounding letters must still be fully recognized metadata.
    for (index, c) in name.char_indices() {
        if !matches!(c, '「' | '『' | '“' | '"' | '\'' | '【') {
            continue;
        }
        let Some((inside, rest)) = opening_group(&name[index..]) else { continue };
        if descriptive_fragment(&name[..index]) && descriptive_fragment(rest) {
            name = inside.trim().to_owned();
            break;
        }
    }
    name = base_edges(&name, &mut qualifiers);
    let bundle = name.to_ascii_lowercase().contains(" and ");
    if bundle {
        return super::AvatarLabelParts { original: name, source_latin: None, qualifiers, name_kind: "bundle" };
    }
    if descriptive_fragment(&name) {
        return super::AvatarLabelParts { original: source, source_latin: None, qualifiers, name_kind: "fallback" };
    }
    let (mut original, mut source_latin) = base_parts(&name);
    if source_latin.is_none() {
        // Source pages also put their Latin spelling first.
        for (index, c) in name.char_indices() {
            if matches!(c, '-' | '—' | '–' | '−') {
                let right = name[index + c.len_utf8()..].trim().trim_end_matches(c).trim();
                if let Some((jp, latin)) = pair(right, &name[..index]) {
                    original = jp;
                    source_latin = latin;
                    break;
                }
            }
        }
    }
    let single = original.chars().any(char::is_alphanumeric)
        && !descriptive_fragment(&original)
        && !original.contains(['/', '#', '(', ')', '[', ']', '「', '」', '『', '』', '【', '】', '"']);
    if !single {
        return super::AvatarLabelParts {
            original: if name.chars().any(char::is_alphanumeric) && !descriptive_fragment(&name) { name } else { source },
            source_latin: None, qualifiers, name_kind: "fallback",
        };
    }
    // A name already written in Latin letters needs no second transliteration.
    // Keep its punctuation (for example Ren♡Ai) as source-title evidence.
    if source_latin.is_none() && original.chars().any(char::is_alphabetic)
        && original.chars().all(|c| c.is_ascii() || !c.is_alphanumeric() || matches!(c as u32, 0x00c0..=0x024f | 0x1e00..=0x1eff)) {
        source_latin = Some(original.clone());
    }
    super::AvatarLabelParts { original, source_latin, qualifiers, name_kind: "single" }
}

fn decode_entities(input: &str) -> String {
    let mut result = String::new();
    let mut rest = input;
    while let Some(start) = rest.find('&') {
        result.push_str(&rest[..start]);
        let tail = &rest[start..];
        let entity = tail.find(';').filter(|end| *end <= 12);
        let decoded = entity.and_then(|end| {
            let body = &tail[1..end];
            match body {
                "amp" => Some('&'), "quot" => Some('"'), "apos" => Some('\''),
                "lt" => Some('<'), "gt" => Some('>'), "nbsp" => Some(' '),
                _ => body.strip_prefix("#x").or_else(|| body.strip_prefix("#X"))
                    .and_then(|hex| u32::from_str_radix(hex, 16).ok())
                    .or_else(|| body.strip_prefix('#').and_then(|decimal| decimal.parse::<u32>().ok()))
                    .and_then(char::from_u32),
            }.map(|c| (end, c))
        });
        if let Some((end, c)) = decoded {
            result.push(c);
            rest = &tail[end + 1..];
        } else {
            result.push('&');
            rest = &tail[1..];
        }
    }
    result.push_str(rest);
    result
}

fn decoration(c: char) -> bool {
    matches!(c, '★' | '☆') || matches!(c as u32, 0x1f000..=0x1ffff)
}

fn compact_fragment(text: &str) -> String {
    text.chars().filter(|c| !c.is_whitespace()
        && !separator(*c) && !decoration(*c)
        && !matches!(c, '「' | '」' | '『' | '』' | '【' | '】' | '[' | ']' | '(' | ')' | '"' | '\'' | '“' | '”'))
        .flat_map(char::to_lowercase).collect()
}

fn technical_marker(text: &str) -> bool {
    ["eng", "gameready", "toribase"].contains(&compact_fragment(text).as_str())
}

fn descriptive_fragment(mut text: &str) -> bool {
    // Language/technical markers are metadata only inside complete groups.
    while let Some((inside, rest)) = opening_group(text.trim()) {
        if !technical_marker(inside) { break; }
        text = rest.trim();
    }
    let compact = compact_fragment(text);
    let mut rest = compact.as_str();
    const DESCRIPTORS: &[&str] = &[
        "vrchat向け", "vrchat想定", "vrchat対応", "vrchat用",
        "二次創作3dモデル", "二次創作3dアバター",
        "オリジナル3dモデル", "オリジナル3dアバター", "オリジナルモデル", "オリジナルアバター",
        "original3dmodel", "original3davatar", "originalavatar",
        "3dモデル", "3dアバター", "3dmodel", "3davatar",
        "forvrchat", "forvrc", "vrc想定", "vrc対応", "vrchat", "vrc",
        "liltoon対応", "quest対応", "無料配布", "無料", "blendfile",
    ];
    let mut consumed = false;
    while !rest.is_empty() {
        let Some(word) = DESCRIPTORS.iter().find(|word| rest.starts_with(**word)) else {
            let version = ["version", "ver.", "ver", "v"].iter().find_map(|marker| rest.strip_prefix(*marker));
            return consumed && version.is_some_and(|value| {
                value.chars().next().is_some_and(|c| c.is_ascii_digit())
                    && value.chars().all(|c| c.is_ascii_digit() || matches!(c, '.' | '_'))
            });
        };
        consumed = true;
        rest = &rest[word.len()..];
    }
    true
}

fn edition(text: &str) -> Option<String> {
    let compact = compact_fragment(text);
    if compact == "r18edition" { return Some("R18 Edition".into()); }
    if compact == "medium" { return Some("Medium".into()); }
    let generation = compact.strip_prefix("gen")?;
    (!generation.is_empty() && generation.chars().next().is_some_and(|c| c.is_ascii_digit())
        && generation.chars().all(|c| c.is_ascii_digit() || c == '.'))
        .then(|| format!("Gen{generation}"))
}

fn retain_edition(qualifiers: &mut Vec<String>, value: String) {
    if !qualifiers.contains(&value) { qualifiers.push(value); }
}

fn strip_hashtags(text: &str) -> &str {
    for (index, c) in text.char_indices() {
        if c != '#' || index == 0 { continue; }
        let before = &text[..index];
        if !before.chars().next_back().is_some_and(|c| c.is_whitespace() || matches!(c, '-' | '」' | '』' | ')' | ']')) {
            continue;
        }
        if text[index..].split_whitespace().all(|part| part.starts_with('#') && part.len() > 1) {
            return before.trim_end();
        }
    }
    text
}

fn base_edges(text: &str, qualifiers: &mut Vec<String>) -> String {
    let mut name = text.trim().to_owned();
    loop {
        let previous = name.clone();
        name = strip_hashtags(&name).trim().trim_matches(decoration).trim().to_owned();
        name = strip_version(&name).to_owned();
        // An all-descriptor title is an uncertain visible label. Never retain
        // just one descriptor word by stripping the rest as a suffix.
        if descriptive_fragment(&name) { return name; }
        // Edition suffixes may directly follow a closing Japanese quote.
        for (index, c) in name.char_indices() {
            if index > 0 && c.is_ascii_alphabetic() && name[..index].chars().next_back().is_some_and(|c| separator(c) || matches!(c, '」' | '』' | ')' | ']' | '~' | '～')) {
                if let Some(value) = edition(&name[index..]) {
                    retain_edition(qualifiers, value);
                    name = name[..index].trim_end_matches(separator).to_owned();
                    break;
                }
            }
        }
        if let Some((inside, rest)) = opening_group(&name) {
            if !rest.trim().is_empty() {
                let qualifier = edition(inside);
                if descriptive_fragment(inside) || technical_marker(inside) || qualifier.is_some() {
                    if let Some(value) = qualifier { retain_edition(qualifiers, value); }
                    name = rest.trim_start_matches(separator).to_owned();
                }
            }
        }
        for (index, c) in name.char_indices() {
            if index == 0 || closing(c).is_none() { continue; }
            if let Some((inside, rest)) = opening_group(&name[index..]) {
                if rest.trim().is_empty() {
                    let qualifier = edition(inside);
                    if descriptive_fragment(inside) || technical_marker(inside) || qualifier.is_some() {
                        if let Some(value) = qualifier { retain_edition(qualifiers, value); }
                        name = name[..index].trim_end_matches(separator).to_owned();
                        break;
                    }
                }
            }
        }
        // Consume only complete descriptor fragments at edges. ASCII words
        // require a real boundary, preserving names such as "Modeler".
        let mut prefix = None;
        for (index, c) in name.char_indices().skip(1) {
            let before = &name[..index];
            if before.chars().next_back().is_some_and(|c| closing(c).is_some()) { continue; }
            let boundary = !before.chars().next_back().is_some_and(|c| c.is_ascii_alphanumeric()) || separator(c) || closing(c).is_some();
            if boundary && before.chars().any(char::is_alphanumeric) && descriptive_fragment(before) && !descriptive_fragment(&name[index..]) {
                prefix = Some(index);
            }
        }
        if let Some(index) = prefix { name = name[index..].trim_start_matches(separator).to_owned(); }
        for (index, c) in name.char_indices().skip(1) {
            let before = &name[..index];
            if c.is_alphanumeric() && before.chars().any(char::is_alphanumeric)
                && name[index..].chars().any(char::is_alphanumeric)
                && before.chars().next_back().is_some_and(|c| separator(c) || matches!(c, '」' | '』' | ')' | ']'))
                && descriptive_fragment(&name[index..]) {
                name = before.trim_end_matches(separator).to_owned();
                break;
            }
        }
        name = unquote(&name).trim().to_owned();
        if name == previous { return name; }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn supplied_base_title_examples_keep_names_source_spellings_and_editions() {
        let cases = [
            ("【VRChat想定 オリジナル3Dモデル『ナナセ・ノワール -Nanase Noir-』Gen3.0", "ナナセ・ノワール", Some("Nanase Noir"), "Gen3.0"),
            ("二次創作3Dモデル「一姫」&#x20;", "一姫", None, ""),
            ("アマト【lilToon対応】", "アマト", None, ""),
            ("【無料】Ren♡Ai", "Ren♡Ai", Some("Ren♡Ai"), ""),
            ("【VRChat向けオリジナル3Dモデル】星凪 ゆらぎ", "星凪 ゆらぎ", None, ""),
            ("【Quest対応】寿司ザウルス", "寿司ザウルス", None, ""),
            ("オリジナル 3Dモデル『ひかるん - Hikarun -』&#x20;", "ひかるん", Some("Hikarun"), ""),
            ("🐲 オリジナル 3Dモデル『リュミィ - Ryumii -』&#x20;", "リュミィ", Some("Ryumii"), ""),
            ("銀髪スーツの悪魔 メイネーラ VRChat向けオリジナル3Dアバター #e62f8b", "銀髪スーツの悪魔 メイネーラ", None, ""),
            ("「stier」-シュティア-#シュティア3D", "シュティア", Some("stier"), ""),
            ("『たゆた -Tayuta-』オリジナル3Dモデル", "たゆた", Some("Tayuta"), ""),
            ("[ENG][TORIBASE][GAMEREADY] ERP FOXIE", "ERP FOXIE", Some("ERP FOXIE"), ""),
            ("【無料】GLC~ゴスもどきちゃん~Medium", "GLC~ゴスもどきちゃん~", None, "Medium"),
            ("【無料配布】オリジナル3Dモデル「フェリス」", "フェリス", None, ""),
            ("島風 (Shimakaze) (blend file)&#x20;", "島風", Some("Shimakaze"), ""),
            ("【デルタフレア】オリジナル3Dモデル", "デルタフレア", None, ""),
            ("ホノカ - Honoka #Honoka3D", "ホノカ", Some("Honoka"), ""),
            ("「ネミア -Nenmir-」 #MARUBODY&#x20;", "ネミア", Some("Nenmir"), ""),
            ("ブルーアーカイブ 二次創作3Dアバター 棗 イロハ", "ブルーアーカイブ 二次創作3Dアバター 棗 イロハ", None, ""),
            ("\"つみげ -Tsumige-\"【VRChat向けオリジナルアバター】&#x20;", "つみげ", Some("Tsumige"), ""),
            ("『だいふくおばけ』 VRC想定オリジナル3Dアバター", "だいふくおばけ", None, ""),
        ];
        let mut failures = Vec::new();
        for (title, original, latin, edition) in cases {
            let parsed = structured_base_parts(title);
            let qualifiers = if edition.is_empty() { vec![] } else { vec![edition.to_owned()] };
            if parsed.original != original || parsed.source_latin.as_deref() != latin
                || parsed.name_kind != "single" || parsed.qualifiers != qualifiers {
                failures.push(format!("{title}: {parsed:?}"));
            }
        }
        assert!(failures.is_empty(), "{failures:#?}");
        let bundle = structured_base_parts("【 R18 Edition】苺佳(ichica) and GLC and 蒼葉(aoba)");
        assert_eq!(bundle.original, "苺佳(ichica) and GLC and 蒼葉(aoba)");
        assert_eq!(bundle.source_latin, None);
        assert_eq!(bundle.name_kind, "bundle");
        assert_eq!(bundle.qualifiers, ["R18 Edition"]);
    }

    #[test]
    fn base_presentation_keeps_uncertain_labels_and_shared_tag_cleanup_conservative() {
        for source in ["星猫 / 海猫", "VRChat / 3D Avatar", "Original 3D Avatar v1.0", "ソラネコ 【別モデル】", "ソラネコ (PC Quest)"] {
            let label = structured_base_parts(source);
            assert_eq!(label.name_kind, "fallback", "{source}");
            assert_eq!(label.source_latin, None, "{source}");
            assert!(!label.original.is_empty());
        }
        assert_eq!(structured_base_parts("Original 3D Modeler").original, "Original 3D Modeler");
        assert_eq!(structured_base_parts("Ren♡Ai").original, "Ren♡Ai");
        for name in ["Engel", "Freedom", "ENG-el", "GAMEReady-name", "TORIBASE-name"] {
            let parsed = structured_base_parts(name);
            assert_eq!(parsed.original, name);
            assert_eq!(parsed.source_latin.as_deref(), Some(name));
            assert_eq!(parsed.name_kind, "single");
        }
        assert_eq!(cleaned_title("アマト【lilToon対応】"), "アマト【lilToon対応】");
        assert_eq!(base_parts("「真冬」対応コート"), ("「真冬」対応コート".into(), None));
        assert_eq!(decode_entities("A&amp;B &#32;&#x20; &unknown;"), "A&B    &unknown;");
    }


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
