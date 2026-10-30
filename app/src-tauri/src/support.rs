//! Manually copied, local support reports. Never serialize source records wholesale.
//!
//! The caller owns account/run selection. This module projects even previously
//! captured diagnostics again: no IDs, arbitrary strings, dates, amounts, paths,
//! URLs, headers, source bodies or hashed substitutes cross this boundary.
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;

const MAX_COUNT: u64 = 1_000_000_000;
const MAX_REPORT_BYTES: usize = 16 * 1024;
const SOURCES: &[&str] = &[
    "library", "purchase", "gift", "free", "unarchived", "archived", "inventory", "bundle",
];
const TRACE_SOURCES: &[&str] = &["library", "purchase", "gift", "free"];
const HTTP_SCOPES: &[&str] = &["library", "purchase", "gift", "free", "receipt", "public", "other"];
const PROGRESS_COUNTS: &[&str] = &[
    "pages", "detailsCompleted", "requestsCompleted", "products", "published",
    "metadataCompleted", "metadataTotal", "remaining", "unresolvedRows", "availabilityWarnings",
];
const COVERAGE_COUNTS: &[&str] = &[
    "pages", "detailsCompleted", "requestsCompleted", "unresolvedRows", "productCount", "availabilityWarnings",
];
const COVERAGE_FLAGS: &[&str] = &[
    "coverageComplete", "terminalSeen", "historicalComplete", "giftEventLedgerComplete",
];
const HTTP_COUNTS: &[&str] = &[
    "requests", "retries", "status2xx", "status3xx", "status4xx", "status5xx",
    "rateLimits", "transportFailures", "timeouts", "redirectRejected", "backoffs", "backoffMs",
];
const PROVIDER_METRIC_COUNTS: &[&str] = &[
    "requests", "pacingWaitMs", "responseWaitMs", "activeRequestLimit", "startSpacingMs",
];
// Exact fields and kinds emitted by stores/http.rs::classified_since.
const KIND_METRIC_COUNTS: &[&str] = &[
    "requests", "pacingWaitMs", "responseWaitMs", "bodyReadMs", "retries",
    "rateLimits", "serverErrors", "transportErrors", "clientErrors",
];
const FINANCIAL_COUNTS: &[&str] = &[
    "receiptPages", "digitalBoundItems", "uniqueMetadataItems", "unitPriceItems",
    "explicitQuantityItems", "omittedQuantityItems", "explicitBoostItems",
    "productLineProofItems", "ambiguousItems", "rows", "rowsWithPaidProof",
    "rowsWithDateProof", "proofRejected", "htmlJa", "htmlEn", "htmlKo", "htmlZhCn", "htmlZhTw", "htmlOther", "htmlUnknown",
    "digitalControlBindingUnprovenItems", "metadataShapeUnrecognizedItems", "unitPriceUnrecognizedItems",
    "quantityUnrecognizedItems", "boostUnrecognizedItems", "dateProofUnavailableRows",
];

// Each accepted string is returned as a static literal, never as an input slice.
macro_rules! closed {
    ($value:expr, $fallback:literal; $($known:literal),+ $(,)?) => {
        match $value { $($known => $known,)+ _ => $fallback }
    };
}

