//! Original fixed taxonomy and curated public avatar identities.
//! Matches are evidence-bearing candidates, never acquisition or entitlement facts.
mod compatibility;
pub use compatibility::{
    account_variant_avatars, is_all_inclusive_variant, listing_avatar_evidence, variant_avatars,
};
mod names;
mod phonetic;
mod resolver;
pub use resolver::AvatarTagResolver;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::OnceLock;
use unicode_normalization::UnicodeNormalization;

pub const VERSION: &str = "2026-10-12.12";
pub const CATEGORY_KEYS: &[&str] = &[
    "Base",
    "Clothes",
    "Hair",
    "Accessory",
    "Texture",
    "Motion",
    "Tool",
    "Shader",
    "Gimmick",
    "World",
    "Udon",
    "NonVRChat",
];

/// Search normalization retains original-script identity; no transliteration or
/// simplified/traditional kanji guessing is performed.
pub fn normalize(text: &str) -> String {
    text.nfkc()
        .collect::<String>()
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

struct Rule {
    category: &'static str,
    specific: bool,
    words: &'static [&'static str],
    fragments: &'static [&'static str],
}

// Priority and weights preserve the accepted prototype: specific component
// evidence beats a broad avatar/world parent; equal scores use this stable order.
const RULES: &[Rule] = &[
    Rule {
        category: "Udon",
        specific: true,
        words: &["udon", "udonsharp"],
        fragments: &["udonシステム", "udon系统", "udon系統"],
    },
    Rule {
        category: "Shader",
        specific: true,
        words: &["shader", "shaders"],
        fragments: &["シェーダ", "셰이더", "着色器", "著色器"],
    },
    Rule {
        category: "Hair",
        specific: true,
        words: &["hair", "hairstyle", "hairstyles"],
        fragments: &["髪型", "ヘア", "헤어", "发型", "髮型"],
    },
    Rule {
        category: "Clothes",
        specific: true,
        words: &[
            "cloth", "clothes", "clothing", "outfit", "outfits", "costume", "costumes", "coat",
            "coats", "dress", "dresses",
        ],
        fragments: &["衣装", "衣服", "服装", "服裝", "의상"],
    },
    Rule {
        category: "Accessory",
        specific: true,
        words: &[
            "accessory",
            "accessories",
            "prop",
            "props",
            "brooch",
            "brooches",
            "jewelry",
            "jewellery",
        ],
        fragments: &[
            "アクセサリー",
            "小物",
            "小道具",
            "装飾品",
            "配饰",
            "配飾",
            "액세서리",
        ],
    },
    Rule {
        category: "Texture",
        specific: true,
        words: &["texture", "textures", "material", "materials"],
        fragments: &[
            "テクスチャ",
            "マテリアル",
            "贴图",
            "貼圖",
            "材质",
            "材質",
            "텍스처",
            "머티리얼",
        ],
    },
    Rule {
        category: "Motion",
        specific: true,
        words: &[
            "motion",
            "motions",
            "animation",
            "animations",
            "gesture",
            "gestures",
            "pose",
            "poses",
        ],
        fragments: &[
            "モーション",
            "アニメーション",
            "动作",
            "動作",
            "모션",
            "애니메이션",
        ],
    },
    Rule {
        category: "Tool",
        specific: true,
        words: &["tool", "tools", "utility", "utilities", "editor extension"],
        fragments: &["ツール", "工具", "도구"],
    },
    Rule {
        category: "Gimmick",
        specific: true,
        words: &["gimmick", "gimmicks", "interaction", "interactions"],
        fragments: &["ギミック", "交互组件", "互動元件", "기믹"],
    },
    Rule {
        category: "World",
        specific: false,
        words: &["world", "worlds", "environment", "environments"],
        fragments: &["ワールド", "世界", "월드"],
    },
    Rule {
        category: "Base",
        specific: false,
        words: &[
            "avatar",
            "avatars",
            "base avatar",
            "3d character",
            "3d characters",
        ],
        fragments: &["アバター", "3dキャラクター", "素体", "素體", "아바타"],
    },
    Rule {
        category: "NonVRChat",
        specific: true,
        words: &[
            "nonvrchat",
            "non-vrchat",
            "non vrchat",
            "brush",
            "brushes",
            "print",
            "printing",
            "illustration",
            "illustrations",
        ],
        fragments: &["vrchat以外", "イラスト", "笔刷", "筆刷"],
    },
];

fn word_match(text: &str, word: &str) -> bool {
    text.match_indices(word).any(|(start, _)| {
        let end = start + word.len();
        !text[..start]
            .chars()
            .next_back()
            .is_some_and(|c| c.is_ascii_alphanumeric())
            && !text[end..]
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_alphanumeric())
    })
}

fn rule_match(text: &str, rule: &Rule) -> bool {
    rule.words.iter().any(|word| word_match(text, word))
        || rule.fragments.iter().any(|word| text.contains(word))
}

fn evidence_excerpt(text: &str) -> String {
    text.chars().take(240).collect()
}

/// Accepts either a source-category string or {name,parentName,...}. Unknown
/// fields remain in the caller's source record; raw input is never modified.
pub fn infer_category(source_category: &Value, tags: &[String]) -> Value {
    let leaf = source_category
        .as_str()
        .or_else(|| source_category["name"].as_str())
        .unwrap_or("");
    let parent = source_category["parentName"].as_str().unwrap_or("");
    let normalized_leaf = normalize(leaf);
    let normalized_parent = normalize(parent);
    let normalized_tags: Vec<_> = tags.iter().map(|tag| normalize(tag)).collect();
    let mut winner =
        json!({"category":"NonVRChat", "basis":"fallback", "inferred":true, "evidence":[]});
    let mut best = 0;
    for rule in RULES {
        let leaf_match = rule_match(&normalized_leaf, rule);
        let parent_match = rule_match(&normalized_parent, rule);
        let matching_tags: Vec<_> = normalized_tags
            .iter()
            .enumerate()
            .filter(|(_, tag)| rule_match(tag, rule))
            .map(|(i, _)| i)
            .collect();
        let tag_match = !matching_tags.is_empty();
        let strength = if leaf_match {
            6
        } else if tag_match {
            4
        } else if parent_match {
            2
        } else {
            0
        };
        let score = strength + if strength > 0 && rule.specific { 5 } else { 0 };
        if score > best {
            let mut evidence = Vec::new();
            if leaf_match {
                evidence.push(json!({"field":"sourceCategory.name", "value":leaf}));
            }
            if parent_match {
                evidence.push(json!({"field":"sourceCategory.parentName", "value":parent}));
            }
            evidence.extend(
                matching_tags
                    .iter()
                    .map(|i| json!({"field":"sourceTags", "value":tags[*i]})),
            );
            winner = json!({"category":rule.category,
                "basis":if leaf_match || parent_match && !tag_match { "sourceCategory" } else { "tags" },
                "inferred":true, "evidence":evidence});
            best = score;
        }
    }
    winner
}

