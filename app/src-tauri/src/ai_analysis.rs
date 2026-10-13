//! Pure, bounded AI suggestions. Callers select/redact evidence before constructing
//! input; these types never read files, fetch URLs, or establish store ownership.
use anyhow::{ensure, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeSet;
use url::Url;

const MAX_TAGS: usize = 128;
const MAX_FILES: usize = 64;
const MAX_URLS: usize = 8;
const MAX_INPUT_BYTES: usize = 128 * 1024;
const MAX_OUTPUT_BYTES: usize = 64 * 1024;

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AnalysisInput {
    pub title: String,
    pub source_tags: Vec<String>,
    pub description: String,
    pub rejected_tags: Vec<String>,
    pub files: Vec<FileEvidence>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FileEvidence {
    /// Opaque local reference, not an absolute path or account identifier.
    pub evidence_ref: String,
    pub file_name: String,
    pub text_excerpt: String,
    pub candidate_public_urls: Vec<String>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AnalysisOutput {
    pub category: Option<CategorySuggestion>,
    pub tags: Vec<TagSuggestion>,
    pub tag_aliases: Vec<TagAliasSuggestion>,
    pub source_url_candidates: Vec<SourceUrlSuggestion>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CategorySuggestion {
    pub category: String,
    pub evidence_refs: Vec<String>,
    pub basis: String,
    pub confidence: f64,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TagSuggestion {
    pub label: String,
    pub evidence_refs: Vec<String>,
    pub basis: String,
    pub confidence: f64,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TagAliasSuggestion {
    pub from: String,
    pub to: String,
    pub evidence_refs: Vec<String>,
    pub basis: String,
    pub confidence: f64,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SourceUrlSuggestion {
    /// A candidate only. Route validation does not verify a product match.
    pub url: String,
    pub evidence_refs: Vec<String>,
    pub basis: String,
    pub confidence: f64,
}

fn bounded(value: &str, max: usize, empty: bool) -> Result<()> {
    ensure!(
        value.len() <= max && (empty || !value.trim().is_empty()),
        "AI_INVALID_ANALYSIS"
    );
    ensure!(
        !value
            .chars()
            .any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t')),
        "AI_INVALID_ANALYSIS"
    );
    Ok(())
}

fn labels(values: &mut Vec<String>) -> Result<()> {
    ensure!(values.len() <= MAX_TAGS, "AI_INVALID_ANALYSIS");
    let mut seen = BTreeSet::new();
    for value in values.iter_mut() {
        bounded(value, 160, false)?;
        *value = value.trim().to_owned();
    }
    values.retain(|value| seen.insert(crate::taxonomy::canonical_tag(value)));
    Ok(())
}

/// Explicit metadata reference names are `title`, `sourceTags`, `description`.
pub fn normalize_input(mut input: AnalysisInput) -> Result<AnalysisInput> {
    bounded(&input.title, 2048, true)?;
    bounded(&input.description, 32 * 1024, true)?;
    labels(&mut input.source_tags)?;
    labels(&mut input.rejected_tags)?;
    ensure!(input.files.len() <= MAX_FILES, "AI_INVALID_ANALYSIS");
    let mut refs = BTreeSet::from([
        "title".to_owned(),
        "sourceTags".to_owned(),
        "description".to_owned(),
    ]);
    for file in &mut input.files {
        ensure!(
            !file.evidence_ref.is_empty()
                && file.evidence_ref.len() <= 80
                && file
                    .evidence_ref
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-' | b':'))
                && refs.insert(file.evidence_ref.clone()),
            "AI_INVALID_ANALYSIS"
        );
        bounded(&file.file_name, 1024, false)?;
        ensure!(
            !file.file_name.contains(['\\', ':'])
                && file
                    .file_name
                    .split('/')
                    .all(|part| !part.is_empty() && !matches!(part, "." | "..")),
            "AI_INVALID_ANALYSIS"
        );
        bounded(&file.text_excerpt, 8192, true)?;
        if let Some(hash) = file.evidence_ref.strip_prefix("i-") {
            ensure!(
                hash.len() == 64
                    && hash.bytes().all(|b| b.is_ascii_hexdigit())
                    && file.text_excerpt.is_empty()
                    && file.candidate_public_urls.is_empty(),
                "AI_INVALID_ANALYSIS"
            );
        }
        ensure!(
            file.candidate_public_urls.len() <= MAX_URLS,
            "AI_INVALID_ANALYSIS"
        );
        for candidate in &mut file.candidate_public_urls {
            *candidate = public_product_url(candidate)
                .ok_or_else(|| anyhow::anyhow!("AI_INVALID_SOURCE_URL"))?;
        }
        let mut seen = BTreeSet::new();
        file.candidate_public_urls
            .retain(|url| seen.insert(url.clone()));
    }
    ensure!(
        serde_json::to_vec(&input)?.len() <= MAX_INPUT_BYTES,
        "AI_INVALID_ANALYSIS"
    );
    Ok(input)
}

fn shop_label(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 63
        && !value.starts_with('-')
        && !value.ends_with('-')
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'-')
}

/// Narrow public product routes only; this does not contact the store or prove
/// that the URL exists, belongs to these files, or grants download access.
pub fn public_product_url(value: &str) -> Option<String> {
    if value.len() > 2048
        || value.trim() != value
        || value.contains(['\\', '%'])
        || value.chars().any(|c| c.is_control() || c.is_whitespace())
    {
        return None;
    }
    let url = Url::parse(value).ok()?;
    if url.scheme() != "https"
        || url.port_or_known_default() != Some(443)
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return None;
    }
    let host = url.host_str()?;
    let parts: Vec<_> = url.path().split('/').skip(1).collect();
    // Reject parser-normalized traversal instead of accepting its destination.
    if value.split('/').any(|part| matches!(part, "." | "..")) {
        return None;
    }
    let numeric = |id: &str| {
        !id.is_empty()
            && id.len() <= 20
            && id.bytes().all(|b| b.is_ascii_digit())
            && id.bytes().any(|b| b != b'0')
    };
    let booth = host == "booth.pm" || host.strip_suffix(".booth.pm").is_some_and(shop_label);
    let accepted = if booth {
        match parts.as_slice() {
            ["items", id] => numeric(id),
            [locale, "items", id] => {
                matches!(*locale, "ja" | "en" | "ko" | "zh-cn" | "zh-tw") && numeric(id)
            }
            _ => false,
        }
    } else if host.strip_suffix(".gumroad.com").is_some_and(|seller| {
        shop_label(seller) && !matches!(seller, "app" | "api" | "www" | "checkout" | "download")
    }) {
        matches!(parts.as_slice(), ["l", slug] if product_segment(slug))
    } else if matches!(
        host,
        "jinxxy.com"
            | "ja.jinxxy.com"
            | "de.jinxxy.com"
            | "es.jinxxy.com"
            | "fr.jinxxy.com"
            | "zh.jinxxy.com"
            | "ko.jinxxy.com"
    ) {
        matches!(parts.as_slice(), [seller, slug] if product_segment(seller) && product_segment(slug)
            && !matches!(*seller, "my" | "api" | "login" | "checkout" | "downloads" | "download" | "orders" | "settings" | "search"))
    } else {
        false
    };
    accepted.then(|| url.to_string())
}

fn product_segment(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && !matches!(value, "." | "..")
        && value
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '-' | '_'))
}

fn proof(
    refs: &mut Vec<String>,
    basis: &str,
    confidence: f64,
    known: &BTreeSet<String>,
) -> Result<()> {
    bounded(basis, 512, false)?;
    ensure!(
        confidence.is_finite()
            && (0.0..=1.0).contains(&confidence)
            && !refs.is_empty()
            && refs.len() <= 16
            && refs.iter().all(|r| known.contains(r)),
        "AI_INVALID_ANALYSIS"
    );
    let mut seen = BTreeSet::new();
    refs.retain(|r| seen.insert(r.clone()));
    Ok(())
}

/// Pass the adapter's explicit refusal field separately. Errors never echo model
/// text. Caller rechecks current manual edits/rejections before durable apply.
pub fn parse_output(
    content: &str,
    refusal: Option<&str>,
    input: &AnalysisInput,
) -> Result<AnalysisOutput> {
    ensure!(!refusal.is_some_and(|r| !r.trim().is_empty()), "AI_REFUSED");
    ensure!(content.len() <= MAX_OUTPUT_BYTES, "AI_INVALID_ANALYSIS");
    let input = normalize_input(input.clone())?;
    let value: Value =
        serde_json::from_str(content).map_err(|_| anyhow::anyhow!("AI_INVALID_ANALYSIS"))?;
    // Serde permits a missing Option field; the explicit nullable schema does not.
    ensure!(value.get("category").is_some(), "AI_INVALID_ANALYSIS");
    let output: AnalysisOutput =
        serde_json::from_value(value).map_err(|_| anyhow::anyhow!("AI_INVALID_ANALYSIS"))?;
    let mut known: BTreeSet<String> = input.files.iter().map(|f| f.evidence_ref.clone()).collect();
    if !input.title.trim().is_empty() {
        known.insert("title".into());
    }
    if !input.source_tags.is_empty() {
        known.insert("sourceTags".into());
    }
    if !input.description.trim().is_empty() {
        known.insert("description".into());
    }
    normalize_output(output, &known, &input.rejected_tags)
}

/// Revalidate portable suggestions against retained opaque evidence references.
/// No excerpts are needed to preserve bounds and reapply current rejections.
pub fn normalize_output(
    mut output: AnalysisOutput,
    known: &BTreeSet<String>,
    rejected_tags: &[String],
) -> Result<AnalysisOutput> {
    ensure!(
        serde_json::to_vec(&output)?.len() <= MAX_OUTPUT_BYTES,
        "AI_INVALID_ANALYSIS"
    );
    if let Some(category) = &mut output.category {
        ensure!(
            crate::taxonomy::CATEGORY_KEYS.contains(&category.category.as_str()),
            "AI_INVALID_ANALYSIS"
        );
        proof(
            &mut category.evidence_refs,
            &category.basis,
            category.confidence,
            &known,
        )?;
    }
    ensure!(
        output.tags.len() <= MAX_TAGS
            && output.tag_aliases.len() <= MAX_TAGS
            && output.source_url_candidates.len() <= MAX_URLS,
        "AI_INVALID_ANALYSIS"
    );
    for tag in &mut output.tags {
        bounded(&tag.label, 160, false)?;
        tag.label = tag.label.trim().to_owned();
        proof(&mut tag.evidence_refs, &tag.basis, tag.confidence, &known)?;
    }
    for alias in &mut output.tag_aliases {
        bounded(&alias.from, 160, false)?;
        bounded(&alias.to, 160, false)?;
        alias.from = alias.from.trim().to_owned();
        alias.to = alias.to.trim().to_owned();
        proof(
            &mut alias.evidence_refs,
            &alias.basis,
            alias.confidence,
            &known,
        )?;
    }
    for candidate in &mut output.source_url_candidates {
        candidate.url = public_product_url(&candidate.url)
            .ok_or_else(|| anyhow::anyhow!("AI_INVALID_SOURCE_URL"))?;
        proof(
            &mut candidate.evidence_refs,
            &candidate.basis,
            candidate.confidence,
            &known,
        )?;
    }
    let key = crate::taxonomy::canonical_tag;
    let rejected: BTreeSet<_> = rejected_tags.iter().map(|t| key(t)).collect();
    let mut seen = BTreeSet::new();
    output
        .tags
        .retain(|t| !rejected.contains(&key(&t.label)) && seen.insert(key(&t.label)));
    let mut aliases = BTreeSet::new();
    output.tag_aliases.retain(|a| {
        !rejected.contains(&key(&a.from))
            && !rejected.contains(&key(&a.to))
            && crate::taxonomy::normalize(&a.from) != crate::taxonomy::normalize(&a.to)
            && aliases.insert((key(&a.from), key(&a.to)))
    });
    let mut urls = BTreeSet::new();
    output
        .source_url_candidates
        .retain(|c| urls.insert(c.url.clone()));
    Ok(output)
}

/// Inner OpenAI JSON-schema format; adapter adds its Responses/Chat wrapper.
/// Original schema following https://developers.openai.com/api/docs/guides/structured-outputs
/// (2026-10-10): required fields, nullable category, closed objects. Semantic
/// limits are also enforced locally for compatible endpoints ignoring schema.
pub fn structured_output_schema() -> Value {
    fn suggestion(fields: Value) -> Value {
        let mut fields = fields.as_object().unwrap().clone();
        fields.insert(
            "evidenceRefs".into(),
            json!({"type":"array","items":{"type":"string"},"minItems":1,"maxItems":16}),
        );
        fields.insert("basis".into(), json!({"type":"string"}));
        fields.insert(
            "confidence".into(),
            json!({"type":"number","minimum":0,"maximum":1}),
        );
        let required: Vec<_> = fields.keys().cloned().collect();
        json!({"type":"object","properties":fields,"required":required,"additionalProperties":false})
    }
    json!({"name":"asset_analysis","strict":true,"schema":{
        "type":"object","additionalProperties":false,
        "required":["category","tags","tagAliases","sourceUrlCandidates"],
        "properties":{
            "category":{"anyOf":[{"type":"null"},suggestion(json!({"category":{"type":"string","enum":crate::taxonomy::CATEGORY_KEYS}}))]},
            "tags":{"type":"array","maxItems":MAX_TAGS,"items":suggestion(json!({"label":{"type":"string"}}))},
            "tagAliases":{"type":"array","maxItems":MAX_TAGS,"items":suggestion(json!({"from":{"type":"string"},"to":{"type":"string"}}))},
            "sourceUrlCandidates":{"type":"array","maxItems":MAX_URLS,"items":suggestion(json!({"url":{"type":"string"}}))}
        }
    }})
}

#[cfg(test)]
mod tests {
    use super::*;
    fn input() -> AnalysisInput {
        AnalysisInput {
            title: "Synthetic casual jacket".into(),
            source_tags: vec!["Clothes".into()],
            description: String::new(),
            rejected_tags: vec!["カジュアル".into()],
            files: vec![FileEvidence {
                evidence_ref: "file:1".into(),
                file_name: "docs/readme.txt".into(),
                text_excerpt: "Synthetic maker product reference".into(),
                candidate_public_urls: vec!["https://maker.gumroad.com/l/jacket".into()],
            }],
        }
    }
    fn result() -> Value {
        json!({"category":{"category":"Clothes","evidenceRefs":["title"],"basis":"Product type","confidence":0.9},
            "tags":[{"label":"Casual","evidenceRefs":["title"],"basis":"Style","confidence":0.8},{"label":"Coat","evidenceRefs":["file:1"],"basis":"Readme","confidence":0.7},{"label":"coat","evidenceRefs":["title"],"basis":"Name","confidence":0.7}],
            "tagAliases":[{"from":"Casual","to":"Everyday","evidenceRefs":["title"],"basis":"Style synonym","confidence":0.7}],
            "sourceUrlCandidates":[{"url":"https://maker.gumroad.com/l/jacket","evidenceRefs":["file:1"],"basis":"Readme reference; not verified","confidence":0.9}]})
    }
    #[test]
    fn analysis_preserves_rejections_and_validates_evidence_without_claiming_ownership() {
        let parsed = parse_output(&result().to_string(), None, &input()).unwrap();
        assert_eq!(parsed.tags.len(), 1);
        assert_eq!(parsed.tags[0].label, "Coat");
        assert!(parsed.tag_aliases.is_empty());
        assert_eq!(parsed.category.unwrap().category, "Clothes");
        assert_eq!(parsed.source_url_candidates[0].evidence_refs, ["file:1"]);
        for (field, value) in [
            ("confidence", json!(1.1)),
            ("evidenceRefs", json!(["missing"])),
        ] {
            let mut output = result();
            output["tags"][1][field] = value;
            assert!(parse_output(&output.to_string(), None, &input()).is_err());
        }
        let mut output = result();
        output["sourceUrlCandidates"][0]["verified"] = json!(true);
        assert!(parse_output(&output.to_string(), None, &input()).is_err());
        let mut output = result();
        output.as_object_mut().unwrap().remove("category");
        assert!(parse_output(&output.to_string(), None, &input()).is_err());
        let mut output = result();
        output["category"] = Value::Null;
        assert!(parse_output(&output.to_string(), None, &input())
            .unwrap()
            .category
            .is_none());
        assert_eq!(
            parse_output("not JSON", Some("Provider refusal text"), &input())
                .err()
                .unwrap()
                .to_string(),
            "AI_REFUSED"
        );
    }
    #[test]
    fn analysis_input_excludes_unknown_fields_absolute_paths_and_excess_content() {
        let mut value = serde_json::to_value(input()).unwrap();
        value["account"] = json!("forbidden");
        assert!(serde_json::from_value::<AnalysisInput>(value).is_err());
        for path in [
            "C:/private/readme.txt",
            "/private/readme.txt",
            "../readme.txt",
            "folder/../readme.txt",
            "\\\\server\\readme.txt",
        ] {
            let mut value = input();
            value.files[0].file_name = path.into();
            assert!(normalize_input(value).is_err());
        }
        let mut value = input();
        value.files[0].text_excerpt = "a".repeat(8193);
        assert!(normalize_input(value).is_err());
        let mut value = input();
        value.files[0].evidence_ref = "title".into();
        assert!(normalize_input(value).is_err());
        let mut value = input();
        value.files = vec![FileEvidence {
            evidence_ref: format!("i-{}", "a".repeat(64)),
            file_name: "image-1.png".into(),
            text_excerpt: String::new(),
            candidate_public_urls: vec![],
        }];
        assert!(normalize_input(value.clone()).is_ok());
        value.files[0]
            .candidate_public_urls
            .push("https://maker.gumroad.com/l/asset".into());
        assert!(normalize_input(value.clone()).is_err());
        value.files[0].candidate_public_urls.clear();
        value.files[0].evidence_ref = "i-not-a-content-hash".into();
        assert!(normalize_input(value).is_err());
    }
    #[test]
    fn analysis_product_urls_accept_only_three_store_public_product_routes() {
        for url in [
            "https://booth.pm/items/123",
            "https://booth.pm/ja/items/123",
            "https://maker.booth.pm/en/items/123",
            "https://maker.gumroad.com/l/jacket",
            "https://jinxxy.com/maker/jacket",
            "https://ja.jinxxy.com/maker/jacket",
            "https://zh.jinxxy.com/maker/jacket",
        ] {
            assert!(public_product_url(url).is_some(), "{url}");
        }
        for url in [
            "https://booth.pm/items/0",
            "https://booth.pm/items/no",
            "https://booth.pm/downloadables/123",
            "https://booth.pm/items/123?token=secret",
            "https://booth.pm/items/123#fragment",
            "https://user:pass@booth.pm/items/123",
            "http://booth.pm/items/123",
            "https://booth.pm:444/items/123",
            "https://booth.pm.evil.test/items/123",
            "https://a.b.booth.pm/items/123",
            "https://gumroad.com/l/jacket",
            "https://checkout.gumroad.com/l/jacket",
            "https://maker.gumroad.com/checkout/jacket",
            "https://api.jinxxy.com/maker/jacket",
            "https://it.jinxxy.com/maker/jacket",
            "https://jinxxy.com/my/inventory",
            "https://jinxxy.com/downloads/file",
            "https://jinxxy.com/maker/%2e%2e",
            "https://jinxxy.com/maker/../jacket",
            "https://127.0.0.1/maker/jacket",
            "https://jinxxy.com/maker/jacket?signature=secret",
        ] {
            assert!(public_product_url(url).is_none(), "{url}");
        }
    }
}