pub(crate) fn safe_code(value: &str) -> &'static str {
    closed!(value, "UNKNOWN_ERROR";
        "ACCOUNT_BUSY", "ACCOUNT_UNAVAILABLE", "ACCOUNT_IDENTITY_MISMATCH",
        "AUTH_CHALLENGE", "AUTH_ISOLATION_UNAVAILABLE", "AUTH_REQUIRED", "AUTH_TIMEOUT",
        "AUTH_WINDOW_FAILED", "AUTH_SESSION_STORAGE_FAILED", "COOKIE_ACCESS_FAILED",
        "BOOTH_ALREADY_RUNNING", "BOOTH_DEVICE_MISMATCH", "BOOTH_LOCK_UNAVAILABLE",
        "BOOTH_OPERATION_FAILED", "BOOTH_WITHHELD_PRODUCT_ID", "CANCELLED",
        "CHECKPOINT_INVALID", "COLLECTOR_UNSUPPORTED", "CONCURRENT_UPDATE",
        "DIGITAL_CONTROLS_UNRESOLVED", "DIGITAL_PROOF_UNPROVEN", "DOWNLOAD_LINK_UNAVAILABLE",
        "DOWNLOAD_RESOLVER_FAILED", "ENTITLEMENT_INVALID", "ENTITLEMENT_UNAVAILABLE",
        "FILE_CHANGED", "FILE_INCOMPLETE", "FILE_UNAVAILABLE", "FREE_FILE_CONTROLS_UNAVAILABLE",
        "FREE_ITEM_CONFLICTING_ENTRIES", "FREE_ITEM_COUNT_UNPROVEN", "FREE_ITEM_DYNAMIC_LIST",
        "FREE_ITEM_ENTRY_UNPROVEN", "FREE_ITEM_FILENAME_UNPROVEN", "FREE_ITEM_HISTORY_UNPROVEN",
        "FREE_ITEM_IDENTITY_UNPROVEN", "FREE_ITEM_INVALID_CONTROL", "FREE_ITEM_INVALID",
        "FREE_ITEM_LIST_UNPROVEN", "FREE_ITEM_ORPHAN_CONTROL", "FREE_ITEM_ORPHAN_ENTRY",
        "FREE_ITEM_ROLE_UNPROVEN", "FREE_ITEM_UNKNOWN_DROPDOWN", "FREE_OFFER_UNAVAILABLE",
        "HTTP_CLIENT_FAILED", "IMPORT_TOO_LARGE", "INVALID_ACCOUNT_ACTION", "INVALID_ACCOUNT_ID",
        "INVALID_ACCOUNT_NAME", "INVALID_ARGUMENT", "INVALID_AUTH_URL", "INVALID_FREE_ITEM_ID",
        "INVALID_JOB", "INVALID_METADATA_URL", "INVALID_PRODUCT_ID", "INVALID_PRODUCT_IDS",
        "INVALID_PRODUCT_URL", "INVALID_RESOLVER", "INVALID_SOURCE_ROUTE", "INVALID_SOURCE",
        "INVALID_SYNC_MODE", "JOB_ACTIVE", "JOB_BUSY", "JOB_UNAVAILABLE", "METADATA_ENCODING",
        "METADATA_TOO_LARGE", "NETWORK_UNAVAILABLE", "NETWORK_TIMEOUT", "NO_RECOGNIZED_ROWS", "PERMISSION_DENIED",
        "PRODUCT_ID_UNRESOLVED", "PRODUCT_INVALID", "PRODUCT_TITLE_UNRESOLVED", "PRODUCT_UNAVAILABLE",
        "PROFILE_CLEAR_FAILED", "PROFILE_UNAVAILABLE", "PUBLIC_ID_MISMATCH", "PUBLIC_METADATA_FAILED",
        "PUBLIC_METADATA_INVALID", "PUBLIC_METADATA_UNAVAILABLE", "PUBLIC_SOURCE_HOST_MISMATCH",
        "RATE_LIMITED", "REDIRECT_LIMIT", "RESPONSE_ROUTE_CHANGED", "RETRY_REQUIRED", "ROW_UNPROVEN",
        "SESSION_CACHE_INVALID", "SESSION_CACHE_UNAVAILABLE", "SIGNED_LINK_EXPIRED", "SOURCE_CHANGED",
        "SOURCE_HTTP_FAILED", "SOURCE_MARKER_UNKNOWN", "SOURCE_TRAVERSAL_LIMIT", "SOURCE_UNSUPPORTED",
        "SOURCE_TERMINAL_UNPROVEN", "SOURCE_HEAD_CHANGED", "SOURCE_MEMBERSHIP_UNBOUND", "SOURCE_MANIFEST_INCOMPLETE",
        "STATE_LOCK_FAILED", "STORE_ACCOUNT_MISMATCH", "STORE_DEVICE_MISMATCH", "STORE_IDENTITY_CHANGED",
        "STORE_UNAVAILABLE", "SYNC_INCOMPLETE", "TERMINAL_WRITE_FAILED", "UNKNOWN_BOOTH_ACTION",
        "UNKNOWN_STORE_ACTION", "UNSAFE_DOWNLOAD_URL", "UNSAFE_REDIRECT", "UNSUPPORTED_PROVIDER",
        "VERIFICATION_PAGE_LIMIT", "GUMROAD_ALREADY_RUNNING", "GUMROAD_BUNDLE_OWNERSHIP_UNPROVEN",
        "GUMROAD_CONTENT_UNAVAILABLE", "GUMROAD_DEVICE_MISMATCH", "GUMROAD_FILE_BINDING_UNPROVEN",
        "GUMROAD_IDENTITY_UNPROVEN", "GUMROAD_INVENTORY_TOO_LARGE", "GUMROAD_INVENTORY_UNPROVEN",
        "GUMROAD_LIBRARY_CHANGED", "GUMROAD_LIBRARY_UNAVAILABLE", "GUMROAD_LOCK_UNAVAILABLE",
        "GUMROAD_METADATA_TOO_LARGE", "GUMROAD_OPERATION_FAILED", "GUMROAD_PAGINATION_UNPROVEN",
        "GUMROAD_PRODUCT_BINDING_UNPROVEN", "GUMROAD_PUBLIC_METADATA_UNAVAILABLE",
        "GUMROAD_PUBLIC_SCHEMA_UNSUPPORTED", "GUMROAD_PURCHASE_AMBIGUOUS", "GUMROAD_ROUTE_UNSUPPORTED",
        "GUMROAD_SCOPE_CHANGED", "GUMROAD_SOURCE_AMBIGUOUS", "GUMROAD_SOURCE_UNSUPPORTED",
        "JINXXY_COMPILED_DOWNLOAD_UNAVAILABLE", "JINXXY_DOWNLOAD_UNAVAILABLE", "JINXXY_DOWNLOAD_UNPROVEN",
        "JINXXY_DUPLICATE_FILE", "JINXXY_DUPLICATE_INVENTORY", "JINXXY_FILE_PATH_UNSUPPORTED",
        "JINXXY_IDENTITY_UNPROVEN", "JINXXY_MANIFEST_INCOMPLETE", "JINXXY_METADATA_TOO_LARGE",
        "JINXXY_PAGINATION_UNPROVEN", "JINXXY_PRODUCT_BINDING_UNPROVEN", "JINXXY_RECEIPT_UNAVAILABLE",
        "JINXXY_SCOPE_CHANGED", "JINXXY_SOURCE_UNAVAILABLE", "JINXXY_SOURCE_UNSUPPORTED",
        "JINXXY_SYNC_FAILED", "JINXXY_TRANSPORT_UNSUPPORTED",
        "cancelled", "root_offline", "root_unavailable", "root_unconfigured", "low_space",
        "auth_required", "permission_denied", "link_expired", "rate_limited", "network_failed",
        "network_timeout", "source_changed", "source_tree_incomplete", "download_size_mismatch",
        "download_resolver_failed", "download_http_failed", "manifest_changed", "manifest_incomplete",
        "file_missing", "file_integrity_failed", "file_not_usable", "original_unavailable",
        "archive_password_required", "archive_missing_volume", "archive_corrupt", "archive_unsafe_path",
        "archive_unsafe_link", "archive_path_collision", "archive_tool_unavailable", "revision_unknown",
        "unsupported_backup_version", "backup_payload_missing", "destination_not_empty", "storage_busy",
        "storage_failed", "local_membership_changed", "local_product_required", "local_delete_unsafe_path",
        "local_delete_shared_path", "local_delete_cleanup_failed", "local_delete_rollback_failed",
        "local_delete_retry_required", "library_not_open", "library_not_configured", "library_opening",
        "worker_failed", "access_denied", "operation_failed", "operationFailed", "nativeUnavailable",
        "clipboard_unavailable", "invalid_clipboard_text",
        "UNITY_BUSY", "UNITY_DISABLED", "UNITY_INTERRUPTED", "UNITY_INVALID_REQUEST", "UNITY_INVALID_RESPONSE",
        "UNITY_OTHER_DEVICE", "UNITY_REGISTRY_INVALID", "UNITY_REGISTRY_UNAVAILABLE", "UNITY_REQUEST_MISSING",
        "UNITY_RESULT_UNRESOLVED", "UNITY_RETRY_UNAVAILABLE", "UNITY_SELECTION_EMPTY",
        "UNITY_SELECTION_TOO_LARGE", "UNITY_SOURCE_CHANGED", "UNITY_TIMEOUT", "UNITY_UNAVAILABLE",
        "unity_compile_errors", "unity_completion_unproven", "unity_import_failed", "unity_interrupted",
        "unity_invalid_error_code", "unity_invalid_payload_rejection", "unity_invalid_phase",
        "unity_invalid_receipt", "unity_manifest_mismatch", "unity_partial_failure", "unity_receipt_mismatch",
        "unity_request_changed", "unity_request_exists", "unity_request_mismatch", "unity_target_unavailable",
        "unity_unresolved_reason_required", "AI_INPUT_STALE", "AI_INVALID_ANALYSIS", "AI_INVALID_SOURCE_URL",
        "ai_auth_failed", "ai_cancelled", "ai_config_changed", "ai_features_disabled", "ai_image_invalid",
        "ai_image_missing", "ai_image_unavailable", "ai_images_unavailable", "ai_interrupted", "ai_job_missing",
        "ai_key_invalid", "ai_key_missing", "ai_malformed", "ai_network", "ai_partial_failure",
        "ai_permission_denied", "ai_quota_exceeded", "ai_rate_limited", "ai_refused", "ai_scope_empty",
        "ai_secure_storage_unavailable", "ai_selection_invalid", "ai_spend_limit_exceeded", "ai_tag_limit",
        "ai_test_required", "ai_test_stale", "ai_timeout", "ai_unavailable", "ai_vision_unsupported",
        "updates_busy", "updates_download_failed", "updates_install_failed", "updates_invalid_manifest",
        "updates_signature_invalid", "updates_stale", "updates_unavailable",
        "catalogue.lock", "catalogue.newer_schema", "catalogue.invalid_record", "catalogue.entity_missing",
        "catalogue.product_missing", "catalogue.invalid_avatar_selection", "catalogue.invalid_avatar_delta",
        "catalogue.invalid_selection", "catalogue.empty_selection", "catalogue.invalid_patch",
        "catalogue.invalid_preferences", "catalogue.invalid_overlay_field", "catalogue.undo_missing"
    )
}