/// Production classification uses retained public metadata, never availability,
/// filenames or private payloads. Manual category overlays are applied later.
pub fn infer_product_category(source: &Value) -> Value {
    let text = |key: &str| source[key].as_str().unwrap_or("");
    let title = ["sourceTitle", "title", "name"]
        .iter()
        .map(|k| text(k))
        .find(|v| !v.trim().is_empty())
        .unwrap_or("");
    let description = ["sourceDescription", "description"]
        .iter()
        .map(|k| text(k))
        .find(|v| !v.trim().is_empty())
        .unwrap_or("");
    let tags: Vec<String> = ["sourceTags", "tags"]
        .iter()
        .flat_map(|key| {
            source[*key]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
        })
        .map(str::to_owned)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let normalized_tags: Vec<_> = tags.iter().map(|v| normalize(v)).collect();
    let title_normalized = normalize(title);
    let description_normalized = normalize(description);
    let jinxxy = ["provider", "store"]
        .iter()
        .any(|key| normalize(text(key)) == "jinxxy");
    let has_vr = |v: &str| {
        word_match(v, "vrchat")
            || word_match(v, "vrc")
            || word_match(v, "vrcfury")
            || v.contains("vrチャット")
    };
    let has_unity = |v: &str| {
        word_match(v, "unity")
            || word_match(v, "unity3d")
            || word_match(v, "udon")
            || word_match(v, "udonsharp")
            || v.contains("unitypackage")
    };
    let explicit_vr_tag = normalized_tags.iter().any(|v| has_vr(v));
    let vr_scope =
        jinxxy || explicit_vr_tag || has_vr(&title_normalized) || has_vr(&description_normalized);
    // A passing mention of Unity in a long description is not sufficient. A
    // retained tag/title or actual engine/package vocabulary provides context.
    let unity_scope = normalized_tags.iter().any(|v| has_unity(v))
        || has_unity(&title_normalized)
        || has_unity(&description_normalized)
            && [
                "unitypackage",
                "ugui",
                "humanoid",
                "shader",
                "シェーダ",
                "インポート",
                "import",
                ".anim",
                "prefab",
                "プレハブ",
                "editor extension",
                "エディタ拡張",
            ]
            .iter()
            .any(|term| description_normalized.contains(term));
    let outside_app_tag = normalized_tags.iter().any(|tag| {
        [
            "aftereffects",
            "after effects",
            "aftereffcts",
            "ae",
            "novelai",
            "photoshop",
            "clip studio",
            "clipstudiopaint",
            "live2d",
            "ココフォリア",
            "trpg",
            "coc",
            "voice",
            "ボイス",
        ]
        .iter()
        .any(|term| word_match(tag, term))
    });
    // Product-opening context is useful even when sellers omit application
    // tags. Do not scan long credits/dependency sections or treat OS names as
    // strong application evidence.
    let description_opening: String = description_normalized.chars().take(240).collect();
    let outside_app_context = outside_app_tag
        || strong_external_application(&title_normalized)
        || strong_external_application(&description_opening);
    let os_tag = normalized_tags
        .iter()
        .any(|tag| matches!(tag.as_str(), "windows" | "macos"));
    let source_category = &source["sourceCategory"];
    let jinxxy_addon = jinxxy
        && source_category["types"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|kind| {
                kind.as_str()
                    .is_some_and(|kind| normalize(kind) == "avatar_addon")
            });
    let category_name = normalize(
        source_category
            .as_str()
            .or_else(|| source_category["name"].as_str())
            .unwrap_or(""),
    );
    let outside_category = [
        "イラスト素材",
        "ボイス",
        "フォント",
        "書体",
        "illustration",
        "voice",
        "font",
    ]
    .iter()
    .any(|term| category_name.contains(term));
    let mut result = infer_category(source_category, &tags);
    // Software describes the product; texture/avatar words often describe its
    // input or output. Only declared software type may override those words.
    let software_category = software_type(&category_name);
    let software_title = software_type(&title_normalized);
    let software_tag = normalized_tags.iter().any(|tag| software_type(tag));
    let software_opening = description_defines_software(&description_opening);
    let software = software_category || software_title || software_tag || software_opening;
    let declared_software = software_category || software_title || software_opening;
    let specialized_software = RULES.iter().any(|rule| {
        matches!(rule.category, "Shader" | "Udon")
            && result["category"] == rule.category
            && rule_match(&title_normalized, rule)
            && !software_title
            && !software_opening
            && !RULES
                .iter()
                .find(|r| r.category == "Tool")
                .is_some_and(|r| rule_match(&title_normalized, r))
    });
    if software
        && !specialized_software
        && (declared_software
            || matches!(
                result["category"].as_str(),
                Some("Base" | "Texture" | "NonVRChat" | "Tool")
            ))
    {
        let (field, value) = if software_category {
            (
                "sourceCategory.name",
                source_category
                    .as_str()
                    .or_else(|| source_category["name"].as_str())
                    .unwrap_or(""),
            )
        } else if software_title {
            ("sourceTitle", title)
        } else if software_tag {
            (
                "sourceTags",
                tags.iter()
                    .find(|tag| software_type(&normalize(tag)))
                    .map(String::as_str)
                    .unwrap_or(""),
            )
        } else {
            ("sourceDescription", description)
        };
        result = json!({"category":"Tool","basis":field,"inferred":true,"evidence":[{"field":field,"value":evidence_excerpt(value)}]});
    }
    // Broad avatar tags/parent categories are compatibility context, not proof
    // that this product supplies an avatar. Description mentions never suffice.
    let base_rule = RULES.iter().find(|rule| rule.category == "Base").unwrap();
    let base_leaf = rule_match(&category_name, base_rule)
        && !compatibility_context(&category_name)
        && !jinxxy_addon;
    let named_original_model = (title_normalized.contains("オリジナル3dモデル")
        || title_normalized.contains("original 3d model"))
        && normalized_tags.iter().any(|tag| rule_match(tag, base_rule));
    let base_title = (affirmative_base_title(&title_normalized) || named_original_model)
        && !compatibility_context(&title_normalized)
        && names::has_name(title);
    if result["category"] == "Base" && !(base_leaf || base_title) {
        result = json!({"category":"NonVRChat","basis":"fallback","inferred":true,"evidence":[]});
    }
    // A generic material/texture label must not defeat an explicit shader or
    // motion tag; dedicated clothing/hair/etc source categories still win.
    if matches!(result["category"].as_str(), Some("Texture" | "NonVRChat")) {
        if let Some(rule) = RULES
            .iter()
            .filter(|r| matches!(r.category, "Shader" | "Motion"))
            .find(|r| normalized_tags.iter().any(|tag| rule_match(tag, r)))
        {
            result = json!({"category":rule.category,"basis":"tags","inferred":true,"evidence":tags.iter().zip(&normalized_tags).filter(|(_, tag)| rule_match(tag, rule)).take(8).map(|(raw, _)| json!({"field":"sourceTags","value":evidence_excerpt(raw)})).collect::<Vec<_>>()});
        }
    }
    // Descriptions can name dependencies of clothing, so they must not replace
    // an existing specific category. They only supply missing type evidence.
    if result["basis"] == "fallback" {
        for (field, content) in [
            ("sourceTitle", title_normalized.as_str()),
            ("sourceDescription", description_normalized.as_str()),
        ] {
            if let Some(rule) = RULES.iter().find(|rule| {
                rule.category != "NonVRChat"
                    && (rule.category != "Base" || field == "sourceTitle" && base_title)
                    && rule_match(content, rule)
                    && (field == "sourceTitle" || vr_scope || unity_scope)
                    && (field != "sourceDescription" || rule.category != "Motion")
                    && (field != "sourceDescription"
                        || rule.category != "Shader"
                        || description_defines_shader(content))
            }) {
                result = json!({"category":rule.category,"basis":field,"inferred":true,"evidence":[{"field":field,"value":evidence_excerpt(if field=="sourceTitle"{title}else{description})}]});
                break;
            }
        }
    }
    let restricted = matches!(result["category"].as_str(), Some("Tool" | "Texture"));
    let os_without_target_context = os_tag && !unity_scope && !explicit_vr_tag;
    if restricted
        && !jinxxy
        && (!(vr_scope || unity_scope)
            || outside_app_context && !explicit_vr_tag
            || os_without_target_context)
    {
        result = json!({"category":"NonVRChat","basis":"scopeGate","inferred":true,"evidence":result["evidence"]});
    }
    // The product policy treats Jinxxy as a VRChat source. Keep any specific
    // observed type; a generic asset falls under Gimmick, never the excluded shelf.
    // VRCFury alone is a scope hint, not proof that clothing is a gimmick.
    if jinxxy && result["category"] == "NonVRChat" {
        result = if jinxxy_addon {
            json!({"category":"Gimmick","basis":"sourceType","inferred":true,
                "evidence":[{"field":"sourceCategory.types","value":"avatar_addon"}]})
        } else {
            json!({"category":"Gimmick","basis":"providerFallback","inferred":true,
                "evidence":[{"field":"provider","value":"jinxxy"}]})
        };
    }
    result["scope"] = json!(if jinxxy {
        "vrchat"
    } else if (outside_app_context || outside_category) && !explicit_vr_tag
        || os_without_target_context
    {
        "other"
    } else if vr_scope {
        "vrchat"
    } else if unity_scope {
        "unity"
    } else if os_tag {
        "other"
    } else {
        "unknown"
    });
    result["scopeEvidence"] = json!({"vrchat":vr_scope,"unity":unity_scope,"explicitVrchatTag":explicit_vr_tag,"externalApplication":outside_app_context,"providerVrchat":jinxxy});
    if let Some(evidence) = result["evidence"].as_array_mut() {
        evidence.truncate(8);
        for item in evidence {
            if let Some(value) = item["value"].as_str() {
                item["value"] = json!(evidence_excerpt(value));
            }
        }
    }
    result
}

// Keep runtime/platform scope separate from product type. Blender alone does
// not imply either software or VRChat; an avatar may include a .blend source.
fn software_type(text: &str) -> bool {
    [
        "software",
        "add-on",
        "addon",
        "add on",
        "plugin",
        "plug-in",
        "editor extension",
    ]
    .iter()
    .any(|word| word_match(text, word))
        || [
            "ソフトウェア",
            "アドオン",
            "プラグイン",
            "エディタ拡張",
            "エディター拡張",
            "软件",
            "軟體",
            "编辑器扩展",
            "編輯器擴充",
            "애드온",
            "플러그인",
        ]
        .iter()
        .any(|word| text.contains(word))
}

