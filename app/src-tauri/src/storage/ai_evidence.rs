//! Optional bounded reads of recorded managed documents. No archive expansion,
//! filesystem discovery, source fetch, or asset-content upload occurs here.
use super::{artifact_id, io, StorageService};
use crate::{
    ai_analysis::{normalize_input, public_product_url, AnalysisInput},
    catalogue::opaque_ai_file_ref,
};
use anyhow::{anyhow, Result};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::File,
    io::Read,
    path::PathBuf,
};

const MAX_DOCUMENTS: usize = 4;
const MAX_DOCUMENT_BYTES: usize = 64 * 1024;
const MAX_EXCERPT_BYTES: usize = 8 * 1024;

struct Document {
    evidence_ref: String,
    name: String,
    path: PathBuf,
    hash: String,
    size: u64,
}

impl StorageService {
    pub async fn collect_ai_evidence(
        &self,
        product_id: &str,
        input: AnalysisInput,
    ) -> Result<AnalysisInput> {
        let mut input = normalize_input(input).map_err(|_| anyhow!("ai_evidence_invalid"))?;
        input
            .files
            .retain(|file| safe_ai_evidence_name(&file.file_name));
        for file in &mut input.files {
            file.text_excerpt.clear();
            file.candidate_public_urls.clear();
        }
        if input.files.is_empty() {
            return Ok(input);
        }
        let _operation = self.operations.read().await;
        let selected: BTreeSet<_> = input
            .files
            .iter()
            .map(|file| file.evidence_ref.as_str())
            .collect();
        let files = self
            .catalogue
            .related("remote_file", product_id)
            .map_err(|_| anyhow!("ai_evidence_unavailable"))?;
        let mut roots = BTreeMap::new();
        let mut documents = Vec::new();
        for file in files {
            let Some(id) = file["id"].as_str() else {
                continue;
            };
            if file["requiresContentVerification"] == true
                || !file["revision"]
                    .as_str()
                    .is_some_and(|revision| !revision.is_empty() && revision != "unknown")
            {
                continue;
            }
            let Some(artifact) = self
                .catalogue
                .get("artifact", &artifact_id(&file))
                .map_err(|_| anyhow!("ai_evidence_unavailable"))?
            else {
                continue;
            };
            if artifact["productId"].as_str() != Some(product_id)
                || artifact["fileId"].as_str() != Some(id)
                || artifact["revision"] != file["revision"]
                || !matches!(
                    artifact["status"].as_str(),
                    Some("ready" | "downloaded" | "extractionFailed")
                )
            {
                continue;
            }
            let Some(root_id) = artifact["rootId"].as_str() else {
                continue;
            };
            if !roots.contains_key(root_id) {
                let path = self
                    .catalogue
                    .get("root", root_id)
                    .map_err(|_| anyhow!("ai_evidence_unavailable"))?
                    .and_then(|root| self.root_path(&root).ok());
                roots.insert(root_id.to_owned(), path);
            }
            let Some(Some(root)) = roots.get(root_id) else {
                continue;
            };
            let original_ref = opaque_ai_file_ref(id, "");
            let name = file["sourceRelativePath"]
                .as_str()
                .or(file["name"].as_str())
                .unwrap_or("");
            if artifact["originalAvailable"] == true && selected.contains(original_ref.as_str()) {
                if let Some(path) = artifact["relativePath"]
                    .as_str()
                    .and_then(|relative| io::safe_join(root, relative).ok())
                {
                    push_document(&mut documents, original_ref, name, path, &artifact);
                }
            }
            if artifact["status"] != "ready" {
                continue;
            }
            let Some(base) = artifact["extractedPath"]
                .as_str()
                .and_then(|relative| io::safe_join(root, relative).ok())
            else {
                continue;
            };
            for entry in artifact["extractedInventory"]
                .as_array()
                .into_iter()
                .flatten()
            {
                if entry["isDirectory"] == true {
                    continue;
                }
                let Some(relative) = entry["relativePath"].as_str() else {
                    continue;
                };
                let reference = opaque_ai_file_ref(id, relative);
                if selected.contains(reference.as_str()) {
                    if let Ok(path) = io::safe_join(&base, relative) {
                        push_document(&mut documents, reference, relative, path, entry);
                    }
                }
            }
        }
        documents.sort_by(|a, b| (&a.name, &a.evidence_ref).cmp(&(&b.name, &b.evidence_ref)));
        documents.dedup_by(|a, b| a.evidence_ref == b.evidence_ref);
        documents.truncate(MAX_DOCUMENTS);
        // Catalogue getters above release their own locks before any document I/O.
        tokio::task::spawn_blocking(move || {
            for document in documents {
                let Some(text) = read_verified_document(&document) else {
                    continue;
                };
                let excerpt = bounded_excerpt(&redact_ai_evidence_text(&text), MAX_EXCERPT_BYTES);
                let urls = ai_public_urls_in_text(&excerpt);
                let Some(index) = input
                    .files
                    .iter()
                    .position(|file| file.evidence_ref == document.evidence_ref)
                else {
                    continue;
                };
                let mut enriched = input.clone();
                enriched.files[index].text_excerpt = excerpt;
                enriched.files[index].candidate_public_urls = urls;
                // Escaping in JSON also counts toward the existing 128KiB input
                // limit. Oversized additions leave the safe metadata usable.
                if normalize_input(enriched.clone()).is_ok() {
                    input = enriched;
                }
            }
            Ok(input)
        })
        .await
        .map_err(|_| anyhow!("ai_evidence_unavailable"))?
    }
}