pub(crate) fn safe_stage(value: &str) -> &'static str {
    closed!(value, "unknown";
        "queued", "running", "collecting", "enriching", "publishing", "finalizing", "authenticating", "cookieAccess", "httpSuccess", "parserRecognition",
        "preparing", "resolving", "downloading", "verifying", "extracting", "importing", "refreshing",
        "compiling", "waitingForUser", "checkTargets", "copyInputs", "readPackage", "snapshotInputs",
        "authRequired", "completed", "complete", "failed", "needsAction", "cancelled", "interrupted", "unresolved"
    )
}

pub(crate) fn operation_stage(value: &str) -> &'static str {
    match safe_stage(value) {
        "queued" | "running" | "completed" | "complete" | "failed" | "needsAction" | "cancelled" | "interrupted" | "unresolved" => "unknown",
        stage => stage,
    }
}

/// A retry keeps business checkpoints/counters, but never borrows diagnostics
/// from the preceding attempt if authentication or queue acquisition fails.
pub(crate) fn reset_attempt(job: &mut Value) {
    if let Some(object) = job.as_object_mut() {
        for key in ["supportDiagnostics", "supportLastStage", "supportCapture", "requestMetrics"] { object.remove(key); }
    }
    if let Some(progress) = job["progress"].as_object_mut() {
        for key in ["loginStage", "lastErrorCode"] { progress.remove(key); }
    }
    if matches!(job["type"].as_str(), Some("boothSync" | "boothUpdate" | "boothLogin")) {
        job["supportCapture"] = json!({"version":1,"state":"queued"});
    }
}

pub(crate) fn begin_attempt(job: &mut Value) {
    reset_attempt(job);
    job["supportCapture"] = json!({"version":1,"state":"running",
        "executionAppVersion":env!("CARGO_PKG_VERSION")});
}

pub(crate) fn end_attempt(job: &mut Value) {
    if job["supportCapture"]["version"].as_u64() == Some(1) {
        job["supportCapture"]["state"] = json!("finished");
    }
}

fn safe_type(value: &str) -> &'static str {
    closed!(value, "unknown";
        "boothSync", "boothUpdate", "boothLogin", "gumroadSync", "gumroadUpdate", "gumroadLogin",
        "jinxxySync", "jinxxyUpdate", "jinxxyLogin", "download", "storage:import_local",
        "storage:delete_local_products", "storage:migrate_root", "storage:export_backup",
        "storage:restore_backup", "storage:export_zip", "storage:verify", "storage:extract",
        "ai:classify", "ai:organize", "unity:import"
    )
}

fn safe_state(value: &str) -> &'static str {
    closed!(value, "unknown"; "queued", "running", "completed", "failed", "needsAction", "cancelled")
}

fn safe_status(value: &str) -> &'static str {
    closed!(value, "unknown";
        "queued", "running", "collected", "complete", "completed", "incomplete", "failed",
        "cancelled", "auth-required", "unsupported", "needsAction"
    )
}

fn string(value: &Value) -> &str {
    value.as_str().unwrap_or("")
}

fn bounded_count(value: &Value) -> Value {
    value.as_u64().filter(|n| *n <= MAX_COUNT).map(Value::from).unwrap_or(Value::Null)
}

fn counts(input: &Value, keys: &[&str]) -> Map<String, Value> {
    keys.iter().filter_map(|key| input.get(*key).map(|value| ((*key).into(), bounded_count(value)))).collect()
}

fn flags(input: &Value, output: &mut Map<String, Value>, keys: &[&str]) {
    for key in keys {
        if let Some(value) = input.get(*key) {
            output.insert((*key).into(), value.as_bool().map(Value::from).unwrap_or(Value::Null));
        }
    }
}

fn code_field(input: &Value, output: &mut Map<String, Value>) {
    if let Some(value) = input.get("errorCode") {
        output.insert("errorCode".into(), if value.is_null() { Value::Null } else { json!(safe_code(string(value))) });
    }
}

fn provider(job: &Value) -> &'static str {
    match string(&job["type"]) {
        "boothSync" | "boothUpdate" | "boothLogin" => "BOOTH",
        "gumroadSync" | "gumroadUpdate" | "gumroadLogin" => "Gumroad",
        "jinxxySync" | "jinxxyUpdate" | "jinxxyLogin" => "Jinxxy",
        _ => closed!(string(&job["store"]), "unknown"; "BOOTH", "Gumroad", "Jinxxy"),
    }
}

fn failure_counts(job: &Value) -> Value {
    let mut codes = BTreeMap::<&'static str, u64>::new();
    let mut omitted = 0u64;
    if let Some(failures) = job["failures"].as_object() {
        // Keys are private file/product identities and are never inspected or copied.
        for failure in failures.values() {
            let value = failure.get("errorCode").unwrap_or(failure);
            let code = safe_code(string(value));
            if codes.contains_key(code) || codes.len() < 16 {
                let count = codes.entry(code).or_default();
                *count = count.saturating_add(1).min(MAX_COUNT);
            } else {
                omitted = omitted.saturating_add(1).min(MAX_COUNT);
            }
        }
    }
    json!({"byCode":codes,"omittedCount":omitted})
}

fn provider_metrics(input: &Value) -> Option<Value> {
    if input["scope"].as_str() != Some("providerCollectionInterval") {
        return None;
    }
    let mut result = counts(input, PROVIDER_METRIC_COUNTS);
    result.insert("scope".into(), json!("providerCollectionInterval"));
    if input.get("byKind").is_some() {
        let mut kinds = Map::new();
        for kind in ["content", "public"] {
            if input["byKind"][kind].is_object() {
                kinds.insert(kind.into(), Value::Object(counts(&input["byKind"][kind], KIND_METRIC_COUNTS)));
            }
        }
        result.insert("byKind".into(), Value::Object(kinds));
    }
    Some(Value::Object(result))
}

