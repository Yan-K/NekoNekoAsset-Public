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
    let source = plain_title(title);
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
    named_parts(original, source_latin, name, source, qualifiers)
}

/// Accept one validated name, or keep the visible label as an uncertain fallback.
fn named_parts(original: String, mut source_latin: Option<String>, name: String, source: String, qualifiers: Vec<String>) -> super::AvatarLabelParts {
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

fn plain_title(title: &str) -> String {
    decode_entities(title).nfkc().collect::<String>().split_whitespace().collect::<Vec<_>>().join(" ")
}

/// A known Base title describes one product, so its name can be located rather
/// than recognized: sale, status and platform text around it needs no vocabulary
/// entry to be left out. Arbitrary labels keep the conservative parser above,
/// because those can describe a garment or several avatars.
pub(super) fn product_base_parts(title: &str) -> super::AvatarLabelParts {
    let source = plain_title(title);
    let mut qualifiers = Vec::new();
    let Some(name) = product_name(&source, &mut qualifiers) else {
        // Add-on wording, two names or none: the label stays visible but names nothing.
        let mut parsed = structured_base_parts(title);
        if parsed.name_kind == "single" {
            parsed.name_kind = "fallback";
            parsed.source_latin = None;
        }
        return parsed;
    };
    if bundle_name(&name) {
        return super::AvatarLabelParts { original: name, source_latin: None, qualifiers, name_kind: "bundle" };
    }
    let (original, source_latin) = bilingual(&name)
        .or_else(|| name.split_once('/').and_then(|(left, right)| reading_pair(left, right)))
        .unwrap_or_else(|| (name.clone(), None));
    named_parts(original, source_latin, name, source, qualifiers)
}

fn product_name(source: &str, qualifiers: &mut Vec<String>) -> Option<String> {
    let unified = source.replace('〚', "『").replace('〛', "』");
    let text = before_contents(cut_hashtags(&unified));
    let quoted = quoted_names(text);
    let name = match quoted[..] {
        [(inside, tail)] => quoted_name(inside, tail, qualifiers)?,
        [] => unquoted_name(text, qualifiers)?,
        _ => return None,
    };
    (name.chars().any(char::is_alphanumeric) && !addon(&name)).then_some(name)
}

/// Unlike the exact strip_hashtags, later non-tag text is discarded too.
fn cut_hashtags(text: &str) -> &str {
    text.char_indices()
        .find(|(index, c)| {
            *c == '#' && text[..*index].chars().any(char::is_alphanumeric)
                && text[..*index].chars().next_back().is_some_and(|c| c.is_whitespace() || matches!(c, '-' | '」' | '』' | ')' | ']'))
                && text[index + 1..].chars().next().is_some_and(|c| !c.is_whitespace())
        })
        .map_or(text, |(index, _)| &text[..index])
}

/// Status text after a dash that follows the model wording lists what the model
/// includes, so an add-on word there is a bonus: 名前 オリジナル3Dモデル -アバター & 追加衣装.
fn before_contents(text: &str) -> &str {
    text.match_indices(['-', '—', '–', '−'])
        .find(|(index, dash)| {
            let before = text[..*index].trim_end();
            status_note(&text[index + dash.len()..])
                && before.rfind(char::is_whitespace).is_some_and(|space| {
                    before[..space].chars().any(char::is_alphanumeric) && descriptive_fragment(&before[space..])
                })
        })
        .map_or(text, |(index, _)| text[..index].trim_end())
}

/// Complete name quotes with the text after each. A quote outranks everything
/// around it, unless it holds a note beside another quote.
fn quoted_names(text: &str) -> Vec<(&str, &str)> {
    let mut found = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find(['「', '『', '“', '"']) {
        if let Some((inside, after)) = opening_group(&rest[start..]) {
            if inside.chars().any(char::is_alphanumeric) && !descriptive_fragment(inside) {
                found.push((inside, after));
            }
            rest = after;
        } else {
            rest = &rest[start + rest[start..].chars().next().map_or(1, char::len_utf8)..];
        }
    }
    if found.len() > 1 {
        found.retain(|(inside, _)| !status_note(inside));
    }
    found
}

fn quoted_name(inside: &str, tail: &str, qualifiers: &mut Vec<String>) -> Option<String> {
    // A bonus after + leaves the product what it was; other add-on text does not.
    let tail = tail.split(['+', '【', '[', '〖', '《', '〔']).next().unwrap_or("");
    if addon(tail) || for_target(tail) {
        return None;
    }
    let name = clean_name(inside, qualifiers);
    let tail = base_edges(tail.trim_matches(|c| title_decoration(c) || c.is_whitespace()), qualifiers);
    let spelled = tail.trim_matches(separator);
    if let Some(value) = edition(spelled) {
        retain_edition(qualifiers, value);
    } else if (2..=3).contains(&spelled.len()) && spelled.chars().all(|c| c.is_ascii_uppercase()) && !platform_word(spelled) {
        // Too short for a spelling: 「Name」RE, 「Name」DX.
        retain_edition(qualifiers, spelled.to_owned());
    } else if !version_text(&compact_fragment(spelled)) && !status_note(spelled) && (spelling(spelled) || kana_form(spelled).is_some()) {
        return Some(spelled_name(&name, spelled).unwrap_or(name));
    }
    Some(name)
}

fn spelled_name(name: &str, spelled: &str) -> Option<String> {
    let joined = format!("{name} -{spelled}-");
    (bilingual(name).is_none() && bilingual(&joined).is_some()).then_some(joined)
}

fn unquoted_name(text: &str, qualifiers: &mut Vec<String>) -> Option<String> {
    let mut runs = Vec::new();
    let mut groups = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find(['【', '[', '〖', '《', '〔']) {
        runs.push(&rest[..start]);
        if let Some((inside, after)) = opening_group(&rest[start..]) {
            groups.push(inside);
            rest = after;
        } else {
            // An unclosed bracket is only punctuation beside the name.
            rest = &rest[start + rest[start..].chars().next().map_or(1, char::len_utf8)..];
        }
    }
    runs.push(rest);
    if runs.iter().any(|run| addon(run.split('+').next().unwrap_or(""))) {
        return None;
    }
    let names: Vec<_> = runs.iter().filter_map(|run| run_name(run, qualifiers)).collect();
    groups.retain(|group| !note_group(group, qualifiers));
    match (&names[..], &groups[..]) {
        ([], [group]) => Some(clean_name(group, qualifiers)),
        ([name], groups) => Some(groups.iter()
            // Latin notes such as 【PhysBones】 are capitalized; a spelling beside
            // a Japanese name needs lowercase evidence to outrank that reading.
            .filter(|group| japanese_only(group) || group.chars().all(|c| !c.is_ascii_uppercase()))
            .find_map(|group| spelled_name(name, group))
            .unwrap_or_else(|| name.clone())),
        ([left, right], _) => spelled_name(left, right),
        _ => None,
    }
}

/// The first part of free text between groups that is more than a note.
fn run_name(run: &str, qualifiers: &mut Vec<String>) -> Option<String> {
    run.split(['◆', '◇', '■', '□', '●', '○', '▲', '△', '▼', '▽', '◎', '※']).find_map(|part| {
        let part = part.trim_matches(|c| title_decoration(c) || c.is_whitespace());
        if part.starts_with('+') || part.is_empty() {
            return None;
        }
        let name = clean_name(part, qualifiers);
        (name.chars().any(char::is_alphanumeric) && !descriptive_fragment(&name) && !version_text(&compact_fragment(&name))).then_some(name)
    })
}

fn clean_name(text: &str, qualifiers: &mut Vec<String>) -> String {
    let dash = |c: char| matches!(c, '-' | '—' | '–' | '−');
    let trimmed = text.trim_matches(|c| title_decoration(c) || separator(c) && !dash(c));
    let name = base_edges(trimmed, qualifiers);
    if bundle_name(&name) {
        return name;
    }
    // Word notes below must not leave part of a complete descriptor behind.
    if descriptive_fragment(&name) {
        return String::new();
    }
    let words: Vec<_> = strip_version(cut_notes(&name)).split_whitespace().collect();
    let note = |word: &&str| !word.chars().any(char::is_alphanumeric) || status_note(word)
        || platform_word(word.trim_matches(|c: char| !c.is_alphanumeric()));
    let Some(end) = words.iter().rposition(|word| !note(&word)) else {
        return String::new();
    };
    let start = words.iter().position(|word| !note(&word)).unwrap_or(end);
    let name = words[start..=end].join(" ");
    // Keep -Pure- whole, but drop a dash left over from a removed neighbour.
    match (name.starts_with(dash), name.ends_with(dash)) {
        (true, true) if !name.trim_matches(dash).contains(dash) => name.trim_matches(dash).trim().to_owned(),
        (true, false) => name.trim_start_matches(dash).trim().to_owned(),
        (false, true) if !name.trim_end_matches(dash).contains(dash) => name.trim_end_matches(dash).trim().to_owned(),
        _ => name,
    }
}

/// Text after a name that never belongs to it. A spelling pair is left whole.
fn cut_notes(mut name: &str) -> &str {
    let named = |text: &str| text.chars().any(char::is_alphanumeric);
    loop {
        let previous = name;
        // + introduces a bonus, unless it joins digits as in 3+1.
        if let Some(index) = name.find('+').filter(|index| {
            named(&name[..*index]) && named(&name[index + 1..]) && !name[..*index].ends_with(|c: char| c.is_ascii_digit())
        }) {
            name = name[..index].trim_end();
        }
        // A stock code such as SHA-25-0201.
        let before = name.trim_end_matches(|c: char| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '-');
        let code = &name[before.len()..];
        if named(before) && code.matches('-').count() >= 2 && code.chars().any(|c| c.is_ascii_uppercase()) && code.chars().any(|c| c.is_ascii_digit()) {
            name = before.trim_end();
        }
        if name.ends_with(')') && bilingual(name).is_none() {
            if let Some((index, _)) = name.char_indices().find(|(index, c)| {
                *c == '(' && named(&name[..*index]) && opening_group(&name[*index..]).is_some_and(|(_, rest)| rest.is_empty())
            }) {
                name = name[..index].trim_end();
            }
        }
        // A trailing -V2.0 or -フルセット版-.
        let body = name.trim_end_matches(['-', '—', '–', '−']);
        if let Some(index) = body.rfind(['-', '—', '–', '−']) {
            let note = body[index..].trim_start_matches(['-', '—', '–', '−']);
            if named(&body[..index]) && (version_text(&compact_fragment(note)) || status_note(note)) {
                name = body[..index].trim_end();
            }
        }
        if name == previous {
            return name;
        }
    }
}

/// One Japanese name and its Latin spelling, in either order and across any
/// separator or bracket. Both sides must be consumed completely.
fn bilingual(name: &str) -> Option<(String, Option<String>)> {
    let divider = |c: char| separator(c) || c == '・' || closing(c).is_some()
        || matches!(c, '」' | '』' | '】' | '〗' | '〉' | '》' | '〕' | ']' | ')' | '”' | '’');
    name.char_indices().filter(|(_, c)| divider(*c)).find_map(|(index, c)| {
        let left = name[..index].trim_matches(divider);
        let right = name[index + c.len_utf8()..].trim_matches(divider);
        [(left, right), (right, left)].into_iter()
            .find(|(japanese, latin)| japanese_only(japanese) && (spelling(latin) || short_spelling(latin, name)))
            .map(|(japanese, latin)| (japanese.to_owned(), Some(latin.to_owned())))
    })
}

fn japanese_only(text: &str) -> bool {
    japanese_name(text) && !text.chars().any(|c| c.is_ascii_alphabetic())
}

/// One or two letters are usually an edition (ソラネコ RE), and an edition word
/// means the Latin text is a variant title rather than a spelling.
fn spelling(text: &str) -> bool {
    latin_name(text)
        && text.chars().filter(|c| c.is_alphabetic()).count() >= 3
        && !text.split(|c: char| !c.is_alphanumeric()).any(|word| word.eq_ignore_ascii_case("edition"))
}

/// Two letters read as a spelling only in mixed case and closing the title, as
/// ソラ -So- does: Re:ソラネコ and ソラネコ RE are titles.
fn short_spelling(text: &str, name: &str) -> bool {
    name.ends_with(['-', '—', '–', '−', ')'])
        && latin_name(text)
        && text.chars().filter(|c| c.is_alphabetic()).count() == 2
        && text.chars().any(char::is_lowercase)
}

fn bundle_name(name: &str) -> bool {
    name.to_ascii_lowercase().contains(" and ") || name.contains(" & ") || name.contains('、')
}

/// Decoration around a name; ♡ or ↑ inside one stays.
fn title_decoration(c: char) -> bool {
    decoration(c) || matches!(c as u32, 0x2190..=0x2bff | 0xfe00..=0xfe0f | 0x200d)
}

/// Sale, status, license and model-type prose. Seeing it never loses a name: a
/// title with nothing else falls back to the conservative parser.
fn status_note(text: &str) -> bool {
    ["セール", "無料", "販売", "公開", "サポート", "発売", "記念", "割引", "限定", "予定", "まで", "対応", "版", "ライセンス", "モデル", "アバター", "素体", "付属"]
        .iter().any(|word| text.contains(word))
        || ["SALE", "FREE", "R18", "R-18"].iter().any(|word| super::word_match(text, word))
}

fn platform_word(word: &str) -> bool {
    ["avatar", "model", "vrchat", "vrc", "quest", "pc", "vrm", "mmd", "fbx", "unity", "blender", "android", "ios", "cluster", "liltoon", "physbones"]
        .iter().any(|known| word.eq_ignore_ascii_case(known))
}

/// A bracket group around a name, rather than the name itself.
fn note_group(group: &str, qualifiers: &mut Vec<String>) -> bool {
    if let Some(value) = edition(group) {
        retain_edition(qualifiers, value);
        return true;
    }
    let compact = compact_fragment(group);
    if compact.len() > "version".len() && compact.ends_with("version") && compact.is_ascii() {
        retain_edition(qualifiers, group.trim().to_owned());
        return true;
    }
    descriptive_fragment(group) || technical_marker(group) || version_text(&compact) || status_note(group)
        || group.split(|c: char| !c.is_alphanumeric()).filter(|word| !word.is_empty()).all(platform_word)
}

/// 「ソラネコ」用ボディ: text directly after a name says who the product is for.
pub(super) fn for_target(text: &str) -> bool {
    ["用", "向け", "対応", "専用"].iter().any(|word| text.trim_start().starts_with(word))
}

/// 無料版 or DX版 beside the name marks another listing of the same avatar.
pub(super) fn variant_listing(title: &str, name: &str) -> bool {
    let rest = plain_title(title).replacen(name, "", 1);
    rest.contains('版') || rest.contains("無料")
}

/// Text that makes a product an add-on for another avatar, not a bonus with one.
pub(super) fn addon(text: &str) -> bool {
    let lower = text.to_lowercase();
    !lower.contains("付き")
        && (["専用", "衣装", "テクスチャ", "キット", "拡張", "コート", "アクセサリー"].iter().any(|word| lower.contains(word))
            || ["dlc", "coat", "jacket", "dress", "outfit", "clothes", "clothing", "costume", "texture", "hair", "accessory"].iter().any(|word| super::word_match(&lower, word)))
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
            return consumed && version_text(rest);
        };
        consumed = true;
        rest = &rest[word.len()..];
    }
    true
}