fn description_defines_software(opening: &str) -> bool {
    if ["requires", "required", "dependency", "必要", "依存"]
        .iter()
        .any(|word| opening.contains(word))
    {
        return false;
    }
    [
        "unity editor tool",
        "unity editor extension",
        "blender addon",
        "blender add-on",
        "アドオンです",
        "ツールです",
        "エディター専用ツール",
        "エディタ専用ツール",
    ]
    .iter()
    .any(|word| opening.contains(word))
        || opening.contains("유니티 에디터") && opening.contains("툴")
}

fn affirmative_base_title(title: &str) -> bool {
    let types = [
        "original 3d avatar",
        "original avatar",
        "base avatar",
        "3d avatar",
        "3d character",
        "vrchat avatar",
    ];
    if types.iter().any(|kind| word_match(title, kind))
        || [
            "オリジナル3dアバター",
            "オリジナルアバター",
            "3dアバター",
            "3dキャラクター",
        ]
        .iter()
        .any(|kind| title.contains(kind))
    {
        return true;
    }
    // A generic avatar word qualifies only when it directly introduces one
    // quoted name and consumes the whole remaining title; suffix prose stays.
    for prefix in ["アバター", "avatar"] {
        if let Some(rest) = title.strip_prefix(prefix).map(str::trim) {
            if rest.starts_with(['「', '『', '"', '“']) {
                let clean = names::cleaned_title(rest);
                if clean != rest
                    && names::has_name(&clean)
                    && !clean.contains(['「', '」', '『', '』', '"', '“', '”'])
                {
                    return true;
                }
            }
        }
    }
    false
}

fn compatibility_context(text: &str) -> bool {
    // Complete known platform qualifiers (e.g. "for VRChat") describe the
    // target platform, whereas "for <avatar name>" describes compatibility.
    let cleaned = names::cleaned_title(text);
    let text = cleaned.as_str();
    [
        "for",
        "compatible",
        "compatibility",
        "support",
        "supports",
        "supported",
    ]
    .iter()
    .any(|word| word_match(text, word))
        || [
            "アバター対応",
            "アバター用",
            "アバター向け",
            "アバター専用",
            "対応アバター",
            "対応モデル",
            "素体用",
            "素體用",
        ]
        .iter()
        .any(|word| text.contains(word))
}

fn strong_external_application(text: &str) -> bool {
    [
        "aftereffects",
        "after effects",
        "aftereffcts",
        "ae",
        "novelai",
        "photoshop",
        "clip studio",
        "clipstudiopaint",
        "live2d",
        "ccfolia",
        "ココフォリア",
        "trpg",
        "coc",
    ]
    .iter()
    .any(|term| word_match(text, term))
}

fn description_defines_shader(description: &str) -> bool {
    let opening: String = description.chars().take(180).collect();
    ![
        "requires",
        "required",
        "dependency",
        "依存",
        "必要",
        "使用シェーダ",
    ]
    .iter()
    .any(|word| opening.contains(word))
        && [
            "unity shader",
            "ugui shader",
            "shader for",
            "shader package",
            "シェーダーです",
            "シェーダです",
            "シェーダーを作成",
            "シェーダを作成",
        ]
        .iter()
        .any(|word| opening.contains(word))
}

struct Avatar {
    value: Value,
    names: Vec<(String, String)>,
}

fn avatars() -> &'static Vec<Avatar> {
    static CACHE: OnceLock<Vec<Avatar>> = OnceLock::new();
    CACHE.get_or_init(|| {
        let entries: Vec<Value> = serde_json::from_str(include_str!("avatars.json")).expect("bundled avatar dictionary must be valid JSON");
        entries.into_iter().map(|mut entry| {
            let official_name = entry["officialName"].as_str().expect("officialName").to_owned();
            let mut names = vec![(normalize(&official_name), official_name)];
            names.extend(entry["aliases"].as_array().expect("aliases").iter().filter_map(Value::as_str).map(|name| (normalize(name), name.to_owned())));
            if let Some(name) = entry["englishName"].as_str() {
                names.push((normalize(name), name.to_owned()));
            }
            names.sort();
            names.dedup();
            entry["id"] = json!(format!("booth:{}", entry["productId"].as_str().expect("productId")));
            entry["provenance"] = json!({"kind":"officialPublicListing", "url":entry["sourceUrl"],
                "observedAt":entry["englishNameEvidence"]["observedAt"].as_str().unwrap_or("2026-10-09"), "dictionaryVersion":VERSION,
                "fields":["productId","officialName","creator"], "aliasPolicy":"see taxonomy/README.md"});
            entry["displayName"] = json!(avatar_display_name(&entry));
            Avatar { value: entry, names }
        }).collect()
    })
}

fn stripped_avatar_name(text: &str) -> String {
    names::cleaned_title(text)
}

struct ExactAvatarLookup {
    names: BTreeMap<String, usize>,
    pairs: BTreeMap<String, usize>,
}

fn exact_avatar_lookup() -> &'static ExactAvatarLookup {
    static LOOKUP: OnceLock<ExactAvatarLookup> = OnceLock::new();
    LOOKUP.get_or_init(|| {
        let mut lookup = ExactAvatarLookup {
            names: BTreeMap::new(),
            pairs: BTreeMap::new(),
        };
        for (index, avatar) in avatars().iter().enumerate() {
            for (name, _) in &avatar.names {
                lookup.names.entry(name.clone()).or_insert(index);
            }
            for (left, _) in &avatar.names {
                for (right, _) in &avatar.names {
                    for separator in [" / ", "/", " ", " -", " - ", "-", " (", "(", " 「", "『"] {
                        lookup
                            .pairs
                            .entry(format!("{left}{separator}{right}").trim().to_owned())
                            .or_insert(index);
                    }
                }
            }
        }
        lookup
    })
}

/// Input must already be normalized. Preserve the prior first-dictionary-entry
/// precedence even if a future direct alias collides with another entry's pair.
fn exact_avatar_index_normalized(name: &str) -> Option<usize> {
    let lookup = exact_avatar_lookup();
    let pair_key = name.trim_end_matches(['-', ')', '」', '』']).trim();
    lookup
        .names
        .get(name)
        .into_iter()
        .chain(lookup.pairs.get(pair_key))
        .copied()
        .min()
}

fn exact_avatar_normalized(name: &str) -> Option<&'static Avatar> {
    exact_avatar_index_normalized(name).map(|index| &avatars()[index])
}

fn avatar_tag_normalized(normalized: &str) -> Option<&'static Avatar> {
    let bare = ["対応", "専用", "用", "對應", "对应", "向け"]
        .iter()
        .find_map(|suffix| normalized.strip_suffix(suffix))
        .unwrap_or(normalized)
        .trim();
    // Resolve full/bare candidates in dictionary order, matching the old loop.
    let full = exact_avatar_index_normalized(normalized);
    if bare == normalized {
        return full.map(|index| &avatars()[index]);
    }
    let bare_index = exact_avatar_index_normalized(bare).or_else(|| {
        // A compatibility suffix can sit outside a complete name wrapper.
        // The cleaned whole name must still be an exact known alias.
        exact_avatar_index_normalized(&normalize(&stripped_avatar_name(bare)))
    });
    full.into_iter()
        .chain(bare_index)
        .min()
        .map(|index| &avatars()[index])
}

/// Exact canonical avatar tags are compatibility evidence, never file-access
/// or purchase evidence. Unlike match_avatars this rejects apparel substrings.
pub fn exact_avatar_tag(text: &str) -> Option<Value> {
    avatar_tag_normalized(&normalize(&stripped_avatar_name(text))).map(|avatar| {
        let mut candidate = avatar.value.clone();
        candidate["basis"] = json!("exactTag");
        candidate["matchedAliases"] = json!([text]);
        candidate["entitlementEstablished"] = json!(false);
        candidate
    })
}

/// Remove recognized model marketing wrappers, not arbitrary avatar mentions in
/// apparel names. Cleaning is presentation only and never establishes identity.
pub fn clean_avatar_name(text: &str) -> String {
    let name = stripped_avatar_name(text);
    exact_avatar_normalized(&normalize(&name))
        .and_then(|avatar| avatar.value["officialName"].as_str())
        .unwrap_or(&name)
        .to_owned()
}

/// Avatar-label presentation only; this does not establish product identity or
/// extract avatar mentions from file names, tags, or apparel titles.
pub fn avatar_label_parts(title: &str) -> (String, Option<String>) {
    let (original, english) = names::base_parts(title);
    (
        clean_avatar_name(if original.trim().is_empty() {
            title
        } else {
            &original
        }),
        english,
    )
}

pub fn avatar_label_is_unambiguous(title: &str) -> bool {
    names::has_name(title)
}