fn job_summary(job: &Value) -> Value {
    let mut result = counts(job, &["completedProducts", "compileErrorCount"]);
    result.insert("type".into(), json!(safe_type(string(&job["type"]))));
    result.insert("provider".into(), json!(provider(job)));
    result.insert("state".into(), json!(safe_state(string(&job["state"]))));
    code_field(job, &mut result);
    if job.get("stage").is_some() {
        result.insert("stage".into(), json!(safe_stage(string(&job["stage"]))));
    }
    if let Some(stage) = job.get("supportLastStage") {
        result.insert("supportLastStage".into(), json!(safe_stage(string(stage))));
    }
    if matches!(string(&job["type"]), "boothSync" | "gumroadSync" | "jinxxySync") {
        // These are actual accepted sync modes. Missing/default modes
        // are not inferred, and no other argument or arbitrary mode is copied.
        match string(&job["args"]["mode"]) {
            "full" => { result.insert("mode".into(), json!("full")); }
            "incremental" => { result.insert("mode".into(), json!("incremental")); }
            "forced" => { result.insert("mode".into(), json!("forced")); }
            _ => {}
        }
    }
    flags(job, &mut result, &["manifestComplete", "writesStarted", "automatic", "redownload"]);
    if job["progress"].is_object() {
        let mut progress = counts(&job["progress"], PROGRESS_COUNTS);
        if job["progress"].get("stage").is_some() {
            progress.insert("stage".into(), json!(safe_stage(string(&job["progress"]["stage"]))));
        }
        if let Some(stage) = job["progress"].get("loginStage") {
            progress.insert("loginStage".into(), json!(safe_stage(string(stage))));
        }
        if let Some(code) = job["progress"].get("lastErrorCode") {
            progress.insert("lastErrorCode".into(), if code.is_null() { Value::Null } else { json!(safe_code(string(code))) });
        }
        let source = string(&job["progress"]["source"]);
        if source == "all" || SOURCES.contains(&source) {
            progress.insert("source".into(), json!(source));
        }
        result.insert("progress".into(), Value::Object(progress));
    }
    if job["resultCounts"].is_object() {
        result.insert("resultCounts".into(), Value::Object(counts(&job["resultCounts"], &["applied", "skipped", "failed"])));
    }
    if job["failures"].is_object() {
        result.insert("failures".into(), failure_counts(job));
    }
    if matches!(string(&job["type"]), "gumroadSync" | "gumroadUpdate") {
        if let Some(metrics) = provider_metrics(&job["requestMetrics"]) {
            result.insert("requestMetrics".into(), metrics);
        }
    }
    Value::Object(result)
}

fn source_summaries(records: &[Value], checkpoint: bool) -> Value {
    let mut result = Map::new();
    for source in SOURCES {
        // A fixed source set bounds rows. The caller selects the relevant run;
        // source identity is never guessed from an ID, route, title or missing field.
        let Some(record) = records.iter().find(|row| row["source"].as_str() == Some(*source)) else { continue };
        let mut row = counts(record, if checkpoint {
            &["pages", "detailsCompleted", "unresolved", "unresolvedRows"]
        } else { COVERAGE_COUNTS });
        if let Some(value) = record.get("status").or_else(|| record.get("state")) {
            row.insert("status".into(), json!(safe_status(string(value))));
        }
        code_field(record, &mut row);
        flags(record, &mut row, if checkpoint { &["terminal", "finished", "complete"] } else { COVERAGE_FLAGS });
        result.insert((*source).into(), Value::Object(row));
    }
    Value::Object(result)
}

fn reason_counts(input: &Value) -> Value {
    let mut result = BTreeMap::<&'static str, Value>::new();
    if let Some(input) = input.as_object() {
        for (key, value) in input {
            let key = safe_code(key);
            if !result.contains_key(key) && result.len() >= 16 { continue; }
            let value = bounded_count(value);
            let total = match result.get(key) {
                None => value,
                Some(previous) => match (previous.as_u64(), value.as_u64()) {
                    (Some(a), Some(b)) => json!(a.saturating_add(b).min(MAX_COUNT)),
                    _ => Value::Null,
                },
            };
            result.insert(key, total);
        }
    }
    json!(result)
}

fn capture_summary(job: &Value, available: bool) -> Value {
    let metadata = &job["supportCapture"];
    let state = if metadata["version"].as_u64() == Some(1) {
        closed!(string(&metadata["state"]), "unknown"; "queued", "running", "finished")
    } else { "unknown" };
    let execution_version = if metadata["version"].as_u64() == Some(1)
        && matches!(state, "running" | "finished") {
        string(&metadata["executionAppVersion"])
    } else { "" };
    // Only public application versions generated by this implementation can be
    // exported. Never echo a version-looking credential from a modified record.
    let execution_version = if execution_version == env!("CARGO_PKG_VERSION") {
        env!("CARGO_PKG_VERSION")
    } else {
        closed!(execution_version, "unknown"; "0.2.0", "0.2.1", "0.2.2", "0.2.3", "0.2.4", "0.2.5", "0.2.6")
    };
    let status = if available { "available" }
        else if job["supportDiagnostics"].is_object() { "unsupportedCapture" }
        else if matches!(state, "running" | "finished") { "missingAfterStart" }
        else if state == "queued" { "notStarted" }
        else { "notRecorded" };
    json!({"status":status,"attemptState":state,"executionAppVersion":execution_version})
}

fn trace(input: &Value) -> Option<Value> {
    if !input.is_object() || input["version"].as_u64() != Some(1) {
        return None;
    }
    let mut result = Map::new();
    result.insert("version".into(), json!(1));
    if let Some(elapsed) = input.get("elapsedMs") {
        result.insert("elapsedMs".into(), bounded_count(elapsed));
    }
    if let Some(stage) = input.get("lastStage") {
        result.insert("lastStage".into(), json!(safe_stage(string(stage))));
    }
    let mut sources = Map::new();
    for source in TRACE_SOURCES {
        let row = &input["sources"][*source];
        if !row.is_object() { continue; }
        let mut projected = counts(row, &["pages", "details", "rows", "unresolved",
            "scansStarted", "scansComplete", "scansIncomplete", "scansFailed",
            "parserCalls", "htmlJa", "htmlEn", "htmlKo", "htmlZhCn", "htmlZhTw", "htmlOther", "htmlUnknown"]);
        if row.get("parserCalls").is_some() {
            projected.insert("htmlLanguageScope".into(), json!("parserCallsThisAttempt"));
        }
        if let Some(state) = row.get("state") {
            projected.insert("state".into(), json!(closed!(string(state), "unknown"; "running", "complete", "incomplete", "failed", "interrupted")));
        }
        code_field(row, &mut projected);
        flags(row, &mut projected, &["terminal"]);
        if row["reasons"].is_object() {
            projected.insert("reasons".into(), reason_counts(&row["reasons"]));
            projected.insert("reasonScope".into(), json!("checkedJournalAndAssemblyObservationsThisAttempt"));
        }
        if let Some(value) = row.get("omittedReasonCount") {
            projected.insert("omittedReasonCount".into(), bounded_count(value));
        }
        if row["lastClosure"].is_object() {
            let mut closure = counts(&row["lastClosure"], &["pendingRequests", "unresolvedRows", "enrichmentPending", "incompleteProducts"]);
            flags(&row["lastClosure"], &mut closure, &["terminalSeen", "membershipUnbound", "headStable"]);
            closure.insert("scope".into(), json!("latestObservedSourceClosure"));
            projected.insert("lastClosure".into(), Value::Object(closure));
        }
        sources.insert((*source).into(), Value::Object(projected));
    }
    if input.get("sources").is_some() {
        result.insert("sources".into(), Value::Object(sources));
    }
    let mut http = Map::new();
    for scope in HTTP_SCOPES {
        let row = &input["http"][*scope];
        if !row.is_object() { continue; }
        let mut projected = counts(row, HTTP_COUNTS);
        if let Some(status) = row.get("lastStatus") {
            projected.insert("lastStatus".into(), status.as_u64().filter(|n| (100..=599).contains(n)).map(Value::from).unwrap_or(Value::Null));
        }
        http.insert((*scope).into(), Value::Object(projected));
    }
    if input.get("http").is_some() {
        result.insert("http".into(), Value::Object(http));
    }
    if input["financial"].is_object() {
        let mut financial = counts(&input["financial"], FINANCIAL_COUNTS);
        financial.insert("scope".into(), json!("parserCallsThisAttempt"));
        result.insert("financial".into(), Value::Object(financial));
    }
    Some(Value::Object(result))
}

