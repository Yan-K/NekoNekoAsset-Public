//! Device-local request evidence. Activity rows are disposable projections, never dedupe authority.
use crate::catalogue::Catalogue;
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::sync::Arc;

const KIND: &str = "unity_request";

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RequestRecord {
    pub request_id: String,
    pub origin_device_id: String,
    pub fingerprint: String,
    pub instance_id: String,
    pub session_id: String,
    pub project_name: String,
    pub project_root: String,
    pub unity_version: String,
    pub product_ids: Vec<String>,
    pub manifest_json: String,
    pub phase: String,
    pub bytes_started: bool,
    pub payload_rejected: bool,
    pub snapshot_ready: bool,
    pub snapshot_removed: bool,
    pub receipt: Option<Value>,
    pub created_at: String,
    pub updated_at: String,
    pub error_code: Option<String>,
}

fn terminal(phase: &str) -> bool {
    matches!(phase, "completed" | "cancelled" | "failed" | "interrupted")
}

impl RequestRecord {
    pub(crate) fn snapshot_cleanup_proved(&self) -> bool {
        !self.bytes_started
            || self.payload_rejected
            || self.receipt.as_ref().is_some_and(|receipt| {
                receipt["staged"] == true || terminal(receipt["stage"].as_str().unwrap_or(""))
            })
    }
    fn validate(&self) -> Result<()> {
        ensure!(
            uuid::Uuid::parse_str(&self.request_id).is_ok()
                && uuid::Uuid::parse_str(&self.session_id).is_ok()
                && [&self.fingerprint, &self.instance_id, &self.origin_device_id]
                    .iter()
                    .all(|v| {
                        v.len() == 64
                            && v.bytes()
                                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
                    }),
            "unity_invalid_request"
        );
        ensure!(
            matches!(
                self.phase.as_str(),
                "preparing"
                    | "staging"
                    | "waitingForUser"
                    | "importing"
                    | "refreshing"
                    | "compiling"
                    | "completed"
                    | "cancelled"
                    | "failed"
                    | "interrupted"
                    | "unresolved"
            ),
            "unity_invalid_phase"
        );
        ensure!(
            self.phase != "unresolved" || self.error_code.is_some(),
            "unity_unresolved_reason_required"
        );
        ensure!(
            !self.payload_rejected
                || (self.phase == "failed"
                    && self.receipt.is_none()
                    && matches!(
                        self.error_code.as_deref(),
                        Some("UNITY_BUSY" | "UNITY_DISABLED" | "UNITY_INVALID_RESPONSE")
                    )),
            "unity_invalid_payload_rejection"
        );
        ensure!(
            self.phase != "completed"
                || self
                    .receipt
                    .as_ref()
                    .is_some_and(|receipt| receipt["stage"] == "completed"),
            "unity_completion_unproven"
        );
        ensure!(
            hex::encode(Sha256::digest(self.manifest_json.as_bytes())) == self.fingerprint,
            "unity_manifest_mismatch"
        );
        if !self.manifest_json.is_empty() {
            let manifest: Value = serde_json::from_str(&self.manifest_json)
                .map_err(|_| anyhow::anyhow!("unity_manifest_mismatch"))?;
            ensure!(
                manifest["instanceId"] == self.instance_id
                    && manifest["sessionId"] == self.session_id,
                "unity_manifest_mismatch"
            );
        }
        ensure!(
            self.error_code.as_ref().is_none_or(|code| !code.is_empty()
                && code.len() <= 128
                && code
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'.')),
            "unity_invalid_error_code"
        );
        if let Some(receipt) = &self.receipt {
            ensure!(
                receipt.is_object()
                    && receipt["requestId"] == self.request_id
                    && receipt["fingerprint"] == self.fingerprint
                    && receipt["instanceId"] == self.instance_id
                    && receipt["sessionId"] == self.session_id,
                "unity_receipt_mismatch"
            );
            // The transport validates full receipt content. Never retain connection credentials.
            ensure!(
                receipt.get("capability").is_none() && receipt.get("port").is_none(),
                "unity_invalid_receipt"
            );
        }
        Ok(())
    }

    fn stored(&self) -> Result<Value> {
        self.validate()?;
        let mut value = serde_json::to_value(self)?;
        value["id"] = json!(self.request_id);
        Ok(value)
    }

    fn from_stored(mut value: Value) -> Result<Self> {
        let id = value
            .as_object_mut()
            .context("unity_invalid_request")?
            .remove("id")
            .context("unity_invalid_request")?;
        let record: Self = serde_json::from_value(value)?;
        ensure!(id == record.request_id, "unity_invalid_request");
        record.validate()?;
        Ok(record)
    }

    fn same_request(&self, other: &Self) -> bool {
        let freeze = self.phase == "preparing"
            && self.manifest_json.is_empty()
            && !self.bytes_started
            && !self.snapshot_ready
            && self.receipt.is_none()
            && other.phase == "staging"
            && !other.manifest_json.is_empty();
        self.request_id == other.request_id
            && self.origin_device_id == other.origin_device_id
            && (freeze
                || (self.fingerprint == other.fingerprint
                    && self.manifest_json == other.manifest_json))
            && self.instance_id == other.instance_id
            && self.session_id == other.session_id
            && self.product_ids == other.product_ids
            && self.project_name == other.project_name
            && self.project_root == other.project_root
            && self.unity_version == other.unity_version
            && self.created_at == other.created_at
    }

    pub(super) fn presented_phase(&self) -> &str {
        // Older v1 Bridges may still wait for compilation after applying the files.
        // Keep that request active without presenting compiler state as importer work.
        if self.phase == "compiling" {
            "refreshing"
        } else {
            &self.phase
        }
    }

    pub(super) fn presented_receipt(&self) -> Option<Value> {
        self.receipt.as_ref().map(|receipt| {
            let mut projected = receipt.clone();
            let failed = receipt["results"]
                .as_array()
                .is_some_and(|results| results.iter().any(|result| result["state"] == "failed"));
            let compiler_summary = match receipt["stage"].as_str() {
                Some("completed") if !failed => {
                    receipt["error"] == "Files imported; Unity compilation reported errors."
                }
                Some("interrupted") => receipt["error"]
                    == "Files imported; Unity compilation/reload could not be confirmed. Applied files and backups remain.",
                _ => false,
            };
            if let Some(fields) = projected.as_object_mut() {
                fields.remove("compileErrors");
                if compiler_summary {
                    fields.insert("error".into(), Value::Null);
                }
                if receipt["stage"] == "compiling" {
                    fields.insert("stage".into(), json!("refreshing"));
                }
            }
            projected
        })
    }

    pub(super) fn presented_error_code(&self) -> Option<&str> {
        self.error_code
            .as_deref()
            .filter(|code| !matches!(*code, "unity_compile_errors" | "UNITY_COMPILE_ERRORS"))
    }

    fn job(&self) -> Value {
        let receipt = self.receipt.as_ref().unwrap_or(&Value::Null);
        let results = receipt["results"].as_array();
        let count = |state: &str| {
            results.map_or(0, |items| {
                items.iter().filter(|r| r["state"] == state).count()
            })
        };
        let failed = count("failed");
        let state = match self.phase.as_str() {
            "completed" if failed > 0 => "needsAction",
            "completed" => "completed",
            "cancelled" => "cancelled",
            "failed" | "interrupted" | "unresolved" => "needsAction",
            "preparing" if self.receipt.is_none() => "queued",
            _ => "running",
        };
        let error = self.presented_error_code().or(match self.phase.as_str() {
            "completed" if failed > 0 => Some("unity_partial_failure"),
            "failed" => Some("unity_import_failed"),
            "interrupted" => Some("unity_interrupted"),
            _ => None,
        });
        let mut job = json!({
            "id": self.request_id, "type":"unity:import", "requestId":self.request_id,
            "originDeviceId":self.origin_device_id,
            "productIds":self.product_ids, "projectName":self.project_name,
            "unityVersion":self.unity_version, "targetInstanceId":self.instance_id,
            "state":state, "stage":self.presented_phase(),
            "writesStarted":receipt["writesStarted"].as_bool().unwrap_or(false),
            "resultCounts":{"applied":count("applied"),"skipped":count("skipped"),"failed":failed},
            "errorCode":error,
            "createdAt":self.created_at,"updatedAt":self.updated_at
        });
        // Only safe counters are projected into Activity; complete results remain in details.
        if self.phase == "preparing" && !receipt.is_null() {
            for field in [
                "preparationPhase",
                "preparationWaitingForEditor",
                "preparationBytes",
                "preparationTotalBytes",
                "preparationEntries",
                "preparationTotalEntries",
            ] {
                if let Some(value) = receipt.get(field) {
                    job[field] = value.clone();
                }
            }
        }
        if terminal(&self.phase) {
            job["completedAt"] = json!(self.updated_at);
        }
        job
    }
}

