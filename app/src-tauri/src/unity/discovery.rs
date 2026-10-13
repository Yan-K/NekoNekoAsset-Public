use super::wire::{hello, is_uuid, LiveEditor, Registry, MAX_CONTROL_BYTES};
use anyhow::{anyhow, Result};
use chrono::{DateTime, Utc};
use futures_util::{stream, StreamExt};
use std::path::Path;
use tokio::io::AsyncReadExt;

// Only its public summary is eligible for a frontend DTO.
pub struct DiscoveredEditor {
    registry: Registry,
    summary: LiveEditor,
}

impl DiscoveredEditor {
    pub fn summary(&self) -> &LiveEditor {
        &self.summary
    }
    pub fn registry(&self) -> &Registry {
        &self.registry
    }
}

fn fresh(registry: &Registry, now: DateTime<Utc>) -> bool {
    let Ok(heartbeat) = DateTime::parse_from_rfc3339(&registry.heartbeat_utc) else {
        return false;
    };
    let age = now.signed_duration_since(heartbeat);
    registry.enabled && age >= chrono::Duration::zero() && age < chrono::Duration::seconds(10)
}

fn linked(metadata: &std::fs::Metadata) -> bool {
    if metadata.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 0x400 != 0 {
            return true;
        }
    }
    false
}