/// Caller supplies only this job's account/run-bound records. This function is
/// pure and never opens a file, follows a URL, schedules work or accesses sessions.
pub(crate) fn report(job: &Value, coverage: &[Value], checkpoints: &[Value]) -> String {
    let captured = trace(&job["supportDiagnostics"]);
    let mut result = json!({
        "diagnosticVersion":1,
        "app":{"version":env!("CARGO_PKG_VERSION"),"versionScope":"reportGeneration","platform":std::env::consts::OS,
            "architecture":std::env::consts::ARCH,"debugBuild":cfg!(debug_assertions)},
        "job":job_summary(job),"captureAvailable":captured.is_some(),
        "capture":capture_summary(job,captured.is_some())
    });
    if !coverage.is_empty() { result["sourceCoverage"] = source_summaries(coverage,false); }
    if !checkpoints.is_empty() { result["checkpoints"] = source_summaries(checkpoints,true); }
    if let Some(captured) = captured {
        result["supportDiagnostics"] = captured;
    }
    let mut text = serde_json::to_string_pretty(&result).expect("support report contains only JSON values");
    // Current closed fields fit this bound. Keep it explicit for future additions;
    // omit optional sections rather than slicing JSON or leaking raw fallbacks.
    for key in ["checkpoints", "sourceCoverage"] {
        if text.len() <= MAX_REPORT_BYTES { break; }
        result.as_object_mut().unwrap().remove(key);
        result["reportTruncated"] = json!(true);
        text = serde_json::to_string_pretty(&result).expect("support report contains only JSON values");
    }
    if text.len() > MAX_REPORT_BYTES {
        // This cannot occur with the fixed trace above, but is safe if it grows.
        result.as_object_mut().unwrap().remove("supportDiagnostics");
        result["capturedDetailsOmitted"] = json!(true);
        result["reportTruncated"] = json!(true);
        text = serde_json::to_string_pretty(&result).expect("support report contains only JSON values");
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_lifecycle_distinguishes_execution_from_report_version_without_inventing_legacy() {
        let mut job = json!({"id":"PRIVATE_JOB","type":"boothSync","state":"needsAction",
            "errorCode":"SYNC_INCOMPLETE","progress":{"stage":"needsAction","pages":7}});
        let legacy = parsed(&job, &[], &[]);
        assert_eq!(legacy["app"]["versionScope"], "reportGeneration");
        assert_eq!(legacy["capture"]["status"], "notRecorded");
        assert_eq!(legacy["capture"]["executionAppVersion"], "unknown");
        assert!(legacy.get("sourceCoverage").is_none());
        assert!(legacy.get("checkpoints").is_none());
        reset_attempt(&mut job);
        assert_eq!(parsed(&job, &[], &[])["capture"]["status"], "notStarted");
        begin_attempt(&mut job);
        let started = parsed(&job, &[], &[]);
        assert_eq!(started["capture"]["status"], "missingAfterStart");
        assert_eq!(started["capture"]["executionAppVersion"], env!("CARGO_PKG_VERSION"));
        assert_eq!(job["progress"]["pages"], 7);
        job["supportDiagnostics"] = json!({"version":1,"lastStage":"publishing"});
        end_attempt(&mut job);
        let captured = parsed(&job, &[], &[]);
        assert_eq!(captured["capture"]["status"], "available");
        assert_eq!(captured["capture"]["attemptState"], "finished");
        job["supportCapture"]["executionAppVersion"] = json!("PRIVATE_VERSION_0.2.3");
        assert_eq!(parsed(&job, &[], &[])["capture"]["executionAppVersion"], "unknown");
        assert!(!report(&job, &[], &[]).contains("PRIVATE"));
        job["supportDiagnostics"]["version"] = json!(2);
        assert_eq!(parsed(&job, &[], &[])["capture"]["status"], "unsupportedCapture");
    }

    #[test]
    fn prior_public_capture_keeps_its_execution_version_after_upgrade() {
        let mut job = json!({"type":"boothSync","state":"completed",
            "supportCapture":{"version":1,"state":"finished","executionAppVersion":"0.2.4"},
            "supportDiagnostics":{"version":1,"lastStage":"publishing"}});
        let value = parsed(&job, &[], &[]);
        assert_eq!(value["app"]["version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(value["app"]["versionScope"], "reportGeneration");
        assert_eq!(value["capture"]["executionAppVersion"], "0.2.4");
        assert_eq!(value["capture"]["attemptState"], "finished");
        assert_eq!(value["capture"]["status"], "available");
        job["supportCapture"]["executionAppVersion"] = json!("0.2.5");
        assert_eq!(parsed(&job, &[], &[])["capture"]["executionAppVersion"], "0.2.5");
        job["supportCapture"]["executionAppVersion"] = json!("0.2.6");
        assert_eq!(parsed(&job, &[], &[])["capture"]["executionAppVersion"], "0.2.6");
        job["supportCapture"]["executionAppVersion"] = json!("0.2.4-PRIVATE_SESSION_VERSION");
        assert_eq!(parsed(&job, &[], &[])["capture"]["executionAppVersion"], "unknown");
        assert!(!report(&job, &[], &[]).contains("PRIVATE"));
    }

    #[test]
    fn source_reason_closure_and_language_projection_never_echo_dynamic_values() {
        let job = json!({"type":"boothSync","supportDiagnostics":{"version":1,"sources":{"purchase":{
            "reasons":{"SOURCE_HEAD_CHANGED":1,"PRIVATE_REASON":2,"SOURCE_MANIFEST_INCOMPLETE":"PRIVATE_COUNT"},
            "reasonScope":"PRIVATE_SCOPE","parserCalls":3,"htmlOther":3,"htmlLanguage":"PRIVATE_LANGUAGE",
            "lastClosure":{"scope":"PRIVATE_SCOPE","pendingRequests":0,"unresolvedRows":2,
                "enrichmentPending":0,"incompleteProducts":1,"terminalSeen":true,"membershipUnbound":false,
                "headStable":false,"url":"PRIVATE_URL"}}}}});
        let text = report(&job, &[], &[]);
        assert!(!text.contains("PRIVATE"));
        let value: Value = serde_json::from_str(&text).unwrap();
        let source = &value["supportDiagnostics"]["sources"]["purchase"];
        assert_eq!(source["reasons"]["SOURCE_HEAD_CHANGED"], 1);
        assert_eq!(source["reasons"]["UNKNOWN_ERROR"], 2);
        assert!(source["reasons"]["SOURCE_MANIFEST_INCOMPLETE"].is_null());
        assert_eq!(source["lastClosure"]["scope"], "latestObservedSourceClosure");
        assert_eq!(source["lastClosure"]["headStable"], false);
        assert_eq!(source["lastClosure"]["incompleteProducts"], 1);
        assert_eq!(source["htmlLanguageScope"], "parserCallsThisAttempt");
        assert_eq!(source["reasonScope"], "checkedJournalAndAssemblyObservationsThisAttempt");
    }

    #[test]
    fn five_language_projection_retains_only_fixed_bounded_numeric_counters() {
        let secret = "PRIVATE_CUSTOM_LOCALE_COOKIE_123456789012345678";
        let keys = ["htmlJa", "htmlEn", "htmlKo", "htmlZhCn", "htmlZhTw", "htmlOther", "htmlUnknown"];
        let mut row = json!({"parserCalls":28,"htmlLanguage":secret,"rawLocale":secret,(secret):9});
        for (index, key) in keys.iter().enumerate() {
            row[*key] = json!(index + 1);
        }
        let mut job = json!({"type":"boothSync","supportDiagnostics":{"version":1,
            "sources":{"purchase":row.clone()},"financial":row}});
        let value = parsed(&job, &[], &[]);
        let source = &value["supportDiagnostics"]["sources"]["purchase"];
        let financial = &value["supportDiagnostics"]["financial"];
        assert_eq!(source["parserCalls"], 28);
        assert_eq!(source["htmlLanguageScope"], "parserCallsThisAttempt");
        assert_eq!(financial["scope"], "parserCallsThisAttempt");
        assert_eq!(source.as_object().unwrap().len(), 9);
        assert_eq!(financial.as_object().unwrap().len(), 8);
        for (index, key) in keys.iter().enumerate() {
            assert_eq!(source[*key], index + 1, "source {key}");
            assert_eq!(financial[*key], index + 1, "financial {key}");
        }
        assert!(!report(&job, &[], &[]).contains(secret));
        for invalid in [json!(secret), json!("1"), json!(-1), json!(1.5), json!(true),
            json!({"locale":secret}), json!([1]), json!(MAX_COUNT + 1), json!(123456789012345678u64)] {
            for key in keys {
                job["supportDiagnostics"]["sources"]["purchase"][key] = invalid.clone();
                job["supportDiagnostics"]["financial"][key] = invalid.clone();
            }
            let value = parsed(&job, &[], &[]);
            for key in keys {
                assert!(value["supportDiagnostics"]["sources"]["purchase"][key].is_null(), "source {key}");
                assert!(value["supportDiagnostics"]["financial"][key].is_null(), "financial {key}");
            }
            let text = report(&job, &[], &[]);
            assert!(!text.contains(secret));
            assert!(!text.contains("123456789012345678"));
        }
    }

    #[test]
    fn retry_removes_previous_attempt_facts_without_changing_business_checkpoints() {
        let mut job = json!({"id":"PRIVATE_JOB","type":"gumroadSync","args":{"mode":"incremental"},
            "supportDiagnostics":{"version":1},"supportLastStage":"enriching","requestMetrics":{"requests":99},
            "progress":{"stage":"needsAction","pages":5,"loginStage":"cookieAccess","lastErrorCode":"AUTH_REQUIRED"}});
        reset_attempt(&mut job);
        for key in ["supportDiagnostics","supportLastStage","requestMetrics"] { assert!(job.get(key).is_none()); }
        assert_eq!(job["id"],"PRIVATE_JOB");
        assert_eq!(job["args"]["mode"],"incremental");
        assert_eq!(job["progress"],json!({"stage":"needsAction","pages":5}));
        assert_eq!(operation_stage("needsAction"),"unknown");
        assert_eq!(operation_stage("PRIVATE_STAGE"),"unknown");
        assert_eq!(operation_stage("cookieAccess"),"cookieAccess");
        let value = parsed(&job,&[],&[]);
        assert_eq!(value["captureAvailable"],false);
        assert!(value["job"].get("requestMetrics").is_none());
    }

    fn parsed(job: &Value, coverage: &[Value], checkpoints: &[Value]) -> Value {
        serde_json::from_str(&report(job, coverage, checkpoints)).unwrap()
    }

    #[test]
    fn old_jobs_do_not_invent_captured_stage_requests_or_financial_failures() {
        let job = json!({"id":"PRIVATE_JOB","type":"boothSync","state":"needsAction","errorCode":"SYNC_INCOMPLETE","progress":{"stage":"needsAction","pages":5}});
        let value = parsed(&job, &[json!({"source":"library","status":"incomplete","coverageComplete":false,"pages":5})], &[]);
        assert_eq!(value["captureAvailable"], false);
        assert!(value.get("supportDiagnostics").is_none());
        assert_eq!(value["job"]["errorCode"], "SYNC_INCOMPLETE");
        assert_eq!(value["sourceCoverage"]["library"]["pages"], 5);
        assert_eq!(value["sourceCoverage"]["library"]["coverageComplete"], false);
        assert!(value["sourceCoverage"].get("purchase").is_none());
        assert!(!report(&job, &[], &[]).contains("PRIVATE_JOB"));
    }

    #[test]
    fn captured_facts_are_reprojected_without_private_strings_or_dynamic_keys() {
        let secret = "PRIVATE_MARKER_COOKIE_ORDER_TITLE_PATH_123456789";
        let job = json!({
            "id":secret,"accountId":secret,"accountName":secret,"type":secret,"store":secret,
            "state":secret,"errorCode":secret,"stage":secret,"createdAt":secret,
            "args":{"url":format!("https://example.invalid/?cookie={secret}")},
            "failures":{(secret):{"errorCode":secret,"fileName":secret,"path":secret}},
            "progress":{"stage":secret,"source":secret,"pages":secret,"private":secret},
            "supportDiagnostics":{"version":1,"lastStage":secret,"rawError":secret,
                "sources":{"library":{"state":secret,"errorCode":secret,"rows":3,"scansStarted":2,"scansComplete":1,"scansIncomplete":1,"scansFailed":0,"terminal":true,"title":secret},(secret):{"rows":9}},
                "http":{"receipt":{"requests":2,"lastStatus":403,"cookie":secret,"url":secret},(secret):{"requests":9}},
                "financial":{"rows":3,"rowsWithPaidProof":2,"paidAmount":991,"acquiredAt":secret,"scope":secret}
            }
        });
        let rows = vec![json!({"source":"library","status":secret,"errorCode":secret,"pages":2,"accountId":secret,"route":secret}),json!({"source":secret,"pages":1})];
        let text = report(&job, &rows, &rows);
        assert!(!text.contains(secret));
        for forbidden in ["rawError", "accountId", "accountName", "fileName", "paidAmount", "acquiredAt", "https:", "createdAt"] {
            assert!(!text.contains(forbidden), "{forbidden}");
        }
        let value: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["job"]["errorCode"], "UNKNOWN_ERROR");
        assert_eq!(value["job"]["type"], "unknown");
        assert_eq!(value["job"]["failures"]["byCode"]["UNKNOWN_ERROR"], 1);
        assert_eq!(value["supportDiagnostics"]["sources"]["library"]["rows"], 3);
        assert_eq!(value["supportDiagnostics"]["sources"]["library"]["scansStarted"], 2);
        assert_eq!(value["supportDiagnostics"]["http"]["receipt"]["lastStatus"], 403);
        assert_eq!(value["supportDiagnostics"]["financial"]["scope"], "parserCallsThisAttempt");
        assert_eq!(value["supportDiagnostics"]["financial"]["rowsWithPaidProof"], 2);
        assert_eq!(value["sourceCoverage"].as_object().unwrap().len(), 1);
    }

    #[test]
    fn codes_and_stages_require_exact_static_membership_even_for_code_like_secrets() {
        for value in ["SECRET_COOKIE", "ORDER_123456789", "AUTH_REQUIRED:PRIVATE_TOKEN", "AUTH_REQUIRED\nPRIVATE", "UNITY_PRIVATE_TITLE", "ai_PRIVATE_KEY", "AUTH_REQUIRED\u{0}"] {
            assert_eq!(safe_code(value), "UNKNOWN_ERROR");
            assert_eq!(safe_stage(value), "unknown");
        }
        assert_eq!(safe_code("FREE_ITEM_ROLE_UNPROVEN"), "FREE_ITEM_ROLE_UNPROVEN");
        assert_eq!(safe_code("NETWORK_TIMEOUT"), "NETWORK_TIMEOUT");
        assert_eq!(safe_code("PUBLIC_METADATA_INVALID"), "PUBLIC_METADATA_INVALID");
        assert_eq!(safe_code("archive_unsafe_link"), "archive_unsafe_link");
        assert_eq!(safe_stage("publishing"), "publishing");
    }

    #[test]
    fn last_operation_stage_and_interrupted_trace_are_safe_without_inferred_sync_modes() {
        let job = json!({"type":"boothSync","state":"failed","errorCode":"NETWORK_TIMEOUT",
            "supportLastStage":"enriching","args":{"mode":"full","accountId":"PRIVATE_ACCOUNT"},
            "supportDiagnostics":{"version":1,"sources":{"purchase":{"state":"interrupted","errorCode":"NETWORK_TIMEOUT","rows":2}}}});
        let value = parsed(&job, &[], &[]);
        assert_eq!(value["job"]["supportLastStage"], "enriching");
        assert_eq!(value["job"]["mode"], "full");
        assert_eq!(value["job"]["errorCode"], "NETWORK_TIMEOUT");
        assert_eq!(value["supportDiagnostics"]["sources"]["purchase"]["state"], "interrupted");
        for invalid in [json!("PRIVATE_STAGE"), json!({"value":"PRIVATE_STAGE"}), Value::Null] {
            let mut poisoned = job.clone();
            poisoned["supportLastStage"] = invalid;
            assert_eq!(parsed(&poisoned, &[], &[])["job"]["supportLastStage"], "unknown");
            assert!(!report(&poisoned, &[], &[]).contains("PRIVATE"));
        }
        for mode in [json!("update"), json!("PRIVATE_MODE"), json!(true), Value::Null] {
            let mut changed = job.clone();
            changed["args"]["mode"] = mode;
            assert!(parsed(&changed, &[], &[])["job"].get("mode").is_none());
        }
        let incremental = json!({"type":"gumroadSync","args":{"mode":"incremental"}});
        assert_eq!(parsed(&incremental, &[], &[])["job"]["mode"], "incremental");
        assert_eq!(parsed(&json!({"type":"boothSync","args":{"mode":"forced"}}), &[], &[])["job"]["mode"], "forced");
        assert!(parsed(&json!({"type":"download","args":{"mode":"full"}}), &[], &[])["job"].get("mode").is_none());
        assert!(parsed(&json!({"type":"boothSync"}), &[], &[])["job"].get("mode").is_none());
    }

    #[test]
    fn elapsed_backoff_and_provider_metrics_keep_only_typed_fixed_operational_facts() {
        let secret = "PRIVATE_METRICS_COOKIE_ORDER_123456789012345678";
        let mut job = json!({"type":"gumroadSync","requestMetrics":{
            "scope":"providerCollectionInterval","requests":7,"pacingWaitMs":23,
            "responseWaitMs":49,"activeRequestLimit":2,"startSpacingMs":500,"private":secret,
            "byKind":{"content":{"requests":3,"bodyReadMs":8,"retries":1,"error":secret},
                "public":{"requests":4,"rateLimits":1,"serverErrors":2},(secret):{"requests":9}}
            },"supportDiagnostics":{"version":1,"elapsedMs":450,"lastStage":"cookieAccess",
                "http":{"receipt":{"backoffs":2,"backoffMs":20,"url":secret}}}});
        let value = parsed(&job, &[], &[]);
        assert_eq!(value["supportDiagnostics"]["elapsedMs"], 450);
        assert_eq!(value["supportDiagnostics"]["lastStage"], "cookieAccess");
        assert_eq!(value["supportDiagnostics"]["http"]["receipt"]["backoffs"], 2);
        assert_eq!(value["supportDiagnostics"]["http"]["receipt"]["backoffMs"], 20);
        assert_eq!(value["job"]["requestMetrics"]["scope"], "providerCollectionInterval");
        assert_eq!(value["job"]["requestMetrics"]["byKind"]["content"]["requests"], 3);
        assert_eq!(value["job"]["requestMetrics"]["byKind"]["public"]["serverErrors"], 2);
        assert_eq!(value["job"]["requestMetrics"]["byKind"].as_object().unwrap().len(), 2);
        assert!(!report(&job, &[], &[]).contains(secret));
        for invalid in [json!(secret), json!("50"), json!(-1), json!(2.5), json!(false), json!(MAX_COUNT + 1), json!({"secret":secret})] {
            job["supportDiagnostics"]["elapsedMs"] = invalid.clone();
            job["supportDiagnostics"]["http"]["receipt"]["backoffMs"] = invalid.clone();
            job["requestMetrics"]["pacingWaitMs"] = invalid.clone();
            job["requestMetrics"]["byKind"]["content"]["requests"] = invalid;
            let projected = parsed(&job, &[], &[]);
            assert!(projected["supportDiagnostics"]["elapsedMs"].is_null());
            assert!(projected["supportDiagnostics"]["http"]["receipt"]["backoffMs"].is_null());
            assert!(projected["job"]["requestMetrics"]["pacingWaitMs"].is_null());
            assert!(projected["job"]["requestMetrics"]["byKind"]["content"]["requests"].is_null());
            assert!(!report(&job, &[], &[]).contains(secret));
        }
        job["type"] = json!("gumroadUpdate");
        assert!(parsed(&job, &[], &[])["job"].get("requestMetrics").is_some());
        job["requestMetrics"]["scope"] = json!(secret);
        assert!(parsed(&job, &[], &[])["job"].get("requestMetrics").is_none());
        job["requestMetrics"]["scope"] = json!("providerCollectionInterval");
        job["type"] = json!("boothSync");
        assert!(parsed(&job, &[], &[])["job"].get("requestMetrics").is_none());
    }

    #[test]
    fn counters_reject_ids_wrong_types_and_non_http_statuses_without_coercion() {
        for invalid in [json!("PRIVATE_NUMERIC_SECRET"), json!("17"), json!(-1), json!(1.5), json!(true), json!({"token":"PRIVATE_NESTED"}), json!([3]), json!(MAX_COUNT + 1), json!(123456789012345678u64)] {
            let job = json!({"type":"boothSync","state":"failed","supportDiagnostics":{"version":1,"sources":{"free":{"rows":invalid.clone(),"terminal":invalid.clone()}},"http":{"public":{"requests":invalid.clone(),"lastStatus":invalid.clone()}},"financial":{"proofRejected":invalid.clone()}}});
            let value = parsed(&job, &[json!({"source":"free","pages":invalid.clone()})], &[]);
            assert!(value["supportDiagnostics"]["sources"]["free"]["rows"].is_null());
            assert_eq!(value["supportDiagnostics"]["sources"]["free"]["terminal"], invalid.as_bool().map(Value::from).unwrap_or(Value::Null));
            assert!(value["supportDiagnostics"]["http"]["public"]["requests"].is_null());
            assert!(value["supportDiagnostics"]["http"]["public"]["lastStatus"].is_null());
            assert!(value["supportDiagnostics"]["financial"]["proofRejected"].is_null());
            assert!(value["sourceCoverage"]["free"]["pages"].is_null());
            assert!(!report(&job, &[], &[]).contains("PRIVATE"));
        }
        let value = parsed(&json!({"supportDiagnostics":{"version":1,"http":{"other":{"requests":0,"lastStatus":600}},"financial":{"rows":MAX_COUNT}}}), &[], &[]);
        assert_eq!(value["supportDiagnostics"]["http"]["other"]["requests"], 0);
        assert!(value["supportDiagnostics"]["http"]["other"]["lastStatus"].is_null());
        assert_eq!(value["supportDiagnostics"]["financial"]["rows"], MAX_COUNT);
    }

    #[test]
    fn unsupported_trace_versions_remain_unavailable_and_fixed_source_rows_are_bounded() {
        for version in [json!(2), json!("1"), json!(true), Value::Null] {
            let value = parsed(&json!({"supportDiagnostics":{"version":version,"lastStage":"publishing","financial":{"proofRejected":99}}}), &[], &[]);
            assert_eq!(value["captureAvailable"], false);
            assert!(value.get("supportDiagnostics").is_none());
        }
        let records: Vec<_> = (0..1000).map(|index| json!({"source":"library","pages":index,"private":"PRIVATE_DUPLICATE"})).collect();
        let value = parsed(&json!({}), &records, &records);
        assert_eq!(value["sourceCoverage"].as_object().unwrap().len(), 1);
        assert_eq!(value["checkpoints"].as_object().unwrap().len(), 1);
        assert_eq!(value["checkpoints"]["library"]["pages"], 0);
    }

    #[test]
    fn maximal_known_report_is_valid_bounded_json_and_preserves_operational_facts() {
        let max_fields = |keys: &[&str]| -> Value { Value::Object(keys.iter().map(|key| ((*key).into(), json!(MAX_COUNT))).collect()) };
        let mut capture = json!({"version":1,"lastStage":"enriching","sources":{},"http":{},"financial":max_fields(FINANCIAL_COUNTS)});
        for source in TRACE_SOURCES {
            let mut row = max_fields(&["pages", "details", "rows", "unresolved",
                "scansStarted", "scansComplete", "scansIncomplete", "scansFailed"]);
            row["state"] = json!("incomplete"); row["errorCode"] = json!("FREE_ITEM_CONFLICTING_ENTRIES"); row["terminal"] = json!(true);
            row["reasons"] = ["SOURCE_HEAD_CHANGED", "SOURCE_TERMINAL_UNPROVEN", "SOURCE_MEMBERSHIP_UNBOUND",
                "SOURCE_MANIFEST_INCOMPLETE", "DIGITAL_CONTROLS_UNRESOLVED", "PRODUCT_ID_UNRESOLVED",
                "PRODUCT_TITLE_UNRESOLVED", "FREE_ITEM_ENTRY_UNPROVEN", "FREE_ITEM_FILENAME_UNPROVEN",
                "FREE_ITEM_HISTORY_UNPROVEN", "FREE_ITEM_IDENTITY_UNPROVEN", "FREE_ITEM_COUNT_UNPROVEN",
                "FREE_ITEM_UNKNOWN_DROPDOWN", "FREE_ITEM_ORPHAN_CONTROL", "FREE_ITEM_ROLE_UNPROVEN",
                "FREE_ITEM_CONFLICTING_ENTRIES"].iter().map(|key| ((*key).into(), json!(MAX_COUNT))).collect::<Map<String, Value>>().into();
            row["lastClosure"] = json!({"terminalSeen":true,"pendingRequests":MAX_COUNT,"unresolvedRows":MAX_COUNT,
                "enrichmentPending":MAX_COUNT,"incompleteProducts":MAX_COUNT,"membershipUnbound":true,"headStable":false});
            row["parserCalls"] = json!(MAX_COUNT);
            for language in ["htmlJa", "htmlEn", "htmlKo", "htmlZhCn", "htmlZhTw", "htmlOther", "htmlUnknown"] { row[language] = json!(MAX_COUNT); }
            capture["sources"][*source] = row;
        }
        for scope in HTTP_SCOPES {
            let mut row = max_fields(HTTP_COUNTS); row["lastStatus"] = json!(599);
            capture["http"][*scope] = row;
        }
        let mut rows = Vec::new();
        for source in SOURCES {
            let mut row = max_fields(COVERAGE_COUNTS);
            row["source"] = json!(source); row["detailsCompleted"] = json!(MAX_COUNT);
            row["unresolved"] = json!(MAX_COUNT); row["state"] = json!("incomplete");
            row["errorCode"] = json!("GUMROAD_PUBLIC_METADATA_UNAVAILABLE");
            for flag in COVERAGE_FLAGS.iter().copied().chain(["terminal", "complete", "finished"]) { row[flag] = json!(true); }
            rows.push(row);
        }
        let job = json!({"type":"boothSync","state":"needsAction","errorCode":"SYNC_INCOMPLETE","supportDiagnostics":capture,"progress":max_fields(PROGRESS_COUNTS)});
        let text = report(&job, &rows, &rows);
        assert!(text.len() <= MAX_REPORT_BYTES, "{}", text.len());
        let value: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["captureAvailable"], true);
        assert_eq!(value["supportDiagnostics"]["lastStage"], "enriching");
        assert_eq!(value["supportDiagnostics"]["financial"]["rows"], MAX_COUNT);
        assert_eq!(value["supportDiagnostics"]["http"]["receipt"]["requests"], MAX_COUNT);
        assert_eq!(value["supportDiagnostics"]["sources"]["library"]["reasons"]["SOURCE_HEAD_CHANGED"], MAX_COUNT);
    }
}