/// Kana romanization with a cached local Windows phonetic fallback for kanji.
/// OS readings are fallback spellings, never authoritative English names.
pub fn romanized_name(input: &str) -> String {
    let clean = stripped_avatar_name(input);
    if let Some(avatar) = exact_avatar_normalized(&normalize(&clean)) {
        return avatar.value["englishName"]
            .as_str()
            .unwrap_or("")
            .to_owned();
    }
    let romanized = romanize_kana(&clean);
    if !romanized.is_empty() {
        return romanized;
    }
    phonetic::reading(&reading_input(&clean))
        .map(|reading| romanize_kana(&reading))
        .unwrap_or_default()
}
// Typography is not a missing pronunciation. Preserve letters (including
// unknown Han) so unsupported readings cannot silently become partial names.
// Strip decoration before the OS reader, which otherwise pronounces ★ as hoshi.
fn reading_input(input: &str) -> String {
    let mut result = String::new();
    for c in input.nfkc() {
        match c {
            '‐' | '‑' | '‒' | '–' | '—' | '―' | '−' => result.push('-'),
            '’' | '‘' | 'ʼ' => result.push('\''),
            c if c.is_alphanumeric()
                || matches!(
                    c,
                    'ー' | '々' | 'ゝ' | 'ゞ' | 'ヽ' | 'ヾ' | '\u{3099}' | '\u{309a}'
                ) =>
            {
                result.push(c)
            }
            c if c.is_ascii() && matches!(c, '-' | '\'' | '_' | '.' | '+' | '&') => result.push(c),
            '\u{0300}'..='\u{036f}' | '\u{fe00}'..='\u{fe0f}' => {}
            _ => result.push(' '),
        }
    }
    result.split_whitespace().collect::<Vec<_>>().join(" ")
}
fn romanize_kana(clean: &str) -> String {
    let prepared = reading_input(clean);
    let kana: Vec<char> = prepared
        .nfkc()
        .map(|c| {
            if ('ァ'..='ヶ').contains(&c) {
                char::from_u32(c as u32 - 0x60).unwrap()
            } else if ('ㇰ'..='ㇿ').contains(&c) {
                [
                    'く', 'し', 'す', 'と', 'ぬ', 'は', 'ひ', 'ふ', 'へ', 'ほ', 'む', 'ら', 'り',
                    'る', 'れ', 'ろ',
                ][(c as u32 - 0x31f0) as usize]
            } else {
                c
            }
        })
        .collect();
    let mut result = String::new();
    let mut index = 0;
    let mut double = false;
    while index < kana.len() {
        let c = kana[index];
        if c == 'っ' {
            double = true;
            index += 1;
            continue;
        }
        if c == 'ー' {
            if let Some(vowel) = result.chars().rev().find(|c| "aeiou".contains(*c)) {
                result.push(vowel);
            }
            index += 1;
            continue;
        }
        let base = match c {
            'あ' | 'ぁ' => "a",
            'い' | 'ぃ' => "i",
            'う' | 'ぅ' => "u",
            'え' | 'ぇ' => "e",
            'お' | 'ぉ' => "o",
            'か' => "ka",
            'き' => "ki",
            'く' => "ku",
            'け' => "ke",
            'こ' => "ko",
            'が' => "ga",
            'ぎ' => "gi",
            'ぐ' => "gu",
            'げ' => "ge",
            'ご' => "go",
            'さ' => "sa",
            'し' => "shi",
            'す' => "su",
            'せ' => "se",
            'そ' => "so",
            'ざ' => "za",
            'じ' => "ji",
            'ず' => "zu",
            'ぜ' => "ze",
            'ぞ' => "zo",
            'た' => "ta",
            'ち' => "chi",
            'つ' => "tsu",
            'て' => "te",
            'と' => "to",
            'だ' => "da",
            'ぢ' => "ji",
            'づ' => "zu",
            'で' => "de",
            'ど' => "do",
            'な' => "na",
            'に' => "ni",
            'ぬ' => "nu",
            'ね' => "ne",
            'の' => "no",
            'は' => "ha",
            'ひ' => "hi",
            'ふ' => "fu",
            'へ' => "he",
            'ほ' => "ho",
            'ば' => "ba",
            'び' => "bi",
            'ぶ' => "bu",
            'べ' => "be",
            'ぼ' => "bo",
            'ぱ' => "pa",
            'ぴ' => "pi",
            'ぷ' => "pu",
            'ぺ' => "pe",
            'ぽ' => "po",
            'ま' => "ma",
            'み' => "mi",
            'む' => "mu",
            'め' => "me",
            'も' => "mo",
            'や' | 'ゃ' => "ya",
            'ゆ' | 'ゅ' => "yu",
            'よ' | 'ょ' => "yo",
            'ら' => "ra",
            'り' => "ri",
            'る' => "ru",
            'れ' => "re",
            'ろ' => "ro",
            'わ' | 'ゎ' => "wa",
            'ゐ' => "i",
            'ゑ' => "e",
            'ゕ' => "ka",
            'ゖ' => "ke",
            'を' => "wo",
            'ん' => "n",
            'ゔ' => "vu",
            _ => {
                if c.is_ascii() {
                    result.push(c);
                    index += 1;
                    continue;
                } else if matches!(c, 'ゝ' | 'ゞ') && index > 0 {
                    let mut repeated = kana[index - 1].to_string();
                    if c == 'ゞ' {
                        repeated.push('\u{3099}');
                    }
                    let syllable = romanize_kana(&repeated).to_ascii_lowercase();
                    if syllable.is_empty() {
                        return String::new();
                    }
                    result.push_str(&syllable);
                    index += 1;
                    continue;
                } else if matches!(c as u32, 0x00c0..=0x024f) {
                    let latin: String = c
                        .to_string()
                        .nfkd()
                        .filter(|c| c.is_ascii_alphabetic())
                        .collect();
                    if latin.is_empty() {
                        return String::new();
                    }
                    result.push_str(&latin);
                    index += 1;
                    continue;
                } else {
                    return String::new();
                }
            }
        };
        let mut syllable = base.to_owned();
        if let Some(small) = kana
            .get(index + 1)
            .filter(|c| matches!(c, 'ゃ' | 'ゅ' | 'ょ'))
        {
            if base.ends_with('i') {
                let stem = match base {
                    "shi" => "sh",
                    "chi" => "ch",
                    "ji" => "j",
                    _ => &base[..base.len() - 1],
                };
                syllable = format!(
                    "{}{}{}",
                    stem,
                    if matches!(base, "shi" | "chi" | "ji") {
                        ""
                    } else {
                        "y"
                    },
                    match small {
                        'ゃ' => "a",
                        'ゅ' => "u",
                        _ => "o",
                    }
                );
                index += 1;
            }
        }
        if double {
            if let Some(first) = syllable.chars().next().filter(|c| !"aeioun".contains(*c)) {
                result.push(first);
            }
            double = false;
        }
        result.push_str(&syllable);
        index += 1;
    }
    let mut result = result.split_whitespace().collect::<Vec<_>>().join(" ");
    if !result.chars().any(|c| c.is_ascii_alphanumeric()) {
        return String::new();
    }
    if let Some(index) = result.find(|c: char| c.is_ascii_alphabetic()) {
        result[index..index + 1].make_ascii_uppercase();
    }
    result
}

pub fn avatar_display_name(avatar: &Value) -> String {
    let original = avatar["officialName"].as_str().unwrap_or("");
    let english = avatar["englishName"].as_str().unwrap_or("");
    if english.is_empty() {
        original.to_owned()
    } else {
        format!("{original} / {english}")
    }
}

// Conservative boundaries keep short names such as 萌 out of unrelated words.
// Compatibility suffixes are only lexical evidence, never ownership evidence.
fn name_match(text: &str, name: &str) -> bool {
    if name.is_empty() {
        return false;
    }
    text.match_indices(name).any(|(start, _)| {
        let before = text[..start].chars().next_back();
        let after = &text[start + name.len()..];
        let first = name.chars().next().unwrap();
        let left_ok =
            !before.is_some_and(|c| c.is_alphanumeric() && (c.is_ascii() == first.is_ascii()));
        let last = name.chars().next_back().unwrap();
        let right_ok = !after
            .chars()
            .next()
            .is_some_and(|c| c.is_alphanumeric() && (c.is_ascii() == last.is_ascii()))
            || ["対応", "専用", "用", "對應", "对应", "向け", "ちゃん"]
                .iter()
                .any(|suffix| after.starts_with(suffix));
        left_ok && right_ok
    })
}

/// Candidate name mentions. A result is never a product-ID resolver and must not
/// be promoted into ownedAvatars without independent account/file/user evidence.
pub fn match_avatars(text: &str) -> Vec<Value> {
    let text = normalize(text);
    avatars()
        .iter()
        .filter_map(|avatar| {
            let matched: Vec<_> = avatar
                .names
                .iter()
                .filter(|(key, _)| name_match(&text, key))
                .map(|(_, name)| name.clone())
                .collect();
            if matched.is_empty() {
                return None;
            }
            let mut candidate = avatar.value.clone();
            candidate["matchedAliases"] = json!(matched);
            candidate["basis"] = json!("nameMention");
            candidate["entitlementEstablished"] = json!(false);
            Some(candidate)
        })
        .collect()
}