#[derive(Clone)]
pub struct RequestStore {
    catalogue: Arc<Catalogue>,
}

impl RequestStore {
    pub fn new(catalogue: Arc<Catalogue>) -> Self {
        Self { catalogue }
    }

    pub fn device_id(&self) -> &str {
        self.catalogue.device_id()
    }

    pub fn is_local(&self, record: &RequestRecord) -> bool {
        self.catalogue.library_root().is_none() || record.origin_device_id == self.device_id()
    }

    /// Leave the origin's durable request/receipt intact for its later restart.
    pub fn mark_other_device(&self, record: &RequestRecord) -> Result<()> {
        if let Some(mut job) = self.catalogue.get("job", &record.request_id)? {
            job["state"] = json!("needsAction");
            job["stage"] = json!("interrupted");
            job["errorCode"] = json!("UNITY_OTHER_DEVICE");
            self.catalogue.put("job", &record.request_id, job)?;
        }
        Ok(())
    }

    pub(crate) fn begin_catalogue_work(&self) -> Result<crate::catalogue::CatalogueWorkPermit> {
        self.catalogue.begin_catalogue_work()
    }

    pub fn create(&self, record: &RequestRecord) -> Result<()> {
        let stored = record.stored()?;
        ensure!(
            self.catalogue.put_many_if_unchanged(
                vec![
                    (KIND.into(), record.request_id.clone(), stored),
                    ("job".into(), record.request_id.clone(), record.job())
                ],
                vec![
                    (KIND.into(), record.request_id.clone(), None),
                    ("job".into(), record.request_id.clone(), None)
                ],
            )?,
            "unity_request_exists"
        );
        Ok(())
    }