fn push_document(
    documents: &mut Vec<Document>,
    evidence_ref: String,
    name: &str,
    path: PathBuf,
    record: &serde_json::Value,
) {
    if !document_name(name) {
        return;
    }
    let Some(size) = record["sizeBytes"]
        .as_u64()
        .filter(|size| *size <= MAX_DOCUMENT_BYTES as u64)
    else {
        return;
    };
    let Some(hash) = record["hash"]
        .as_str()
        .filter(|hash| hash.len() == 64 && hash.bytes().all(|byte| byte.is_ascii_hexdigit()))
    else {
        return;
    };
    documents.push(Document {
        evidence_ref,
        name: name.into(),
        path,
        hash: hash.into(),
        size,
    });
}

fn read_verified_document(document: &Document) -> Option<String> {
    io::ensure_regular(&document.path).ok()?;
    let file = File::open(&document.path).ok()?;
    let metadata = file.metadata().ok()?;
    if !metadata.is_file() || metadata.len() != document.size {
        return None;
    }
    let mut bytes = Vec::new();
    file.take((MAX_DOCUMENT_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .ok()?;
    verified_document_text(&bytes, &document.hash, document.size)
}

fn verified_document_text(bytes: &[u8], hash: &str, size: u64) -> Option<String> {
    if bytes.len() > MAX_DOCUMENT_BYTES
        || bytes.len() as u64 != size
        || hex::encode(Sha256::digest(bytes)) != hash
    {
        return None;
    }
    let text = std::str::from_utf8(bytes)
        .ok()?
        .trim_start_matches('\u{feff}');
    if text
        .chars()
        .any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t'))
    {
        return None;
    }
    Some(text.to_owned())
}

pub(crate) fn safe_ai_evidence_name(name: &str) -> bool {
    let lower = name.to_lowercase();
    super::archive::validate_relative_path(name).is_ok()
        && ![
            "license",
            "licence",
            "eula",
            "serial",
            "password",
            "token",
            "receipt",
            "invoice",
            "payment",
            "billing",
            "cookie",
            "credential",
            "secret",
            "api_key",
            "apikey",
            ".env",
            "ライセンス",
            "注文",
            "密钥",
            "密鑰",
            "密码",
            "訂單",
            "订单",
        ]
        .iter()
        .any(|pattern| lower.contains(pattern))
        && !order_marker(&lower)
        && !lower
            .split('/')
            .any(|part| matches!(part.split('.').next(), Some("key" | "keys")))
}

fn order_marker(lower: &str) -> bool {
    lower
        .split(|c: char| !c.is_ascii_alphanumeric())
        .any(|word| {
            matches!(word, "order" | "orders")
                || word.strip_prefix("order").is_some_and(|tail| {
                    !tail.is_empty() && tail.bytes().all(|byte| byte.is_ascii_digit())
                })
        })
}

fn document_name(name: &str) -> bool {
    if !safe_ai_evidence_name(name) {
        return false;
    }
    let lower = name.to_lowercase();
    let leaf = lower.rsplit('/').next().unwrap_or("");
    let Some((stem, extension)) = leaf.rsplit_once('.') else {
        return false;
    };
    matches!(extension, "txt" | "md" | "markdown" | "rst")
        && [
            "readme",
            "read_me",
            "read-me",
            "manual",
            "guide",
            "instructions",
            "使い方",
            "説明",
            "使用",
            "说明",
            "說明",
        ]
        .iter()
        .any(|pattern| stem.contains(pattern))
}

fn url_end(text: &str) -> usize {
    text.char_indices()
        .find(|(_, c)| {
            c.is_whitespace()
                || matches!(
                    c,
                    '"' | '\'' | '<' | '>' | '(' | ')' | '[' | ']' | '{' | '}'
                )
        })
        .map(|(index, _)| index)
        .unwrap_or(text.len())
}

pub(crate) fn ai_public_urls_in_text(text: &str) -> Vec<String> {
    let mut urls = BTreeSet::new();
    for (index, _) in text.match_indices("https://") {
        let remaining = &text[index..];
        let raw =
            remaining[..url_end(remaining)].trim_end_matches(['.', ',', ';', '!', '。', '、']);
        if let Some(url) = public_product_url(raw) {
            urls.insert(url);
        }
        if urls.len() == 8 {
            break;
        }
    }
    urls.into_iter().collect()
}

pub(crate) fn redact_ai_evidence_text(text: &str) -> String {
    let mut lines = Vec::new();
    for line in text.lines() {
        let lower = line.to_lowercase();
        let local_path = line.contains('\\')
            || line
                .as_bytes()
                .windows(3)
                .enumerate()
                .any(|(index, bytes)| {
                    bytes[0].is_ascii_alphabetic()
                        && bytes[1] == b':'
                        && bytes[2] == b'/'
                        && (index == 0 || !line.as_bytes()[index - 1].is_ascii_alphanumeric())
                })
            || ["file://", "/Users/", "/home/", "/mnt/", "/tmp/"]
                .iter()
                .any(|prefix| line.contains(prefix))
            || line.split_whitespace().any(|word| {
                word.trim_start_matches(['\'', '"', '(', '['])
                    .starts_with('/')
            });
        if local_path
            || order_marker(&lower)
            || [
                "api key",
                "api_key",
                "apikey",
                "authorization",
                "bearer ",
                "token",
                "cookie",
                "password",
                "license",
                "licence",
                "serial",
                "key:",
                "key=",
                "key =",
                "invoice",
                "receipt",
                "payment",
                "billing",
                "secret",
                "credential",
                "ライセンス",
                "注文",
                "密钥",
                "密鑰",
                "密码",
                "訂單",
                "订单",
            ]
            .iter()
            .any(|pattern| lower.contains(pattern))
        {
            lines.push("[redacted]".to_owned());
            continue;
        }
        let mut sanitized = String::new();
        let mut remaining = line;
        while let Some(index) = match (remaining.find("https://"), remaining.find("http://")) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        } {
            sanitized.push_str(&remaining[..index]);
            let rest = &remaining[index..];
            let end = url_end(rest);
            let token = &rest[..end];
            if public_product_url(token.trim_end_matches(['.', ',', ';', '!', '。', '、']))
                .is_some()
            {
                sanitized.push_str(token);
            } else {
                sanitized.push_str("[redacted URL]");
            }
            remaining = &rest[end..];
        }
        sanitized.push_str(remaining);
        let words: Vec<_> = sanitized
            .split_whitespace()
            .map(|word| {
                let lower = word.to_lowercase();
                if word.contains('@')
                    || lower.contains("sk-")
                    || lower.starts_with("eyj") && word.len() > 20
                {
                    "[redacted]"
                } else {
                    word
                }
            })
            .collect();
        lines.push(words.join(" "));
    }
    lines.join("\n")
}

fn bounded_excerpt(text: &str, max_bytes: usize) -> String {
    let mut end = text.len().min(max_bytes);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recorded_text_requires_exact_bytes_and_never_decodes_binary_or_oversize() {
        let bytes = b"Synthetic README\nUnity shader instructions";
        let hash = hex::encode(Sha256::digest(bytes));
        assert_eq!(
            verified_document_text(bytes, &hash, bytes.len() as u64).unwrap(),
            std::str::from_utf8(bytes).unwrap()
        );
        assert!(verified_document_text(b"tampered", &hash, bytes.len() as u64).is_none());
        let mut same_size_tampered = bytes.to_vec();
        same_size_tampered[0] = b'X';
        assert!(verified_document_text(&same_size_tampered, &hash, bytes.len() as u64).is_none());
        assert!(verified_document_text(bytes, &hash, bytes.len() as u64 + 1).is_none());
        for bytes in [vec![0, 1, 2], vec![255], vec![b'x'; MAX_DOCUMENT_BYTES + 1]] {
            assert!(verified_document_text(
                &bytes,
                &hex::encode(Sha256::digest(&bytes)),
                bytes.len() as u64
            )
            .is_none());
        }
    }

    #[test]
    fn document_policy_excludes_secrets_archives_and_unsafe_relative_names() {
        for name in ["docs/README.md", "説明書.txt", "guide/usage-manual.txt"] {
            assert!(document_name(name));
        }
        for name in [
            "license/readme.txt",
            "docs/invoice.txt",
            "README.zip",
            "README.unitypackage",
            "shader.txt",
            "../README.txt",
            "C:/README.txt",
            "docs/.env",
            "key.txt",
        ] {
            assert!(!document_name(name));
        }
        assert!(!safe_ai_evidence_name("assets/license-key.png"));
        assert!(safe_ai_evidence_name("assets/Border Shader.fbx"));
    }

    #[test]
    fn evidence_redaction_retains_observed_public_routes_without_private_links() {
        let text = "Synthetic guide https://maker.gumroad.com/l/shader\nContact owner@example.invalid\nLicense key ABCD-1234\nAPI key sk-synthetic-only\nhttps://jinxxy.com/downloads/file?token=private\nSee [item](https://booth.pm/ja/items/900001).";
        let redacted = redact_ai_evidence_text(text);
        for secret in [
            "owner@example.invalid",
            "ABCD-1234",
            "sk-synthetic-only",
            "token=private",
        ] {
            assert!(!redacted.contains(secret));
        }
        assert_eq!(
            ai_public_urls_in_text(&redacted),
            [
                "https://booth.pm/ja/items/900001",
                "https://maker.gumroad.com/l/shader"
            ]
        );
        assert!(ai_public_urls_in_text(
            "https://maker.gumroad.com/l/shader?license=secret https://booth.pm/downloadables/1"
        )
        .is_empty());
        let mixed = redact_ai_evidence_text(
            "http://private.invalid/path?Signature=synthetic https://maker.gumroad.com/l/shader",
        );
        assert!(!mixed.contains("private.invalid"));
        assert!(mixed.contains("https://maker.gumroad.com/l/shader"));
        assert_eq!(
            redact_ai_evidence_text("https://maker.gumroad.com/l/shader"),
            "https://maker.gumroad.com/l/shader"
        );
        let paths = redact_ai_evidence_text("Example F:/Synthetic/private/readme.txt\nInstall C:\\Users\\Synthetic\\Private\nContact <owner@example.invalid>\nPlain shader instructions");
        assert!(!paths.contains("Synthetic"));
        assert!(!paths.contains("owner@example.invalid"));
        assert!(paths.contains("Plain shader instructions"));
        assert_eq!(
            redact_ai_evidence_text("Border shader 日本語"),
            "Border shader 日本語"
        );
        assert!(!redact_ai_evidence_text("Key: synthetic-value").contains("synthetic-value"));
        let bounded = bounded_excerpt(&"あ".repeat(4000), MAX_EXCERPT_BYTES);
        assert!(bounded.len() <= MAX_EXCERPT_BYTES);
        assert!(bounded.chars().all(|c| c == 'あ'));
    }
}