/// Normalized index expansion, including the normalized input itself. Do not
/// AND all these values as query terms: they are alternate searchable spellings.
pub fn aliases(text: &str) -> Vec<String> {
    let mut values = BTreeSet::new();
    let text = normalize(text);
    if !text.is_empty() {
        values.insert(text.clone());
    }
    for avatar in avatars() {
        if avatar.names.iter().any(|(key, _)| name_match(&text, key)) {
            values.extend(avatar.names.iter().map(|(key, _)| key.clone()));
        }
    }
    for tag in TAGS {
        let names: Vec<_> = std::iter::once(tag.label)
            .chain(tag.aliases.iter().copied())
            .map(normalize)
            .collect();
        if names.iter().any(|name| name_match(&text, name)) {
            values.extend(names);
        }
    }
    values.into_iter().collect()
}

/// Exact tag equivalence (including a bare compatibility suffix). Unlike a
/// text mention matcher this cannot turn "Mafuyu jacket" into a whole-avatar tag.
pub fn canonical_tag(text: &str) -> String {
    let normalized = normalize(&stripped_avatar_name(text));
    if let Some(avatar) = avatar_tag_normalized(&normalized) {
        return avatar.value["id"].as_str().unwrap().to_owned();
    }
    for tag in TAGS {
        if normalize(tag.label) == normalized
            || tag
                .aliases
                .iter()
                .any(|alias| normalize(alias) == normalized)
        {
            return tag.id.to_owned();
        }
    }
    normalized
}

/// Caller keeps the returned raw spelling and owns durable overlay/rejection
/// persistence. This only de-duplicates equivalents and applies explicit rejection.
pub fn effective_tags(source: &[String], user: &[String], rejected: &[String]) -> Vec<String> {
    let rejected: BTreeSet<_> = rejected.iter().map(|tag| canonical_tag(tag)).collect();
    let mut seen = BTreeSet::new();
    user.iter()
        .chain(source)
        .filter(|tag| !tag.trim().is_empty())
        .filter(|tag| {
            let key = canonical_tag(tag);
            !rejected.contains(&key) && seen.insert(key)
        })
        .cloned()
        .collect()
}

/// Library Base records supplement choices by explicit product identity only.
/// The supplied records must be effective products already in the catalogue.
/// Classification and presence in that catalogue do not establish file access.
pub fn owned_base_options(products: &[Value]) -> Vec<Value> {
    let mut options = BTreeMap::<String, Value>::new();
    for product in products {
        if product["category"].as_str() != Some("Base") {
            continue;
        }
        let Some(id) = product["id"].as_str().filter(|id| !id.trim().is_empty()) else {
            continue;
        };
        let known = avatars()
            .iter()
            .find(|avatar| avatar.value["productId"].as_str() == Some(id));
        let mut entry = known.map(|avatar| avatar.value.clone()).unwrap_or_else(|| json!({
            "id":format!("product:{id}"), "productId":id,
            "officialName":product["sourceTitle"].as_str().filter(|name| !name.trim().is_empty()).or_else(|| product["title"].as_str()).unwrap_or(id),
            "aliases":[], "provenance":{"kind":"catalogueBaseProduct", "productId":id},
        }));
        if known.is_none() {
            if !names::has_name(entry["officialName"].as_str().unwrap_or("")) {
                continue;
            }
            let (name, source_spelling) =
                avatar_label_parts(entry["officialName"].as_str().unwrap_or(id));
            let english = source_spelling
                .clone()
                .unwrap_or_else(|| romanized_name(&name));
            entry["officialName"] = json!(name);
            entry["englishNameStatus"] = json!(if english.is_empty() {
                "unresolvedReading"
            } else if source_spelling.is_some() {
                "sourceTitleSpelling"
            } else {
                "romanizationFallback"
            });
            entry["englishName"] = json!(english);
            entry["englishNameEvidence"] = json!({"kind":if source_spelling.is_some(){"sourceTitleSpelling"}else{"presentationFallback"},"identityEstablished":false});
        }
        entry["displayName"] = json!(avatar_display_name(&entry));
        entry["basis"] = json!("catalogueBaseProduct");
        entry["entitlementEstablished"] = json!(false);
        entry["ownedProductIds"] = json!([id]);
        options.insert(entry["id"].as_str().unwrap().to_owned(), entry);
    }
    options.into_values().collect()
}

struct Tag {
    id: &'static str,
    label: &'static str,
    kind: &'static str,
    aliases: &'static [&'static str],
}
// Original ordinary-language vocabulary, not imported from any tag/rule table.
const TAGS: &[Tag] = &[
    Tag {
        id: "style:casual",
        label: "Casual",
        kind: "style",
        aliases: &["カジュアル", "休闲", "休閒", "캐주얼"],
    },
    Tag {
        id: "style:formal",
        label: "Formal",
        kind: "style",
        aliases: &["フォーマル", "正装", "正裝", "포멀"],
    },
    Tag {
        id: "style:gothic",
        label: "Gothic",
        kind: "style",
        aliases: &["ゴシック", "哥特", "哥德", "고딕"],
    },
    Tag {
        id: "style:fantasy",
        label: "Fantasy",
        kind: "style",
        aliases: &["ファンタジー", "奇幻", "판타지"],
    },
    Tag {
        id: "style:cyberpunk",
        label: "Cyberpunk",
        kind: "style",
        aliases: &["サイバーパンク", "赛博朋克", "賽博龐克", "사이버펑크"],
    },
    Tag {
        id: "use:vrchat",
        label: "VRChat",
        kind: "use",
        aliases: &["vrc"],
    },
    Tag {
        id: "use:vrm",
        label: "VRM",
        kind: "use",
        aliases: &[],
    },
    Tag {
        id: "use:streaming",
        label: "Streaming",
        kind: "use",
        aliases: &["配信", "直播", "스트리밍"],
    },
    Tag {
        id: "rating:r18",
        label: "R-18",
        kind: "rating",
        aliases: &["r18", "成人向け", "成人向", "성인용"],
    },
    Tag {
        id: "rating:all-ages",
        label: "All ages",
        kind: "rating",
        aliases: &["全年齢", "全年龄", "全年齡", "전연령"],
    },
];

/// Suggestions retain uncertainty; rating suggestions never set sensitive=true.
pub fn tag_suggestions(text: &str, rejected: &[String]) -> Vec<Value> {
    let normalized = normalize(text);
    let rejected: BTreeSet<_> = rejected.iter().map(|tag| canonical_tag(tag)).collect();
    TAGS.iter().filter(|tag| !rejected.contains(tag.id)).filter(|tag| {
        std::iter::once(tag.label).chain(tag.aliases.iter().copied()).any(|alias| name_match(&normalized, &normalize(alias)))
    }).map(|tag| json!({"id":tag.id,"label":tag.label,"kind":tag.kind,"basis":"nameMention","inferred":true})).collect()
}

pub fn dictionary() -> Value {
    json!({"version":VERSION, "categories":CATEGORY_KEYS,
        "avatars":avatars().iter().map(|avatar| avatar.value.clone()).collect::<Vec<_>>(),
        "tags":TAGS.iter().map(|tag| json!({"id":tag.id,"label":tag.label,"kind":tag.kind,"aliases":tag.aliases,"provenance":{"kind":"originalVocabulary","version":VERSION}})).collect::<Vec<_>>(),
        "coverage":{"populationCoverage":null,"targetPopulationCoverage":0.8,"status":"finitePublicBaselineOnly",
            "evaluation":include_str!("evaluation.json").parse::<Value>().expect("bundled evaluation must be valid JSON")}})
}

