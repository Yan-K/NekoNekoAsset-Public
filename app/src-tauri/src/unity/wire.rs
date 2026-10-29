use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{collections::BTreeSet, path::PathBuf, time::Duration};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::TcpStream,
    time::timeout,
};

pub const MAX_CONTROL_BYTES: usize = 16 * 1024 * 1024;
const COMMAND_TIMEOUT: Duration = Duration::from_secs(10);
const STREAM_IDLE: Duration = Duration::from_secs(30);

// Listener capabilities must never be serialized to the frontend or diagnostics.
#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Registry {
    pub version: u32,
    pub instance_id: String,
    pub session_id: String,
    pub process_id: u32,
    pub project_name: String,
    pub project_root: String,
    pub unity_version: String,
    #[serde(default)]
    pub bridge_version: Option<String>,
    pub host: String,
    pub port: u16,
    capability: String,
    pub heartbeat_utc: String,
    pub enabled: bool,
    pub busy: bool,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveEditor {
    pub instance_id: String,
    pub session_id: String,
    pub project_name: String,
    pub project_root: String,
    pub unity_version: String,
    #[serde(default)]
    pub bridge_version: Option<String>,
    pub busy: bool,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestIdentity {
    pub request_id: String,
    pub instance_id: String,
    // Original request session, even when querying a restarted listener.
    pub session_id: String,
    pub fingerprint: String,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Manifest {
    pub version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub locale: Option<String>,
    pub instance_id: String,
    pub session_id: String,
    pub entries: Vec<ManifestEntry>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManifestEntry {
    pub entry_id: String,
    pub kind: String,
    pub display_name: String,
    pub relative_path: String,
    pub bytes: i64,
    pub sha256: String,
    pub product_id: String,
    pub file_id: String,
    pub revision: String,
    pub member_path: String,
}

pub struct PayloadFile {
    pub entry_id: String,
    pub path: PathBuf,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BridgeResponse {
    pub version: u32,
    pub ok: bool,
    pub code: String,
    #[serde(default)]
    pub message: Option<String>,
    pub instance_id: String,
    pub session_id: String,
    #[serde(default)]
    pub request_id: Option<String>,
    #[serde(default)]
    pub receipt: Option<Receipt>,
    #[serde(default)]
    pub project_name: Option<String>,
    #[serde(default)]
    pub project_root: Option<String>,
    #[serde(default)]
    pub unity_version: Option<String>,
    #[serde(default)]
    pub bridge_version: Option<String>,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub busy: bool,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Receipt {
    pub request_id: String,
    pub fingerprint: String,
    pub instance_id: String,
    pub session_id: String,
    pub stage: String,
    pub staged: bool,
    pub writes_started: bool,
    pub updated_utc: String,
    #[serde(default)]
    pub preparation_phase: Option<String>,
    #[serde(default)]
    pub preparation_waiting_for_editor: bool,
    #[serde(default)]
    pub preparation_bytes: Option<u64>,
    #[serde(default)]
    pub preparation_total_bytes: Option<u64>,
    #[serde(default)]
    pub preparation_entries: Option<u64>,
    #[serde(default)]
    pub preparation_total_entries: Option<u64>,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default, deserialize_with = "null_vec")]
    pub compile_errors: Vec<String>,
    #[serde(default, deserialize_with = "null_vec")]
    pub results: Vec<FileResult>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileResult {
    pub entry_id: String,
    pub path: String,
    pub state: String,
    #[serde(default)]
    pub message: Option<String>,
}

fn null_vec<'de, D, T>(deserializer: D) -> std::result::Result<Vec<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Ok(Option::<Vec<T>>::deserialize(deserializer)?.unwrap_or_default())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Command<'a> {
    version: u32,
    command: &'a str,
    capability: &'a str,
    instance_id: &'a str,
    session_id: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    request_id: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    fingerprint: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    manifest_json: Option<&'a str>,
}

pub(crate) fn is_hash(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

pub(crate) fn is_uuid(value: &str) -> bool {
    value.len() == 36 && uuid::Uuid::parse_str(value).is_ok()
}

impl Registry {
    pub(crate) fn validate(&self) -> Result<()> {
        if self.version != 1
            || !is_hash(&self.instance_id)
            || !is_uuid(&self.session_id)
            || !is_hash(&self.capability)
            || self.host != "127.0.0.1"
            || self.port == 0
            || self.process_id == 0
            || self.project_name.is_empty()
            || self.project_name.len() > 1024
            || self.project_root.is_empty()
            || self.project_root.len() > 4096
            || self.unity_version.is_empty()
            || self.unity_version.len() > 128
            || self.bridge_version.as_ref().is_some_and(|version| {
                version.is_empty() || version.len() > 128 || version.chars().any(char::is_control)
            })
        {
            return Err(anyhow!("UNITY_REGISTRY_INVALID"));
        }
        Ok(())
    }
}

fn command<'a>(
    registry: &'a Registry,
    name: &'a str,
    request: Option<&'a RequestIdentity>,
    manifest: Option<&'a str>,
) -> Result<Command<'a>> {
    registry.validate()?;
    if let Some(request) = request {
        if request.instance_id != registry.instance_id
            || !is_uuid(&request.request_id)
            || !is_uuid(&request.session_id)
            || !is_hash(&request.fingerprint)
        {
            return Err(anyhow!("UNITY_INVALID_REQUEST"));
        }
    }
    Ok(Command {
        version: 1,
        command: name,
        capability: &registry.capability,
        instance_id: &registry.instance_id,
        session_id: request.map_or(registry.session_id.as_str(), |r| &r.session_id),
        request_id: request.map(|r| r.request_id.as_str()),
        fingerprint: request.map(|r| r.fingerprint.as_str()),
        manifest_json: manifest,
    })
}

async fn idle_read<R: AsyncRead + Unpin>(reader: &mut R, buffer: &mut [u8]) -> Result<usize> {
    timeout(STREAM_IDLE, reader.read(buffer))
        .await
        .map_err(|_| anyhow!("UNITY_TIMEOUT"))?
        .map_err(|_| anyhow!("UNITY_UNAVAILABLE"))
}

async fn write_bytes<W: AsyncWrite + Unpin>(writer: &mut W, mut bytes: &[u8]) -> Result<()> {
    while !bytes.is_empty() {
        let count = timeout(STREAM_IDLE, writer.write(bytes))
            .await
            .map_err(|_| anyhow!("UNITY_TIMEOUT"))?
            .map_err(|_| anyhow!("UNITY_UNAVAILABLE"))?;
        if count == 0 {
            return Err(anyhow!("UNITY_UNAVAILABLE"));
        }
        bytes = &bytes[count..];
    }
    Ok(())
}

async fn read_bytes<R: AsyncRead + Unpin>(reader: &mut R, mut buffer: &mut [u8]) -> Result<()> {
    while !buffer.is_empty() {
        let count = idle_read(reader, buffer).await?;
        if count == 0 {
            return Err(anyhow!("UNITY_INVALID_RESPONSE"));
        }
        buffer = &mut buffer[count..];
    }
    Ok(())
}

async fn write_frame<W: AsyncWrite + Unpin, T: Serialize>(writer: &mut W, value: &T) -> Result<()> {
    let bytes = serde_json::to_vec(value).map_err(|_| anyhow!("UNITY_INVALID_REQUEST"))?;
    if bytes.is_empty() || bytes.len() > MAX_CONTROL_BYTES {
        return Err(anyhow!("UNITY_INVALID_REQUEST"));
    }
    write_bytes(writer, &(bytes.len() as u32).to_le_bytes()).await?;
    write_bytes(writer, &bytes).await
}

async fn read_frame<R: AsyncRead + Unpin>(reader: &mut R) -> Result<BridgeResponse> {
    let mut prefix = [0; 4];
    read_bytes(reader, &mut prefix).await?;
    let size = u32::from_le_bytes(prefix) as usize;
    if size == 0 || size > MAX_CONTROL_BYTES {
        return Err(anyhow!("UNITY_INVALID_RESPONSE"));
    }
    let mut bytes = vec![0; size];
    read_bytes(reader, &mut bytes).await?;
    let text = std::str::from_utf8(&bytes).map_err(|_| anyhow!("UNITY_INVALID_RESPONSE"))?;
    serde_json::from_str(text).map_err(|_| anyhow!("UNITY_INVALID_RESPONSE"))
}

fn validate_response(
    response: &BridgeResponse,
    registry: &Registry,
    request: Option<&RequestIdentity>,
) -> Result<()> {
    let invalid = || anyhow!("UNITY_INVALID_RESPONSE");
    if response.version != 1
        || response.instance_id != registry.instance_id
        || response.session_id != registry.session_id
        || response
            .message
            .as_ref()
            .is_some_and(|s| !diagnostic(s, 256))
        || ![
            "ready",
            "accepted",
            "staged",
            "status",
            "cancelled",
            "busy",
            "disabled",
            "unauthorized",
            "versionMismatch",
            "invalidRequest",
            "fingerprintMismatch",
            "notFound",
            "failed",
        ]
        .contains(&response.code.as_str())
    {
        return Err(invalid());
    }
    if let Some(request) = request {
        if response.request_id.as_deref() != Some(&request.request_id) {
            return Err(invalid());
        }
        if let Some(receipt) = &response.receipt {
            if receipt.request_id != request.request_id
                || receipt.instance_id != request.instance_id
                || receipt.session_id != request.session_id
                || receipt.fingerprint != request.fingerprint
                || ![
                    "staging",
                    "preparing",
                    "waitingForUser",
                    "importing",
                    "refreshing",
                    "compiling",
                    "completed",
                    "cancelled",
                    "failed",
                    "interrupted",
                ]
                .contains(&receipt.stage.as_str())
                || chrono::DateTime::parse_from_rfc3339(&receipt.updated_utc).is_err()
                || receipt.compile_errors.len() > 50
                || receipt.compile_errors.iter().any(|s| !diagnostic(s, 1024))
                || receipt.error.as_ref().is_some_and(|s| !diagnostic(s, 256))
                || receipt.preparation_phase.as_deref().is_some_and(|phase| {
                    !phase.is_empty() && !["verify", "readPackage", "checkTargets"].contains(&phase)
                })
                || receipt
                    .preparation_bytes
                    .zip(receipt.preparation_total_bytes)
                    .is_some_and(|(done, total)| total > 0 && done > total)
                || receipt
                    .preparation_entries
                    .zip(receipt.preparation_total_entries)
                    .is_some_and(|(done, total)| total > 0 && done > total)
                || receipt.results.iter().any(|r| {
                    !is_uuid(&r.entry_id)
                        || !["applied", "skipped", "failed"].contains(&r.state.as_str())
                        || r.message.as_ref().is_some_and(|s| !diagnostic(s, 256))
                })
            {
                return Err(invalid());
            }
        }
        if ["accepted", "staged", "status", "cancelled"].contains(&response.code.as_str())
            && (!response.ok || response.receipt.is_none())
        {
            return Err(invalid());
        }
    } else if response.request_id.as_ref().is_some_and(|s| !s.is_empty())
        || response.receipt.is_some()
    {
        return Err(invalid());
    }
    Ok(())
}

fn diagnostic(value: &str, maximum: usize) -> bool {
    value.len() <= maximum && !value.chars().any(char::is_control)
}

async fn exchange(
    registry: &Registry,
    command: &Command<'_>,
    request: Option<&RequestIdentity>,
) -> Result<(TcpStream, BridgeResponse)> {
    timeout(COMMAND_TIMEOUT, async {
        let mut stream = TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, registry.port))
            .await
            .map_err(|_| anyhow!("UNITY_UNAVAILABLE"))?;
        stream
            .set_nodelay(true)
            .map_err(|_| anyhow!("UNITY_UNAVAILABLE"))?;
        write_frame(&mut stream, command).await?;
        let response = read_frame(&mut stream).await?;
        validate_response(&response, registry, request)?;
        Ok((stream, response))
    })
    .await
    .map_err(|_| anyhow!("UNITY_TIMEOUT"))?
}

pub async fn hello(registry: &Registry) -> Result<LiveEditor> {
    let (_, response) = exchange(registry, &command(registry, "hello", None, None)?, None).await?;
    if !response.ok
        || response.code != "ready"
        || !response.enabled
        || response.project_name.as_deref() != Some(&registry.project_name)
        || response.project_root.as_deref() != Some(&registry.project_root)
        || response.unity_version.as_deref() != Some(&registry.unity_version)
        || response.bridge_version != registry.bridge_version
    {
        return Err(anyhow!("UNITY_INVALID_RESPONSE"));
    }
    Ok(LiveEditor {
        instance_id: registry.instance_id.clone(),
        session_id: registry.session_id.clone(),
        project_name: registry.project_name.clone(),
        project_root: registry.project_root.clone(),
        unity_version: registry.unity_version.clone(),
        bridge_version: response.bridge_version,
        busy: response.busy,
    })
}

pub async fn status(registry: &Registry, request: &RequestIdentity) -> Result<BridgeResponse> {
    Ok(exchange(
        registry,
        &command(registry, "status", Some(request), None)?,
        Some(request),
    )
    .await?
    .1)
}

pub async fn cancel(registry: &Registry, request: &RequestIdentity) -> Result<BridgeResponse> {
    Ok(exchange(
        registry,
        &command(registry, "cancel", Some(request), None)?,
        Some(request),
    )
    .await?
    .1)
}

fn safe_relative(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 4096
        && !value.contains('\\')
        && value.split('/').all(|part| {
            if part.is_empty()
                || part == "."
                || part == ".."
                || part.ends_with('.')
                || part.ends_with(' ')
                || part
                    .chars()
                    .any(|c| c.is_control() || ":<>\"|?*".contains(c))
            {
                return false;
            }
            let stem = part
                .split('.')
                .next()
                .unwrap_or_default()
                .to_ascii_uppercase();
            !["CON", "PRN", "AUX", "NUL"].contains(&stem.as_str())
                && !(stem.len() == 4
                    && (stem.starts_with("COM") || stem.starts_with("LPT"))
                    && (b'1'..=b'9').contains(&stem.as_bytes()[3]))
        })
}

fn validate_manifest(
    registry: &Registry,
    request: &RequestIdentity,
    text: &str,
    files: &[PayloadFile],
) -> Result<Manifest> {
    let invalid = || anyhow!("UNITY_INVALID_REQUEST");
    if text.len() > MAX_CONTROL_BYTES
        || hex::encode(Sha256::digest(text.as_bytes())) != request.fingerprint
    {
        return Err(invalid());
    }
    let manifest: Manifest = serde_json::from_str(text).map_err(|_| invalid())?;
    if manifest
        .locale
        .as_deref()
        .is_some_and(|locale| !matches!(locale, "en" | "ja" | "ko" | "zh-Hans" | "zh-Hant"))
        || manifest.version != 1
        || manifest.instance_id != request.instance_id
        || manifest.instance_id != registry.instance_id
        || manifest.session_id != request.session_id
        || manifest.entries.is_empty()
        || manifest.entries.len() != files.len()
    {
        return Err(invalid());
    }
    let mut ids = BTreeSet::new();
    let mut total = 0i64;
    for (entry, file) in manifest.entries.iter().zip(files) {
        if !is_uuid(&entry.entry_id)
            || !ids.insert(entry.entry_id.to_ascii_lowercase())
            || entry.entry_id != file.entry_id
            || !["unitypackage", "file"].contains(&entry.kind.as_str())
            || entry.bytes < 0
            || !is_hash(&entry.sha256)
            || entry.display_name.is_empty()
            || entry.display_name.len() > 1024
            || entry.product_id.is_empty()
            || entry.product_id.len() > 256
            || entry.file_id.is_empty()
            || entry.file_id.len() > 256
            || entry.revision.is_empty()
            || entry.revision.len() > 1024
            || !safe_relative(&entry.relative_path)
            || (!entry.member_path.is_empty() && !safe_relative(&entry.member_path))
        {
            return Err(invalid());
        }
        total = total.checked_add(entry.bytes).ok_or_else(invalid)?;
    }
    Ok(manifest)
}

fn prepared_manifest(
    registry: &Registry,
    request: &RequestIdentity,
    manifest_json: &str,
    files: &[PayloadFile],
) -> Result<Manifest> {
    if manifest_json.len() > MAX_CONTROL_BYTES {
        return Err(anyhow!("UNITY_SELECTION_TOO_LARGE"));
    }
    let manifest = validate_manifest(registry, request, manifest_json, files)?;
    let command = command(registry, "submit", Some(request), Some(manifest_json))?;
    let bytes = serde_json::to_vec(&command).map_err(|_| anyhow!("UNITY_INVALID_REQUEST"))?;
    if bytes.len() > MAX_CONTROL_BYTES {
        return Err(anyhow!("UNITY_SELECTION_TOO_LARGE"));
    }
    Ok(manifest)
}

// Call before recording bytesStarted. Escaping manifestJson can exceed the frame bound
// even when the manifest string itself fits; no TCP request has been sent here.
pub fn preflight_submit(
    registry: &Registry,
    request: &RequestIdentity,
    manifest_json: &str,
    files: &[PayloadFile],
) -> Result<()> {
    prepared_manifest(registry, request, manifest_json, files).map(|_| ())
}

pub async fn submit(
    registry: &Registry,
    request: &RequestIdentity,
    manifest_json: &str,
    files: &[PayloadFile],
) -> Result<BridgeResponse> {
    let manifest = prepared_manifest(registry, request, manifest_json, files)?;
    let (mut stream, response) = exchange(
        registry,
        &command(registry, "submit", Some(request), Some(manifest_json))?,
        Some(request),
    )
    .await?;
    if response.code != "accepted" {
        let _ = timeout(STREAM_IDLE, stream.shutdown()).await;
        return Ok(response);
    }
    let receipt = response
        .receipt
        .as_ref()
        .ok_or_else(|| anyhow!("UNITY_INVALID_RESPONSE"))?;
    if request.session_id != registry.session_id
        || receipt.stage != "staging"
        || receipt.staged
        || receipt.writes_started
    {
        return Err(anyhow!("UNITY_INVALID_RESPONSE"));
    }
    let mut buffer = vec![0; 128 * 1024];
    for (entry, payload) in manifest.entries.iter().zip(files) {
        let mut file = timeout(STREAM_IDLE, tokio::fs::File::open(&payload.path))
            .await
            .map_err(|_| anyhow!("UNITY_TIMEOUT"))?
            .map_err(|_| anyhow!("UNITY_SOURCE_CHANGED"))?;
        let mut remaining = entry.bytes as u64;
        let mut hash = Sha256::new();
        while remaining > 0 {
            let capacity = buffer.len().min(remaining as usize);
            let count = timeout(STREAM_IDLE, file.read(&mut buffer[..capacity]))
                .await
                .map_err(|_| anyhow!("UNITY_TIMEOUT"))?
                .map_err(|_| anyhow!("UNITY_SOURCE_CHANGED"))?;
            if count == 0 {
                return Err(anyhow!("UNITY_SOURCE_CHANGED"));
            }
            hash.update(&buffer[..count]);
            write_bytes(&mut stream, &buffer[..count]).await?;
            remaining -= count as u64;
        }
        let extra = timeout(STREAM_IDLE, file.read(&mut buffer[..1]))
            .await
            .map_err(|_| anyhow!("UNITY_TIMEOUT"))?
            .map_err(|_| anyhow!("UNITY_SOURCE_CHANGED"))?;
        if extra != 0 || hex::encode(hash.finalize()) != entry.sha256 {
            let _ = timeout(STREAM_IDLE, stream.shutdown()).await;
            return Err(anyhow!("UNITY_SOURCE_CHANGED"));
        }
    }
    timeout(STREAM_IDLE, stream.shutdown())
        .await
        .map_err(|_| anyhow!("UNITY_TIMEOUT"))?
        .map_err(|_| anyhow!("UNITY_UNAVAILABLE"))?;
    let response = read_frame(&mut stream).await?;
    validate_transfer_response(&response, registry, request)?;
    Ok(response)
}

fn validate_transfer_response(
    response: &BridgeResponse,
    registry: &Registry,
    request: &RequestIdentity,
) -> Result<()> {
    validate_response(&response, registry, Some(request))?;
    // After accepted, an error envelope alone cannot prove durable staging or safe cleanup.
    if response.receipt.is_none() {
        return Err(anyhow!("UNITY_INVALID_RESPONSE"));
    }
    if response.code == "staged" && !response.receipt.as_ref().is_some_and(|r| r.staged) {
        return Err(anyhow!("UNITY_INVALID_RESPONSE"));
    }
    if !["staged", "cancelled", "failed"].contains(&response.code.as_str()) {
        return Err(anyhow!("UNITY_INVALID_RESPONSE"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};
    use tokio::net::TcpListener;

    pub(super) fn registry(port: u16) -> Registry {
        serde_json::from_value(json!({"version":1,"instanceId":"a".repeat(64),"sessionId":"11111111-1111-4111-8111-111111111111","processId":42,"projectName":"Authored","projectRoot":"F:/AuthoredUnity","unityVersion":"2022.3.62f1","host":"127.0.0.1","port":port,"capability":"b".repeat(64),"heartbeatUtc":chrono::Utc::now().to_rfc3339(),"enabled":true,"busy":false})).unwrap()
    }

    fn response(registry: &Registry, request: &RequestIdentity, code: &str) -> BridgeResponse {
        serde_json::from_value(json!({"version":1,"ok":true,"code":code,"instanceId":registry.instance_id,"sessionId":registry.session_id,"requestId":request.request_id,"receipt":{"requestId":request.request_id,"fingerprint":request.fingerprint,"instanceId":request.instance_id,"sessionId":request.session_id,"stage":if code=="accepted" {"staging"} else {"waitingForUser"},"staged":code!="accepted","writesStarted":false,"updatedUtc":chrono::Utc::now().to_rfc3339(),"error":null,"compileErrors":null,"results":null},"projectName":null,"projectRoot":null,"unityVersion":null})).unwrap()
    }

    fn manifest(registry: &Registry, bytes: &[u8]) -> (RequestIdentity, String, String) {
        let entry_id = "33333333-3333-4333-8333-333333333333".to_string();
        let text = serde_json::to_string(&Manifest {
            version: 1,
            locale: None,
            instance_id: registry.instance_id.clone(),
            session_id: registry.session_id.clone(),
            entries: vec![ManifestEntry {
                entry_id: entry_id.clone(),
                kind: "file".into(),
                display_name: "Authored.txt".into(),
                relative_path: "Authored.txt".into(),
                bytes: bytes.len() as i64,
                sha256: hex::encode(Sha256::digest(bytes)),
                product_id: "local:authored".into(),
                file_id: "authored-file".into(),
                revision: "r1".into(),
                member_path: String::new(),
            }],
        })
        .unwrap();
        let request = RequestIdentity {
            request_id: "22222222-2222-4222-8222-222222222222".into(),
            instance_id: registry.instance_id.clone(),
            session_id: registry.session_id.clone(),
            fingerprint: hex::encode(Sha256::digest(text.as_bytes())),
        };
        (request, text, entry_id)
    }

    async fn read_command(stream: &mut TcpStream) -> Value {
        let mut prefix = [0; 4];
        stream.read_exact(&mut prefix).await.unwrap();
        let mut bytes = vec![0; u32::from_le_bytes(prefix) as usize];
        stream.read_exact(&mut bytes).await.unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test]
    async fn framing_rejects_zero_oversize_invalid_utf8_and_truncated_json() {
        for bytes in [
            0u32.to_le_bytes().to_vec(),
            ((MAX_CONTROL_BYTES + 1) as u32).to_le_bytes().to_vec(),
            vec![1, 0, 0, 0, 255],
            vec![2, 0, 0, 0, b'{'],
        ] {
            let (mut writer, mut reader) = tokio::io::duplex(64);
            writer.write_all(&bytes).await.unwrap();
            writer.shutdown().await.unwrap();
            assert!(read_frame(&mut reader).await.is_err());
        }
    }

    #[test]
    fn encoded_response_preserves_absent_preparation_counters() {
        let registry = registry(9000);
        let (request, _, _) = manifest(&registry, b"authored");
        for code in ["accepted", "staged", "status"] {
            let original = response(&registry, &request, code);
            let encoded = serde_json::to_vec(&original).unwrap();
            let decoded: BridgeResponse = serde_json::from_slice(&encoded).unwrap();
            validate_response(&decoded, &registry, Some(&request)).unwrap();
        }
    }

    #[test]
    fn preparing_receipt_accepts_progress_without_changing_request_identity() {
        let registry = registry(9000);
        let (request, _, _) = manifest(&registry, b"authored");
        let mut preparing = response(&registry, &request, "staged");
        let receipt = preparing.receipt.as_mut().unwrap();
        receipt.stage = "preparing".into();
        receipt.preparation_phase = Some("readPackage".into());
        receipt.preparation_bytes = Some(50);
        receipt.preparation_total_bytes = Some(100);
        assert!(validate_transfer_response(&preparing, &registry, &request).is_ok());
        preparing.receipt.as_mut().unwrap().preparation_phase = None;
        preparing.receipt.as_mut().unwrap().preparation_waiting_for_editor = true;
        assert!(validate_transfer_response(&preparing, &registry, &request).is_ok());
        preparing.receipt.as_mut().unwrap().preparation_phase = Some("inventedPhase".into());
        assert!(validate_transfer_response(&preparing, &registry, &request).is_err());
        preparing.receipt.as_mut().unwrap().preparation_phase = Some("readPackage".into());
        preparing.receipt.as_mut().unwrap().preparation_waiting_for_editor = false;
        preparing.receipt.as_mut().unwrap().preparation_bytes = Some(101);
        assert!(validate_transfer_response(&preparing, &registry, &request).is_err());
    }

    #[test]
    fn response_binds_current_listener_and_original_receipt_identity() {
        let mut registry = registry(9000);
        let (request, _, _) = manifest(&registry, b"authored");
        registry.session_id = "44444444-4444-4444-8444-444444444444".into();
        let original = response(&registry, &request, "status");
        assert!(validate_response(&original, &registry, Some(&request)).is_ok());
        for field in ["request", "instance", "session", "fingerprint", "envelope"] {
            let mut altered = original.clone();
            let receipt = altered.receipt.as_mut().unwrap();
            match field {
                "request" => receipt.request_id = "bad".into(),
                "instance" => receipt.instance_id = "c".repeat(64),
                "session" => receipt.session_id = registry.session_id.clone(),
                "fingerprint" => receipt.fingerprint = "c".repeat(64),
                _ => altered.session_id = request.session_id.clone(),
            }
            assert!(validate_response(&altered, &registry, Some(&request)).is_err());
        }
        let mut failed_without_receipt = original.clone();
        failed_without_receipt.code = "failed".into();
        failed_without_receipt.ok = false;
        failed_without_receipt.receipt = None;
        assert!(validate_transfer_response(&failed_without_receipt, &registry, &request).is_err());
        let staged = response(&registry, &request, "staged");
        assert!(validate_transfer_response(&staged, &registry, &request).is_ok());
    }

    #[test]
    fn manifest_uses_exact_json_fingerprint_and_ordered_unique_safe_entries() {
        let registry = registry(9000);
        let (request, text, id) = manifest(&registry, b"authored");
        let files = vec![PayloadFile {
            entry_id: id,
            path: PathBuf::from("unused-authored"),
        }];
        assert!(validate_manifest(&registry, &request, &text, &files).is_ok());
        assert!(validate_manifest(&registry, &request, &format!("{text} "), &files).is_err());
        assert!(!safe_relative("../escape"));
        assert!(!safe_relative("Assets/CON.txt"));
        assert!(!safe_relative("F:/absolute"));
        assert!(!safe_relative("Assets\\file"));
    }

    #[test]
    fn preflight_counts_outer_command_escaping_before_any_payload_or_connection() {
        let registry = registry(9000);
        let (mut request, text, _) = manifest(&registry, b"");
        let mut manifest: Manifest = serde_json::from_str(&text).unwrap();
        let mut entry = manifest.entries.remove(0);
        entry.display_name = "\"".repeat(1024);
        let files: Vec<_> = (1..=4096)
            .map(|index| {
                let mut current = entry.clone();
                current.entry_id = uuid::Uuid::from_u128(index).to_string();
                let file = PayloadFile {
                    entry_id: current.entry_id.clone(),
                    path: PathBuf::from("not-read-by-preflight"),
                };
                manifest.entries.push(current);
                file
            })
            .collect();
        let text = serde_json::to_string(&manifest).unwrap();
        assert!(text.len() < MAX_CONTROL_BYTES);
        request.fingerprint = hex::encode(Sha256::digest(text.as_bytes()));
        assert_eq!(
            preflight_submit(&registry, &request, &text, &files)
                .err()
                .unwrap()
                .to_string(),
            "UNITY_SELECTION_TOO_LARGE"
        );
    }

    #[tokio::test]
    async fn existing_submit_status_sends_no_payload_even_after_listener_restart() {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let mut registry = registry(listener.local_addr().unwrap().port());
        let (request, text, id) = manifest(&registry, b"must not send");
        registry.session_id = "44444444-4444-4444-8444-444444444444".into();
        let reply = response(&registry, &request, "status");
        let task = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            assert_eq!(read_command(&mut socket).await["command"], "submit");
            write_frame(&mut socket, &reply).await.unwrap();
            let mut byte = [0];
            assert_eq!(socket.read(&mut byte).await.unwrap(), 0);
        });
        let files = [PayloadFile {
            entry_id: id,
            path: PathBuf::from("does-not-exist-no-read"),
        }];
        assert_eq!(
            submit(&registry, &request, &text, &files)
                .await
                .unwrap()
                .code,
            "status"
        );
        task.await.unwrap();
    }

    #[tokio::test]
    async fn fresh_submit_streams_large_payload_then_eof_before_durable_ack() {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let registry = registry(listener.local_addr().unwrap().port());
        let bytes = vec![0x5a; 3 * 1024 * 1024 + 7];
        let (request, text, id) = manifest(&registry, &bytes);
        let path =
            std::env::temp_dir().join(format!("nna-unity-authored-{}", uuid::Uuid::new_v4()));
        tokio::fs::write(&path, &bytes).await.unwrap();
        let accepted = response(&registry, &request, "accepted");
        let staged = response(&registry, &request, "staged");
        let task = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let command = read_command(&mut socket).await;
            assert_eq!(command["command"], "submit");
            write_frame(&mut socket, &accepted).await.unwrap();
            let mut hash = Sha256::new();
            let mut total = 0;
            let mut buffer = [0; 65536];
            loop {
                let count = socket.read(&mut buffer).await.unwrap();
                if count == 0 {
                    break;
                }
                total += count;
                hash.update(&buffer[..count]);
            }
            assert_eq!(total, bytes.len());
            assert_eq!(
                hex::encode(hash.finalize()),
                hex::encode(Sha256::digest(&bytes))
            );
            write_frame(&mut socket, &staged).await.unwrap();
            socket.shutdown().await.unwrap();
        });
        let result = submit(
            &registry,
            &request,
            &text,
            &[PayloadFile {
                entry_id: id,
                path: path.clone(),
            }],
        )
        .await;
        tokio::fs::remove_file(path).await.unwrap();
        assert!(result.unwrap().receipt.unwrap().staged);
        task.await.unwrap();
    }

    #[tokio::test]
    async fn changed_snapshot_hash_never_returns_a_staged_ack() {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let registry = registry(listener.local_addr().unwrap().port());
        let (request, text, id) = manifest(&registry, b"authored-original");
        let path =
            std::env::temp_dir().join(format!("nna-unity-authored-{}", uuid::Uuid::new_v4()));
        tokio::fs::write(&path, b"authored-modified").await.unwrap();
        let accepted = response(&registry, &request, "accepted");
        let task = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            read_command(&mut socket).await;
            write_frame(&mut socket, &accepted).await.unwrap();
            let mut received = Vec::new();
            socket.read_to_end(&mut received).await.unwrap();
            assert_eq!(received, b"authored-modified");
        });
        let result = submit(
            &registry,
            &request,
            &text,
            &[PayloadFile {
                entry_id: id,
                path: path.clone(),
            }],
        )
        .await;
        tokio::fs::remove_file(path).await.unwrap();
        assert_eq!(result.err().unwrap().to_string(), "UNITY_SOURCE_CHANGED");
        task.await.unwrap();
    }
}