    pub fn get(&self, id: &str) -> Result<Option<RequestRecord>> {
        self.catalogue
            .get(KIND, id)?
            .map(RequestRecord::from_stored)
            .transpose()
            .context("UNITY_REQUEST_INVALID")
    }

    fn readable_records(&self) -> Result<Vec<RequestRecord>> {
        let mut records = vec![];
        for raw in self.catalogue.list(KIND)? {
            match RequestRecord::from_stored(raw.clone()) {
                Ok(record) => records.push(record),
                Err(_) => {
                    // Keep unknown/corrupt dedupe evidence intact. It must neither
                    // authorize cleanup/replay nor disable unrelated requests.
                    let Some(id) = raw["id"].as_str().filter(|id| uuid::Uuid::parse_str(id).is_ok()) else { continue };
                    for _ in 0..3 {
                        if self.catalogue.get(KIND, id)?.as_ref() != Some(&raw) { break; }
                        let Some(before) = self.catalogue.get("job", id)? else { break };
                        if before["type"] != "unity:import" || !matches!(before["state"].as_str(), Some("queued" | "running")) { break; }
                        let mut job = before.clone();
                        job["state"] = json!("needsAction");
                        job["errorCode"] = json!("UNITY_REQUEST_INVALID");
                        if self.catalogue.put_many_if_unchanged(
                            vec![("job".into(), id.into(), job)],
                            vec![(KIND.into(), id.into(), Some(raw.clone())), ("job".into(), id.into(), Some(before))],
                        )? { break; }
                    }
                }
            }
        }
        Ok(records)
    }