#[cfg(test)]
mod tests {
    #[test]
    fn jinxxy_scope_preserves_specific_types_and_falls_back_inside_vrchat() {
        let generic = infer_product_category(
            &json!({"provider":"jinxxy","sourceTitle":"Authored View (VRCFury)"}),
        );
        assert_eq!(generic["category"], "Gimmick");
        assert_eq!(generic["scope"], "vrchat");
        assert_eq!(generic["basis"], "providerFallback");
        let props = infer_product_category(
            &json!({"provider":"jinxxy","sourceTitle":"Authored View (VRCFury)","sourceCategory":{"name":"avatar-props","types":["avatar_addon"]},"sourceTags":["vrcfury","animation","xray","screenshader"]}),
        );
        assert_eq!(props["category"], "Accessory");
        assert_eq!(props["scope"], "vrchat");
        let addon = infer_product_category(
            &json!({"provider":"jinxxy","sourceTitle":"Authored View","sourceCategory":{"name":"avatars","types":["avatar_addon"]}}),
        );
        assert_eq!(addon["category"], "Gimmick");
        assert_eq!(addon["basis"], "sourceType");
        for (title, expected) in [
            ("Authored Dress (VRCFury)", "Clothes"),
            ("Authored Hair", "Hair"),
            ("Authored Texture", "Texture"),
            ("Authored Tool", "Tool"),
        ] {
            let value = infer_product_category(&json!({"store":"Jinxxy","sourceTitle":title}));
            assert_eq!(value["category"], expected);
            assert_eq!(value["scope"], "vrchat");
        }
        // Source default must never recategorize another store's unrelated files.
        assert_eq!(
            infer_product_category(&json!({"provider":"booth","sourceTitle":"Authored Texture"}))
                ["category"],
            "NonVRChat"
        );
    }
    #[test]
    fn typography_does_not_discard_valid_readings_or_slice_unicode_indices() {
        for (input, expected) in [
            ("“そらねこ”", "Soraneko"),
            ("★そらねこ★", "Soraneko"),
            ("そら—ねこ", "Sora-neko"),
            ("シロネコ\u{fe0f}", "Shironeko"),
            ("ゐろは", "Iroha"),
            ("さゞなみ", "Sazanami"),
            ("くゝり", "Kukuri"),
            ("Étoile", "Etoile"),
        ] {
            assert_eq!(super::romanized_name(input), expected, "{input}");
        }
        assert_eq!(super::clean_avatar_name("İro Ver.2"), "İro");
        assert_eq!(super::clean_avatar_name("Silver2"), "Silver2");
        assert_eq!(super::clean_avatar_name("Version2"), "Version2");
        assert_eq!(
            super::avatar_label_parts("Version2"),
            ("Version2".into(), None)
        );
        assert_eq!(super::avatar_label_parts("“V2”"), ("V2".into(), None));
        assert_eq!(
            super::avatar_label_parts("Original 3D Avatar"),
            ("Original 3D Avatar".into(), None)
        );
        assert!(super::owned_base_options(&[serde_json::json!({"id":"synthetic-unnamed","category":"Base","sourceTitle":"Original 3D Avatar"})]).is_empty());
        assert_eq!(
            super::clean_avatar_name("Original 3D Modeler"),
            "Original 3D Modeler"
        );
        assert_eq!(super::romanized_name("İro"), "Iro");
        assert_eq!(super::romanize_kana("そらΩ"), "");
        assert_eq!(super::romanize_kana("そら龘"), "");
        assert_eq!(super::reading_input("★星空猫★"), "星空猫");
        #[cfg(target_os = "windows")]
        assert_eq!(super::romanized_name("★星空猫★"), "Hoshizoraneko");
    }
    use super::*;

    #[test]
    fn categories_preserve_precedence_and_unknown_fallback() {
        assert_eq!(infer_category(&Value::Null, &[])["category"], "NonVRChat");
        assert_eq!(
            infer_category(&json!({"name":"3Dモデル（その他）"}), &[])["basis"],
            "fallback"
        );
        let source = json!({"name":"アバター", "parentName":"3D", "id":null});
        let result = infer_category(&source, &["衣装".into()]);
        assert_eq!(result["category"], "Clothes");
        assert_eq!(result["basis"], "tags");
        assert_eq!(source["name"], "アバター");
        assert!(result.get("ownedAvatars").is_none());
        assert_eq!(
            infer_category(
                &json!({"name":"3D衣装", "parentName":"アバター"}),
                &["shader".into()]
            )["category"],
            "Clothes"
        );
        assert_eq!(
            infer_category(&json!("３Ｄキャラクター"), &[])["category"],
            "Base"
        );
        assert_eq!(
            infer_category(&Value::Null, &["chair".into(), "worldwide".into()])["category"],
            "NonVRChat"
        );
    }

    #[test]
    fn every_fixed_category_is_reachable_without_ownership() {
        let examples = [
            "avatar",
            "clothes",
            "hair",
            "props",
            "textures",
            "motion",
            "tool",
            "shader",
            "gimmick",
            "world",
            "udon",
            "illustration",
        ];
        for (category, example) in CATEGORY_KEYS.iter().zip(examples) {
            assert_eq!(infer_category(&json!(example), &[])["category"], *category);
        }
    }

    #[test]
    fn nfkc_aliases_and_boundaries_do_not_guess_ids() {
        assert!(aliases("Ｍａｆｕｙｕ").contains(&"真冬".into()));
        assert!(aliases("ﾏﾇｶ対応").contains(&"manuka".into()));
        assert_eq!(match_avatars("Mafuyu_coat.zip")[0]["id"], "booth:5007531");
        assert_eq!(match_avatars("真冬用・マヌカ対応").len(), 2);
        assert!(match_avatars("萌芽").is_empty());
        assert!(match_avatars("minted manukau limelight").is_empty());
        assert!(match_avatars("5007531").is_empty());
        assert!(match_avatars("").is_empty());
        assert_eq!(match_avatars("Mafuyu")[0]["entitlementEstablished"], false);
    }

    #[test]
    fn rejection_blocks_equivalent_alias_after_refresh() {
        assert_eq!(canonical_tag("真冬対応"), canonical_tag("Ｍａｆｕｙｕ"));
        assert_eq!(canonical_tag("真冬 / Mafuyu"), canonical_tag("真冬"));
        assert_ne!(canonical_tag("真冬 / マヌカ"), canonical_tag("真冬"));
        assert_ne!(canonical_tag("Mafuyu coat"), canonical_tag("Mafuyu"));
        assert_eq!(
            effective_tags(
                &["Mafuyu".into(), "まふゆ".into(), "new".into()],
                &["manual".into()],
                &["真冬".into()]
            ),
            vec!["manual", "new"]
        );
        assert!(tag_suggestions("casual R18", &["カジュアル".into(), "R-18".into()]).is_empty());
    }

    #[test]
    fn creator_spellings_and_legacy_readings_share_exact_compatibility_identity() {
        for (id, original, english, legacy) in [
            ("3443188", "竜胆", "Rindo", "Rindo"),
            ("3659436", "リーファ", "Leefa", "Riifa"),
            ("3564947", "あのん", "Anon", "Anon"),
            ("6373683", "りりか", "ririka", "Ririka"),
            ("6846646", "真央", "mao", "mao"),
            ("5650156", "しお", "Sio", "Shio"),
            ("2780069", "ここあ", "Kokoa", "Kokoa"),
        ] {
            for spelling in [
                original.to_owned(),
                english.to_owned(),
                legacy.to_owned(),
                format!("{original} / {english}対応"),
                format!("【{original}】対応"),
            ] {
                let candidate = exact_avatar_tag(&spelling).unwrap();
                assert_eq!(candidate["id"], format!("booth:{id}"));
                assert_eq!(candidate["englishName"], english);
                assert_eq!(candidate["englishNameStatus"], "official");
                assert_eq!(candidate["entitlementEstablished"], false);
            }
            assert!(exact_avatar_tag(&format!("{original}用コート")).is_none());
            assert_eq!(canonical_tag(legacy), canonical_tag(original));
            assert!(effective_tags(&[legacy.into()], &[], &[original.into()]).is_empty());
        }
    }

