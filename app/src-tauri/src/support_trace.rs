//! Per-attempt operational facts. Task-local scope never contains request data.
use serde_json::{json, Value};
use std::{future::Future, sync::{Arc, Mutex}};

tokio::task_local! { static ACTIVE: Arc<Mutex<Value>>; }

pub(crate) async fn capture<F: Future>(future: F) -> (F::Output, Value) {
    let started = std::time::Instant::now();
    let facts = Arc::new(Mutex::new(json!({"version":1,"lastStage":"unknown",
        "sources":{},"http":{},"financial":{}})));
    let result = ACTIVE.scope(facts.clone(), future).await;
    let snapshot = facts.lock().map(|mut v| {
        v["elapsedMs"] = json!(started.elapsed().as_millis().min(1_000_000_000) as u64);
        if let Some(sources) = v["sources"].as_object_mut() {
            for source in sources.values_mut() {
                if source["state"] == "running" { source["state"] = json!("interrupted"); }
            }
        }
        v.clone()
    }).unwrap_or(Value::Null);
    (result, snapshot)
}
fn update(f: impl FnOnce(&mut Value)) {
    let _ = ACTIVE.try_with(|facts| { if let Ok(mut value) = facts.lock() { f(&mut value); } });
}
pub(crate) fn snapshot() -> Option<Value> {
    ACTIVE.try_with(|facts| facts.lock().ok().map(|v| v.clone())).ok().flatten()
}
fn source(value: &str) -> Option<&'static str> {
    match value { "library"=>Some("library"),"purchase"=>Some("purchase"),"gift"=>Some("gift"),"free"=>Some("free"),_=>None }
}
fn increment(value: &mut Value, key: &'static str, amount: u64) {
    value[key] = json!(value[key].as_u64().unwrap_or(0).saturating_add(amount));
}
pub(crate) fn stage(value: &str) {
    let value = crate::support::operation_stage(value);
    if value != "unknown" {
        update(|trace| trace["lastStage"] = json!(value));
    }
}
pub(crate) fn job_stage(job: &Value) {
    let progress = &job["progress"];
    let stage_value = if job["type"] == "boothLogin" {
        progress["loginStage"].as_str().or_else(|| progress["stage"].as_str())
    } else { progress["stage"].as_str() };
    stage(stage_value.unwrap_or("unknown"));
}
pub(crate) fn source_start(value: &str) {
    if let Some(key) = source(value) {
        update(|trace| {
            let row = &mut trace["sources"][key];
            row["state"] = json!("running");
            increment(row, "scansStarted", 1);
        });
    }
}
pub(crate) fn source_done(value: &str, complete: Option<bool>, error: Option<&str>) {
    if let Some(key) = source(value) {
        update(|trace| {
            let row = &mut trace["sources"][key];
            increment(row, if error.is_some() { "scansFailed" } else if complete == Some(true) { "scansComplete" } else { "scansIncomplete" }, 1);
            row["state"] = json!(if row["scansFailed"].as_u64().unwrap_or(0)>0 {"failed"}
                else if row["scansIncomplete"].as_u64().unwrap_or(0)>0 {"incomplete"} else {"complete"});
            if let Some(error) = error { row["errorCode"] = json!(crate::support::safe_code(error)); }
        });
    }
}
/// Counts checked journal/assembly observations, including replayed checkpoint
/// pages. These are reasons observed this attempt, never unique products/orders.
pub(crate) fn source_reason(value: &str, code: &str, count: u64) {
    if count == 0 { return; }
    if let Some(key) = source(value) {
        let code = crate::support::safe_code(code);
        update(|trace| {
            let row = &mut trace["sources"][key];
            if !row["reasons"].is_object() { row["reasons"] = json!({}); }
            if row["reasons"].get(code).is_some() || row["reasons"].as_object().unwrap().len() < 16 {
                increment(&mut row["reasons"], code, count);
            } else {
                increment(row, "omittedReasonCount", count);
            }
        });
    }
}
/// This snapshot is the latest observed source closure. Other account scans
/// retain their cumulative reasons/results; a later closure cannot clear them.
pub(crate) fn source_closure(value: &str, terminal: bool, pending: u64, unresolved: u64,
    enrichment_pending: u64, incomplete_products: u64, unbound: bool, head_stable: bool) {
    if let Some(key) = source(value) {
        update(|trace| trace["sources"][key]["lastClosure"] = json!({
            "scope":"latestObservedSourceClosure", "terminalSeen":terminal,
            "pendingRequests":pending, "unresolvedRows":unresolved,
            "enrichmentPending":enrichment_pending,"incompleteProducts":incomplete_products,
            "membershipUnbound":unbound,"headStable":head_stable
        }));
    }
}
/// Count only a closed language group, including parser calls which fail before
/// producing a Page. Locale tags and page contents never leave the parser.
pub(crate) fn parser_language(value: &str, language: &str) {
    if let Some(key) = source(value) {
        update(|trace| {
            let row = &mut trace["sources"][key];
            increment(row, "parserCalls", 1);
            increment(row, match language { "ja"=>"htmlJa", "en"=>"htmlEn", "other"=>"htmlOther", _=>"htmlUnknown" }, 1);
        });
    }
}
pub(crate) fn page(value: &str, detail: bool, rows: u64, unresolved: u64, terminal: bool) {
    if let Some(key) = source(value) {
        update(|trace| {
            let row = &mut trace["sources"][key];
            increment(row,if detail {"details"} else {"pages"},1);
            increment(row,"rows",rows); increment(row,"unresolved",unresolved);
            row["terminal"] = json!(row["terminal"].as_bool().unwrap_or(false)||terminal);
        });
    }
}
fn request_kind(url: &url::Url) -> &'static str {
    // Classify inside the transport; retain only this fixed kind, never the URL.
    if url.host_str()==Some("accounts.booth.pm") {
        return match url.path() {
            "/library"=>"library", "/orders"=>"purchase", "/library/gifts"=>"gift",
            "/library/free_downloads"=>"free",
            path if path.strip_prefix("/orders/").is_some_and(|s| !s.is_empty()&&s.bytes().all(|c| c.is_ascii_digit()))=>"receipt",
            _=>"other",
        };
    }
    "public"
}
pub(crate) fn request(url: &url::Url, attempt: u32) {
    let key=request_kind(url);
    update(|trace| {let row=&mut trace["http"][key]; increment(row,"requests",1); if attempt>0 {increment(row,"retries",1);} });
}
pub(crate) fn response(url: &url::Url, status: u16) {
    let key=request_kind(url);
    update(|trace| {
        let row=&mut trace["http"][key]; row["lastStatus"]=json!(status);
        let group=match status {200..=299=>Some("status2xx"),300..=399=>Some("status3xx"),400..=499=>Some("status4xx"),500..=599=>Some("status5xx"),_=>None};
        if let Some(group)=group {increment(row,group,1);}
        if matches!(status,429|503) {increment(row,"rateLimits",1);}
    });
}
pub(crate) fn transport(url: &url::Url, code: &str) {
    let key=request_kind(url);
    update(|trace| {
        let row=&mut trace["http"][key];
        increment(row,"transportFailures",1);
        if code=="NETWORK_TIMEOUT" {increment(row,"timeouts",1);}
    });
}
pub(crate) fn backoff(url: &url::Url, duration: std::time::Duration) {
    let key = request_kind(url);
    update(|trace| {
        let row = &mut trace["http"][key];
        increment(row, "backoffs", 1);
        increment(row, "backoffMs", duration.as_millis().min(1_000_000_000) as u64);
    });
}
pub(crate) fn redirect_rejected(url: &url::Url) {
    let key=request_kind(url);
    update(|trace| increment(&mut trace["http"][key],"redirectRejected",1));
}
pub(crate) fn receipt(fields: &Value, rows: u64, paid: u64, dated: u64, rejected: bool, language: &str) {
    update(|trace| {
        let facts=&mut trace["financial"];
        increment(facts,"receiptPages",1); increment(facts,"rows",rows);
        increment(facts,"rowsWithPaidProof",paid); increment(facts,"rowsWithDateProof",dated);
        increment(facts,"dateProofUnavailableRows",rows.saturating_sub(dated));
        increment(facts,"proofRejected",u64::from(rejected));
        for key in ["digitalBoundItems","uniqueMetadataItems","unitPriceItems","explicitQuantityItems",
            "omittedQuantityItems","explicitBoostItems","productLineProofItems","ambiguousItems",
            "digitalControlBindingUnprovenItems","metadataShapeUnrecognizedItems","unitPriceUnrecognizedItems",
            "quantityUnrecognizedItems","boostUnrecognizedItems"] {
            if let Some(value)=fields[key].as_u64() {increment(facts,key,value);}
        }
        increment(facts,match language {"ja"=>"htmlJa","en"=>"htmlEn","other"=>"htmlOther",_=>"htmlUnknown"},1);
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn attempts_are_isolated_and_do_not_retain_url_or_unknown_strings() {
        let ((), facts)=capture(async {
            let url=url::Url::parse("https://accounts.booth.pm/orders/123456789?cookie=PRIVATE_COOKIE").unwrap();
            source_start("purchase"); stage("PRIVATE_STAGE"); stage("collecting");
            request(&url,0); response(&url,503); request(&url,1); transport(&url,"NETWORK_TIMEOUT");
            backoff(&url, std::time::Duration::from_secs(2));
            page("purchase",true,2,1,false); source_done("purchase",None,Some("PRIVATE_ERROR"));
            receipt(&json!({"unitPriceItems":1,"private":"PRIVATE_COOKIE"}),2,1,0,false,"PRIVATE_LANGUAGE");
        }).await;
        assert_eq!(facts["lastStage"],"collecting");
        assert_eq!(facts["http"]["receipt"]["requests"],2);
        assert_eq!(facts["http"]["receipt"]["retries"],1);
        assert_eq!(facts["http"]["receipt"]["timeouts"],1);
        assert_eq!(facts["http"]["receipt"]["backoffMs"],2000);
        assert_eq!(facts["sources"]["purchase"]["errorCode"],"UNKNOWN_ERROR");
        assert_eq!(facts["financial"]["htmlUnknown"],1);
        let text=facts.to_string();
        for private in ["PRIVATE","123456789","https://","orders/"] {assert!(!text.contains(private));}
        let (_, next)=capture(async {}).await;
        assert_eq!(next["http"],json!({}));
        assert!(snapshot().is_none());
    }
    #[tokio::test]
    async fn simultaneous_attempts_and_dropped_sources_keep_separate_operational_facts() {
        let first = capture(async {
            source_start("purchase");
            source_start("gift");
            tokio::task::yield_now().await;
            page("purchase", true, 3, 1, false);
            source_done("purchase", Some(false), None);
            source_start("purchase");
            page("purchase", true, 2, 0, true);
            source_done("purchase", Some(true), None);
        });
        let second = capture(async {
            source_start("library");
            tokio::task::yield_now().await;
            page("library", false, 2, 0, true);
            source_done("library", Some(true), None);
        });
        let (((), first), ((), second)) = tokio::join!(first, second);
        assert_eq!(first["sources"]["purchase"]["rows"],5);
        assert_eq!(first["sources"]["purchase"]["scansStarted"],2);
        assert_eq!(first["sources"]["purchase"]["scansComplete"],1);
        assert_eq!(first["sources"]["purchase"]["scansIncomplete"],1);
        assert_eq!(first["sources"]["purchase"]["state"],"incomplete");
        assert_eq!(first["sources"]["gift"]["state"],"interrupted");
        assert!(first["sources"].get("library").is_none());
        assert_eq!(second["sources"]["library"]["state"],"complete");
        assert_eq!(second["sources"]["library"]["rows"],2);
        assert!(second["sources"].get("purchase").is_none());
        assert!(first["elapsedMs"].as_u64().is_some());
    }
    #[tokio::test]
    async fn login_substages_are_recorded_as_fixed_operational_phases() {
        let ((), facts) = capture(async {
            job_stage(&json!({"type":"boothLogin","progress":{"stage":"collecting","loginStage":"cookieAccess"}}));
            assert_eq!(snapshot().unwrap()["lastStage"],"cookieAccess");
            job_stage(&json!({"type":"boothLogin","progress":{"stage":"collecting","loginStage":"parserRecognition"}}));
            job_stage(&json!({"type":"boothLogin","progress":{"stage":"failed","loginStage":"PRIVATE_STAGE"}}));
        }).await;
        assert_eq!(facts["lastStage"],"parserRecognition");
    }
}