    pub fn pending(&self) -> Result<Vec<RequestRecord>> {
        Ok(self.readable_records()?.into_iter().filter(|record| !terminal(&record.phase)).collect())
    }

    /// Startup-only cleanup candidates; terminal receipts remain eligible.
    /// The caller owns filesystem cleanup and persists snapshot_removed after success.
    pub fn cleanup_pending(&self) -> Result<Vec<RequestRecord>> {
        Ok(self.readable_records()?.into_iter().filter(|record|
            self.is_local(record) && !record.snapshot_removed && record.snapshot_cleanup_proved()
        ).collect())
    }

    pub fn update(&self, record: &RequestRecord) -> Result<()> {
        self.project(record)
    }

    /// Receipt and any surviving Activity row commit together. An absent job stays absent.
    pub fn project(&self, record: &RequestRecord) -> Result<()> {
        let stored = record.stored()?;
        let before = self
            .catalogue
            .get(KIND, &record.request_id)?
            .context("unity_request_missing")?;
        let previous = RequestRecord::from_stored(before.clone())?;
        ensure!(previous.same_request(record), "unity_request_mismatch");
        ensure!(
            !terminal(&previous.phase) || previous.phase == record.phase,
            "unity_request_changed"
        );
        ensure!(
            (!previous.bytes_started || record.bytes_started)
                && (!previous.payload_rejected || record.payload_rejected)
                && (!previous.snapshot_ready || record.snapshot_ready)
                && (!previous.snapshot_removed || record.snapshot_removed),
            "unity_request_changed"
        );
        let mut old_job = self.catalogue.get("job", &record.request_id)?;
        for attempt in 0..2 {
            let mut records = vec![(KIND.into(), record.request_id.clone(), stored.clone())];
            if old_job.is_some() {
                records.push(("job".into(), record.request_id.clone(), record.job()));
            }
            if self.catalogue.put_many_if_unchanged(
                records,
                vec![
                    (KIND.into(), record.request_id.clone(), Some(before.clone())),
                    ("job".into(), record.request_id.clone(), old_job.clone()),
                ],
            )? {
                return Ok(());
            }
            // Retry only a concurrent Activity clear, never a different receipt update.
            ensure!(
                attempt == 0
                    && old_job.is_some()
                    && self.catalogue.get(KIND, &record.request_id)?.as_ref() == Some(&before)
                    && self.catalogue.get("job", &record.request_id)?.is_none(),
                "unity_request_changed"
            );
            old_job = None;
        }
        unreachable!()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    struct Fixture(std::path::PathBuf);
    impl Fixture {
        fn new() -> Self {
            Self(std::env::temp_dir().join(format!("nna-unity-jobs-test-{}", Uuid::new_v4())))
        }
        fn open(&self) -> Arc<Catalogue> {
            Arc::new(Catalogue::open(&self.0).unwrap())
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn record() -> RequestRecord {
        RequestRecord {
            request_id: Uuid::new_v4().to_string(),
            origin_device_id: "a".repeat(64),
            fingerprint: hex::encode(Sha256::digest(b"")),
            instance_id: "b".repeat(64),
            session_id: Uuid::new_v4().to_string(),
            project_name: "Authored Project".into(),
            project_root: "C:/authored/project".into(),
            unity_version: "6000.0".into(),
            product_ids: vec!["authored-product".into()],
            manifest_json: String::new(),
            phase: "preparing".into(),
            bytes_started: false,
            payload_rejected: false,
            snapshot_ready: false,
            snapshot_removed: false,
            receipt: None,
            created_at: "2026-10-12T00:00:00Z".into(),
            updated_at: "2026-10-12T00:00:00Z".into(),
            error_code: None,
        }
    }
    fn freeze(record: &mut RequestRecord) {
        record.manifest_json = json!({"version":1,"instanceId":record.instance_id,
            "sessionId":record.session_id,"entries":[]})
        .to_string();
        record.fingerprint = hex::encode(Sha256::digest(record.manifest_json.as_bytes()));
        record.phase = "staging".into();
        record.snapshot_ready = true;
    }
    fn receipt(record: &RequestRecord, stage: &str) -> Value {
        json!({"requestId":record.request_id,"fingerprint":record.fingerprint,
            "instanceId":record.instance_id,"sessionId":record.session_id,"stage":stage,
            "staged":true,"writesStarted":true,"results":[{"entryId":"entry","path":"Assets/Example.txt","state":"applied","message":""}],"compileErrors":[]})
    }

    #[test]
    fn unreadable_request_is_preserved_without_blocking_other_requests() {
        let fixture = Fixture::new();
        let c = fixture.open();
        let store = RequestStore::new(c.clone());
        let valid = record();
        store.create(&valid).unwrap();
        let mut invalid = record();
        invalid.request_id = Uuid::new_v4().to_string();
        store.create(&invalid).unwrap();
        let mut raw = c.get(KIND, &invalid.request_id).unwrap().unwrap();
        raw["futureField"] = json!(true);
        c.put(KIND, &invalid.request_id, raw.clone()).unwrap();
        let pending = store.pending().unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].request_id, valid.request_id);
        assert_eq!(store.cleanup_pending().unwrap().len(), 1);
        assert_eq!(c.get(KIND, &invalid.request_id).unwrap(), Some(raw));
        let job = c.get("job", &invalid.request_id).unwrap().unwrap();
        assert_eq!(job["state"], "needsAction");
        assert_eq!(job["errorCode"], "UNITY_REQUEST_INVALID");
        store.pending().unwrap();
        assert_eq!(c.get("job", &invalid.request_id).unwrap(), Some(job));
        assert!(format!("{:#}", store.get(&invalid.request_id).err().unwrap()).contains("UNITY_REQUEST_INVALID"));
        store.create(&record()).unwrap();
        assert_eq!(store.pending().unwrap().len(), 2);
    }

    #[test]
    fn another_profile_does_not_resume_or_clean_the_origin_request() {
        let base = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join(format!("nna-authored-unity-handoff-{}", Uuid::new_v4()));
        let library = base.join("library");
        std::fs::create_dir_all(&library).unwrap();
        let origin = Arc::new(Catalogue::open_bound(&library, &base.join("origin")).unwrap());
        let origin_id = origin.device_id().to_owned();
        let origin_store = RequestStore::new(origin.clone());
        let mut request = record();
        request.origin_device_id = origin_id;
        freeze(&mut request);
        origin_store.create(&request).unwrap();
        let original = origin.get(KIND, &request.request_id).unwrap().unwrap();
        drop(origin_store);
        drop(origin);
        let other = Arc::new(Catalogue::open_bound(&library, &base.join("other")).unwrap());
        let other_store = RequestStore::new(other.clone());
        let foreign = other_store.pending().unwrap().pop().unwrap();
        assert!(!other_store.is_local(&foreign));
        assert!(other_store.cleanup_pending().unwrap().is_empty());
        other_store.mark_other_device(&foreign).unwrap();
        assert_eq!(
            other.get(KIND, &request.request_id).unwrap(),
            Some(original.clone())
        );
        let job = other.get("job", &request.request_id).unwrap().unwrap();
        assert_eq!(job["state"], "needsAction");
        assert_eq!(job["errorCode"], "UNITY_OTHER_DEVICE");
        drop(other_store);
        drop(other);
        let returned = Arc::new(Catalogue::open_bound(&library, &base.join("origin")).unwrap());
        let returned_store = RequestStore::new(returned.clone());
        let pending = returned_store.pending().unwrap().pop().unwrap();
        assert!(returned_store.is_local(&pending));
        assert_eq!(
            returned.get(KIND, &request.request_id).unwrap(),
            Some(original)
        );
        drop(returned_store);
        drop(returned);
        std::fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn terminal_receipts_allow_snapshot_cleanup_without_a_staged_ack() {
        let fixture = Fixture::new();
        let c = fixture.open();
        let store = RequestStore::new(c.clone());
        for stage in ["cancelled", "failed", "interrupted", "completed"] {
            let mut r = record();
            freeze(&mut r);
            r.bytes_started = true;
            store.create(&r).unwrap();
            assert!(!r.snapshot_cleanup_proved());
            r.phase = stage.into();
            let mut proof = receipt(&r, stage);
            proof["staged"] = json!(false);
            r.receipt = Some(proof);
            store.update(&r).unwrap();
            assert!(r.snapshot_cleanup_proved());
        }
        assert_eq!(store.cleanup_pending().unwrap().len(), 4);
        drop(store);
        drop(c);
        let reopened = RequestStore::new(fixture.open());
        assert_eq!(reopened.cleanup_pending().unwrap().len(), 4);
    }

    #[test]
    fn cleared_activity_does_not_erase_request_or_reappear_after_late_result() {
        let fixture = Fixture::new();
        let c = fixture.open();
        let store = RequestStore::new(c.clone());
        let mut r = record();
        store.create(&r).unwrap();
        assert!(store.create(&r).is_err());
        assert_eq!(store.cleanup_pending().unwrap().len(), 1);
        freeze(&mut r);
        r.bytes_started = true;
        store.update(&r).unwrap();
        r.phase = "unresolved".into();
        r.error_code = Some("unity_target_unavailable".into());
        store.update(&r).unwrap();
        assert_eq!(store.pending().unwrap().len(), 1);
        assert!(store.cleanup_pending().unwrap().is_empty());
        assert_eq!(
            c.command("clear_activity", json!({})).unwrap()["cleared"],
            1
        );
        r.phase = "completed".into();
        r.error_code = None;
        r.receipt = Some(receipt(&r, "completed"));
        store.update(&r).unwrap();
        assert!(c.get("job", &r.request_id).unwrap().is_none());
        assert!(store.pending().unwrap().is_empty());
        // A locked cleanup may leave the terminal proof durable and retryable.
        assert_eq!(store.cleanup_pending().unwrap().len(), 1);
        assert_eq!(
            store.get(&r.request_id).unwrap().unwrap().phase,
            "completed"
        );
        drop(store);
        drop(c);
        let c = fixture.open();
        let store = RequestStore::new(c.clone());
        assert_eq!(store.cleanup_pending().unwrap().len(), 1);
        r.snapshot_removed = true;
        store.update(&r).unwrap();
        assert!(store.cleanup_pending().unwrap().is_empty());
        assert_eq!(
            store.get(&r.request_id).unwrap().unwrap().receipt.unwrap()["results"][0]["state"],
            "applied"
        );
        assert!(c.get("job", &r.request_id).unwrap().is_none());
    }

    #[test]
    fn request_and_job_stay_out_of_sync_and_portable_manifest() {
        let fixture = Fixture::new();
        let c = fixture.open();
        let store = RequestStore::new(c.clone());
        let r = record();
        store.create(&r).unwrap();
        let manifest = c.manifest().unwrap();
        let serialized = serde_json::to_string(&manifest).unwrap();
        assert!(!serialized.contains(&r.request_id));
        assert!(!serialized.contains("unity_request"));
        let conn = rusqlite::Connection::open(fixture.0.join("catalogue.sqlite3")).unwrap();
        assert_eq!(
            conn.query_row(
                "SELECT COUNT(*) FROM sync_outbox WHERE kind IN ('job','unity_request')",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
    }

    #[test]
    fn activity_reports_actual_receipt_outcome_without_full_results() {
        let mut r = record();
        assert_eq!(r.job()["state"], "queued");
        r.phase = "completed".into();
        assert_eq!(
            r.stored().unwrap_err().to_string(),
            "unity_completion_unproven"
        );
        freeze(&mut r);
        for phase in [
            "staging",
            "preparing",
            "waitingForUser",
            "importing",
            "refreshing",
            "compiling",
        ] {
            r.phase = phase.into();
            r.receipt = Some(receipt(&r, phase));
            assert_eq!(r.job()["state"], "running");
            assert_eq!(r.job()["writesStarted"], true);
            assert_eq!(
                r.job()["stage"],
                if phase == "compiling" {
                    "refreshing"
                } else {
                    phase
                }
            );
        }
        r.phase = "preparing".into();
        r.receipt = Some(receipt(&r, "preparing"));
        r.receipt.as_mut().unwrap()["preparationPhase"] = json!("readPackage");
        r.receipt.as_mut().unwrap()["preparationBytes"] = json!(50);
        r.receipt.as_mut().unwrap()["preparationTotalBytes"] = json!(100);
        assert_eq!(r.job()["preparationBytes"], 50);
        assert_eq!(r.job()["preparationPhase"], "readPackage");
        assert!(r.job().get("receipt").is_none());
        r.phase = "completed".into();
        r.receipt = Some(receipt(&r, "completed"));
        assert_eq!(r.job()["state"], "completed");
        r.receipt.as_mut().unwrap()["compileErrors"] = json!(["authored compile failure"]);
        assert_eq!(r.job()["state"], "completed");
        assert!(r.job()["errorCode"].is_null());
        assert!(r.job().get("compileErrorCount").is_none());
        assert_eq!(r.job()["resultCounts"]["applied"], 1);
        assert!(r.job().get("receipt").is_none());
        assert!(r.job().get("manifestJson").is_none());
        // Compiler diagnostics must not hide a real failure in the same receipt.
        r.receipt.as_mut().unwrap()["results"][0]["state"] = json!("failed");
        assert_eq!(r.job()["state"], "needsAction");
        assert_eq!(r.job()["errorCode"], "unity_partial_failure");
        for phase in ["failed", "interrupted"] {
            r.phase = phase.into();
            assert_eq!(r.job()["state"], "needsAction");
        }
        r.phase = "cancelled".into();
        assert_eq!(r.job()["state"], "cancelled");
    }

    #[test]
    fn legacy_compiler_diagnostics_are_not_presented_or_rewritten() {
        let mut r = record();
        freeze(&mut r);
        r.phase = "completed".into();
        r.error_code = Some("unity_compile_errors".into());
        r.receipt = Some(receipt(&r, "completed"));
        r.receipt.as_mut().unwrap()["compileErrors"] = json!(["authored compiler error"]);
        r.receipt.as_mut().unwrap()["error"] =
            json!("Files imported; Unity compilation reported errors.");
        let stored = r.stored().unwrap();
        let projected = r.presented_receipt().unwrap();
        assert!(projected.get("compileErrors").is_none());
        assert!(projected["error"].is_null());
        assert_eq!(projected["results"][0]["state"], "applied");
        assert_eq!(r.job()["state"], "completed");
        assert!(r.job()["errorCode"].is_null());
        assert_eq!(r.stored().unwrap(), stored);

        r.receipt.as_mut().unwrap()["error"] = json!("An authored import failure summary");
        assert_eq!(
            r.presented_receipt().unwrap()["error"],
            "An authored import failure summary"
        );
        r.receipt.as_mut().unwrap()["results"][0]["state"] = json!("failed");
        assert_eq!(r.job()["state"], "needsAction");
        assert_eq!(r.job()["errorCode"], "unity_partial_failure");
        assert_eq!(
            r.presented_receipt().unwrap()["results"][0]["state"],
            "failed"
        );

        r.phase = "interrupted".into();
        r.receipt.as_mut().unwrap()["stage"] = json!("interrupted");
        r.receipt.as_mut().unwrap()["results"][0]["state"] = json!("applied");
        r.receipt.as_mut().unwrap()["error"] = json!(
            "Files imported; Unity compilation/reload could not be confirmed. Applied files and backups remain."
        );
        assert_eq!(r.job()["state"], "needsAction");
        assert_eq!(r.job()["errorCode"], "unity_interrupted");
        assert!(r.presented_receipt().unwrap()["error"].is_null());

        r.phase = "compiling".into();
        r.receipt.as_mut().unwrap()["stage"] = json!("compiling");
        assert_eq!(r.presented_phase(), "refreshing");
        assert_eq!(r.presented_receipt().unwrap()["stage"], "refreshing");
        assert_eq!(r.receipt.as_ref().unwrap()["stage"], "compiling");
    }

    #[test]
    fn preparation_freezes_one_bound_manifest_and_never_rewrites_it() {
        let fixture = Fixture::new();
        let store = RequestStore::new(fixture.open());
        let mut r = record();
        store.create(&r).unwrap();
        freeze(&mut r);
        let mut wrong = r.clone();
        wrong.manifest_json.push(' ');
        assert_eq!(
            store.update(&wrong).unwrap_err().to_string(),
            "unity_manifest_mismatch"
        );
        wrong.fingerprint = hex::encode(Sha256::digest(wrong.manifest_json.as_bytes()));
        wrong.session_id = Uuid::new_v4().to_string();
        assert_eq!(
            store.update(&wrong).unwrap_err().to_string(),
            "unity_manifest_mismatch"
        );
        store.update(&r).unwrap();
        r.phase = "waitingForUser".into();
        store.update(&r).unwrap();
        let frozen = r.manifest_json.clone();
        r.manifest_json.push(' ');
        r.fingerprint = hex::encode(Sha256::digest(r.manifest_json.as_bytes()));
        assert_eq!(
            store.update(&r).unwrap_err().to_string(),
            "unity_request_mismatch"
        );
        assert_eq!(
            store.get(&r.request_id).unwrap().unwrap().manifest_json,
            frozen
        );
        // Resetting phase/flags cannot reopen the preparation exception.
        r.phase = "preparing".into();
        r.snapshot_ready = false;
        assert!(store.update(&r).is_err());
    }

    #[test]
    fn initial_payload_rejection_preserves_owned_cleanup_proof() {
        let fixture = Fixture::new();
        let store = RequestStore::new(fixture.open());
        let mut r = record();
        store.create(&r).unwrap();
        freeze(&mut r);
        r.bytes_started = true;
        store.update(&r).unwrap();
        assert!(store.cleanup_pending().unwrap().is_empty());
        r.phase = "failed".into();
        r.error_code = Some("UNITY_BUSY".into());
        r.payload_rejected = true;
        store.update(&r).unwrap();
        assert_eq!(store.cleanup_pending().unwrap().len(), 1);
        let mut incompatible = r.clone();
        incompatible.receipt = Some(receipt(&r, "staging"));
        assert_eq!(
            store.update(&incompatible).unwrap_err().to_string(),
            "unity_invalid_payload_rejection"
        );
        incompatible = r.clone();
        incompatible.error_code = Some("UNITY_TIMEOUT".into());
        assert_eq!(
            store.update(&incompatible).unwrap_err().to_string(),
            "unity_invalid_payload_rejection"
        );
        incompatible = r.clone();
        incompatible.payload_rejected = false;
        assert_eq!(
            store.update(&incompatible).unwrap_err().to_string(),
            "unity_request_changed"
        );
        r.snapshot_removed = true;
        store.update(&r).unwrap();
        assert!(store.cleanup_pending().unwrap().is_empty());
    }
}