/// Input is a compact fragment.
fn version_text(compact: &str) -> bool {
    let version = ["version", "ver.", "ver", "v"].iter().find_map(|marker| compact.strip_prefix(*marker));
    version.is_some_and(|value| {
        value.chars().next().is_some_and(|c| c.is_ascii_digit())
            && value.chars().all(|c| c.is_ascii_digit() || matches!(c, '.' | '_'))
    })
}

fn edition(text: &str) -> Option<String> {
    let compact = compact_fragment(text);
    if compact == "r18edition" { return Some("R18 Edition".into()); }
    if compact == "medium" { return Some("Medium".into()); }
    match compact.strip_suffix("polygonedition") {
        Some("low") => return Some("Low".into()),
        Some("medium") => return Some("Medium".into()),
        Some("high") => return Some("High".into()),
        _ => {}
    }
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
    fn product_titles_locate_their_name_through_unlisted_marketing_text() {
        let cases: &[(&str, &str, Option<&str>, &[&str])] = &[
            // Status and sale wording needs no vocabulary entry.
            ("【発売記念SALE✧】ソラネコ -Soraneko- / オリジナル3Dモデル", "ソラネコ", Some("Soraneko"), &[]),
            ("【オリジナル3Dモデル】soraneko-ソラネコ 【販売終了】", "ソラネコ", Some("soraneko"), &[]),
            ("【オリジナル３Ｄモデル】ソラネコ・ノワール【個人商用ライセンス版】", "ソラネコ・ノワール", None, &[]),
            ("【サポート終了】【version2.5】ソラネコ【オリジナル3Dモデル】", "ソラネコ", None, &[]),
            ("【オリジナル3Dモデル】ソラネコ (2030年1月31日まで販売)", "ソラネコ", None, &[]),
            ("🎊セール中！ 12/24まで🎊【VRChat向けオリジナル3Dモデル】星空 ソラネコ", "星空 ソラネコ", None, &[]),
            ("★R18★ オリジナル3Ｄモデル 【ソラネコ】", "ソラネコ", None, &[]),
            ("[DX版]ソラネコ (VRChat向け3Dモデル)", "ソラネコ", None, &[]),
            // A quote outranks all text around it, malformed or not.
            ("SALE中✨オリジナル3Dモデル『ソラネコ』☕3000→1500", "ソラネコ", None, &[]),
            ("「ソラ&ネコ」キャラクター素体 v3.2 【VRChat Blender】", "ソラ&ネコ", None, &[]),
            ("オリジナル3Dモデル『ソラネコ』Avatar3.0向け", "ソラネコ", None, &[]),
            ("VRChat向けオリジナル3Dモデル「Soraneko」Mobile対応", "Soraneko", Some("Soraneko"), &[]),
            ("VRC向け3Dモデル「Soraneko - ソラネコ -」 +VRM付き PB対応", "ソラネコ", Some("Soraneko"), &[]),
            ("Quest・PC対応『ソラネコ -Soraneko-』オリジナル3Dモデル", "ソラネコ", Some("Soraneko"), &[]),
            ("【無料版あり】オリジナル3Dモデル「ソラネコ」【VRC / VRM / MMD】", "ソラネコ", None, &[]),
            ("オリジナル3Dモデル『ソラネコ -Soraneko- 』 #ソラネコ3D #Soraneko3D / AC9", "ソラネコ", Some("Soraneko"), &[]),
            ("オリジナル3Dモデル『ソラネコ - Soraneko』💎✨️", "ソラネコ", Some("Soraneko"), &[]),
            ("【オリジナル3Dモデル】星空の主『ソラネコ』", "ソラネコ", None, &[]),
            ("ブランドオリジナルアバター “K”", "K", Some("K"), &[]),
            ("オリジナル3Dモデル〚ソラネコ〛", "ソラネコ", None, &[]),
            // A spelling can sit on either side, across any separator or bracket.
            ("【軽量版無料】オリジナル3Dモデル｢ソラネコ｣ -Soraneko-", "ソラネコ", Some("Soraneko"), &[]),
            ("【3Dモデル】『ソラネコ ver.1.3』Soraneko", "ソラネコ", Some("Soraneko"), &[]),
            ("オリジナル３Dモデル 【ソラネコ】 - Soraneko", "ソラネコ", Some("Soraneko"), &[]),
            ("オリジナル3Dモデル 『Soraneko -ソラネコ-』", "ソラネコ", Some("Soraneko"), &[]),
            ("【オリジナル3Dモデル】Soraneko - ソラネコ", "ソラネコ", Some("Soraneko"), &[]),
            ("-ソラネコ- Soraneko【オリジナル3Dモデル】", "ソラネコ", Some("Soraneko"), &[]),
            ("【オリジナル3Dモデル】Soraneko -ソラネコ-【AuthoredBody】", "ソラネコ", Some("Soraneko"), &[]),
            ("オリジナル3Dモデル【ソラネコ:Soraneko】", "ソラネコ", Some("Soraneko"), &[]),
            ("オリジナル3Dモデル「ソラネコ・Soraneko」", "ソラネコ", Some("Soraneko"), &[]),
            ("オリジナル3Dモデル『ソラ -So-』", "ソラ", Some("So"), &[]),
            ("【オリジナル3Dモデル】Re:ソラネコ", "Re:ソラネコ", None, &[]),
            ("【オリジナル3Dモデル】ソラネコ[soraneko]", "ソラネコ", Some("soraneko"), &[]),
            ("【VRCアバター】 Soraneko (ソラネコ)", "ソラネコ", Some("Soraneko"), &[]),
            ("VRChat想定アバター ソラネコ(Soraneko)", "ソラネコ", Some("Soraneko"), &[]),
            ("【-999円】「Soraneko（空猫）」VRChat/VRM", "空猫", Some("Soraneko"), &[]),
            // Text after a name stays out of it.
            ("【オリジナル3Dモデル】 ソラネコ （SORANEKO）◆シリーズ共通素体", "ソラネコ", Some("SORANEKO"), &[]),
            ("◆オリジナル3Dモデル◆ソラネコ ver1.0 (通常版)", "ソラネコ", None, &[]),
            ("オリジナル3Dモデル／ソラネコ (Soraneko) AAA-99-0001", "ソラネコ", Some("Soraneko"), &[]),
            ("【3Dモデル】Soraneko -ソラネコ- + AUTHORED SCARF -スカーフ-【PhysBones対応】", "ソラネコ", Some("Soraneko"), &[]),
            ("ソラネコ+ソラの詰め合わせパック【オリジナル3Dモデル】", "ソラネコ", None, &[]),
            ("VRC向け3Dモデル【ソラネコ -Soraneko- 】V2", "ソラネコ", Some("Soraneko"), &[]),
            ("２＋２（に↑ぷらす）【オリジナル3Dモデル】", "2+2", None, &[]),
            ("ソラネコ オリジナル3Dモデル -VRChatアバター & VRM & 追加衣装", "ソラネコ", None, &[]),
            ("「ソラネコ」 オリジナル3Dモデル -VRChatアバター & VRM & 追加衣装", "ソラネコ", None, &[]),
            // Editions are kept beside the name, never as a spelling.
            ("オリジナル3Dモデル「ソラネコ」RE", "ソラネコ", None, &["RE"]),
            ("【無料】AAA～ソラネコ～Low Polygon edition【オリジナル3Dモデル】", "AAA~ソラネコ~", None, &["Low"]),
            ("【無料】AAA～ソラネコ～Medium Polygon edition【オリジナル3Dモデル】", "AAA~ソラネコ~", None, &["Medium"]),
            ("【無料】Sora♡Neko / オリジナル3Dモデル【AuthoredVersion】", "Sora♡Neko", Some("Sora♡Neko"), &["AuthoredVersion"]),
            ("【VRChat想定 オリジナル3Dモデル『ソラネコ -Soraneko-』Gen3.0", "ソラネコ", Some("Soraneko"), &["Gen3.0"]),
            // Latin variants and series names stay complete and distinct.
            ("【3Dモデル】Soraneko -Winter-【PhysBones対応】", "Soraneko -Winter-", Some("Soraneko -Winter-"), &[]),
            ("【無料】ソラネコ Builder Edition 2.0【ブロックゲーム、vrchat、vrm対応】", "ソラネコ Builder Edition 2.0", None, &[]),
            ("【オリジナル3Dモデル】ウミネコ-UMINEKO-", "ウミネコ", Some("UMINEKO"), &[]),
            ("【オリジナル3Dモデル】ソラネコ_NEW", "ソラネコ_NEW", None, &[]),
            ("Authored Modeler", "Authored Modeler", Some("Authored Modeler"), &[]),
            ("Umineko【セール中】", "Umineko", Some("Umineko"), &[]),
            // Nothing marks where an epithet ends, so it stays beside the name.
            ("星空の悪魔 ソラネコ VRChat向けオリジナル3Dアバター #abc123", "星空の悪魔 ソラネコ", None, &[]),
        ];
        let mut failures = Vec::new();
        for (title, original, latin, qualifiers) in cases {
            let parsed = product_base_parts(title);
            if parsed.original != *original || parsed.source_latin.as_deref() != *latin
                || parsed.name_kind != "single" || parsed.qualifiers != *qualifiers {
                failures.push(format!("{title}: {parsed:?}"));
            }
        }
        assert!(failures.is_empty(), "{failures:#?}");
    }

    #[test]
    fn product_titles_without_one_clear_avatar_name_stay_uncertain() {
        // An add-on keeps its visible label and creates no automatic name.
        for title in [
            "ソラネコ専用【センシティブボディ】",
            "【AUTHORED】ソラネコ&ウミネコ専用テクスチャ",
            "「ソラネコ」キャラクター素体 R18改変支援キット",
            "２＋２（に↑ぷらす）SE拡張【DLC】",
            "「ソラネコ」対応コート",
            "「ソラネコ」 Jacket",
            "【オリジナル3Dモデル】ソラネコ 素体用追加テクスチャ",
            "ソラネコ オリジナル3Dモデル -ウミネコ専用衣装",
            "ソラネコ -VRChatアバター用衣装",
            "「ソラネコ」用R18ボディ v1.1",
            "オリジナル3Dモデル「ソラネコ」用ワンピース",
            "「ソラネコ」向けアバターギミック",
        ] {
            let parsed = product_base_parts(title);
            assert_ne!(parsed.original, "ソラネコ", "{title}");
            assert_eq!(parsed.source_latin, None, "{title}");
            assert_eq!(parsed.name_kind, "fallback", "{title}");
        }
        for title in ["オリジナル3Dモデル", "VRChat / 3D Avatar", "Original 3D Avatar v1.0", "星猫 / 海猫", "「星猫」「海猫」", "【星猫】【海猫】オリジナル3Dモデル"] {
            let parsed = product_base_parts(title);
            assert_eq!(parsed.name_kind, "fallback", "{title}");
            assert_eq!(parsed.source_latin, None, "{title}");
        }
        for (title, original) in [
            ("【3Dモデル】Soraneko -ソラネコ- & Umineko -ウミネコ-【PhysBones対応】", "Soraneko -ソラネコ- & Umineko -ウミネコ-"),
            ("【 R18 Edition】ソラ(sora) and AAA and ウミ(umi)【オリジナル3Dモデル】", "ソラ(sora) and AAA and ウミ(umi)"),
            ("オリジナル3Dモデル「ソラネコ、ウミネコ」", "ソラネコ、ウミネコ"),
        ] {
            let parsed = product_base_parts(title);
            assert_eq!(parsed.name_kind, "bundle", "{title}");
            assert_eq!(parsed.original, original, "{title}");
        }
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