    #[test]
    fn product_type_is_not_the_tools_output_or_avatar_compatibility() {
        // Authored structural equivalents of public 8643321 and 8606444;
        // no product-ID dispatch or copied descriptions.
        let addon = json!({"sourceCategory":"ソフトウェア", "sourceTitle":"Blenderアドオン：形状の生成", "sourceTags":["Blender","アバター"], "sourceDescription":"Blender addon for editing shapes. VRChat supports the exported shapes."});
        let maker = json!({"sourceCategory":{"name":"ソフトウェア"},"sourceTitle":"[Unity] Surface Mask Maker", "sourceTags":["テクスチャ","マテリアル"],"sourceDescription":"Unity editor tool that generates texture masks."});
        assert_eq!(infer_product_category(&addon)["category"], "Tool");
        assert_eq!(infer_product_category(&addon)["scope"], "vrchat");
        assert_eq!(infer_product_category(&maker)["category"], "Tool");
        assert_eq!(infer_product_category(&maker)["scope"], "unity");
        for output_tag in ["Hair", "Clothes", "Shader", "Motion", "アバター"] {
            let software = json!({"sourceCategory":"Software", "sourceTitle":"Unity Editor Extension", "sourceTags":["Unity",output_tag]});
            assert_eq!(
                infer_product_category(&software)["category"],
                "Tool",
                "{output_tag}"
            );
        }
        let shader = json!({"sourceCategory":"Software", "sourceTitle":"Unity Water Shader", "sourceTags":["Unity","Shader"]});
        assert_eq!(infer_product_category(&shader)["category"], "Shader");
        for source in [
            json!({"sourceTitle":"Blender add-on", "sourceTags":["Blender","アバター"]}),
            json!({"sourceTitle":"Shape helper", "sourceTags":["VRChat","アバター"]}),
            json!({"sourceTitle":"Example", "description":"Compatible with VRChat avatars and original 3D characters."}),
            json!({"sourceTitle":"8アバター対応", "sourceTags":["VRChat","アバター"]}),
            json!({"sourceTitle":"Avatar Configuration", "sourceTags":["VRChat","Avatar"]}),
            json!({"sourceTitle":"Example for Soraneko avatar", "sourceTags":["VRChat","Avatar"]}),
            json!({"sourceTitle":"アバター改変素材", "sourceTags":["VRChat","アバター"]}),
            json!({"sourceTitle":"アバター「ソラネコ」設定", "sourceTags":["VRChat","アバター"]}),
            json!({"sourceCategory":{"name":"その他","parentName":"3Dキャラクター"},"sourceTitle":"Example", "sourceTags":["VRChat"]}),
        ] {
            assert_eq!(
                infer_product_category(&source)["category"],
                "NonVRChat",
                "{source}"
            );
        }
        // Strong leaf/title product evidence survives, as does a real avatar
        // whose description merely names Blender or a required addon.
        for source in [
            json!({"sourceCategory":"3Dキャラクター", "sourceTitle":"ソラネコ", "sourceTags":["VRChat"]}),
            json!({"sourceTitle":"Original 3D Avatar Soraneko", "sourceTags":["VRChat"],"description":"Created in Blender. Requires an addon for an optional edit."}),
            json!({"sourceTitle":"Original 3D Avatar Soraneko for VRChat", "sourceTags":["VRChat","アバター"]}),
            json!({"sourceTitle":"オリジナル3Dモデル「ソラネコ」", "sourceTags":["VRChat","アバター"]}),
        ] {
            assert_eq!(
                infer_product_category(&source)["category"],
                "Base",
                "{source}"
            );
        }
        for (title, description) in [
            (
                "Example",
                "Requires a Unity editor extension to install this avatar.",
            ),
            ("Example", "Optional Blender addon is required for editing."),
        ] {
            assert_ne!(
                infer_product_category(
                    &json!({"sourceTitle":title,"description":description,"sourceTags":["VRChat"]})
                )["category"],
                "Base"
            );
        }
        let raw = addon.clone();
        infer_product_category(&addon);
        assert_eq!(addon, raw);
    }

    #[test]
    fn mishe_exact_aliases_infer_compatibility_without_entitlement() {
        for tag in [
            "ミーシェ",
            "Mishe",
            "Ｍｉｓｈｅ",
            "ﾐｰｼｪ",
            "Miishie",
            "ミーシェ / Mishe",
            "ミーシェ / Miishie",
            "ミーシェ対応",
            "オリジナル3Dモデル「ミーシェ」",
        ] {
            let candidate = exact_avatar_tag(tag).expect("exact Mishe alias");
            assert_eq!(candidate["id"], "booth:1256087", "{tag}");
            assert_eq!(candidate["displayName"], "ミーシェ / Mishe");
            assert_eq!(candidate["basis"], "exactTag");
            assert_eq!(candidate["entitlementEstablished"], false);
            assert_eq!(canonical_tag(tag), "booth:1256087");
        }
        for tag in [
            "Mishe coat",
            "ミーシェ用ジャケット",
            "ミーシェ / 真冬",
            "Mishe / jacket",
            "booth:1256087",
            "1256087",
        ] {
            assert!(exact_avatar_tag(tag).is_none(), "not an exact alias: {tag}");
        }
        assert!(aliases("Miishie").contains(&"mishe".to_owned()));
        assert_eq!(romanized_name("ミーシェ"), "Mishe");
        assert_eq!(romanized_name("Miishie"), "Mishe");
        let dictionary = dictionary();
        let entry = dictionary["avatars"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["productId"] == "1256087")
            .unwrap();
        assert_eq!(entry["englishName"], "Mishe");
        assert_eq!(entry["englishNameStatus"], "userAccepted");
        assert_eq!(
            entry["englishNameEvidence"]["url"],
            "https://booth.pm/ja/items/2873084"
        );
        assert_eq!(
            effective_tags(
                &["Mishe".into(), "ミーシェ".into()],
                &["Miishie".into()],
                &[]
            ),
            vec!["Miishie"]
        );
        assert!(effective_tags(&["Mishe".into()], &[], &["ミーシェ".into()]).is_empty());
    }

    #[test]
    fn cached_exact_lookup_preserves_alias_pairs_suffixes_and_boundaries() {
        for spelling in [
            "真冬",
            "Ｍａｆｕｙｕ",
            "真冬 / Mafuyu",
            "Mafuyu (真冬)",
            "真冬 -Mafuyu-",
            "真冬『Mafuyu』",
            "Mafuyu対応",
            "ミルク Re / Milk Re",
        ] {
            let candidate = exact_avatar_tag(spelling).unwrap();
            let expected = if spelling.contains("Milk Re") {
                "booth:2953391"
            } else {
                "booth:5007531"
            };
            assert_eq!(candidate["id"], expected, "{spelling}");
            assert_eq!(canonical_tag(spelling), expected);
        }
        for spelling in [
            "Mafuyu_coat.zip",
            "Mafuyu / マヌカ",
            "萌芽",
            "minted",
            "Milk Re jacket",
        ] {
            assert!(exact_avatar_tag(spelling).is_none(), "{spelling}");
        }
        assert_eq!(clean_avatar_name("真冬 / Mafuyu"), "真冬");
        assert_eq!(romanized_name("真冬 / Mafuyu"), "Mafuyu");
    }

    #[test]
    fn public_classification_complaints_use_specific_type_and_scope() {
        // Independently authored minimal public-metadata patterns; no private
        // catalogue records, descriptions or paid file lists are included.
        let cases = [
            (
                "7329831",
                "ソフトウェア",
                "BeatPad",
                vec!["ツール", "Windows", "AfterEffects", "AE"],
                "NonVRChat",
                "other",
            ),
            (
                "7969637",
                "ソフトウェア",
                "NAI Smart Studio",
                vec!["Windows", "NovelAI", "生成AI"],
                "NonVRChat",
                "other",
            ),
            (
                "3282463",
                "イラスト素材",
                "配信デスク素材",
                vec!["VTuber", "配信枠", "配信素材"],
                "NonVRChat",
                "other",
            ),
            (
                "2153720",
                "ボイス",
                "声ネタ",
                vec!["ボイス", "サンプリング素材", "VTuber"],
                "NonVRChat",
                "other",
            ),
            (
                "1906822",
                "素材（その他）",
                "星空の表紙素材",
                vec!["小説表紙素材", "同人誌表紙"],
                "NonVRChat",
                "unknown",
            ),
            (
                "2662823",
                "素材（その他）",
                "炎 APNG",
                vec!["trpg素材", "trpg立ち絵素材"],
                "NonVRChat",
                "other",
            ),
            (
                "4070235",
                "フォント・書体",
                "テロッププリセット",
                vec!["AE", "aftereffcts", "テロップ"],
                "NonVRChat",
                "other",
            ),
            (
                "2826022",
                "素材（その他）",
                "Deco-UI",
                vec!["Unity", "ゲーム素材", "VRChat", "シェーダー"],
                "Shader",
                "vrchat",
            ),
            (
                "2832898",
                "素材（その他）",
                "ExWire",
                vec!["Unity", "VRChat", "シェーダー"],
                "Shader",
                "vrchat",
            ),
            (
                "3776089",
                "素材（その他）",
                "スケーティングロード",
                vec![
                    "アニメーション",
                    "Unity",
                    "VRChat",
                    "Humanoid",
                    "Emote",
                    "モーション素材",
                ],
                "Motion",
                "vrchat",
            ),
            (
                "3799864",
                "素材（その他）",
                "AFK 足組み",
                vec![
                    "アニメーション",
                    "Unity",
                    "VRChat",
                    "Humanoid",
                    "Emote",
                    "モーションデータ",
                ],
                "Motion",
                "vrchat",
            ),
        ];
        for (id, category, title, tags, expected, scope) in cases {
            let result = infer_product_category(
                &json!({"sourceCategory":{"name":category},"sourceTitle":title,"sourceTags":tags}),
            );
            assert_eq!(result["category"], expected, "public pattern {id}");
            assert_eq!(result["scope"], scope, "public scope {id}");
        }
        assert_eq!(
            infer_product_category(
                &json!({"sourceCategory":"素材（その他）","sourceTags":["VRChat"]})
            )["category"],
            "NonVRChat"
        );
        for (tag, expected) in [("Shader", "Shader"), ("Motion", "Motion")] {
            assert_eq!(
                infer_product_category(
                    &json!({"sourceCategory":"Materials","sourceTags":["Unity",tag]})
                )["category"],
                expected
            );
        }
    }