pub async fn discover(registry_dir: &Path) -> Result<Vec<DiscoveredEditor>> {
    let directory = match tokio::fs::symlink_metadata(registry_dir).await {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(_) => return Err(anyhow!("UNITY_REGISTRY_INVALID")),
    };
    if !directory.is_dir() || linked(&directory) {
        return Err(anyhow!("UNITY_REGISTRY_INVALID"));
    }
    let mut entries = tokio::fs::read_dir(registry_dir)
        .await
        .map_err(|_| anyhow!("UNITY_REGISTRY_INVALID"))?;
    let mut registries = Vec::new();
    while let Some(entry) = entries
        .next_entry()
        .await
        .map_err(|_| anyhow!("UNITY_REGISTRY_INVALID"))?
    {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        let Some(session) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        if !is_uuid(session) {
            continue;
        }
        let Ok(metadata) = tokio::fs::symlink_metadata(&path).await else {
            continue;
        };
        if !metadata.is_file()
            || linked(&metadata)
            || metadata.len() == 0
            || metadata.len() > MAX_CONTROL_BYTES as u64
        {
            continue;
        }
        // Atomic replacement gives either published generation; partial/invalid files are skipped.
        let Ok(file) = tokio::fs::File::open(&path).await else {
            continue;
        };
        let mut bytes = Vec::new();
        if file
            .take(MAX_CONTROL_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .await
            .is_err()
            || bytes.len() > MAX_CONTROL_BYTES
        {
            continue;
        }
        let Ok(registry) = serde_json::from_slice::<Registry>(&bytes) else {
            continue;
        };
        if registry.session_id != session
            || registry.validate().is_err()
            || !fresh(&registry, Utc::now())
        {
            continue;
        }
        registries.push(registry);
    }
    let mut live: Vec<_> = stream::iter(registries)
        .map(|registry| async move {
            let summary = hello(&registry).await.ok()?;
            if !fresh(&registry, Utc::now()) {
                return None;
            }
            Some(DiscoveredEditor { registry, summary })
        })
        .buffer_unordered(4)
        .filter_map(|editor| async move { editor })
        .collect()
        .await;
    live.sort_by(|a, b| {
        a.summary
            .instance_id
            .cmp(&b.summary.instance_id)
            .then(a.summary.session_id.cmp(&b.summary.session_id))
    });
    Ok(live)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn record(heartbeat: DateTime<Utc>, enabled: bool) -> Registry {
        serde_json::from_value(json!({"version":1,"instanceId":"a".repeat(64),"sessionId":"11111111-1111-4111-8111-111111111111","processId":42,"projectName":"Authored","projectRoot":"F:/AuthoredUnity","unityVersion":"2022.3.62f1","host":"127.0.0.1","port":9000,"capability":"b".repeat(64),"heartbeatUtc":heartbeat.to_rfc3339(),"enabled":enabled,"busy":false})).unwrap()
    }

    #[test]
    fn registry_heartbeat_enabled_and_endpoint_boundaries() {
        let now = Utc::now();
        assert!(fresh(
            &record(now - chrono::Duration::seconds(9), true),
            now
        ));
        assert!(!fresh(
            &record(now - chrono::Duration::seconds(10), true),
            now
        ));
        assert!(!fresh(
            &record(now + chrono::Duration::seconds(1), true),
            now
        ));
        assert!(!fresh(&record(now, false), now));
        let mut external = record(now, true);
        external.host = "127.0.0.2".into();
        assert!(external.validate().is_err());
        let mut versioned = record(now, true);
        versioned.bridge_version = Some("0.1.3".into());
        assert!(versioned.validate().is_ok());
        for invalid in ["", "0.1.3\n", &"x".repeat(129)] {
            versioned.bridge_version = Some(invalid.into());
            assert!(versioned.validate().is_err());
        }
    }

    #[tokio::test]
    async fn authored_registry_is_only_visible_after_matching_hello() {
        for (advertised, reported, visible) in [
            (None, None, true),
            (Some("0.1.3"), Some("0.1.3"), true),
            (Some("0.1.3"), Some("0.1.2"), false),
            (Some("0.1.3"), None, false),
        ] {
            check_hello(advertised, reported, visible).await;
        }
    }

    async fn check_hello(advertised: Option<&str>, reported: Option<&str>, visible: bool) {
        use tokio::{io::AsyncWriteExt, net::TcpListener};
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let dir = std::env::temp_dir().join(format!(
            "nna-unity-registry-authored-{}",
            uuid::Uuid::new_v4()
        ));
        tokio::fs::create_dir(&dir).await.unwrap();
        let session = "11111111-1111-4111-8111-111111111111";
        let mut value = json!({"version":1,"instanceId":"a".repeat(64),"sessionId":session,"processId":42,"projectName":"Authored","projectRoot":"F:/AuthoredUnity","unityVersion":"2022.3.62f1","host":"127.0.0.1","port":listener.local_addr().unwrap().port(),"capability":"b".repeat(64),"heartbeatUtc":Utc::now().to_rfc3339(),"enabled":true,"busy":false});
        if let Some(version) = advertised {
            value["bridgeVersion"] = json!(version);
        }
        tokio::fs::write(
            dir.join(format!("{session}.json")),
            serde_json::to_vec(&value).unwrap(),
        )
        .await
        .unwrap();
        tokio::fs::write(dir.join("ignored.tmp"), b"invalid atomic temp")
            .await
            .unwrap();
        let reported = reported.map(str::to_owned);
        let task = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut prefix = [0; 4];
            socket.read_exact(&mut prefix).await.unwrap();
            let mut body = vec![0; u32::from_le_bytes(prefix) as usize];
            socket.read_exact(&mut body).await.unwrap();
            let command: serde_json::Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(command["command"], "hello");
            let mut reply = json!({"version":1,"ok":true,"code":"ready","instanceId":"a".repeat(64),"sessionId":session,"requestId":null,"receipt":null,"projectName":"Authored","projectRoot":"F:/AuthoredUnity","unityVersion":"2022.3.62f1","enabled":true,"busy":true});
            if let Some(version) = reported {
                reply["bridgeVersion"] = json!(version);
            }
            let body = serde_json::to_vec(&reply).unwrap();
            socket
                .write_all(&(body.len() as u32).to_le_bytes())
                .await
                .unwrap();
            socket.write_all(&body).await.unwrap();
        });
        let editors = discover(&dir).await.unwrap();
        assert_eq!(editors.len(), usize::from(visible));
        if visible {
            assert!(editors[0].summary().busy);
            assert_eq!(editors[0].summary().bridge_version.as_deref(), advertised);
            let public = serde_json::to_value(editors[0].summary()).unwrap();
            assert!(public.get("capability").is_none());
            assert_eq!(public["bridgeVersion"], json!(advertised));
        }
        task.await.unwrap();
        // A fresh registry without a responding listener is not a live Editor.
        assert!(discover(&dir).await.unwrap().is_empty());
        tokio::fs::remove_dir_all(dir).await.unwrap();
    }
}
