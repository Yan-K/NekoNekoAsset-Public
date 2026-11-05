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

pub const VERSION: &str = "2026-11-03.15";
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
    let gumroad = ["provider", "store"]
        .iter()
        .any(|key| normalize(text(key)) == "gumroad");
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
    let explicit_vr_text = has_vr(&title_normalized) || has_vr(&description_normalized);
    let vr_scope = jinxxy || (!gumroad && explicit_vr_tag) || explicit_vr_text;
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
    let target_scope = vr_scope || unity_scope;
    // BOOTH's top level says whether a product is 3D data at all.
    let booth = ["provider", "store"]
        .iter()
        .any(|key| normalize(text(key)) == "booth");
    let parent_name = normalize(source_category["parentName"].as_str().unwrap_or(""));
    let in_3d = parent_name == "3dモデル";
    // Bare アニメーション also describes videos, GIFs and particle effects, and a store
    // category by that name is BOOTH's video shelf. Outside 3D categories it counts only
    // in VRChat or Unity context with none of those words beside it.
    let video_or_effect = std::iter::once(&title_normalized)
        .chain(&normalized_tags)
        .any(|text| mentions_video_or_effect(text));
    let evidence = |text: &str, rule: &Rule| match rule.category {
        "Motion" if booth => {
            names_motion(text)
                || bare_animation(text) && (in_3d || target_scope && !video_or_effect)
        }
        // 世界 is as often 世界観 or 異世界, and effect sellers tag every platform.
        "World" if booth => {
            (in_3d || !video_or_effect) && rule_match(&text.replace("世界", ""), rule)
        }
        _ => rule_match(text, rule),
    };
    let counts = |text: &str| {
        !RULES.iter().any(|rule| rule_match(text, rule))
            || RULES.iter().any(|rule| evidence(text, rule))
    };
    let type_tags: Vec<String> = tags
        .iter()
        .zip(&normalized_tags)
        .filter(|(_, tag)| counts(tag))
        .map(|(tag, _)| tag.clone())
        .collect();
    let mut result = infer_category(
        if counts(&category_name) {
            source_category
        } else {
            &Value::Null
        },
        &type_tags,
    );
    let store = booth
        .then(|| store_type(&category_name, &title_normalized, &normalized_tags))
        .flatten();
    if let Some((kind, basis)) = store {
        let found: Vec<Value> = match basis {
            "sourceTitle" => vec![json!({"field":"sourceTitle","value":title})],
            "sourceTags" => tags
                .iter()
                .zip(&normalized_tags)
                .filter(|(_, tag)| names_type(tag, kind))
                .map(|(tag, _)| json!({"field":"sourceTags","value":tag}))
                .collect(),
            _ => vec![json!({"field":"sourceCategory.name","value":source_category.as_str().or_else(|| source_category["name"].as_str()).unwrap_or("")})],
        };
        result = json!({"category":kind,"basis":basis,"inferred":true,"evidence":found});
    }
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
    // In a garment, hair, accessory or texture category an add-on is one more pack,
    // and a software tag one more tag.
    let store_content = matches!(
        store,
        Some(("Clothes" | "Hair" | "Accessory" | "Texture", _))
    );
    let software_decides = if store_content {
        software_opening || software_title && !add_on_pack(&title_normalized)
    } else {
        software
            && (declared_software
                || matches!(
                    result["category"].as_str(),
                    Some("Base" | "Texture" | "NonVRChat" | "Tool")
                ))
    };
    if software_decides && !specialized_software {
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
    // BOOTH's own character category outranks tags, which also list what an avatar
    // includes: a hair style, gestures, a shader. With no specific store category,
    // a title that calls itself one named model is enough; a removed page is not.
    if result["category"] != "Base" && !declared_software && !jinxxy_addon {
        if category_name == "3dキャラクター" {
            result = json!({"category":"Base","basis":"sourceCategory","inferred":true,
                "evidence":[{"field":"sourceCategory.name","value":source_category.as_str().or_else(|| source_category["name"].as_str()).unwrap_or("")}]});
        } else if matches!(category_name.as_str(), "" | "3dモデル(その他)")
            && names_one_model(title, &title_normalized)
        {
            result = json!({"category":"Base","basis":"sourceTitle","inferred":true,
                "evidence":[{"field":"sourceTitle","value":evidence_excerpt(title)}]});
        }
    }
    // A generic material/texture label must not defeat an explicit shader or
    // motion tag; a store category that says what the product is still wins.
    if store.is_none() && matches!(result["category"].as_str(), Some("Texture" | "NonVRChat")) {
        if let Some(rule) = RULES
            .iter()
            .filter(|r| matches!(r.category, "Shader" | "Motion"))
            .find(|r| normalized_tags.iter().any(|tag| evidence(tag, r)))
        {
            result = json!({"category":rule.category,"basis":"tags","inferred":true,"evidence":tags.iter().zip(&normalized_tags).filter(|(_, tag)| evidence(tag, rule)).take(8).map(|(raw, _)| json!({"field":"sourceTags","value":evidence_excerpt(raw)})).collect::<Vec<_>>()});
        }
    }
    // A description mentions dependencies, contents and what a product goes with. On
    // BOOTH, where tags and a category are there to read, it must define a shader; a
    // software definition was read above. It remains the evidence other stores have.
    if result["basis"] == "fallback" {
        for (field, content) in [
            ("sourceTitle", title_normalized.as_str()),
            ("sourceDescription", description_normalized.as_str()),
        ] {
            if let Some(rule) = RULES.iter().find(|rule| {
                rule.category != "NonVRChat"
                    && (rule.category != "Base" || field == "sourceTitle" && base_title)
                    && evidence(content, rule)
                    && (field == "sourceTitle" || target_scope)
                    && (field != "sourceDescription" || !booth || rule.category == "Shader")
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
    let store_texture = result["category"] == "Texture" && matches!(store, Some(("Texture", _)));
    let os_without_target_context = os_tag && !unity_scope && !explicit_vr_tag;
    if restricted
        && !jinxxy
        && !store_texture
        && (!target_scope || outside_app_context && !explicit_vr_tag || os_without_target_context)
    {
        result = json!({"category":"NonVRChat","basis":"scopeGate","inferred":true,"evidence":result["evidence"]});
    }
    // Under any other top level, such as 素材データ, ゲーム or 映像作品, a word needs
    // VRChat or Unity evidence too.
    if booth
        && !parent_name.is_empty()
        && !in_3d
        && !target_scope
        && result["category"] != "NonVRChat"
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
    // Gumroad is a general-purpose store. Generic type/engine tags are not
    // evidence of a VRChat product without an explicit source title/body mention.
    let gumroad_without_vr_text = gumroad && !explicit_vr_text;
    if gumroad_without_vr_text {
        result = json!({"category":"NonVRChat","basis":"scopeGate","inferred":true,"evidence":[]});
    }
    // BOOTH's own 3D category stands against a Photoshop or Windows tag, which often
    // says what the source files need. Tools have to show where they run.
    let store_asset = store.is_some_and(|(kind, _)| kind != "Tool" && result["category"] == kind);
    result["scope"] = json!(if gumroad_without_vr_text {
        "other"
    } else if jinxxy {
        "vrchat"
    } else if !store_asset
        && ((outside_app_context || outside_category) && !explicit_vr_tag
            || os_without_target_context)
    {
        "other"
    } else if vr_scope {
        "vrchat"
    } else if unity_scope {
        "unity"
    } else if os_tag && !store_asset {
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

fn names_type(text: &str, category: &str) -> bool {
    RULES
        .iter()
        .any(|rule| rule.category == category && rule_match(text, rule))
}

/// What BOOTH's own category says a product is, with where that came from. Tags also
/// list what a product contains or suits, so they do not outvote it. BOOTH has no
/// category for shaders, Udon or gimmicks: sellers file those under tools or worlds and
/// say so in the title, and in tools their tags can tell them apart too.
fn store_type(
    leaf: &str,
    title: &str,
    tags: &[String],
) -> Option<(&'static str, &'static str)> {
    let stated = match leaf {
        "3d衣装" | "3d靴" => "Clothes",
        "3d髪型" => "Hair",
        "3d装飾品" | "3d小道具" => "Accessory",
        "3dモーション・アニメーション" => "Motion",
        "3dテクスチャ" => "Texture",
        "3dツール・システム" => "Tool",
        "3d環境・ワールド" => "World",
        _ if title.contains("販売ワールド") => return Some(("World", "sourceTitle")),
        _ => return None,
    };
    if !matches!(stated, "Tool" | "World") || title.contains("販売ワールド") {
        return Some((stated, "sourceCategory"));
    }
    let tagged = |category| tags.iter().any(|tag| names_type(tag, category));
    // Neither an installer that comes with a product nor a stool makes it a tool.
    let title = title.replace("導入ツール", "").replace("スツール", "");
    let sold = |words: &[&str]| words.iter().any(|word| sold_as(&title, word));
    let tool = sold(&["ツール", "工具", "도구"])
        || ["tool", "tools", "utility", "utilities", "editor extension"]
            .iter()
            .any(|word| word_match(&title, word));
    let gimmick = sold(&["ギミック", "gimmick", "기믹", "交互组件", "互動元件"]);
    let for_worlds = ["ワールド用", "ワールド向け", "for world", "for vrchat world", "for vrc world"]
        .iter()
        .any(|phrase| title.contains(phrase));
    let named = if tool {
        Some("Tool")
    } else if sold(&["udon"]) {
        Some("Udon")
    } else if sold(&["シェーダ", "shader", "셰이더", "着色器", "著色器"]) {
        Some("Shader")
    } else if tagged("Udon")
        && (gimmick
            || for_worlds
            || title.contains("システム")
            || word_match(&title, "system"))
    {
        Some("Udon")
    } else if gimmick {
        Some("Gimmick")
    } else {
        None
    };
    if let Some(kind) = named {
        return Some((kind, "sourceTitle"));
    }
    if stated == "Tool" && !tagged("Tool") && !tags.iter().any(|tag| software_type(tag)) {
        if let Some(kind) = ["Udon", "Shader", "Gimmick"].into_iter().find(|kind| tagged(kind)) {
            return Some((kind, "sourceTags"));
        }
    }
    Some((stated, "sourceCategory"))
}

/// A word for what is sold, not for something the product comes with or suits.
fn sold_as(title: &str, word: &str) -> bool {
    title.match_indices(word).any(|(start, word)| {
        let after = title[start + word.len()..].trim_start_matches('ー');
        let after = after.strip_prefix("ギミック").unwrap_or(after);
        ![
            "付", "つき", "搭載", "あり", "有り", "多数", "満載", "入り", "対応", "専用", "用", "向け",
        ]
        .iter()
        .any(|suffix| after.starts_with(suffix))
    })
}

/// Add-on wording that sells one more pack rather than naming an application.
fn add_on_pack(title: &str) -> bool {
    let rest = ["add-on", "addon", "add on", "アドオン", "애드온"]
        .iter()
        .fold(title.to_owned(), |rest, word| rest.replace(word, ""));
    !software_type(&rest) && !title.contains("blender")
}

/// Words that name motion data. モーショングラフィックス is video work.
fn names_motion(text: &str) -> bool {
    let text = text
        .replace("モーショングラフィック", "")
        .replace("motion graphic", "");
    [
        "モーション",
        "모션",
        "エモート",
        "ポーズ",
        "动作",
        "アニメーションファイル",
        "アニメーションクリップ",
    ]
    .iter()
    .any(|word| text.contains(word))
        // 動作 usually says that something works.
        || text == "動作"
        || [
            "motion",
            "motions",
            "gesture",
            "gestures",
            "pose",
            "poses",
            "emote",
            "emotes",
            "animationclip",
            "animation clip",
        ]
        .iter()
        .any(|word| word_match(&text, word))
}

fn bare_animation(text: &str) -> bool {
    text.contains("アニメーション")
        || text.contains("애니메이션")
        || word_match(text, "animation")
        || word_match(text, "animations")
}

fn mentions_video_or_effect(text: &str) -> bool {
    ["エフェクト", "パーティクル", "動画", "映像", "動く背景"]
        .iter()
        .any(|word| text.contains(word))
        || ["effect", "effects", "particle", "particles", "vfx", "gif", "vj"]
            .iter()
            .any(|word| word_match(text, word))
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

/// How sellers call one 3D model. Compared with spaces removed, because they also
/// write オリジナル 3Dモデル and VRChat対応 3Dモデル.
fn model_wording() -> impl Iterator<Item = String> {
    let platform = ["vrchat", "vrc"].into_iter().flat_map(|platform| {
        ["向け", "用", "対応"].into_iter().flat_map(move |purpose| {
            ["3dモデル", "モデル", "アバター"]
                .into_iter()
                .map(move |kind| format!("{platform}{purpose}{kind}"))
        })
    });
    [
        "オリジナル3dモデル".to_owned(),
        "original3dmodel".to_owned(),
    ]
    .into_iter()
    .chain(platform)
}

/// The wording must introduce the name or directly follow a quoted one, because a
/// garment can end with the same phrase. Product wording after + or & can be a bonus
/// that comes with the model, so there only the name parser decides.
fn names_one_model(title: &str, normalized: &str) -> bool {
    let compact: String = normalized.split_whitespace().collect();
    let product = normalized.split(['+', '&']).next().unwrap_or(normalized);
    model_wording().any(|word| {
        compact.match_indices(&word).any(|(start, _)| {
            let after = &compact[start + word.len()..];
            // "original 3d modeler" is not the phrase.
            !(word.is_ascii() && after.starts_with(|c: char| c.is_ascii_alphanumeric()))
                && !names::for_target(after)
                && (after.chars().any(char::is_alphanumeric)
                    || compact[..start]
                        .trim_end_matches('【')
                        .ends_with(['」', '』', '”', '"', '〛']))
        })
    }) && !compatibility_context(normalized)
        && !names::addon(product)
        && !RULES
            .iter()
            .any(|rule| rule.specific && rule_match(product, rule))
        && names::product_base_parts(title).name_kind != "fallback"
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

/// Compact Base-label presentation. Kind describes parsing certainty, never
/// product identity, entitlement or automatic enablement.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AvatarLabelParts {
    pub original: String,
    pub source_latin: Option<String>,
    pub qualifiers: Vec<String>,
    pub name_kind: &'static str,
}

pub fn parsed_avatar_label(title: &str) -> AvatarLabelParts {
    names::structured_base_parts(title)
}

/// A special edition or another listing of one avatar leaves its plain listing's
/// name unambiguous.
pub fn avatar_edition(entry: &Value) -> bool {
    entry["variantListing"] == true
        || entry["labelQualifiers"].as_array().is_some_and(|values| !values.is_empty())
}

/// The name located in a known Base product's title, plus bounded presentation
/// corrections tied to the public product identity. Similar titles and user
/// renames never inherit another product's name.
pub fn parsed_product_avatar_label(product_id: &str, title: &str) -> AvatarLabelParts {
    let mut parsed = names::product_base_parts(title);
    let corrected = match product_id.strip_prefix("booth:").unwrap_or(product_id) {
        // https://booth.pm/ja/items/8547624 — name stem only; Latin unverified.
        "8547624" if title.contains("メイネーラ") => Some("メイネーラ"),
        // https://booth.pm/en/items/4309253 — franchise prefix is not the name.
        "4309253" if title.contains("棗") && title.contains("イロハ") => Some("棗 イロハ"),
        _ => None,
    };
    if let Some(original) = corrected {
        parsed.original = original.to_owned();
        parsed.source_latin = None;
        parsed.name_kind = "single";
    }
    parsed
}

/// Conservative legacy label parsing used by exact identity/alias paths.
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
    let mut display = if english.is_empty() || normalize(original) == normalize(english) {
        original.to_owned()
    } else {
        format!("{original} / {english}")
    };
    let qualifiers: Vec<_> = avatar["labelQualifiers"].as_array().into_iter().flatten()
        .filter_map(Value::as_str).filter(|value| !value.is_empty()).collect();
    if !qualifiers.is_empty() {
        display.push_str(&format!(" ({})", qualifiers.join(", ")));
    }
    display
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
        let known = avatars().iter().find(|avatar| avatar.value["productId"].as_str() == Some(id));
        let source_title = product["sourceTitle"].as_str().filter(|name| !name.trim().is_empty())
            .or_else(|| product["title"].as_str().filter(|name| !name.trim().is_empty()));
        let title = source_title.unwrap_or(id);
        let mut entry = known.map(|avatar| avatar.value.clone()).unwrap_or_else(|| json!({
            "id":format!("product:{id}"), "productId":id, "officialName":title,
            "aliases":[], "provenance":{"kind":"catalogueBaseProduct", "productId":id},
        }));
        if known.is_none() {
            let parsed = if source_title.is_some() { parsed_product_avatar_label(id, title) } else {
                AvatarLabelParts { original: id.to_owned(), source_latin: None, qualifiers: vec![], name_kind: "fallback" }
            };
            let english = if parsed.name_kind == "single" {
                parsed.source_latin.clone().unwrap_or_else(|| romanized_name(&parsed.original))
            } else {
                String::new()
            };
            entry["officialName"] = json!(parsed.original);
            entry["nameKind"] = json!(parsed.name_kind);
            entry["labelQualifiers"] = json!(parsed.qualifiers);
            entry["variantListing"] = json!(names::variant_listing(title, &parsed.original));
            entry["englishNameStatus"] = json!(if english.is_empty() {
                "unresolvedReading"
            } else if parsed.source_latin.is_some() {
                "sourceTitleSpelling"
            } else {
                "romanizationFallback"
            });
            entry["englishName"] = json!(english);
            entry["englishNameEvidence"] = json!({"kind":if parsed.source_latin.is_some(){"sourceTitleSpelling"}else{"presentationFallback"},"identityEstablished":false});
        } else {
            entry["nameKind"] = json!("single");
            entry["labelQualifiers"] = json!([]);
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
    fn gumroad_requires_explicit_vrchat_in_source_title_or_description() {
        for (category, title, description) in [
            ("Shader", "Authored paint shader for Blender", "A rendering material."),
            ("World", "Authored Unreal classroom", "An environment tutorial."),
            ("Clothes", "Authored clothing mesh", "For Unity and other engines."),
            ("Base", "Original 3D Avatar Soraneko", "A generic 3D character."),
            ("Udon", "Authored script", "Includes tools and VR controller models."),
            ("Shader", "Authored shader", "VRChatters and avrcollision are unrelated words."),
        ] {
            let source = json!({"store":"Gumroad","sourceTitle":title,
                "sourceDescription":description,"sourceCategory":{"name":category,"parentName":"VRChat"},
                "sourceTags":["VRChat","VRC","Unity","Avatar","Shader","Udon"]});
            let unchanged = source.clone();
            let inference = infer_product_category(&source);
            assert_eq!(inference["category"], "NonVRChat", "{title}");
            assert_eq!(inference["scope"], "other", "{title}");
            assert_eq!(inference["scopeEvidence"]["vrchat"], false);
            assert_eq!(source, unchanged);
        }
        for (title, description, expected) in [
            ("[VRC] Authored Hair", "", "Hair"),
            ("Authored Shader", "Works with VRChat avatars.", "Shader"),
            ("ＶＲＣ対応 Authored Hair", "", "Hair"),
            ("Authored Hair", "Designed for vRc.", "Hair"),
            ("Authored Hair for VRチャット", "", "Hair"),
            ("Authored Hair (VRCFury)", "", "Hair"),
        ] {
            let inference = infer_product_category(&json!({"provider":"gumroad",
                "sourceTitle":title,"sourceDescription":description}));
            assert_eq!(inference["category"], expected, "{title}");
            assert_eq!(inference["scope"], "vrchat", "{title}");
        }
        let renamed = infer_product_category(&json!({"store":"Gumroad",
            "sourceTitle":"Authored Hair","title":"Human VRChat nickname"}));
        assert_eq!(renamed["category"], "NonVRChat");
    }

    #[test]
    fn gumroad_text_gate_does_not_change_booth_or_jinxxy() {
        for store in ["BOOTH", "Jinxxy"] {
            let inference = infer_product_category(&json!({"store":store,
                "sourceTitle":"Authored Shader","sourceTags":["VRChat","Shader"]}));
            assert_eq!(inference["category"], "Shader", "{store}");
            assert_eq!(inference["scope"], "vrchat", "{store}");
        }
    }

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
        let unnamed = super::owned_base_options(&[serde_json::json!({"id":"synthetic-unnamed","category":"Base","sourceTitle":"Original 3D Avatar"})]);
        assert_eq!(unnamed.len(), 1);
        assert_eq!(unnamed[0]["id"], "product:synthetic-unnamed");
        assert_eq!(unnamed[0]["nameKind"], "fallback");
        assert_eq!(unnamed[0]["englishName"], "");
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
    fn booth_character_category_outranks_tags_unless_software_is_declared() {
        // Authored structural equivalents; the human chose the category alone,
        // so a gimmick or pet filed there follows it too.
        for (title, tag) in [
            ("星猫オリジナル3Dモデル「ソラネコ」-soraneko-", "髪型"),
            (
                "オリジナル3Dアバター『星空ソラネコ』 #ソラネコ3D",
                "ギミック",
            ),
            (
                "【VRChat 】オリジナルコラボ3Dアバター SORA_NEKO",
                "アニメーション",
            ),
            (
                "ソラネコ オリジナル3Dモデル -VRChatアバター & VRM & 追加衣装",
                "衣装",
            ),
            (
                "オリジナル3Ｄモデル「ソラネコ」Soraneko v1.17",
                "lens texture",
            ),
            ("月猫オリジナル3Dモデル「ソラネコ」", "シェーダー"),
            ("ペットウサギ「ソラネコ」", "vrc想定アクセサリー"),
            ("Authored Open Car", "アバターギミック"),
            ("Authored×Summer", "イラスト"),
        ] {
            for category in [json!({"name":"3Dキャラクター"}), json!("３Ｄキャラクター")]
            {
                let result = infer_product_category(
                    &json!({"sourceCategory":category,"sourceTitle":title,"sourceTags":[tag]}),
                );
                assert_eq!(result["category"], "Base", "{title}");
                assert_eq!(result["basis"], "sourceCategory", "{title}");
            }
        }
        // A software tag alone is one more tag.
        assert_eq!(
            infer_product_category(
                &json!({"sourceCategory":{"name":"3Dキャラクター"},"sourceTitle":"Authored","sourceTags":["プラグイン"]})
            )["category"],
            "Base"
        );
        for source in [
            json!({"sourceCategory":{"name":"3Dキャラクター"},"sourceTitle":"「ソラネコ」用R18体Body Add-on v1.1","sourceTags":["VRChat","アバター"]}),
            json!({"sourceCategory":{"name":"3Dキャラクター"},"sourceTitle":"Authored","sourceTags":["VRChat"],"sourceDescription":"Unity editor tool for avatars."}),
        ] {
            assert_eq!(
                infer_product_category(&source)["category"],
                "Tool",
                "{source}"
            );
        }
        // Only this exact store category: not a parent, and not Jinxxy's own names.
        for (source, expected) in [
            (
                json!({"sourceCategory":{"name":"その他","parentName":"3Dキャラクター"},"sourceTitle":"Authored","sourceTags":["髪型"]}),
                "Hair",
            ),
            (
                json!({"sourceCategory":{"name":"avatars"},"sourceTitle":"Authored","sourceTags":["hair"]}),
                "Hair",
            ),
            (
                json!({"sourceCategory":{"name":"3D衣装"},"sourceTitle":"Authored","sourceTags":["アバター"]}),
                "Clothes",
            ),
        ] {
            assert_eq!(
                infer_product_category(&source)["category"],
                expected,
                "{source}"
            );
        }
        // Managed paths freeze this lower layer's answer, so it must not move.
        assert_eq!(
            infer_category(&json!({"name":"3Dキャラクター"}), &["髪型".into()])["category"],
            "Hair"
        );
        // Gumroad's explicit VRChat text requirement still has the last word.
        assert_eq!(
            infer_product_category(
                &json!({"provider":"gumroad","sourceCategory":{"name":"3Dキャラクター"},"sourceTitle":"Authored"})
            )["category"],
            "NonVRChat"
        );
    }

    #[test]
    fn title_alone_makes_a_base_only_when_its_wording_places_one_model_name() {
        let without_store_category = |title: &str, extra: &Value| {
            let mut source = json!({"sourceTitle":title});
            for (key, value) in extra.as_object().unwrap() {
                source[key] = value.clone();
            }
            infer_product_category(&source)
        };
        let generic = json!({"sourceCategory":{"name":"3Dモデル（その他）"}});
        for title in [
            "オリジナル3Dモデル「ソラネコ」",
            "オリジナル3Dモデル 『Soraneko -ソラネコ-』",
            "オリジナル 3Dモデル『ソラネコ』",
            "『ソラネコ』オリジナル3Dモデル",
            "AuthoredCats vol.2『ソラネコ』オリジナル3Dモデル",
            "【非公開】【オリジナル3Dモデル】Soraneko-ソラネコ-",
            "【オリジナル3Dモデル】ソラネコ(フルセット)",
            "◆オリジナル3Dモデル◆ソラネコ ver1.0 (通常版)",
            "Original 3D Model ［soraneko -ソラネコ-］",
            "※11/30非公開予定 オリジナル3Dモデル「Soraneko〈ソラネコ〉」",
            "非公開済＠オリジナル3Dモデル「Soraneko〈ソラネコ〉」（Quest版付属）",
            "【ARCHIVE】【VRChat想定 オリジナル3Dモデル 『ソラネコ Soraneko 』フルセット版",
            "【VRChat対応 オリジナル3Dモデル 『ソラネコ -フルセット版-』v1.1＆V2.0】",
            "オリジナル3Dモデル『ソラネコ』+追加衣装:Authored",
            "VRChat対応オリジナル3Dモデル「ソラネコ」",
            "VRChat用3Dモデル「ソラネコ(そらねこ)」",
            "VRChat向けモデル【ソラネコ Ver1.1】",
            "VRC向け3Dモデル【ソラネコ -Soraneko- 】V2",
            "OculusQuest VRChat対応 3Dモデル 『ソラネコ -フルセット版-』v1.91",
            "[VRChat向けアバター]ソラネコ・ノワール",
            "[VRChat向けアバター] SORANEKO - 空猫",
        ] {
            for extra in [&json!({}), &generic] {
                let result = without_store_category(title, extra);
                assert_eq!(result["category"], "Base", "{title} {extra}");
                assert_eq!(result["basis"], "sourceTitle", "{title} {extra}");
            }
        }
        // Tags and descriptions, which often list contents, do not outrank the title.
        assert_eq!(
            without_store_category(
                "オリジナル3Dモデル「ソラネコ」",
                &json!({"sourceCategory":{"name":"3Dモデル（その他）"},"sourceTags":["シェーダー"],"sourceDescription":"VRChat 向け。衣装とテクスチャを含みます。"})
            )["category"],
            "Base"
        );
        for title in [
            // The phrase can end a garment's title, or sit beside a garment's name.
            "【version1.1】ふわふわワンピース【オリジナル3Dモデル】",
            "『ソラT01』Ver2.0【オリジナル3Dモデル】アバター追加衣装",
            "【オリジナル3Dモデル】ソラネコ 素体用追加テクスチャ",
            "オリジナル3Dモデル「ソラネコ」専用コート",
            "オリジナル3Dモデル「ソラネコ」用ヘアスタイル",
            "ソラネコ対応オリジナル3Dモデル",
            "オリジナル3Dモデル「ソラネコ」 Add-on",
            "オリジナル3Dモデル「ソラネコ」用ワンピース",
            "オリジナル3Dモデル ソラネコ&ウミネコ専用衣装",
            "オリジナル3Dモデル用メガネ",
            // A bare 3Dモデル also labels props.
            "【3Dモデル】洗面器",
            "[３Dモデル]単三電池 [VRC]",
            "【VRChat向け】オルゴール 【3Dモデル】",
            "VRChat向けスプーン",
            // Platform wording followed by another product type.
            "VRChat向けアバターギミック",
            "VRChat向けアバター用ヘアピン",
            "【VRC想定アバターLOADING時に】アニメーションファイル",
            // No name, or not this phrase.
            "オリジナル3Dモデル",
            "オリジナル3Dモデル ver1.0",
            "Original 3D Modeler",
            "Original 3D Modeler Soraneko",
            "「ソラネコ」「ウミネコ」オリジナル3Dモデル",
        ] {
            for extra in [&json!({}), &generic] {
                assert_ne!(
                    without_store_category(title, extra)["category"],
                    "Base",
                    "{title} {extra}"
                );
            }
        }
        // A specific store category wins, and a removed page itself proves nothing.
        for (leaf, expected) in [
            ("3D衣装", "Clothes"),
            ("3D小道具", "Accessory"),
            ("3Dテクスチャ", "Texture"),
        ] {
            let result = without_store_category(
                "オリジナル3Dモデル「ソラネコ」",
                &json!({"sourceCategory":{"name":leaf},"sourceTags":["VRChat"]}),
            );
            assert_eq!(result["category"], expected, "{leaf}");
        }
        assert_eq!(
            without_store_category(
                "Authored Soraneko",
                &json!({"publicPageAvailability":"unavailable"})
            )["category"],
            "NonVRChat"
        );
        // Store-specific gates keep their last word.
        let gumroad = json!({"provider":"gumroad"});
        assert_eq!(
            without_store_category("オリジナル3Dモデル「ソラネコ」", &gumroad)["category"],
            "NonVRChat"
        );
        assert_eq!(
            without_store_category(
                "オリジナル3Dモデル「ソラネコ」",
                &json!({"provider":"gumroad","sourceDescription":"For VRChat."})
            )["category"],
            "Base"
        );
        assert_eq!(
            without_store_category(
                "Original 3D Model Soraneko",
                &json!({"provider":"jinxxy","sourceCategory":{"name":"avatars","types":["avatar_addon"]}})
            )["category"],
            "Gimmick"
        );
        assert_eq!(
            without_store_category(
                "Original 3D Model Soraneko",
                &json!({"provider":"jinxxy","sourceCategory":{"name":"","types":["avatar_addon"]}})
            )["category"],
            "Gimmick"
        );
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

    fn booth_listing(parent: &str, leaf: &str, title: &str, tags: &[&str]) -> Value {
        json!({"store":"BOOTH","sourceCategory":{"name":leaf,"parentName":parent},"sourceTitle":title,"sourceTags":tags})
    }

    #[test]
    fn booth_3d_categories_outrank_tags() {
        // Independently authored structures of what sellers file in each category.
        for (leaf, title, tags, expected) in [
            ("3D環境・ワールド", "【販売ワールド】Authored Hotel", &["VRChat", "Udon", "シェーダー", "ギミック"][..], "World"),
            ("3D環境・ワールド", "【販売ワールド】Authored Bar ギミック シェーダー", &["VRChat", "Udon"], "World"),
            ("3D環境・ワールド", "Authored Room", &["VRChat", "Udon", "UdonSharp"], "World"),
            ("3D環境・ワールド", "Authored Room", &["シェーダー", "小物"], "World"),
            ("3D環境・ワールド", "ギミック付き Authored Room", &["VRChat", "Udon"], "World"),
            ("3D環境・ワールド", "Authored Room ギミック有り", &["VRChat"], "World"),
            ("3D環境・ワールド", "Authored Room（Udonギミック付き）", &["VRChat"], "World"),
            ("3D環境・ワールド", "Udon対応 Authored Room", &["Udon"], "World"),
            ("3D環境・ワールド", "モダン Authored スツール", &[], "World"),
            ("3D環境・ワールド", "Authored Solar System", &["VRChat"], "World"),
            ("3D環境・ワールド", "Authored 雨粒シェーダー", &["VRChat"], "Shader"),
            ("3D環境・ワールド", "AuthoredWaterShader", &["VRChat"], "Shader"),
            ("3D環境・ワールド", "【VRC Udonギミック】Authored Darts", &["VRChat"], "Udon"),
            ("3D環境・ワールド", "Authored Slot【UdonChips】", &["VRChat"], "Udon"),
            ("3D環境・ワールド", "【VRChatワールドギミック】Authored Camera", &["VRChat", "Udon"], "Udon"),
            ("3D環境・ワールド", "[VRCワールド] AUTHORED CAR SYSTEM", &["udon"], "Udon"),
            ("3D環境・ワールド", "Authored Lift for VRChat Worlds", &["Udon"], "Udon"),
            ("3D環境・ワールド", "【ワールド用】Authored 表示ギミック", &["VRChat"], "Gimmick"),
            ("3D環境・ワールド", "ワールド制作支援ツール", &["VRChat"], "Tool"),
            ("3Dツール・システム", "Authored 影シェーダー【導入ツール同梱】", &["VRChat"], "Shader"),
            ("3Dツール・システム", "シェーダー変換ツール", &["VRChat"], "Tool"),
            ("3Dツール・システム", "Authored 音ギミック", &["VRChat", "シェーダー"], "Gimmick"),
            ("3Dツール・システム", "AuthoredLens", &["VRChat", "VRChatギミック"], "Gimmick"),
            ("3Dツール・システム", "AuthoredMat", &["VRChat", "シェーダー", "VRChatギミック"], "Shader"),
            ("3Dツール・システム", "AuthoredBed", &["VRChat", "Udon", "ギミック"], "Udon"),
            ("3Dツール・システム", "AuthoredTool", &["VRChat", "ツール", "ギミック"], "Tool"),
            ("3Dツール・システム", "AuthoredKit", &["VRChat", "エディタ拡張", "ギミック"], "Tool"),
            ("3Dツール・システム", "Authored Fitter", &["VRChat", "衣装", "テクスチャ"], "Tool"),
            ("3Dモーション・アニメーション", "Authored AFK", &["VRChat", "衣装", "シェーダー"], "Motion"),
            ("3Dテクスチャ", "Authored Eye", &["VRChat", "モーション", "シェーダー"], "Texture"),
            ("3Dテクスチャ", "Authored Eye", &[], "Texture"),
            ("3Dテクスチャ", "AuthoredShader専用マテリアル", &["VRChat", "シェーダー"], "Texture"),
            ("3Dテクスチャ", "Authored Skin シェーダー", &["VRChat"], "Texture"),
            ("3Dテクスチャ", "Authored Eye Add-on", &["ソフトウェア"], "Texture"),
            ("3D衣装", "Authored Dress Add-on", &[], "Clothes"),
            ("3D衣装", "Authored Dress", &["VRChat", "ソフトウェア"], "Clothes"),
            ("3D衣装", "着せ替え エディタ拡張", &["VRChat"], "Tool"),
            ("3D靴", "Authored Boots", &["VRChat", "シェーダー"], "Clothes"),
            ("3D髪型", "Authored Bob", &["VRChat", "衣装"], "Hair"),
            ("3D装飾品", "Authored Ring", &["VRChat", "テクスチャ"], "Accessory"),
            ("3D小道具", "Authored Cup", &["VRChat", "ワールド"], "Accessory"),
            // A generic category leaves the type to the tags.
            ("3Dモデル（その他）", "Authored Pin", &["VRChat", "アクセサリー"], "Accessory"),
            ("3Dモデル（その他）", "【販売ワールド】Authored Room", &["VRChat", "Udon"], "World"),
            ("3Dモデル（その他）", "Authored Cape", &["VRChat", "世界観衣装"], "Clothes"),
            // It is 3D data already, so these need no VRChat or Unity word.
            ("3Dモデル（その他）", "Authored Walk", &["アニメーション"], "Motion"),
            ("3Dモデル（その他）", "Authored World 紹介動画あり", &[], "World"),
        ] {
            let source = booth_listing("3Dモデル", leaf, title, tags);
            assert_eq!(infer_product_category(&source)["category"], expected, "{leaf} {title}");
        }
        // Declared software in tools and motion categories still decides, as for Base.
        assert_eq!(
            infer_product_category(&booth_listing("3Dモデル", "3Dモーション・アニメーション", "Blenderアドオン Authored", &["Blender", "Unity"]))["category"],
            "Tool"
        );
        // A tool outside any VRChat or Unity context stays out of the default list.
        let result = infer_product_category(&booth_listing("3Dモデル", "3Dツール・システム", "Authored Blender Tool", &["Blender"]));
        assert_eq!(result["category"], "NonVRChat");
        assert_eq!(result["scope"], "unknown");
        // The store's own 3D category stands against a tag for what its sources need.
        for (leaf, title, tags, expected, scope) in [
            ("3D衣装", "VRChat向け Authored Dress", &["Photoshop"][..], "Clothes", "vrchat"),
            ("3D衣装", "Authored Dress", &["Photoshop", "Windows"], "Clothes", "unknown"),
            ("3Dテクスチャ", "Authored Eye", &["Photoshop"], "Texture", "unknown"),
            ("3D環境・ワールド", "Authored Room", &["Windows"], "World", "unknown"),
            // A tool has to show where it runs.
            ("3Dツール・システム", "Authored Tool", &["Unity", "AE"], "NonVRChat", "other"),
        ] {
            let result = infer_product_category(&booth_listing("3Dモデル", leaf, title, tags));
            assert_eq!(result["category"], expected, "{leaf} {title}");
            assert_eq!(result["scope"], scope, "{leaf} {title}");
        }
        // Evidence is what decided, and a listing with no category has none to cite.
        let titled = infer_product_category(&json!({"store":"BOOTH","sourceTitle":"【販売ワールド】Authored"}));
        assert_eq!(titled["category"], "World");
        assert_eq!(titled["evidence"], json!([{"field":"sourceTitle","value":"【販売ワールド】Authored"}]));
        let shader = infer_product_category(&booth_listing("3Dモデル", "3D環境・ワールド", "Authored シェーダー", &["VRChat"]));
        assert_eq!(shader["evidence"], json!([{"field":"sourceTitle","value":"Authored シェーダー"}]));
        let world = infer_product_category(&booth_listing("3Dモデル", "3D環境・ワールド", "Authored", &["Udon"]));
        assert_eq!(world["evidence"], json!([{"field":"sourceCategory.name","value":"3D環境・ワールド"}]));
    }

    #[test]
    fn stores_other_than_booth_keep_their_rules() {
        for (source, expected) in [
            (json!({"provider":"gumroad","sourceTitle":"Authored","sourceDescription":"VRChat outfit for Authored."}), "Clothes"),
            (json!({"provider":"jinxxy","sourceTitle":"Authored","sourceDescription":"A cute outfit for Authored."}), "Clothes"),
            (json!({"provider":"jinxxy","sourceTitle":"Authored World VFX"}), "World"),
            (json!({"provider":"local","sourceTitle":"Authored Walk Animation"}), "Motion"),
            (json!({"provider":"gumroad","sourceCategory":{"name":"3D環境・ワールド"},"sourceTitle":"VRChat Authored","sourceTags":["Udon"]}), "Udon"),
            (json!({"provider":"gumroad","sourceCategory":{"name":"3D環境・ワールド"},"sourceTitle":"Authored"}), "NonVRChat"),
        ] {
            assert_eq!(infer_product_category(&source)["category"], expected, "{source}");
        }
    }

    #[test]
    fn tags_outside_booth_3d_categories_need_vrchat_or_unity_evidence() {
        for (parent, leaf, title, tags, expected) in [
            ("素材データ", "背景画像", "【動く背景】Authored Room", &["アニメーション", "配信背景"][..], "NonVRChat"),
            ("映像作品", "アニメーション", "Authored 第1話 DVD", &["アニメーション"], "NonVRChat"),
            ("映像作品", "アニメーション", "Authored Blu-ray", &[], "NonVRChat"),
            ("素材データ", "素材（その他）", "Authored Loop", &["アニメーション", "モーショングラフィックス", "VJ素材"], "NonVRChat"),
            ("素材データ", "素材（その他）", "Authored プリセット", &["モーション", "プリセット"], "NonVRChat"),
            ("イラスト", "メイキング・講座", "Authored 講座", &["アニメーション"], "NonVRChat"),
            ("ファッション", "パーカー", "Authored Hoodie", &["衣装"], "NonVRChat"),
            ("アクセサリー", "ペンダント", "Authored Pendant", &[], "NonVRChat"),
            ("素材データ", "BGM素材", "Authored BGM", &["異世界"], "NonVRChat"),
            // The motion data sellers file there keeps its place.
            ("素材データ", "素材（その他）", "Authored Idle", &["VRChat", "アニメーション", "モーション素材"], "Motion"),
            ("映像作品", "アニメーション", "VRC用 Authored Motion", &["VRChat", "Emote"], "Motion"),
            ("映像作品", "アニメーション", "Authored Animation Pack", &["VRChat"], "Motion"),
            ("映像作品", "アニメーション", "Authored 表情アニメーション", &["Unity", "アニメーション"], "Motion"),
            ("ゲーム", "ゲーム関連商品", "Authored Combo", &["VRChat", "アニメーション", "ツール"], "Motion"),
            ("素材データ", "素材（その他）", "Authored Idle", &["VRChat", "エモート"], "Motion"),
            ("素材データ", "素材（その他）", "Authored Set", &["VRChat", "ポーズ集"], "Motion"),
            ("素材データ", "素材（その他）", "Authored ポーズ集", &["イラスト用"], "NonVRChat"),
            // Particle sellers tag アニメーション and every platform; none of that is the type.
            ("素材データ", "素材（その他）", "【Unity/VRChat】Authored Fire", &["アニメーション", "Unity", "パーティクル", "VRChat"], "NonVRChat"),
            ("映像作品", "アニメーション", "【Unity/VRChat】World of Authored", &["アニメーション", "Unity", "エフェクト", "vrchat world"], "NonVRChat"),
            ("映像作品", "アニメーション", "【VRChatライブ映像】Authored", &["VRChat"], "NonVRChat"),
            // Other types still come from their tags once the scope is explicit.
            ("ゲーム", "ゲーム関連商品", "Authored Lift script for VRChat Worlds", &["VRChat", "Udon"], "Udon"),
            ("ゲーム", "ゲーム関連商品", "【販売ワールド】Authored Room", &["VRChat", "Udon"], "World"),
            ("素材データ", "素材（その他）", "Authored Water", &["Unity", "シェーダー"], "Shader"),
            ("素材データ", "背景画像", "Authored Skybox", &["VRChat", "ワールド"], "World"),
            ("素材データ", "背景画像", "Authored Eye", &["VRChat", "テクスチャ"], "Texture"),
        ] {
            let source = booth_listing(parent, leaf, title, tags);
            assert_eq!(infer_product_category(&source)["category"], expected, "{leaf} {title}");
        }
        // A VRChat video is no asset type, and its VRChat word keeps it in the default list.
        let video = infer_product_category(&booth_listing("映像作品", "アニメーション", "【VRChatライブ映像】Authored", &["VRChat"]));
        assert_eq!(video["category"], "NonVRChat");
        assert_eq!(video["scope"], "vrchat");
    }

    #[test]
    fn a_removed_listing_title_names_motion_only_with_motion_words() {
        for (title, expected) in [
            ("Authored 待機モーション", "Motion"),
            ("Authored Motion Pack", "Motion"),
            ("Authored アニメーション", "NonVRChat"),
            ("VRChat用 Authored アニメーション", "Motion"),
            ("Authored モーショングラフィックス", "NonVRChat"),
            ("Authored 動作確認済み", "NonVRChat"),
        ] {
            assert_eq!(
                infer_product_category(&json!({"store":"BOOTH","sourceTitle":title,"publicPageAvailability":"unavailable"}))["category"],
                expected,
                "{title}"
            );
        }
    }

    #[test]
    fn a_description_names_a_type_only_by_defining_it() {
        for description in [
            "テクスチャと衣装を含みます。ワールドでも使えます。",
            "VRChat 向けの小物です。ヘアゴムとアクセサリーがセットです。",
            "Includes a texture, a tool and a world prefab.",
        ] {
            let result = infer_product_category(&json!({"store":"BOOTH","sourceCategory":{"name":"3Dモデル（その他）","parentName":"3Dモデル"},"sourceTitle":"Authored","sourceTags":["VRChat"],"sourceDescription":description}));
            assert_eq!(result["category"], "NonVRChat", "{description}");
            // It stays in the default list as Unclassified, because it says VRChat.
            assert_eq!(result["scope"], "vrchat", "{description}");
        }
        for (description, expected) in [
            ("Unity shader for skin.", "Shader"),
            ("Unity editor tool for avatars.", "Tool"),
        ] {
            assert_eq!(
                infer_product_category(&json!({"store":"BOOTH","sourceCategory":{"name":"3Dモデル（その他）","parentName":"3Dモデル"},"sourceTitle":"Authored","sourceTags":["VRChat"],"sourceDescription":description}))["category"],
                expected,
                "{description}"
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
        assert_eq!(romanize_kana("未確認漢字"), "");
        assert_eq!(romanize_kana("未確認漢字ねこ"), "");
        #[cfg(target_os = "windows")]
        assert_eq!(romanized_name("星空猫"), "Hoshizoraneko");
        #[cfg(not(target_os = "windows"))]
        assert_eq!(romanized_name("未確認漢字"), "");
        assert_eq!(
            avatar_display_name(&json!({"officialName":"Example","englishName":"Example"})),
            "Example"
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
    fn every_effective_base_keeps_its_identity_even_when_label_is_uncertain() {
        let products = [
            json!({"id":"authored-wrapper","category":"Base","sourceTitle":"Original 3D Avatar"}),
            json!({"id":"authored-ambiguous","category":"Base","sourceTitle":"星猫 / 海猫"}),
            json!({"id":"authored-bundle","category":"Base","sourceTitle":"【 R18 Edition】苺佳(ichica) and GLC and 蒼葉(aoba)"}),
            json!({"id":"authored-latin","category":"Base","sourceTitle":"【無料】Ren♡Ai"}),
            json!({"id":"authored-unsupported","category":"Base","sourceTitle":"そらΩ"}),
            json!({"id":"authored-empty","category":"Base","sourceTitle":"","title":""}),
            json!({"id":"authored-other","category":"Clothes","sourceTitle":"アマト"}),
        ];
        let options = owned_base_options(&products);
        assert_eq!(options.len(), 6);
        for product in &products[..6] {
            assert!(options.iter().any(|option| option["id"] == format!("product:{}", product["id"].as_str().unwrap())));
        }
        for id in ["authored-wrapper", "authored-ambiguous", "authored-bundle"] {
            let option = options.iter().find(|option| option["productId"] == id).unwrap();
            assert!(!option["displayName"].as_str().unwrap().is_empty());
            assert_eq!(option["englishName"], "");
            assert_ne!(option["nameKind"], "single");
        }
        let latin = options.iter().find(|option| option["productId"] == "authored-latin").unwrap();
        assert_eq!(latin["displayName"], "Ren♡Ai");
        assert_eq!(latin["englishNameStatus"], "sourceTitleSpelling");
        assert!(options.iter().all(|option| option["entitlementEstablished"] == false));
    }

    #[test]
    fn public_identity_presentation_corrections_never_spread_to_similar_products() {
        for (id, source, expected) in [
            ("8547624", "銀髪スーツの悪魔 メイネーラ VRChat向けオリジナル3Dアバター #e62f8b", "メイネーラ"),
            ("4309253", "ブルーアーカイブ 二次創作3Dアバター 棗 イロハ", "棗 イロハ"),
        ] {
            let options = owned_base_options(&[json!({"id":id,"category":"Base","sourceTitle":source})]);
            assert_eq!(options[0]["id"], format!("product:{id}"));
            assert_eq!(options[0]["officialName"], expected);
            assert!(matches!(options[0]["englishNameStatus"].as_str(), Some("romanizationFallback" | "unresolvedReading")));
            assert_eq!(options[0]["englishNameEvidence"]["kind"], "presentationFallback");
            assert_ne!(parsed_product_avatar_label("authored-other", source).original, expected);
        }
        assert_eq!(parsed_product_avatar_label("8547624", "Authored other name").original, "Authored other name");
        let label = owned_base_options(&[json!({"id":"authored-nanase","category":"Base","sourceTitle":"【VRChat想定 オリジナル3Dモデル『ナナセ・ノワール -Nanase Noir-』Gen3.0"})]);
        assert_eq!(label[0]["displayName"], "ナナセ・ノワール / Nanase Noir (Gen3.0)");
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