    #[test]
    fn product_scope_and_description_evidence_are_bounded() {
        let base = json!({"sourceTitle":"Texture Tool","sourceTags":["Windows","AE"],"description":"Exports textures usable in VRChat and Unity"});
        assert_eq!(infer_product_category(&base)["category"], "NonVRChat");
        assert_eq!(infer_product_category(&base)["scope"], "other");
        let mut explicit = base.clone();
        explicit["tags"] = json!(["VRChat"]);
        assert_eq!(infer_product_category(&explicit)["category"], "Texture");
        assert_eq!(infer_product_category(&explicit)["scope"], "vrchat");
        assert_eq!(
            infer_product_category(
                &json!({"sourceTitle":"Editor Tool","description":"Compare Unity alternatives"})
            )["category"],
            "NonVRChat"
        );
        assert_eq!(
            infer_product_category(
                &json!({"sourceTitle":"Editor Tool","description":"Import this Unity editor extension"})
            )["scope"],
            "unity"
        );
        assert_eq!(
            infer_product_category(
                &json!({"sourceTitle":"Editor Tool","sourceTags":["Windows","Unity"]})
            )["category"],
            "Tool"
        );
        assert_ne!(
            infer_product_category(
                &json!({"sourceTitle":"New item","sourceTags":["VRChat"],"description":"Requires lilToon shader and Unity."})
            )["category"],
            "Shader"
        );
        assert_eq!(
            infer_product_category(
                &json!({"sourceTitle":"Dress","sourceTags":["VRChat","衣装"],"description":"Unity shader package required: lilToon"})
            )["category"],
            "Clothes"
        );
        let fallback = infer_product_category(
            &json!({"sourceDescription":"","description":format!("Unity shader uGUI {}", "x".repeat(40_000))}),
        );
        assert_eq!(fallback["category"], "Shader");
        assert_eq!(fallback["scope"], "unity");
        assert!(fallback.to_string().len() < 1500);
    }

    #[test]
    fn external_opening_context_and_animation_dependencies_do_not_define_vr_products() {
        for app in [
            "After Effects",
            "AE",
            "NovelAI",
            "CoC",
            "Live2D",
            "ココフォリア",
        ] {
            let title_context = json!({"sourceTitle":format!("{app} Tool"),"sourceTags":["ツール"],"description":"Results may be used in VRChat"});
            let description_context = json!({"sourceTitle":"Texture Pack","sourceTags":["Texture"],"sourceDescription":format!("Materials for {app}. Some results can also be used in VRChat.")});
            for mut source in [title_context, description_context] {
                let result = infer_product_category(&source);
                assert_eq!(result["category"], "NonVRChat", "{app}");
                assert_eq!(result["scope"], "other", "{app}");
                source["tags"] = json!(["VRChat"]);
                let explicit = infer_product_category(&source);
                assert_ne!(explicit["category"], "NonVRChat", "explicit tag {app}");
                assert_eq!(explicit["scope"], "vrchat", "explicit tag {app}");
            }
        }
        let windows = infer_product_category(
            &json!({"sourceTitle":"Windows Unity Tool","sourceTags":["Windows","Unity"]}),
        );
        assert_eq!(windows["category"], "Tool");
        assert_eq!(windows["scope"], "unity");
        let late_mention = infer_product_category(
            &json!({"sourceTitle":"Unity Tool","sourceTags":["Unity"],"description":format!("{} Photoshop is used in an optional example.", "x".repeat(250))}),
        );
        assert_eq!(late_mention["category"], "Tool");
        assert_eq!(late_mention["scope"], "unity");
        assert!(!strong_external_application("aesthetic tool coconut"));
        let mut motion = json!({"sourceTitle":"Ambient effect","sourceTags":["Unity"],"description":"Requires an animation controller."});
        assert_eq!(infer_product_category(&motion)["category"], "NonVRChat");
        motion["sourceTags"] = json!(["Unity", "Motion"]);
        assert_eq!(infer_product_category(&motion)["category"], "Motion");
        motion["sourceTags"] = json!(["Unity"]);
        motion["sourceTitle"] = json!("Humanoid Motion");
        assert_eq!(infer_product_category(&motion)["category"], "Motion");
    }

    #[test]
    fn windows_tool_requires_explicit_vr_tag_or_meaningful_unity_context() {
        let mut source = json!({"sourceTitle":"Windows Tool","sourceTags":["Windows","Tools"],"description":"Its output may also be used in VRChat."});
        let unrelated = infer_product_category(&source);
        assert_eq!(unrelated["category"], "NonVRChat");
        assert_eq!(unrelated["scope"], "other");
        source["tags"] = json!(["VRChat"]);
        let explicit = infer_product_category(&source);
        assert_eq!(explicit["category"], "Tool");
        assert_eq!(explicit["scope"], "vrchat");
        source["tags"] = json!([]);
        source["description"] = json!("Import this Unity editor extension.");
        let unity = infer_product_category(&source);
        assert_eq!(unity["category"], "Tool");
        assert_eq!(unity["scope"], "unity");
    }

    #[test]
    fn avatar_labels_clean_only_exact_names_and_keep_unknown_identity() {
        for text in [
            "オリジナル3Dモデル「真冬」Ver.1.02",
            "真冬 Mafuyu / オリジナル3Dモデル",
            "真冬 / Mafuyu",
            "【オリジナル3Dモデル】真冬",
        ] {
            assert_eq!(clean_avatar_name(text), "真冬");
            assert_eq!(canonical_tag(text), "booth:5007531");
        }
        assert_eq!(
            clean_avatar_name("シフォン -Chiffon- / オリジナル3Dモデル"),
            "シフォン"
        );
        assert_ne!(canonical_tag("真冬用コート"), "booth:5007531");
        assert_ne!(canonical_tag("Mafuyu / coat"), "booth:5007531");
        assert_ne!(canonical_tag("真冬 / マヌカ"), "booth:5007531");
        assert_eq!(romanized_name("シロネコ"), "Shironeko");
        assert_eq!(romanized_name("きゃっと"), "Kyatto");
        #[cfg(target_os = "windows")]
        assert_eq!(romanized_name("未確認漢字"), "Mikakuninkanji");
        #[cfg(not(target_os = "windows"))]
        assert_eq!(romanized_name("未確認漢字"), "");
        assert_eq!(
            avatar_display_name(&json!({"officialName":"Example","englishName":"Example"})),
            "Example / Example"
        );
        let options = owned_base_options(&[
            json!({"id":"unknown-id","category":"Base","sourceTitle":"オリジナル3Dモデル「真冬」"}),
        ]);
        assert_eq!(options[0]["id"], "product:unknown-id");
        assert_eq!(options[0]["displayName"], "真冬 / Mafuyu");
        assert_eq!(options[0]["englishNameStatus"], "romanizationFallback");
        assert_eq!(options[0]["entitlementEstablished"], false);
        let mut statuses = BTreeMap::new();
        for avatar in avatars() {
            assert!(!avatar.value["englishName"].as_str().unwrap().is_empty());
            *statuses
                .entry(avatar.value["englishNameStatus"].as_str().unwrap())
                .or_insert(0) += 1;
        }
        assert_eq!(statuses["official"], 28);
        assert_eq!(statuses["romanizationFallback"], 3);
        assert_eq!(statuses["userAccepted"], 1);
    }

    #[test]
    fn base_supplement_uses_identity_not_similar_title() {
        let options = owned_base_options(&[
            json!({"id":"5007531","category":"Base","title":"custom rename"}),
            json!({"id":"5007531","category":"Base","title":"same ID"}),
            json!({"id":"synthetic-other","category":"Base","sourceTitle":"真冬","title":"renamed"}),
            json!({"id":"synthetic-clothes","category":"Clothes","title":"真冬"}),
        ]);
        assert_eq!(options.len(), 2);
        assert_eq!(options[0]["officialName"], "真冬");
        assert_eq!(options[1]["id"], "product:synthetic-other");
        assert!(options
            .iter()
            .all(|option| option["entitlementEstablished"] == false));
    }

    #[test]
    fn bundled_identities_are_unique_and_have_public_provenance() {
        let mut ids = BTreeSet::new();
        for avatar in avatars() {
            assert!(ids.insert(avatar.value["id"].as_str().unwrap()));
            assert!(avatar.value["productId"]
                .as_str()
                .unwrap()
                .chars()
                .all(|c| c.is_ascii_digit()));
            let url = avatar.value["sourceUrl"].as_str().unwrap();
            assert!(url.starts_with("https://") && url.contains("booth.pm/") && !url.contains('?'));
            assert!(!avatar.names.is_empty());
        }
    }

    #[test]
    fn independently_nominated_baseline_remains_bounded() {
        let evaluation: Value = serde_json::from_str(include_str!("evaluation.json")).unwrap();
        let rows = evaluation["identities"].as_array().unwrap();
        let matched = rows
            .iter()
            .filter(|row| {
                match_avatars(row["name"].as_str().unwrap())
                    .iter()
                    .any(|candidate| candidate["productId"] == row["productId"])
            })
            .count();
        assert_eq!(rows.len(), 10);
        assert_eq!(matched, 8);
        assert!(dictionary()["coverage"]["populationCoverage"].is_null());
    }
}
