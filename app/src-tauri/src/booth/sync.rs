#[cfg(test)]
#[path = "targeted_receipt_tests.rs"]
mod targeted_receipt_tests;

use super::{
    field, free_item, http,
    incremental::{Baseline, Tracker},
    metadata, now, parser, safety, BoothService,
};
use anyhow::{bail, ensure, Result};
use futures_util::{stream::FuturesUnordered, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Emitter};
use tokio::sync::mpsc;
const FREE_ITEM_ADAPTER_VERSION: u32 = 3;
pub(super) fn source_changed(app: &AppHandle, ids: impl IntoIterator<Item = String>) {
    let product_ids: BTreeSet<_> = ids.into_iter().collect();
    if !product_ids.is_empty() {
        let _ = app.emit(
            "nna:changed",
            json!({"scope":"catalogue","productIds":product_ids}),
        );
    }
}

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Checkpoint {
    version: u32,
    account_id: String,
    source: String,
    run_id: String,
    mode: String,
    pending: Vec<String>,
    committed: BTreeMap<String, String>,
    head: String,
    boundary: Option<String>,
    terminal: bool,
    unresolved: usize,
    product_ids: BTreeSet<String>,
    enrichment_pending: BTreeSet<String>,
    pages: usize,
    #[serde(default)]
    details_completed: usize,
    finished: bool,
    journal_ids: Vec<String>,
    #[serde(default)]
    ordering: parser::Ordering,
    #[serde(default)]
    overlap_end: Option<usize>,
    free_item_adapter_version: u32,
    publication_version: u32,
    published_products: BTreeSet<String>,
}
#[derive(Clone)]
struct PublicPrefetch {
    requests: mpsc::UnboundedSender<PublicRequest>,
    enqueued: std::sync::Arc<Mutex<BTreeSet<String>>>,
    early: std::sync::Arc<Mutex<EarlyPublications>>,
    support_pages: Arc<Mutex<SupportPages>>,
}
#[derive(Default)]
struct SupportPages {
    observations: BTreeMap<String, Value>,
    reused: usize,
}
impl SupportPages {
    fn remember(&mut self, evidence: &free_item::Evidence, body: &str) -> Result<()> {
        // The free adapter has already checked exact canonical/OG/root identity,
        // login/challenge and file controls. Retain only its public support facts.
        if !evidence.manifest_complete
            && evidence.error_code.as_deref() != Some("FREE_OFFER_UNAVAILABLE")
        {
            return Ok(());
        }
        let mut observation = json!({});
        metadata::apply_public_support_html(&mut observation, body)?;
        if observation["publicSupportObservedAt"].is_string() {
            self.observations
                .insert(evidence.product_id.clone(), observation);
        }
        Ok(())
    }
    fn apply(&mut self, id: &str, product: &mut Value) -> bool {
        let Some(observation) = self.observations.get(id) else {
            return false;
        };
        for key in ["publicSupportAvatars", "publicSupportObservedAt"] {
            product[key] = observation[key].clone();
        }
        self.reused += 1;
        true
    }
}
impl PublicPrefetch {
    fn enqueue_once(&self, product: &str) -> Result<bool> {
        let mut enqueued = self
            .enqueued
            .lock()
            .map_err(|_| anyhow::anyhow!("STATE_LOCK_FAILED"))?;
        if enqueued.contains(product) {
            return Ok(false);
        }
        if enqueued.len() >= 10000 {
            bail!("SOURCE_TRAVERSAL_LIMIT");
        }
        enqueued.insert(product.into());
        Ok(true)
    }
}
#[derive(Default)]
struct EarlyPublications {
    remaining_sources: BTreeSet<String>,
    publications: Publications,
    observations: BTreeMap<String, Result<Value, String>>,
    published: BTreeSet<String>,
    closed: bool,
}
struct PublicRequest {
    product: String,
    draft: Value,
    account: String,
    source: String,
    run_id: String,
}
impl PublicRequest {
    fn draft_id(&self) -> String {
        format!("booth-product:{}:{}", self.run_id, self.product)
    }
    fn saved_product(&self, saved: &Value) -> Option<Value> {
        (saved["accountId"] == self.account
            && saved["source"] == self.source
            && saved["runId"] == self.run_id
            && saved["productId"] == self.product
            && saved["enriched"] == true
            && saved["product"]["id"] == self.product)
            .then(|| saved["product"].clone())
    }
}
fn product_draft_scope(
    saved: &Value,
    account: &str,
    source: &str,
    run: &str,
    product: &str,
) -> bool {
    saved["accountId"] == account
        && saved["source"] == source
        && saved["runId"] == run
        && saved["productId"] == product
        && saved["product"]["id"] == product
}
fn publication_manifest<'a>(
    account: &str,
    source: &str,
    product: &str,
    rows: impl IntoIterator<Item = &'a parser::Row>,
    routes: &BTreeMap<String, String>,
    files: &BTreeSet<String>,
) -> Result<String> {
    // Bind publication to these exact journal facts, independent of page order.
    // Public metadata drafts alone do not establish a certified manifest.
    let mut rows: Vec<String> = rows
        .into_iter()
        .map(|row| {
            let route = routes
                .get(&row.source_record_id)
                .ok_or_else(|| anyhow::anyhow!("CHECKPOINT_INVALID"))?;
            Ok(safety::hash(serde_json::to_vec(&(row, route))?))
        })
        .collect::<Result<_>>()?;
    rows.sort_unstable();
    Ok(safety::hash(serde_json::to_vec(&(
        "booth-published-manifest-v1",
        account,
        source,
        product,
        rows,
        files,
    ))?))
}
#[derive(Clone)]
struct ProductPublication {
    account: String,
    source: String,
    product: String,
    rows: Vec<parser::Row>,
    routes: BTreeMap<String, String>,
    draft: Value,
    files: BTreeSet<String>,
    checkpoint_key: String,
    force_verify: bool,
    needs_enrichment: bool,
}
#[derive(Default)]
struct Publications {
    products: BTreeMap<String, Vec<ProductPublication>>,
    // Authenticated history remains ownership evidence when the current free
    // offer is unavailable. These facts never certify a file membership.
    availability: BTreeMap<String, Vec<ProductPublication>>,
    availability_published: BTreeSet<String>,
    checkpoints: BTreeMap<String, Checkpoint>,
    final_records: Vec<(String, String, Value)>,
    incomplete_scopes: BTreeSet<(String, String, String)>,
    unbound_sources: BTreeSet<(String, String)>,
    // Exact scan facts retained only until publication refreshes diagnostics.
    // These booleans never enter a source checkpoint or catalogue record.
    source_closures: BTreeMap<(String, String), (bool, bool)>,
    additive_versions: BTreeMap<String, String>,
}
fn source_incomplete_product_count(
    checkpoint: &Checkpoint,
    scopes: &BTreeSet<(String, String, String)>,
) -> u64 {
    scopes.iter().filter(|(account, source, _)| {
        account == &checkpoint.account_id && source == &checkpoint.source
    }).count() as u64
}
fn trace_source_closure(checkpoint: &Checkpoint, publications: &Publications) {
    let Some(&(unbound, head_stable)) = publications.source_closures
        .get(&(checkpoint.account_id.clone(), checkpoint.source.clone())) else { return };
    crate::support_trace::source_closure(
        &checkpoint.source, checkpoint.terminal, checkpoint.pending.len() as u64,
        checkpoint.unresolved as u64, checkpoint.enrichment_pending.len() as u64,
        source_incomplete_product_count(checkpoint, &publications.incomplete_scopes),
        unbound, head_stable,
    );
}
fn trace_source_assembly(checkpoint: &Checkpoint, publications: &Publications, terminal_unproven_observed: bool) {
    let incomplete_products = source_incomplete_product_count(checkpoint, &publications.incomplete_scopes);
    if incomplete_products != 0 {
        crate::support_trace::source_reason(&checkpoint.source, "SOURCE_MANIFEST_INCOMPLETE", incomplete_products);
    }
    if !checkpoint.terminal && !terminal_unproven_observed {
        crate::support_trace::source_reason(&checkpoint.source, "SOURCE_TERMINAL_UNPROVEN", 1);
    }
    trace_source_closure(checkpoint, publications);
}
fn trace_checked_journal(page: &parser::Page, unbound: bool) -> bool {
    // Journal observations can repeat within an attempt; these are neither
    // unique orders nor committed catalogue counts. Only closed codes escape.
    for code in page.rows.iter().filter_map(|row| row.error_code.as_deref())
        .chain(page.free_item.as_ref().and_then(|e| e.error_code.as_deref()))
    {
        crate::support_trace::source_reason(&page.source, crate::support::safe_code(code), 1);
    }
    let terminal_unproven = !page.is_detail && !page.terminal && page.next.is_empty();
    if terminal_unproven {
        crate::support_trace::source_reason(&page.source, "SOURCE_TERMINAL_UNPROVEN", 1);
    }
    if unbound {
        crate::support_trace::source_reason(&page.source, "SOURCE_MEMBERSHIP_UNBOUND", 1);
    }
    terminal_unproven
}
fn record_source_head(checkpoint: &mut Checkpoint, final_head: &parser::Page) {
    if final_head.fingerprint != checkpoint.head {
        checkpoint.unresolved += 1;
        crate::support_trace::source_reason(&checkpoint.source, "SOURCE_HEAD_CHANGED", 1);
    }
}
fn source_local_failure(code: &str) -> bool {
    matches!(code, "COLLECTOR_UNSUPPORTED" | "SOURCE_UNSUPPORTED" | "CHECKPOINT_INVALID"
        | "NO_RECOGNIZED_ROWS" | "SOURCE_MARKER_UNKNOWN" | "DIGITAL_PROOF_UNPROVEN"
        | "SOURCE_TRAVERSAL_LIMIT" | "METADATA_TOO_LARGE")
}
fn source_has_incomplete_product(
    checkpoint: &Checkpoint,
    scopes: &BTreeSet<(String, String, String)>,
) -> bool {
    scopes.iter().any(|(account, source, _)| {
        account == &checkpoint.account_id && source == &checkpoint.source
    })
}
fn checkpoint_finished_after_collection(
    checkpoint: &Checkpoint,
    complete: bool,
    incomplete_scopes: &BTreeSet<(String, String, String)>,
) -> bool {
    complete
        || checkpoint.unresolved != 0
        || checkpoint.pending.is_empty()
            && checkpoint.terminal
            && source_has_incomplete_product(checkpoint, incomplete_scopes)
}
fn observed_file_ids<'a>(
    account: &str,
    rows: impl IntoIterator<Item = &'a parser::Row>,
) -> BTreeSet<String> {
    rows.into_iter()
        .flat_map(|row| &row.files)
        .map(|file| {
            format!(
                "{account}:{}:{}",
                file.id,
                file.variant_id.as_deref().unwrap_or("")
            )
        })
        .collect()
}
fn observed_grant_rows<'a>(
    source: &str,
    rows: &[&'a parser::Row],
    manifest_complete: bool,
    availability_only: bool,
) -> Vec<&'a parser::Row> {
    rows.iter()
        .copied()
        .filter(|row| {
            if source == "purchase" && row.files.is_empty() {
                return row.error_code.is_none();
            }
            (manifest_complete || availability_only || row.manifest_complete)
                && row.error_code.is_none()
        })
        .collect()
}
fn additive_version(entries: &[ProductPublication]) -> Result<String> {
    Ok(safety::hash(serde_json::to_vec(
        &entries
            .iter()
            .map(|entry| {
                (
                    &entry.account,
                    &entry.source,
                    &entry.rows,
                    &entry.routes,
                    &entry.draft,
                    &entry.files,
                )
            })
            .collect::<Vec<_>>(),
    )?))
}
fn publication_products(publications: &Publications) -> BTreeSet<String> {
    publications
        .products
        .keys()
        .chain(publications.availability.keys())
        .cloned()
        .collect()
}
fn publication_entries<'a>(
    publications: &'a Publications,
    product: &str,
) -> impl Iterator<Item = &'a ProductPublication> {
    publications
        .products
        .get(product)
        .into_iter()
        .flatten()
        .chain(publications.availability.get(product).into_iter().flatten())
}
fn product_publication_done(publications: &Publications, product: &str) -> bool {
    publications
        .products
        .get(product)
        .into_iter()
        .flatten()
        .all(|entry| publication_entry_published(entry, &publications.checkpoints))
        && (!publications.availability.contains_key(product)
            || publications.availability_published.contains(product))
}
fn publication_entry_published(
    entry: &ProductPublication,
    checkpoints: &BTreeMap<String, Checkpoint>,
) -> bool {
    checkpoints
        .get(&entry.checkpoint_key)
        .is_some_and(|checkpoint| {
            checkpoint.account_id == entry.account
                && checkpoint.source == entry.source
                && checkpoint.published_products.contains(&entry.product)
        })
}
fn publication_entry_certifiable(entry: &ProductPublication, publications: &Publications) -> bool {
    let Some(checkpoint) = publications.checkpoints.get(&entry.checkpoint_key) else {
        return false;
    };
    checkpoint.account_id == entry.account
        && checkpoint.source == entry.source
        && publication_entry_has_complete_grant(entry)
        && !publications
            .unbound_sources
            .contains(&(entry.account.clone(), entry.source.clone()))
        && (checkpoint.mode == "selected" && entry.checkpoint_key.starts_with("booth-selected:")
            || collection_membership_complete(checkpoint, &publications.incomplete_scopes))
}
fn publication_entry_has_complete_grant(entry: &ProductPublication) -> bool {
    let mut observed = false;
    for row in &entry.rows {
        if entry.source == "purchase" && row.files.is_empty() {
            continue;
        }
        observed = true;
        if !row.manifest_complete || row.error_code.is_some() {
            return false;
        }
    }
    observed
}
fn ready_product_entries(
    publications: &Publications,
    product: &str,
) -> (Vec<ProductPublication>, Vec<ProductPublication>) {
    publications
        .products
        .get(product)
        .into_iter()
        .flatten()
        .filter(|entry| {
            !entry.needs_enrichment
                && !publication_entry_published(entry, &publications.checkpoints)
                && publication_entry_has_complete_grant(entry)
        })
        .cloned()
        .partition(|entry| publication_entry_certifiable(entry, publications))
}
fn published_product_count(publications: &Publications) -> usize {
    publication_products(publications)
        .iter()
        .filter(|product| product_publication_done(publications, product))
        .count()
}
fn remember_product_context(publications: &mut Publications, entry: ProductPublication) {
    let entries = publications
        .products
        .entry(entry.product.clone())
        .or_default();
    entries.retain(|old| old.account != entry.account || old.source != entry.source);
    entries.push(entry);
}
// A free product endpoint is independently scoped to its exact product and
// complete offered file list. It cannot close a different source's unknown row.
fn publication_membership_proven(
    entries: &[ProductPublication],
    unbound: &BTreeSet<(String, String)>,
) -> bool {
    unbound.iter().all(|(account, source)| {
        !entries.iter().any(|entry| &entry.account == account)
            || source == "free"
                && entries
                    .iter()
                    .any(|entry| &entry.account == account && entry.source == "free")
    })
}
fn publication_progress(
    publications: &Publications,
    stage: &str,
    completed: usize,
    published: usize,
) -> Value {
    let pages: usize = publications.checkpoints.values().map(|c| c.pages).sum();
    let details: usize = publications
        .checkpoints
        .values()
        .map(|c| c.details_completed)
        .sum();
    let total = publication_products(publications).len();
    let availability_warnings: usize = publications.availability.values().map(Vec::len).sum();
    json!({"stage":stage,"source":"all","pages":pages,"detailsCompleted":details,
        "requestsCompleted":pages+details,"products":total,"metadataCompleted":completed,
        "metadataTotal":total,"published":published,"availabilityWarnings":availability_warnings})
}
fn set_collecting_totals(job: &mut Value, early: &EarlyPublications, source: &str) {
    let products: BTreeSet<_> = early
        .publications
        .checkpoints
        .values()
        .flat_map(|c| c.product_ids.iter())
        .collect();
    let completed = products
        .iter()
        .filter(|id| early.observations.contains_key(**id))
        .count();
    job["progress"] = publication_progress(
        &early.publications,
        "collecting",
        completed,
        early.published.len(),
    );
    job["progress"]["products"] = json!(products.len());
    job["progress"]["metadataTotal"] = json!(products.len());
    job["progress"]["source"] = json!(source);
}
impl Checkpoint {
    fn fresh(account: &str, source: &str, mode: &str) -> Result<Self> {
        Ok(Self {
            version: 1,
            publication_version: 1,
            account_id: account.into(),
            source: source.into(),
            mode: mode.into(),
            run_id: uuid::Uuid::new_v4().to_string(),
            pending: vec![format!("{}?page=1", safety::source_path(source)?)],
            free_item_adapter_version: if source == "free" {
                FREE_ITEM_ADAPTER_VERSION
            } else {
                0
            },
            ..Self::default()
        })
    }
    fn complete(&self) -> bool {
        self.pending.is_empty()
            && self.terminal
            && self.unresolved == 0
            && self.enrichment_pending.is_empty()
    }
}
#[cfg(test)]
fn source_traversal_verified(checkpoint: &Checkpoint, final_head: &parser::Page) -> bool {
    checkpoint.terminal
        && checkpoint.pending.is_empty()
        && checkpoint.unresolved == 0
        && !checkpoint.head.is_empty()
        && final_head.fingerprint == checkpoint.head
}
#[derive(Clone, Copy)]
pub(super) enum FreeItemDiagnostic {
    Entry,
    Identity,
    Role,
    Controls,
}
impl FreeItemDiagnostic {
    fn error_code(self) -> &'static str {
        match self {
            Self::Entry => "FREE_ITEM_ENTRY_UNPROVEN",
            Self::Identity => "FREE_ITEM_IDENTITY_UNPROVEN",
            Self::Role => "FREE_ITEM_ROLE_UNPROVEN",
            Self::Controls => "FREE_FILE_CONTROLS_UNAVAILABLE",
        }
    }
}
// Reserve existing job keys without holding the mutex across native/network awaits.
struct DiagnosticReservation<'a> {
    running: &'a Mutex<BTreeSet<String>>,
    account_key: String,
}
impl<'a> DiagnosticReservation<'a> {
    fn acquire(running: &'a Mutex<BTreeSet<String>>, account: &str) -> Result<Self> {
        let account_key = format!("account:{account}");
        let mut active = running
            .lock()
            .map_err(|_| anyhow::anyhow!("STATE_LOCK_FAILED"))?;
        if !active.is_empty() {
            bail!("ACCOUNT_BUSY");
        }
        active.insert("booth-global".into());
        active.insert(account_key.clone());
        Ok(Self {
            running,
            account_key,
        })
    }
}
impl Drop for DiagnosticReservation<'_> {
    fn drop(&mut self) {
        let mut active = self
            .running
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        active.remove("booth-global");
        active.remove(&self.account_key);
    }
}
impl BoothService {
    pub(super) async fn diagnose_product_line_fields(
        &self,
        app: &AppHandle,
        account: &str,
        receipt_id: &str,
    ) -> Result<parser::ProductLineFieldDiagnostics> {
        if !safety::numeric(receipt_id) {
            bail!("INVALID_DIAGNOSTIC_ARGUMENTS");
        }
        let _reservation = DiagnosticReservation::acquire(&self.inner.running, account)?;
        let (body, saved) = self.targeted_receipt_page(app, account, receipt_id).await?;
        let witnessed = saved.is_some();
        let proof = match saved {
            Some(rows) => rows,
            None => parser::parse(&body, "purchase", &format!("/orders/{receipt_id}"))?.rows,
        };
        parser::diagnose_product_line_fields(&body, &proof, receipt_id, witnessed)
    }
    async fn targeted_receipt_page(
        &self,
        app: &AppHandle,
        account: &str,
        receipt_id: &str,
    ) -> Result<(String, Option<Vec<parser::Row>>)> {
        if !safety::numeric(receipt_id) {
            bail!("INVALID_DIAGNOSTIC_ARGUMENTS");
        }
        // An explicitly requested, unsaved receipt is still fetchable. Existing
        // checkpoints, when present, must validate in full before the GET.
        let saved = match self
            .inner
            .catalogue
            .get("checkpoint", &format!("booth:{account}:purchase"))?
        {
            Some(_) => {
                let (checkpoint, journals) = self.acquisition_diagnostic_journals(account)?;
                select_targeted_receipt_proof(&checkpoint, &journals, account, receipt_id)?
            }
            None => None,
        };
        let route = format!("/orders/{receipt_id}");
        let client = self.client(app, account, false).await?;
        let (status, body) = self
            .inner
            .http
            .get(&client, safety::route(&route, "purchase")?)
            .await?;
        if !status.is_success() {
            bail!("SOURCE_HTTP_FAILED");
        }
        Ok((body, saved))
    }
    pub(super) async fn diagnose_receipt_template(
        &self,
        app: &AppHandle,
        account: &str,
    ) -> Result<parser::ReceiptTemplateDiagnostics> {
        let _reservation = DiagnosticReservation::acquire(&self.inner.running, account)?;
        let (body, proof, receipt_id) = self.acquisition_diagnostic_page(app, account).await?;
        parser::diagnose_receipt_template(&body, &proof, &receipt_id)
    }
    pub(super) async fn diagnose_receipt_notice_envelope(
        &self,
        app: &AppHandle,
        account: &str,
    ) -> Result<parser::ReceiptNoticeDiagnostics> {
        let _reservation = DiagnosticReservation::acquire(&self.inner.running, account)?;
        let (body, proof, receipt_id) = self.acquisition_diagnostic_page(app, account).await?;
        parser::diagnose_receipt_notice_envelope(&body, &proof, &receipt_id)
    }
    pub(super) async fn diagnose_receipt_contact_envelope(
        &self,
        app: &AppHandle,
        account: &str,
    ) -> Result<parser::ReceiptContactDiagnostics> {
        let _reservation = DiagnosticReservation::acquire(&self.inner.running, account)?;
        let (body, proof, receipt_id) = self.acquisition_diagnostic_page(app, account).await?;
        parser::diagnose_receipt_contact_envelope(&body, &proof, &receipt_id)
    }
    fn acquisition_diagnostic_journals(&self, account: &str) -> Result<(Checkpoint, Vec<Value>)> {
        let checkpoint: Checkpoint = serde_json::from_value(
            self.inner
                .catalogue
                .get("checkpoint", &format!("booth:{account}:purchase"))?
                .ok_or_else(|| anyhow::anyhow!("DIAGNOSTIC_ENTRY_NOT_FOUND"))?,
        )
        .map_err(|_| anyhow::anyhow!("CHECKPOINT_INVALID"))?;
        let mut journals = Vec::new();
        for id in &checkpoint.journal_ids {
            journals.push(
                self.inner
                    .catalogue
                    .get("checkpoint", id)?
                    .ok_or_else(|| anyhow::anyhow!("CHECKPOINT_INVALID"))?,
            );
        }
        Ok((checkpoint, journals))
    }
    async fn acquisition_diagnostic_page(
        &self,
        app: &AppHandle,
        account: &str,
    ) -> Result<(String, Vec<parser::Row>, String)> {
        let (checkpoint, journals) = self.acquisition_diagnostic_journals(account)?;
        let (route, proof) = select_acquisition_diagnostic_detail(&checkpoint, &journals, account)?;
        let client = self.client(app, account, false).await?;
        let (status, body) = self
            .inner
            .http
            .get(&client, safety::route(&route, "purchase")?)
            .await?;
        if !status.is_success() {
            bail!("SOURCE_HTTP_FAILED");
        }
        let receipt_id = safety::route(&route, "purchase")?
            .path()
            .strip_prefix("/orders/")
            .filter(|id| safety::numeric(id))
            .ok_or_else(|| anyhow::anyhow!("CHECKPOINT_INVALID"))?
            .to_owned();
        Ok((body, proof, receipt_id))
    }
    pub(super) async fn diagnose_free_item_entry(
        &self,
        app: &AppHandle,
        account: &str,
        issue: FreeItemDiagnostic,
    ) -> Result<free_item::EntryDiagnostics> {
        let (id, body) = self.free_item_diagnostic_page(app, account, issue).await?;
        free_item::diagnose_entry(&body, &id)
    }
    pub(super) async fn diagnose_free_item_identity(
        &self,
        app: &AppHandle,
        account: &str,
    ) -> Result<free_item::IdentityDiagnostics> {
        let (id, body) = self
            .free_item_diagnostic_page(app, account, FreeItemDiagnostic::Identity)
            .await?;
        free_item::diagnose_identity(&body, &id)
    }
    async fn free_item_diagnostic_page(
        &self,
        app: &AppHandle,
        account: &str,
        issue: FreeItemDiagnostic,
    ) -> Result<(String, String)> {
        let _reservation = DiagnosticReservation::acquire(&self.inner.running, account)?;
        let key = format!("booth:{account}:free");
        let value = self
            .inner
            .catalogue
            .get("checkpoint", &key)?
            .ok_or_else(|| anyhow::anyhow!("DIAGNOSTIC_ENTRY_NOT_FOUND"))?;
        let checkpoint: Checkpoint =
            serde_json::from_value(value).map_err(|_| anyhow::anyhow!("CHECKPOINT_INVALID"))?;
        let mut journals = vec![];
        for id in &checkpoint.journal_ids {
            journals.push(
                self.inner
                    .catalogue
                    .get("checkpoint", id)?
                    .ok_or_else(|| anyhow::anyhow!("CHECKPOINT_INVALID"))?,
            );
        }
        let route = select_free_item_diagnostic_route(&checkpoint, &journals, account, issue)?;
        let id =
            safety::free_item_id(&route).ok_or_else(|| anyhow::anyhow!("CHECKPOINT_INVALID"))?;
        let client = self.client(app, account, false).await?;
        let (status, body) = self
            .inner
            .http
            .get(
                &client,
                url::Url::parse(&route).map_err(|_| anyhow::anyhow!("CHECKPOINT_INVALID"))?,
            )
            .await?;
        if !status.is_success() {
            bail!("SOURCE_HTTP_FAILED");
        }
        Ok((id.to_owned(), body))
    }
    pub(super) async fn sync_job(&self, app: &AppHandle, job: &mut Value) -> Result<()> {
        let args = job["args"].clone();
        let account = field(&args, "accountId")?;
        let sources = if args["source"].as_str().unwrap_or("all") == "all" {
            vec!["library", "purchase", "gift", "free"]
        } else {
            vec![safety::source_name(field(&args, "source")?)?]
        };
        let client = self.client(app, account, false).await?;
        let mode = args["mode"].as_str().unwrap_or("incremental");
        let mut incomplete = false;
        let public =
            http::build_client(std::sync::Arc::new(reqwest::cookie::Jar::default()), false)?;
        // Queue unique observations without coupling traversal to public-page latency.
        // Active requests remain bounded to two and source traversal to 10,000 products.
        let (requests_tx, requests) = mpsc::unbounded_channel();
        let early = std::sync::Arc::new(Mutex::new(EarlyPublications {
            remaining_sources: sources.iter().map(|s| s.to_string()).collect(),
            ..EarlyPublications::default()
        }));
        let prefetch = PublicPrefetch {
            requests: requests_tx,
            enqueued: std::sync::Arc::new(Mutex::new(BTreeSet::new())),
            early: early.clone(),
            support_pages: Arc::default(),
        };
        let support_pages = prefetch.support_pages.clone();
        let (observations, results) = mpsc::unbounded_channel();
        let template = job.clone();
        let collect_and_publish = async {
            let mut scans = FuturesUnordered::new();
            for source in sources {
                let mut source_job = template.clone();
                let prefetch = prefetch.clone();
                let client = &client;
                let public = &public;
                scans.push(async move {
                    let mut enriched = BTreeMap::new();
                    let mut publications = Publications::default();
                    let result = self
                        .scan_source(
                            app,
                            &mut source_job,
                            account,
                            source,
                            mode,
                            client,
                            None,
                            public,
                            &mut enriched,
                            &mut publications,
                            Some(&prefetch),
                        )
                        .await;
                    (source, result, publications)
                });
            }
            let mut publications = Publications::default();
            while let Some((source, result, collected)) = scans.next().await {
                match result {
                    Ok(complete) => incomplete |= !complete,
                    Err(error) => {
                        let coverage_id = format!("{account}:{source}");
                        let mut coverage = self
                            .inner
                            .catalogue
                            .get("source_coverage", &coverage_id)?
                            .unwrap_or_else(
                                || json!({"id":coverage_id,"accountId":account,"source":source}),
                            );
                        coverage["status"] = json!("incomplete");
                        coverage["coverageComplete"] = json!(false);
                        coverage["errorCode"] = json!(super::code(&error));
                        coverage["updatedAt"] = json!(now());
                        self.inner
                            .catalogue
                            .put("source_coverage", &coverage_id, coverage)?;
                        if matches!(
                            super::code(&error).as_str(),
                            "AUTH_REQUIRED" | "AUTH_CHALLENGE" | "PERMISSION_DENIED"
                        ) {
                            let mut account_record = self.account(account)?;
                            account_record["state"] = json!("needsAction");
                            account_record["status"] = json!("needsAction");
                            account_record["errorCode"] = json!(super::code(&error));
                            self.inner
                                .catalogue
                                .put("account", account, account_record)?;
                        }
                        if !source_local_failure(&super::code(&error)) {
                            return Err(error);
                        }
                        incomplete = true;
                        publications.unbound_sources.insert((account.into(), source.into()));
                        let mut early = early.lock().map_err(|_| anyhow::anyhow!("STATE_LOCK_FAILED"))?;
                        early.remaining_sources.remove(source);
                        early.publications.unbound_sources.insert((account.into(), source.into()));
                    }
                }
                for (product, entries) in collected.products {
                    publications
                        .products
                        .entry(product)
                        .or_default()
                        .extend(entries);
                }
                for (product, entries) in collected.availability {
                    publications
                        .availability
                        .entry(product)
                        .or_default()
                        .extend(entries);
                }
                publications.checkpoints.extend(collected.checkpoints);
                publications.source_closures.extend(collected.source_closures);
                publications.final_records.extend(collected.final_records);
                publications
                    .incomplete_scopes
                    .extend(collected.incomplete_scopes);
                publications
                    .unbound_sources
                    .extend(collected.unbound_sources);
            }
            drop(prefetch);
            // Preserve early commits when final source snapshots are merged.
            {
                let mut early = early
                    .lock()
                    .map_err(|_| anyhow::anyhow!("STATE_LOCK_FAILED"))?;
                early.closed = true;
                publications
                    .additive_versions
                    .extend(std::mem::take(&mut early.publications.additive_versions));
                for product in &early.published {
                    if let Some(entries) = publications.products.get_mut(product) {
                        for entry in entries {
                            if let Some(Ok(patch)) = early.observations.get(product) {
                                entry
                                    .draft
                                    .as_object_mut()
                                    .unwrap()
                                    .extend(patch.as_object().unwrap().clone());
                                entry.needs_enrichment = false;
                            }
                        }
                    }
                }
            }
            self.publish_pipeline(app, job, &mut publications, results)
                .await
        };
        tokio::try_join!(
            collect_and_publish,
            self.prefetch_metadata(
                app,
                &template,
                &early,
                &public,
                requests,
                observations,
                &support_pages
            )
        )?;
        job["progress"]["publicHtmlReused"] = json!(
            support_pages
                .lock()
                .map_err(|_| anyhow::anyhow!("STATE_LOCK_FAILED"))?
                .reused
        );
        if incomplete {
            bail!("SYNC_INCOMPLETE");
        }
        Ok(())
    }
    pub(super) async fn update_job(&self, app: &AppHandle, job: &mut Value) -> Result<()> {
        let ids: BTreeSet<String> = job["args"]["productIds"]
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("INVALID_PRODUCT_IDS"))?
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect();
        let mut owned_sources = self.inner.catalogue.list("entitlement")?;
        owned_sources.extend(
            self.inner
                .catalogue
                .list("acquisition")?
                .into_iter()
                .filter(|record| record["source"] == "free"),
        );
        let accounts: BTreeSet<String> = owned_sources
            .iter()
            .filter(|e| e["productId"].as_str().is_some_and(|p| ids.contains(p)))
            .filter(|e| job["args"]["publicOnly"] != true
                && job["accountId"].as_str().is_none_or(|account| e["accountId"] == account))
            .filter_map(|e| e["accountId"].as_str().map(str::to_owned))
            .collect();
        let owned_accounts_present = !accounts.is_empty();
        let accounts: BTreeSet<_> = accounts
            .into_iter()
            .filter(|account| {
                self.known_account(account).ok().is_some_and(|record| {
                    crate::stores::has_local_session(&self.inner.catalogue, &record)
                })
            })
            .collect();
        if owned_accounts_present && accounts.is_empty() {
            anyhow::bail!("AUTH_REQUIRED");
        }
        let mut incomplete = false;
        let public =
            http::build_client(std::sync::Arc::new(reqwest::cookie::Jar::default()), false)?;
        let mut enriched = BTreeMap::new();
        let mut publications = Publications::default();
        for account in accounts {
            // A selected refresh re-traverses account manifests; saved page numbers cannot locate moved cards reliably.
            self.account(&account)?;
            let client = self.client(app, &account, false).await?;
            let mut sources: Vec<String> = owned_sources
                .iter()
                .filter(|e| {
                    e["accountId"] == account
                        && e["productId"].as_str().is_some_and(|p| ids.contains(p))
                })
                .filter_map(|e| e["source"].as_str().map(str::to_owned))
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect();
            sources.sort_by_key(|source| if source == "purchase" { 0 } else { 1 });
            for source in sources {
                if source == "purchase" {
                    let selected: BTreeSet<_> = owned_sources
                        .iter()
                        .filter(|e| e["accountId"] == account && e["source"] == "purchase")
                        .filter_map(|e| e["productId"].as_str())
                        .filter(|id| ids.contains(*id))
                        .map(str::to_owned)
                        .collect();
                    if self
                        .refresh_selected_purchase(
                            app,
                            job,
                            &account,
                            &client,
                            &selected,
                            &public,
                            &mut enriched,
                            &mut publications,
                        )
                        .await?
                    {
                        for product in &selected {
                            self.publish_ready_product(app, job, &mut publications, product)?;
                        }
                        continue;
                    }
                }
                incomplete |= !self
                    .scan_source(
                        app,
                        job,
                        &account,
                        &source,
                        "full",
                        &client,
                        Some(&ids),
                        &public,
                        &mut enriched,
                        &mut publications,
                        None,
                    )
                    .await?;
            }
        }
        self.publish_collected(app, job, &mut publications).await?;
        for id in &ids {
            if self.cancelled(job)? {
                bail!("CANCELLED");
            }
            if !enriched.contains_key(id) {
                self.enrich(&public, id).await?;
                source_changed(app, std::iter::once(id.clone()));
            }
        }
        if incomplete {
            bail!("SYNC_INCOMPLETE");
        }
        Ok(())
    }
    async fn refresh_selected_purchase(
        &self,
        app: &AppHandle,
        job: &mut Value,
        account: &str,
        client: &reqwest::Client,
        selected: &BTreeSet<String>,
        public: &reqwest::Client,
        enriched: &mut BTreeMap<String, Value>,
        publications: &mut Publications,
    ) -> Result<bool> {
        let acquisitions = self.inner.catalogue.list("acquisition")?;
        let remote = self.inner.catalogue.list("remote_file")?;
        let Some(routes) = selected_purchase_routes(&acquisitions, &remote, account, selected)
        else {
            return Ok(false);
        };
        let mut manifests: BTreeMap<String, (BTreeSet<String>, bool)> = BTreeMap::new();
        let mut collected = Vec::new();
        let mut row_routes = BTreeMap::new();
        for (route, expected) in routes {
            if self.cancelled(job)? {
                bail!("CANCELLED");
            }
            let page = match self.fetch_page(client, "purchase", &route, None).await {
                Ok(page) => page,
                Err(error)
                    if matches!(
                        super::code(&error).as_str(),
                        "AUTH_REQUIRED"
                            | "AUTH_CHALLENGE"
                            | "PERMISSION_DENIED"
                            | "NETWORK_UNAVAILABLE"
                            | "RATE_LIMITED"
                    ) =>
                {
                    return Err(error)
                }
                Err(_) => return Ok(false),
            };
            let found: BTreeSet<_> = page
                .rows
                .iter()
                .filter(|row| parser::product_field_file_keys(row).is_some())
                .filter_map(|row| row.product_id.clone())
                .collect();
            if page.unresolved != 0 || !expected.is_subset(&found) {
                return Ok(false);
            }
            if self.cancelled(job)? {
                bail!("CANCELLED");
            }
            let rows: Vec<_> = page
                .rows
                .iter()
                .filter(|row| {
                    row.product_id
                        .as_ref()
                        .is_some_and(|id| selected.contains(id))
                })
                .collect();
            let journal_id = format!(
                "booth-selected-page:{}:{}",
                field(job, "id")?,
                safety::hash(&page.route)
            );
            let journal = json!({"version":1,"id":journal_id,"accountId":account,"source":"purchase","selected":true,"page":page});
            self.inner
                .catalogue
                .put("checkpoint", &journal_id, journal)?;
            for row in rows {
                row_routes.insert(row.source_record_id.clone(), page.route.clone());
                collected.push(row.clone());
                let id = row.product_id.as_ref().unwrap();
                let manifest = manifests
                    .entry(id.clone())
                    .or_insert((BTreeSet::new(), true));
                manifest.1 &= row.manifest_complete;
                manifest.0.extend(observed_file_ids(account, [row]));
            }
            job["progress"] = json!({"stage":"collecting","source":"purchase","products":manifests.len(),"mode":"selected"});
            self.progress(app, job)?;
        }
        if manifests.values().any(|(_, complete)| !complete) {
            return Ok(false);
        }
        let key = format!("booth-selected:{}:{account}", field(job, "id")?);
        let checkpoint = Checkpoint::fresh(account, "purchase", "selected")?;
        for (product, (files, _)) in manifests {
            let rows: Vec<_> = collected
                .iter()
                .filter(|row| row.product_id.as_deref() == Some(&product))
                .collect();
            let mut batch = super::batch::RecordBatch::new(&self.inner.catalogue);
            for row in &rows {
                self.plan_row(
                    &mut batch,
                    account,
                    "purchase",
                    row,
                    &row_routes[&row.source_record_id],
                    true,
                )?;
            }
            let mut draft = batch
                .get("product", &product)?
                .ok_or_else(|| anyhow::anyhow!("CHECKPOINT_INVALID"))?;
            job["progress"] = json!({"stage":"enriching","source":"purchase","products":selected.len(),"published":checkpoint.published_products.len(),"mode":"selected"});
            self.progress(app, job)?;
            if let Some(observation) = enriched.get(&product) {
                draft = observation.clone();
            } else {
                self.enrich_into(public, &product, &mut draft, None).await?;
                enriched.insert(product.clone(), draft.clone());
            }
            let draft_id = format!("booth-product:{}:{product}", checkpoint.run_id);
            self.save_product_draft(&draft_id, json!({"id":draft_id,"accountId":account,"source":"purchase","runId":checkpoint.run_id,"productId":product,"product":draft,"enriched":true}))?;
            publications
                .products
                .entry(product.clone())
                .or_default()
                .push(ProductPublication {
                    account: account.into(),
                    source: "purchase".into(),
                    product: product.clone(),
                    rows: rows.into_iter().cloned().collect(),
                    routes: row_routes.clone(),
                    draft,
                    files,
                    checkpoint_key: key.clone(),
                    force_verify: true,
                    needs_enrichment: false,
                });
        }
        publications.checkpoints.insert(key, checkpoint);
        Ok(true)
    }
    async fn fetch_page(
        &self,
        client: &reqwest::Client,
        source: &str,
        route: &str,
        prefetch: Option<&PublicPrefetch>,
    ) -> Result<parser::Page> {
        if source == "free" {
            if let Some(id) = safety::free_item_id(route) {
                let url =
                    url::Url::parse(route).map_err(|_| anyhow::anyhow!("INVALID_FREE_ITEM_ID"))?;
                let (status, body) = self.inner.http.get(client, url).await?;
                let page = if matches!(status.as_u16(), 404 | 410) {
                    free_item::Evidence::unknown(id, "PUBLIC_METADATA_UNAVAILABLE", true).page()
                } else if status.is_success() {
                    let evidence = free_item::parse(&body, id)?;
                    if let Some(prefetch) = prefetch {
                        prefetch
                            .support_pages
                            .lock()
                            .map_err(|_| anyhow::anyhow!("STATE_LOCK_FAILED"))?
                            .remember(&evidence, &body)?;
                    }
                    evidence.page()
                } else {
                    bail!("SOURCE_HTTP_FAILED")
                }?;
                crate::support_trace::page(source, page.is_detail, page.rows.len() as u64, page.unresolved as u64, page.terminal);
                return Ok(page);
            }
        }
        let (status, body) = self
            .inner
            .http
            .get(client, safety::route(route, source)?)
            .await?;
        if !status.is_success() {
            bail!("SOURCE_HTTP_FAILED");
        }
        let mut page = parser::parse(&body, source, route)?;
        if source == "free" {
            page.details = eligible_free_rows(&page)
                .filter_map(|row| row.product_id.as_deref())
                .map(safety::free_item_route)
                .collect::<Result<BTreeSet<_>>>()?
                .into_iter()
                .collect();
            page.fingerprint = safety::hash(format!(
                "{}:{}",
                page.fingerprint,
                serde_json::to_string(&page.details)
                    .map_err(|_| anyhow::anyhow!("FREE_ITEM_INVALID"))?
            ));
        }
        crate::support_trace::page(source, page.is_detail, page.rows.len() as u64, page.unresolved as u64, page.terminal);
        Ok(page)
    }
    async fn scan_source(
        &self,
        app: &AppHandle,
        job: &mut Value,
        account: &str,
        source: &str,
        mode: &str,
        client: &reqwest::Client,
        selected: Option<&BTreeSet<String>>,
        public: &reqwest::Client,
        enriched: &mut BTreeMap<String, Value>,
        publications: &mut Publications,
        prefetch: Option<&PublicPrefetch>,
    ) -> Result<bool> {
        crate::support_trace::source_start(source);
        let result = self.scan_source_inner(app, job, account, source, mode, client, selected,
            public, enriched, publications, prefetch).await;
        let error = result.as_ref().err().map(super::code);
        crate::support_trace::source_done(source, result.as_ref().ok().copied(), error.as_deref());
        result
    }
    async fn scan_source_inner(
        &self,
        app: &AppHandle,
        job: &mut Value,
        account: &str,
        source: &str,
        mode: &str,
        client: &reqwest::Client,
        selected: Option<&BTreeSet<String>>,
        public: &reqwest::Client,
        enriched: &mut BTreeMap<String, Value>,
        publications: &mut Publications,
        prefetch: Option<&PublicPrefetch>,
    ) -> Result<bool> {
        let key = format!("booth:{account}:{source}");
        let baseline_key = format!("booth-complete:{account}:{source}");
        let previous_baseline = self
            .inner
            .catalogue
            .get("checkpoint", &baseline_key)?
            .and_then(|v| serde_json::from_value::<Baseline>(v).ok());
        let baseline = previous_baseline
            .as_ref()
            .filter(|b| b.usable(account, source));
        let previous = self
            .inner
            .catalogue
            .get("checkpoint", &key)?
            .map(serde_json::from_value::<Checkpoint>)
            .transpose()
            .map_err(|_| anyhow::anyhow!("CHECKPOINT_INVALID"))?;
        let mut checkpoint = if let Some(previous) = previous.filter(|p| {
            !p.finished
                && p.version == 1
                && p.publication_version == 1
                && p.account_id == account
                && p.source == source
                && p.mode == mode
                && mode != "forced"
                && selected.is_none()
                && (source != "free" || p.free_item_adapter_version == FREE_ITEM_ADAPTER_VERSION)
        }) {
            previous
        } else {
            Checkpoint::fresh(account, source, mode)?
        };
        // An interrupted source rereads both overlap boundaries before continuing its queue.
        if !checkpoint.committed.is_empty() {
            let head_route = format!("{}?page=1", safety::source_path(source)?);
            let head = self
                .fetch_page(client, source, &head_route, prefetch)
                .await?;
            let mut boundary_page = None;
            if let Some(boundary) = checkpoint.boundary.as_deref().filter(|b| *b != head_route) {
                safety::route(boundary, source)?;
                let page = self.fetch_page(client, source, boundary, prefetch).await?;
                boundary_page = Some(page);
            }
            let moved = overlap_changed(&checkpoint, &head, boundary_page.as_ref());
            if moved {
                checkpoint = Checkpoint::fresh(account, source, mode)?;
            }
        }
        let mut tracker = if mode == "incremental" && selected.is_none() && source != "free" {
            baseline.map(Tracker::new)
        } else {
            None
        };
        // Reconstruct exactly the durable parsed prefix after a cancellation/crash.
        let mut recovered_overlap = None;
        let mut free_history = BTreeMap::new();
        for id in &checkpoint.journal_ids {
            let value = self
                .inner
                .catalogue
                .get("checkpoint", id)?
                .ok_or_else(|| anyhow::anyhow!("CHECKPOINT_INVALID"))?;
            let page = checked_journal_page(&value, account, source, &checkpoint.run_id)?;
            remember_free_history(&page, &mut free_history);
            if page
                .free_item
                .as_ref()
                .is_some_and(|e| !free_history.contains_key(&e.product_id))
            {
                bail!("FREE_ITEM_HISTORY_UNPROVEN");
            }
            if let Some(tracker) = tracker.as_mut() {
                let proof = tracker.observe(&page);
                if checkpoint.overlap_end.is_some() && proof == checkpoint.overlap_end {
                    recovered_overlap = proof;
                    break;
                }
            }
        }
        if checkpoint.overlap_end.is_some() && checkpoint.overlap_end != recovered_overlap {
            bail!("CHECKPOINT_INVALID");
        }
        self.save_checkpoint(&key, &checkpoint)?;
        self.inner.catalogue.prune_store_checkpoints("BOOTH", account)?;
        let coverage_id = format!("{account}:{source}");
        if let Some((kind, id, value)) = source_coverage_record(
            selected,
            &coverage_id,
            json!({"id":coverage_id,"accountId":account,"source":source,"status":"running","mode":mode,"freeHistoryStartsAt":if source=="free"{Some("2026-06-15")}else{None},"historicalComplete":false,"incrementalBoundary":if tracker.is_some(){"awaitingNewestFirstOverlap"}else{"fullTraversalWhenOrderingUnproven"},"lastFullTraversalAt":previous_baseline.as_ref().map(|b| &b.full_traversal_at),"updatedAt":now()}),
        ) {
            self.inner.catalogue.put(&kind, &id, value)?;
        }
        let mut reads = FuturesUnordered::new();
        let mut active_routes = BTreeSet::new();
        while !checkpoint.pending.is_empty() {
            if checkpoint.pages + checkpoint.details_completed >= 10000
                || checkpoint.pending.len() > 50000
            {
                bail!("SOURCE_TRAVERSAL_LIMIT");
            }
            if self.cancelled(job)? {
                bail!("CANCELLED");
            }
            let routes = available_routes(&checkpoint.pending, &active_routes)?;
            for route in routes {
                if let Some(id) = safety::free_item_id(&route) {
                    if source != "free" || !free_history.contains_key(id) {
                        bail!("FREE_ITEM_HISTORY_UNPROVEN");
                    }
                }
                active_routes.insert(route.clone());
                reads.push(async move {
                    let page = self.fetch_page(client, source, &route, prefetch).await?;
                    Ok::<_, anyhow::Error>((route, page))
                });
            }
            if let Some(read) = reads.next().await {
                let (route, page) = read?;
                active_routes.remove(&route);
                if self.cancelled(job)? {
                    bail!("CANCELLED");
                }
                // Persist only parsed digital projections. Replaying this journal is idempotent.
                let journal_id = format!(
                    "booth-page:{}:{}",
                    checkpoint.run_id,
                    safety::hash(format!("{}:{}", page.route, page.fingerprint))
                );
                let journal = json!({"version":1,"id":journal_id,"accountId":account,"source":source,"runId":checkpoint.run_id,"page":page});
                remember_free_history(&page, &mut free_history);
                if let Some(evidence) = &page.free_item {
                    free_history
                        .get(&evidence.product_id)
                        .ok_or_else(|| anyhow::anyhow!("FREE_ITEM_HISTORY_UNPROVEN"))?;
                }
                for row in &page.rows {
                    if let Some(id) = &row.product_id {
                        checkpoint.product_ids.insert(id.clone());
                    }
                    if selected.is_some_and(|ids| {
                        row.product_id.as_ref().is_some_and(|id| !ids.contains(id))
                    }) {
                        continue;
                    }
                    if let Some(id) = &row.product_id {
                        checkpoint.product_ids.insert(id.clone());
                        if mode != "incremental"
                            || self.inner.catalogue.get("product", id)?.is_none_or(|p| {
                                p["publicMetadataObservedAt"].is_null()
                                    || p["publicMetadataState"] == "incomplete"
                            })
                        {
                            checkpoint.enrichment_pending.insert(id.clone());
                        }
                    }
                }
                record_source_page(&mut checkpoint, source, &route, &page, selected)?;
                if let Some(limit) = crate::verification::source_page_limit() {
                    limit_listing_pages(&mut checkpoint, limit);
                }
                checkpoint.journal_ids.push(journal_id.clone());
                if let Some(end) = tracker
                    .as_mut()
                    .and_then(|t| t.observe(&page))
                    .filter(|_| !page.terminal)
                {
                    checkpoint.overlap_end = Some(end);
                    checkpoint.pending.clear();
                    // This terminal belongs to the previous complete tail, not this page.
                    checkpoint.terminal = true;
                }
                if !page.is_detail && !page.terminal && page.next.is_empty() {
                    checkpoint.unresolved += 1;
                }
                self.commit_source_page(&journal_id, &journal, &key, &checkpoint)?;
                if let Some(prefetch) = prefetch {
                    let mut discovered = page.clone();
                    if let Some(evidence) = &page.free_item {
                        discovered.rows = free_history
                            .get(&evidence.product_id)
                            .cloned()
                            .unwrap_or_default();
                        for row in &mut discovered.rows {
                            augment_free_row(row, evidence);
                        }
                    }
                    // Free history queues metadata after its item endpoint so
                    // that support HTML is already available to reuse. Other
                    // sources keep publishing while these endpoints are pending.
                    let products: BTreeSet<_> = discovered
                        .rows
                        .iter()
                        .filter(|_| source != "free" || page.free_item.is_some())
                        .filter_map(|row| row.product_id.as_ref())
                        .filter(|id| checkpoint.enrichment_pending.contains(*id))
                        .collect();
                    for product in products {
                        let first = prefetch.enqueue_once(product)?;
                        if !first {
                            continue;
                        }
                        let mut batch = super::batch::RecordBatch::new(&self.inner.catalogue);
                        for row in discovered
                            .rows
                            .iter()
                            .filter(|r| r.product_id.as_ref() == Some(product))
                        {
                            self.plan_row(
                                &mut batch,
                                account,
                                source,
                                row,
                                &page.route,
                                mode != "incremental",
                            )?;
                        }
                        if let Some(draft) = batch.get("product", product)? {
                            prefetch
                                .requests
                                .send(PublicRequest {
                                    product: product.clone(),
                                    draft,
                                    account: account.into(),
                                    source: source.into(),
                                    run_id: checkpoint.run_id.clone(),
                                })
                                .map_err(|_| anyhow::anyhow!("PUBLIC_METADATA_FAILED"))?;
                        }
                    }
                    let published = self.stage_discovered_page(
                        job,
                        account,
                        source,
                        mode,
                        &key,
                        &checkpoint,
                        &discovered,
                        prefetch,
                    )?;
                    source_changed(app, published);
                }
                job["progress"] = json!({"stage":"collecting","pages":checkpoint.pages,"detailsCompleted":checkpoint.details_completed,"requestsCompleted":checkpoint.pages+checkpoint.details_completed,"products":checkpoint.product_ids.len(),"published":prefetch.and_then(|p|p.early.lock().ok().map(|e|e.published.len())).unwrap_or(checkpoint.published_products.len()),"source":source,"metadataCompleted":checkpoint.product_ids.len()-checkpoint.enrichment_pending.len(),"metadataTotal":checkpoint.product_ids.len()});
                if let Some(prefetch) = prefetch {
                    let early = prefetch
                        .early
                        .lock()
                        .map_err(|_| anyhow::anyhow!("STATE_LOCK_FAILED"))?;
                    set_collecting_totals(job, &early, source);
                }
                self.progress(app, job)?;
            }
        }
        // A source that shifted during the pass cannot be labelled complete.
        let head_route = format!("{}?page=1", safety::source_path(source)?);
        let final_head = self
            .fetch_page(client, source, &head_route, prefetch)
            .await?;
        if self.cancelled(job)? {
            bail!("CANCELLED");
        }
        record_source_head(&mut checkpoint, &final_head);
        if let Some(prefetch) = prefetch {
            let early = prefetch
                .early
                .lock()
                .map_err(|_| anyhow::anyhow!("STATE_LOCK_FAILED"))?;
            set_collecting_totals(job, &early, source);
        }
        job["progress"]["stage"] = json!("enriching");
        self.progress(app, job)?;
        // Replace the accessible manifest only after its current item rows were durably read.
        // Earlier acquisition observations/artifacts remain; retired remote files are not deleted.
        let mut current_rows = vec![];
        let mut free_evidence = BTreeMap::new();
        let mut row_routes = BTreeMap::new();
        let mut free_routes = BTreeMap::new();
        let mut unbound_membership = false;
        let mut terminal_unproven_observed = false;
        let mut incomplete_products = BTreeSet::new();
        let mut availability_products = BTreeSet::new();
        for (index, journal_id) in checkpoint.journal_ids.iter().enumerate() {
            if index % 16 == 0 {
                // Scanners and public metadata share one try_join task. Let the
                // sibling future run during this synchronous assembly work.
                tokio::task::yield_now().await;
                if self.cancelled(job)? {
                    bail!("CANCELLED");
                }
            }
            let journal = self
                .inner
                .catalogue
                .get("checkpoint", journal_id)?
                .ok_or_else(|| anyhow::anyhow!("CHECKPOINT_INVALID"))?;
            let page = checked_journal_page(&journal, account, source, &checkpoint.run_id)?;
            let mut page_unbound = false;
            // Normal parser unresolved rows are either exact product-bound
            // failures or unbound controls/routes. Free item evidence has its
            // own complete product-file-list scope.
            if page.unresolved != 0 {
                if let Some(evidence) = &page.free_item {
                    if settled_free_absence(&page, &free_history) {
                        availability_products.insert(evidence.product_id.clone());
                    } else {
                        incomplete_products.insert(evidence.product_id.clone());
                    }
                } else if !page.rows.is_empty()
                    && page
                        .rows
                        .iter()
                        .filter(|row| {
                            row.error_code.as_deref() != Some("BOOTH_WITHHELD_PRODUCT_ID")
                        })
                        .all(|row| row.product_id.is_some())
                    && page.rows.iter().any(|row| row.error_code.is_some())
                {
                    incomplete_products.extend(
                        page.rows
                            .iter()
                            .filter(|row| row.error_code.is_some())
                            .filter_map(|row| row.product_id.clone()),
                    );
                } else {
                    unbound_membership = true;
                    page_unbound = true;
                }
            }
            if !page.is_detail && !page.terminal && page.next.is_empty() {
                unbound_membership = true;
                page_unbound = true;
            }
            terminal_unproven_observed |= trace_checked_journal(&page, page_unbound);
            for row in &page.rows {
                row_routes.insert(row.source_record_id.clone(), page.route.clone());
            }
            if let Some(evidence) = page.free_item {
                free_routes.insert(evidence.product_id.clone(), page.route);
                free_evidence.insert(evidence.product_id.clone(), evidence);
            }
            current_rows.extend(page.rows);
        }
        if source == "free" {
            for row in &mut current_rows {
                if let Some(evidence) = row.product_id.as_ref().and_then(|id| free_evidence.get(id))
                {
                    augment_free_row(row, evidence);
                    if let Some(route) = free_routes.get(&evidence.product_id) {
                        row_routes.insert(row.source_record_id.clone(), route.clone());
                    }
                }
            }
        }
        let refreshed_products: BTreeSet<_> = current_rows
            .iter()
            .filter_map(|r| r.product_id.clone())
            .collect();
        let all_rows = if let Some(end) = checkpoint.overlap_end {
            super::incremental::merged_rows(
                baseline.ok_or_else(|| anyhow::anyhow!("CHECKPOINT_INVALID"))?,
                &current_rows,
                end,
            )
            .ok_or_else(|| anyhow::anyhow!("CHECKPOINT_INVALID"))?
        } else {
            current_rows
        };
        // A verified incremental tail retains its original order identity; its
        // receipt route can be reconstructed without a page-number guess.
        for row in &all_rows {
            row_routes
                .entry(row.source_record_id.clone())
                .or_insert_with(|| {
                    row.order_id
                        .as_deref()
                        .filter(|id| source == "purchase" && safety::numeric(id))
                        .map(|id| format!("/orders/{id}"))
                        .unwrap_or_else(|| head_route.clone())
                });
        }
        // Refreshed prefix rows replace old rows. A changed file list must not be
        // unioned with an obsolete journal row from the previous complete pass.
        let mut manifests: BTreeMap<String, (BTreeSet<String>, bool)> = BTreeMap::new();
        let mut rows_by_product: BTreeMap<&str, Vec<&parser::Row>> = BTreeMap::new();
        for row in &all_rows {
            let Some(id) = row.product_id.as_ref() else {
                continue;
            };
            // Preserve every bound receipt row and its original order. Product
            // assembly must not rescan the entire source for each manifest.
            rows_by_product.entry(id.as_str()).or_default().push(row);
            if selected.is_some_and(|s| !s.contains(id))
                || (source == "purchase" && row.files.is_empty())
            {
                continue;
            }
            let manifest = manifests
                .entry(id.clone())
                .or_insert((BTreeSet::new(), true));
            manifest.1 &= row.manifest_complete;
            manifest.0.extend(observed_file_ids(account, [row]));
        }
        let mut records = vec![];
        let closed_traversal = checkpoint.terminal
            && checkpoint.pending.is_empty()
            && !checkpoint.head.is_empty()
            && checkpoint.head == final_head.fingerprint;
        let mut enrichment_error = None;
        let metadata_total = manifests.len();
        let mut metadata_completed = checkpoint.published_products.len();
        if let Some(prefetch) = prefetch {
            let mut early = prefetch
                .early
                .lock()
                .map_err(|_| anyhow::anyhow!("STATE_LOCK_FAILED"))?;
            early.remaining_sources.remove(source);
            if unbound_membership || !closed_traversal {
                early
                    .publications
                    .unbound_sources
                    .insert((account.into(), source.into()));
            }
            early
                .publications
                .checkpoints
                .insert(key.clone(), checkpoint.clone());
            if early.remaining_sources.is_empty() {
                self.publish_early(app, job, &mut early, None)?;
            }
        }
        for (index, (product, (files, complete))) in manifests.into_iter().enumerate() {
            if index % 16 == 0 {
                tokio::task::yield_now().await;
                if self.cancelled(job)? {
                    bail!("CANCELLED");
                }
            }
            if checkpoint.overlap_end.is_some() && !refreshed_products.contains(&product) {
                continue; // The untouched tail retains its actual prior observation time.
            }
            let already_published = checkpoint.published_products.contains(&product);
            let manifest_complete = complete
                && !incomplete_products.contains(&product)
                && (free_evidence
                    .get(&product)
                    .is_some_and(|e| e.manifest_complete)
                    || (closed_traversal && !unbound_membership));
            let draft_id = format!("booth-product:{}:{product}", checkpoint.run_id);
            let product_rows = rows_by_product
                .remove(product.as_str())
                .ok_or_else(|| anyhow::anyhow!("CHECKPOINT_INVALID"))?;
            let availability_only = source == "free"
                && availability_products.contains(&product)
                && !incomplete_products.contains(&product);
            let observed_rows =
                observed_grant_rows(source, &product_rows, manifest_complete, availability_only);
            let observed_files = observed_file_ids(account, observed_rows.iter().copied());
            let saved_draft = self.inner.catalogue.get("checkpoint", &draft_id)?;
            if !availability_only
                && self.restore_product_publication(
                    &product,
                    &product_rows,
                    &row_routes,
                    &files,
                    manifest_complete,
                    saved_draft.as_ref(),
                    &mut checkpoint,
                )?
            {
                if !already_published {
                    metadata_completed += 1;
                }
                let draft = if let Some(saved) = saved_draft.as_ref().filter(|saved| {
                    product_draft_scope(saved, account, source, &checkpoint.run_id, &product)
                        && saved["enriched"] == true
                }) {
                    saved["product"].clone()
                } else {
                    self.inner
                        .catalogue
                        .get("product", &product)?
                        .ok_or_else(|| anyhow::anyhow!("CHECKPOINT_INVALID"))?
                };
                self.retain_published_context(
                    publications,
                    prefetch,
                    ProductPublication {
                        account: account.into(),
                        source: source.into(),
                        product: product.clone(),
                        rows: product_rows.iter().map(|row| (*row).clone()).collect(),
                        routes: product_rows
                            .iter()
                            .filter_map(|row| {
                                row_routes
                                    .get(&row.source_record_id)
                                    .map(|route| (row.source_record_id.clone(), route.clone()))
                            })
                            .collect(),
                        draft,
                        files,
                        checkpoint_key: key.clone(),
                        force_verify: mode != "incremental",
                        needs_enrichment: false,
                    },
                )?;
                continue;
            }
            let mut batch = super::batch::RecordBatch::new(&self.inner.catalogue);
            for row in &product_rows {
                let route = row_routes
                    .get(&row.source_record_id)
                    .map(String::as_str)
                    .unwrap_or(&head_route);
                self.plan_row_mode(
                    &mut batch,
                    account,
                    source,
                    row,
                    route,
                    mode != "incremental",
                    availability_only,
                    None,
                )?;
            }
            let mut draft = batch
                .get("product", &product)?
                .ok_or_else(|| anyhow::anyhow!("CHECKPOINT_INVALID"))?;
            let mut needs_enrichment = checkpoint.enrichment_pending.contains(&product);
            if let Some(saved) = saved_draft.filter(|s| {
                product_draft_scope(s, account, source, &checkpoint.run_id, &product)
                    && s["enriched"] == true
            }) {
                draft = saved["product"].clone();
                needs_enrichment = false;
            } else if let Some(observation) = enriched.get(&product) {
                draft = observation.clone();
                needs_enrichment = false;
            } else if checkpoint.enrichment_pending.contains(&product) && prefetch.is_none() {
                job["progress"] = json!({"stage":"enriching","source":source,"pages":checkpoint.pages,"detailsCompleted":checkpoint.details_completed,"requestsCompleted":checkpoint.pages+checkpoint.details_completed,"products":checkpoint.product_ids.len(),"remaining":checkpoint.enrichment_pending.len(),"published":checkpoint.published_products.len(),"metadataCompleted":metadata_completed,"metadataTotal":metadata_total});
                self.progress(app, job)?;
                if let Err(error) = self.enrich_into(public, &product, &mut draft, None).await {
                    let error_code = super::code(&error);
                    self.save_product_draft(&draft_id,
                        json!({"id":draft_id,"accountId":account,"source":source,"runId":checkpoint.run_id,"productId":product,"product":draft,"enriched":false,"errorCode":error_code}))?;
                    enrichment_error.get_or_insert(error_code);
                    continue;
                }
                enriched.insert(product.clone(), draft.clone());
                needs_enrichment = false;
            }
            if needs_enrichment {
                if let Some(prefetch) = prefetch {
                    let first = prefetch.enqueue_once(&product)?;
                    if first {
                        prefetch
                            .requests
                            .send(PublicRequest {
                                product: product.clone(),
                                draft: draft.clone(),
                                account: account.into(),
                                source: source.into(),
                                run_id: checkpoint.run_id.clone(),
                            })
                            .map_err(|_| anyhow::anyhow!("PUBLIC_METADATA_FAILED"))?;
                    }
                }
            }
            if needs_enrichment && prefetch.is_some() {
                let request = PublicRequest {
                    product: product.clone(),
                    draft: draft.clone(),
                    account: account.into(),
                    source: source.into(),
                    run_id: checkpoint.run_id.clone(),
                };
                if let Some(saved) = self
                    .inner
                    .catalogue
                    .get("checkpoint", &draft_id)?
                    .and_then(|saved| request.saved_product(&saved))
                {
                    draft = saved;
                    needs_enrichment = false;
                }
            }
            self.save_product_draft(&draft_id,
                json!({"id":draft_id,"accountId":account,"source":source,"runId":checkpoint.run_id,"productId":product,"product":draft,"enriched":!needs_enrichment,"errorCode":if manifest_complete{Value::Null}else{json!("SYNC_INCOMPLETE")}}))?;
            if self.cancelled(job)? {
                bail!("CANCELLED");
            }
            metadata_completed += 1;
            if !manifest_complete && !availability_only {
                checkpoint.enrichment_pending.insert(product.clone());
                if let Some(prefetch) = prefetch {
                    prefetch
                        .early
                        .lock()
                        .map_err(|_| anyhow::anyhow!("STATE_LOCK_FAILED"))?
                        .publications
                        .incomplete_scopes
                        .insert((account.into(), source.into(), product.clone()));
                }
                publications.incomplete_scopes.insert((
                    account.into(),
                    source.into(),
                    product.clone(),
                ));
                if observed_rows.is_empty() {
                    continue;
                }
            }
            job["progress"] = json!({"stage":if prefetch.is_some(){"collecting"}else{"publishing"},"source":source,"pages":checkpoint.pages,"detailsCompleted":checkpoint.details_completed,"requestsCompleted":checkpoint.pages+checkpoint.details_completed,"products":checkpoint.product_ids.len(),"remaining":checkpoint.enrichment_pending.len(),"published":checkpoint.published_products.len(),"metadataCompleted":metadata_completed,"metadataTotal":metadata_total});
            if let Some(prefetch) = prefetch {
                let early = prefetch
                    .early
                    .lock()
                    .map_err(|_| anyhow::anyhow!("STATE_LOCK_FAILED"))?;
                set_collecting_totals(job, &early, source);
            }
            job["progress"]["stage"] = json!("enriching");
            self.progress(app, job)?;
            checkpoint.enrichment_pending.insert(product.clone());
            let product_routes = observed_rows
                .iter()
                .filter_map(|row| {
                    row_routes
                        .get(&row.source_record_id)
                        .map(|route| (row.source_record_id.clone(), route.clone()))
                })
                .collect();
            let entry = ProductPublication {
                account: account.into(),
                source: source.into(),
                product: product.clone(),
                rows: observed_rows.into_iter().cloned().collect(),
                routes: product_routes,
                draft,
                files: observed_files,
                checkpoint_key: key.clone(),
                force_verify: mode != "incremental",
                needs_enrichment,
            };
            if availability_only {
                publications
                    .availability
                    .entry(product.clone())
                    .or_default()
                    .push(entry);
            } else {
                remember_product_context(publications, entry);
            }
            if let Some(prefetch) = prefetch.filter(|_| !availability_only) {
                let mut early = prefetch
                    .early
                    .lock()
                    .map_err(|_| anyhow::anyhow!("STATE_LOCK_FAILED"))?;
                let entry = publications.products[&product].last().unwrap().clone();
                remember_product_context(&mut early.publications, entry);
                self.publish_early(app, job, &mut early, Some(&BTreeSet::from([product])))?;
            }
        }
        drop(rows_by_product);
        // Collection closure and publication completion are distinct. Pending
        // prepared products remain checkpoint work until their atomic commit.
        let collected_complete =
            collection_membership_complete(&checkpoint, &publications.incomplete_scopes);
        let full_traversal_at =
            if checkpoint.overlap_end.is_some() || !collected_complete || selected.is_some() {
                previous_baseline
                    .as_ref()
                    .map(|b| b.full_traversal_at.clone())
                    .unwrap_or_default()
            } else {
                now()
            };
        if collected_complete && selected.is_none() && super::incremental::unique(&all_rows) {
            checkpoint
                .product_ids
                .extend(all_rows.iter().filter_map(|r| r.product_id.clone()));
            let base = Baseline {
                version: 1,
                account_id: account.into(),
                source: source.into(),
                complete: true,
                ordering: checkpoint.ordering,
                rows: all_rows,
                full_traversal_at: full_traversal_at.clone(),
            };
            let mut value = serde_json::to_value(base)?;
            value["id"] = json!(baseline_key);
            records.push(("checkpoint".into(), baseline_key.clone(), value));
        }
        checkpoint.finished = false;
        let mut finished = serde_json::to_value(&checkpoint)?;
        finished["id"] = json!(key);
        records.push(("checkpoint".into(), key.clone(), finished));
        records.extend(source_coverage_record(selected, &coverage_id, json!({"id":coverage_id,"accountId":account,"source":source,"status":if checkpoint.complete(){"complete"}else{"incomplete"},"coverageComplete":checkpoint.complete(),"terminalSeen":checkpoint.terminal,"unresolvedRows":checkpoint.unresolved,"availabilityWarnings":availability_products.len(),"pages":checkpoint.pages,"detailsCompleted":checkpoint.details_completed,"requestsCompleted":checkpoint.pages+checkpoint.details_completed,"productCount":checkpoint.product_ids.len(),"mode":mode,"freeHistoryStartsAt":if source=="free"{Some("2026-06-15")}else{None},"historicalComplete":false,"giftEventLedgerComplete":false,"incrementalBoundary":if checkpoint.overlap_end.is_some(){"verifiedNewestFirstStableOverlap"}else{"fullTraversalWhenOrderingUnproven"},"lastFullTraversalAt":full_traversal_at,"ordering":checkpoint.ordering,"completedAt":now()})));
        // Advance baseline, completion and manifests together: a crash cannot resume
        // an old prefix against a newly replaced baseline.
        let mut collected = serde_json::to_value(&checkpoint)?;
        collected["id"] = json!(key);
        let mut coverage = records
            .iter()
            .find(|(kind, _, _)| kind == "source_coverage")
            .map(|(_, _, value)| value.clone());
        let mut staged = vec![("checkpoint".into(), key.clone(), collected)];
        if let Some(coverage) = &mut coverage {
            coverage["status"] = json!("collected");
            coverage["coverageComplete"] = json!(false);
            coverage.as_object_mut().unwrap().remove("completedAt");
            staged.push(("source_coverage".into(), coverage_id, coverage.clone()));
        }
        self.inner.catalogue.put_many(staged)?;
        if unbound_membership || !closed_traversal {
            publications
                .unbound_sources
                .insert((account.into(), source.into()));
        }
        publications.checkpoints.insert(key, checkpoint.clone());
        publications.source_closures.insert(
            (account.into(), source.into()),
            (unbound_membership, !checkpoint.head.is_empty() && checkpoint.head == final_head.fingerprint),
        );
        trace_source_assembly(&checkpoint, publications, terminal_unproven_observed);
        publications.final_records.extend(records);
        if prefetch.is_none() {
            job["progress"]["stage"] = json!("finalizing");
            self.progress(app, job)?;
        }
        if let Some(error_code) = enrichment_error {
            bail!(error_code);
        }
        Ok(collection_settled(
            &checkpoint,
            collected_complete,
            closed_traversal,
            unbound_membership || !incomplete_products.is_empty(),
            availability_products.len(),
        ))
    }
    fn commit_source_page(
        &self,
        journal_id: &str,
        journal: &Value,
        key: &str,
        checkpoint: &Checkpoint,
    ) -> Result<()> {
        let mut durable = serde_json::to_value(checkpoint)?;
        durable["id"] = json!(key);
        for _ in 0..4 {
            let mut batch = super::batch::RecordBatch::new(&self.inner.catalogue);
            batch.stage("checkpoint", journal_id, journal.clone())?;
            batch.stage("checkpoint", key, durable.clone())?;
            if batch.try_commit()?.is_some() {
                return Ok(());
            }
        }
        bail!("CONCURRENT_UPDATE")
    }
    async fn prefetch_metadata(
        &self,
        app: &AppHandle,
        job: &Value,
        early: &std::sync::Arc<Mutex<EarlyPublications>>,
        client: &reqwest::Client,
        mut requests: mpsc::UnboundedReceiver<PublicRequest>,
        results: mpsc::UnboundedSender<(String, Result<Value, String>)>,
        support_pages: &Arc<Mutex<SupportPages>>,
    ) -> Result<()> {
        let mut seen = BTreeSet::new();
        let mut workers = FuturesUnordered::new();
        let mut closed = false;
        loop {
            if closed && workers.is_empty() {
                return Ok(());
            }
            tokio::select! {
                request = requests.recv(), if !closed && workers.len() < 2 => {
                    match request {
                        Some(request) if seen.insert(request.product.clone()) => {
                            if seen.len() > 10000 { bail!("SOURCE_TRAVERSAL_LIMIT"); }
                            workers.push(async move {
                                let id = request.product.clone();
                                let draft_id = request.draft_id();
                                let before = request.draft.clone();
                                let mut draft = request.draft.clone();
                                let saved = self.inner.catalogue.get("checkpoint", &draft_id)?
                                    .and_then(|saved| request.saved_product(&saved));
                                let outcome = if let Some(saved) = saved {
                                    draft = saved;
                                    Ok(())
                                } else {
                                    self.enrich_into(client, &id, &mut draft, Some(support_pages)).await
                                };
                                let result = outcome.map(|_| public_patch(&before, &draft))
                                    .map_err(|error| super::code(&error));
                                // Current-run parsed product draft only; never publish it here.
                                // Completed public requests survive cancellation/restart.
                                self.save_product_draft(&draft_id,
                                    json!({"id":draft_id,"accountId":request.account,"source":request.source,
                                        "runId":request.run_id,"productId":id,"product":draft,
                                        "enriched":result.is_ok(),"errorCode":result.as_ref().err()}))?;
                                // Exhausted server throttling stops the whole pipeline;
                                // do not advance immediately to another product request.
                                if result.as_ref().err().is_some_and(|code| code == "RATE_LIMITED") {
                                    bail!("RATE_LIMITED");
                                }
                                Ok::<_, anyhow::Error>((id, result))
                            });
                        }
                        Some(_) => {},
                        None => closed = true,
                    }
                }
                result = workers.next(), if !workers.is_empty() => {
                    if let Some(result) = result {
                        let result = result?;
                        {
                            let mut early = early.lock().map_err(|_| anyhow::anyhow!("STATE_LOCK_FAILED"))?;
                            early.observations.insert(result.0.clone(),result.1.clone());
                            self.publish_early(app,job,&mut early,Some(&BTreeSet::from([result.0.clone()])))?;
                        }
                        if results.send(result).is_err() { return Ok(()); }
                    }
                }
            }
        }
    }
    fn publish_early(
        &self,
        app: &AppHandle,
        job: &Value,
        early: &mut EarlyPublications,
        candidates: Option<&BTreeSet<String>>,
    ) -> Result<()> {
        let products = self.publish_early_records(job, early, candidates)?;
        source_changed(app, products);
        Ok(())
    }
    // Each complete, bound grant can be consumed before the next listing page.
    // No source-wide closure is claimed by these additive observations.
    fn stage_discovered_page(
        &self,
        job: &Value,
        account: &str,
        source: &str,
        mode: &str,
        key: &str,
        checkpoint: &Checkpoint,
        page: &parser::Page,
        prefetch: &PublicPrefetch,
    ) -> Result<Vec<String>> {
        let mut early = prefetch
            .early
            .lock()
            .map_err(|_| anyhow::anyhow!("STATE_LOCK_FAILED"))?;
        early
            .publications
            .checkpoints
            .insert(key.into(), checkpoint.clone());
        let products: BTreeSet<_> = page
            .rows
            .iter()
            .filter(|row| !row.files.is_empty())
            .filter_map(|row| row.product_id.clone())
            .collect();
        for product in &products {
            let incomplete = page.rows.iter().any(|row| {
                row.product_id.as_deref() == Some(product.as_str())
                    && (!row.manifest_complete || row.error_code.is_some())
            });
            if incomplete {
                if source == "free"
                    && (page.free_item.is_none()
                        || page.free_item.as_ref().is_some_and(|evidence| {
                            settled_free_absence(
                                page,
                                &BTreeMap::from([(evidence.product_id.clone(), page.rows.clone())]),
                            )
                        }))
                {
                    // History awaits its product endpoint; a confirmed absent
                    // offer cannot block a different source's complete grant.
                } else {
                    early.publications.incomplete_scopes.insert((
                        account.into(),
                        source.into(),
                        product.clone(),
                    ));
                }
            }
            let incoming: Vec<_> = page
                .rows
                .iter()
                .filter(|row| {
                    row.product_id.as_deref() == Some(product.as_str())
                        && !row.files.is_empty()
                        && row.manifest_complete
                        && row.error_code.is_none()
                })
                .collect();
            if incoming.is_empty() {
                continue;
            }
            let mut batch = super::batch::RecordBatch::new(&self.inner.catalogue);
            for row in &incoming {
                self.plan_row(
                    &mut batch,
                    account,
                    source,
                    row,
                    &page.route,
                    mode != "incremental",
                )?;
            }
            let draft = batch
                .get("product", product)?
                .ok_or_else(|| anyhow::anyhow!("PRODUCT_INVALID"))?;
            let entries = early
                .publications
                .products
                .entry(product.clone())
                .or_default();
            if !entries
                .iter()
                .any(|entry| entry.source == source && entry.account == account)
            {
                entries.push(ProductPublication {
                    account: account.into(),
                    source: source.into(),
                    product: product.clone(),
                    rows: vec![],
                    routes: BTreeMap::new(),
                    draft: draft.clone(),
                    files: BTreeSet::new(),
                    checkpoint_key: key.into(),
                    force_verify: mode != "incremental",
                    needs_enrichment: checkpoint.enrichment_pending.contains(product),
                });
            }
            let entry = entries
                .iter_mut()
                .find(|entry| entry.source == source && entry.account == account)
                .unwrap();
            entry.draft = draft;
            for row in incoming {
                entry
                    .rows
                    .retain(|previous| previous.source_record_id != row.source_record_id);
                entry.rows.push(row.clone());
                entry
                    .routes
                    .insert(row.source_record_id.clone(), page.route.clone());
            }
            entry.files = observed_file_ids(account, entry.rows.iter());
        }
        self.publish_early_records(job, &mut early, Some(&products))
    }
    fn publish_early_records(
        &self,
        job: &Value,
        early: &mut EarlyPublications,
        candidates: Option<&BTreeSet<String>>,
    ) -> Result<Vec<String>> {
        let mut published = Vec::new();
        if early.closed {
            return Ok(published);
        }
        // Most callbacks change one product or one listing page. Do not visit
        // or serialize every earlier draft to discover it is unchanged.
        let candidates = candidates
            .cloned()
            .unwrap_or_else(|| early.publications.products.keys().cloned().collect());
        let candidates: Vec<_> = candidates
            .into_iter()
            .filter(|product| {
                // Library-only observations cannot supply an unseen receipt's
                // date/paid fields. A complete receipt/gift/free grant can.
                early
                    .publications
                    .products
                    .get(product)
                    .is_some_and(|entries| {
                        entries.iter().any(|entry| {
                            entry.source != "library"
                                || early.remaining_sources.is_empty()
                                    && publication_membership_proven(
                                        entries,
                                        &early.publications.unbound_sources,
                                    )
                        })
                    })
            })
            .collect();
        for product in candidates {
            if early.publications.products[&product]
                .iter()
                .all(|entry| publication_entry_published(entry, &early.publications.checkpoints))
            {
                // Restored scopes supply membership/progress context but have
                // no new facts to replay in the additive discovery pass.
                early.published.insert(product);
                continue;
            }
            if let Some(observation) = early.observations.get(&product) {
                match observation {
                    Ok(patch) => {
                        for entry in early.publications.products.get_mut(&product).unwrap() {
                            if publication_entry_published(entry, &early.publications.checkpoints) {
                                continue;
                            }
                            entry
                                .draft
                                .as_object_mut()
                                .unwrap()
                                .extend(patch.as_object().unwrap().clone());
                            entry.needs_enrichment = false;
                        }
                    }
                    Err(_) => {}
                }
            }
            let entries = &early.publications.products[&product];
            let ready: Vec<_> = entries
                .iter()
                .filter(|entry| {
                    !entry.needs_enrichment
                        && !publication_entry_published(entry, &early.publications.checkpoints)
                })
                .cloned()
                .collect();
            if !ready.is_empty()
                && (ready.iter().any(|entry| entry.source != "library")
                    || early.remaining_sources.is_empty()
                        && publication_membership_proven(
                            entries,
                            &early.publications.unbound_sources,
                        ))
            {
                let version = additive_version(&ready)?;
                if early.publications.additive_versions.get(&product) == Some(&version) {
                    continue;
                }
                self.commit_product_group_mode(
                    job,
                    &product,
                    &ready,
                    &mut early.publications.checkpoints,
                    false,
                )?;
                early
                    .publications
                    .additive_versions
                    .insert(product.clone(), version);
                early.published.insert(product.clone());
                published.push(product);
            }
        }
        Ok(published)
    }
    async fn publish_pipeline(
        &self,
        app: &AppHandle,
        job: &mut Value,
        publications: &mut Publications,
        mut results: mpsc::UnboundedReceiver<(String, Result<Value, String>)>,
    ) -> Result<()> {
        let products = publication_products(publications);
        let total = products.len();
        let mut pending: BTreeSet<_> = products
            .iter()
            .filter(|id| publication_entries(publications, id).any(|e| e.needs_enrichment))
            .cloned()
            .collect();
        let mut error_code = None;
        let mut completed = total - pending.len();
        let mut published = published_product_count(publications);
        // This must also run when every prefetched observation is already done.
        // Source jobs are clones; the original job has only enqueue counters.
        job["progress"] = publication_progress(publications, "enriching", completed, published);
        self.progress(app, job)?;
        // Products with retained valid public metadata need no new request.
        for (index, product) in products.iter().enumerate() {
            if index % 16 == 0 {
                // Certification still shares the public-prefetch task. Give
                // pending HTTP futures a turn before each bounded group.
                tokio::task::yield_now().await;
                if self.cancelled(job)? {
                    bail!("CANCELLED");
                }
            }
            if self.publish_ready_product(app, job, publications, product)? {
                published += 1;
            }
        }
        job["progress"] = publication_progress(publications, "enriching", completed, published);
        self.progress(app, job)?;
        while !pending.is_empty() {
            if self.cancelled(job)? {
                bail!("CANCELLED");
            }
            let (product, observation) = results
                .recv()
                .await
                .ok_or_else(|| anyhow::anyhow!("PUBLIC_METADATA_FAILED"))?;
            if !pending.remove(&product) {
                continue;
            }
            completed += 1;
            match observation {
                Ok(patch) => {
                    for (availability, entry) in publications
                        .products
                        .get_mut(&product)
                        .into_iter()
                        .flatten()
                        .map(|entry| (false, entry))
                        .chain(
                            publications
                                .availability
                                .get_mut(&product)
                                .into_iter()
                                .flatten()
                                .map(|entry| (true, entry)),
                        )
                    {
                        if !availability
                            && publication_entry_published(entry, &publications.checkpoints)
                        {
                            continue;
                        }
                        {
                            entry
                                .draft
                                .as_object_mut()
                                .ok_or_else(|| anyhow::anyhow!("PRODUCT_INVALID"))?
                                .extend(patch.as_object().unwrap().clone());
                            entry.needs_enrichment = false;
                            let checkpoint = &publications.checkpoints[&entry.checkpoint_key];
                            let draft_id = format!("booth-product:{}:{product}", checkpoint.run_id);
                            self.save_product_draft(&draft_id,
                                json!({"id":draft_id,"accountId":entry.account,"source":entry.source,"runId":checkpoint.run_id,"productId":product,"product":entry.draft,"enriched":true}))?;
                        }
                    }
                }
                Err(code) => {
                    error_code.get_or_insert(code);
                    let keys: Vec<_> = publication_entries(publications, &product)
                        .map(|entry| entry.checkpoint_key.clone())
                        .collect();
                    for key in keys {
                        publications
                            .checkpoints
                            .get_mut(&key)
                            .unwrap()
                            .enrichment_pending
                            .insert(product.clone());
                    }
                }
            }
            if self.publish_ready_product(app, job, publications, &product)? {
                published += 1;
            }
            job["progress"] = publication_progress(publications, "enriching", completed, published);
            self.progress(app, job)?;
        }
        self.publish_collected(app, job, publications).await?;
        if let Some(error_code) = error_code {
            bail!(error_code);
        }
        Ok(())
    }
    fn publish_ready_product(
        &self,
        app: &AppHandle,
        job: &mut Value,
        publications: &mut Publications,
        product: &str,
    ) -> Result<bool> {
        if product_publication_done(publications, product) {
            return Ok(false);
        }
        if let Some(entries) = publications.availability.get(product) {
            if !publications.availability_published.contains(product)
                && entries.iter().all(|entry| !entry.needs_enrichment)
                && entries.iter().all(|entry| {
                    !publications
                        .unbound_sources
                        .contains(&(entry.account.clone(), entry.source.clone()))
                })
            {
                self.commit_availability_product(job, product, entries, &publications.checkpoints)?;
                for entry in entries {
                    publications
                        .checkpoints
                        .get_mut(&entry.checkpoint_key)
                        .unwrap()
                        .enrichment_pending
                        .remove(product);
                }
                publications.availability_published.insert(product.into());
                source_changed(app, std::iter::once(product.into()));
            }
        }
        if !publications.products.contains_key(product) {
            return Ok(product_publication_done(publications, product));
        }
        let (authoritative, additive) = ready_product_entries(publications, product);
        if authoritative.is_empty() && additive.is_empty() {
            return Ok(false);
        }
        job["progress"]["stage"] = json!("publishing");
        self.progress(app, job)?;
        if !additive.is_empty() {
            let version = additive_version(&additive)?;
            if publications.additive_versions.get(product) != Some(&version) {
                self.commit_product_group_mode(
                    job,
                    product,
                    &additive,
                    &mut publications.checkpoints,
                    false,
                )?;
                publications
                    .additive_versions
                    .insert(product.into(), version);
                source_changed(app, std::iter::once(product.into()));
            }
        }
        if !authoritative.is_empty() {
            self.commit_product_group(job, product, &authoritative, &mut publications.checkpoints)?;
            source_changed(app, std::iter::once(product.into()));
        }
        Ok(product_publication_done(publications, product))
    }
    async fn publish_collected(
        &self,
        app: &AppHandle,
        job: &mut Value,
        publications: &mut Publications,
    ) -> Result<()> {
        let total = publication_products(publications).len();
        let mut published = published_product_count(publications);
        for (index, product) in publication_products(publications).iter().enumerate() {
            if index % 16 == 0 {
                tokio::task::yield_now().await;
                if self.cancelled(job)? {
                    bail!("CANCELLED");
                }
            }
            if product_publication_done(publications, product) {
                continue;
            }
            job["progress"]["stage"] = json!("publishing");
            job["progress"]["metadataTotal"] = json!(total);
            job["progress"]["metadataCompleted"] = json!(total);
            job["progress"]["published"] = json!(published);
            self.progress(app, job)?;
            if self.publish_ready_product(app, job, publications, product)? {
                published += 1;
            }
        }
        let mut final_records = Vec::new();
        for (kind, id, mut value) in std::mem::take(&mut publications.final_records) {
            let source_checkpoint = publications.checkpoints.values().find(|checkpoint| {
                value["accountId"] == checkpoint.account_id && value["source"] == checkpoint.source
            });
            let complete = source_checkpoint.is_some_and(|checkpoint| {
                checkpoint.complete()
                    && !publications
                        .unbound_sources
                        .contains(&(checkpoint.account_id.clone(), checkpoint.source.clone()))
                    && !source_has_incomplete_product(checkpoint, &publications.incomplete_scopes)
            });
            if kind == "checkpoint" && id.starts_with("booth-complete:") && !complete {
                continue;
            }
            if kind == "source_coverage" {
                value["status"] = json!(if complete { "complete" } else { "incomplete" });
                value["coverageComplete"] = json!(complete);
                value["errorCode"] = if complete {
                    Value::Null
                } else {
                    json!("SYNC_INCOMPLETE")
                };
            }
            if kind == "checkpoint" {
                if let Some(checkpoint) = publications.checkpoints.get(&id) {
                    let mut checkpoint = checkpoint.clone();
                    // Retry completed discovery with pending public work; a
                    // genuinely unresolved source must be traversed afresh.
                    checkpoint.finished = checkpoint_finished_after_collection(
                        &checkpoint,
                        complete,
                        &publications.incomplete_scopes,
                    );
                    value = serde_json::to_value(checkpoint)?;
                    value["id"] = json!(id);
                }
            }
            final_records.push((kind, id, value));
        }
        let completed = publication_products(publications)
            .iter()
            .filter(|product| {
                publication_entries(publications, product).all(|entry| !entry.needs_enrichment)
            })
            .count();
        // Publication can settle enrichment work after source assembly. Refresh
        // the last closure without counting its observed reasons a second time.
        for checkpoint in publications.checkpoints.values() {
            trace_source_closure(checkpoint, publications);
        }
        job["progress"] = publication_progress(publications, "finalizing", completed, published);
        self.progress(app, job)?;
        for _ in 0..4 {
            let mut batch = super::batch::RecordBatch::new(&self.inner.catalogue);
            let current = batch
                .get("job", field(job, "id")?)?
                .ok_or_else(|| anyhow::anyhow!("JOB_UNAVAILABLE"))?;
            if current["state"] == "cancelled" {
                bail!("CANCELLED");
            }
            for (kind, id, value) in &final_records {
                batch.stage(kind, id, value.clone())?;
            }
            if batch.try_commit()?.is_some() {
                return Ok(());
            }
        }
        bail!("CONCURRENT_UPDATE")
    }
    fn commit_product_group(
        &self,
        job: &Value,
        product: &str,
        entries: &[ProductPublication],
        checkpoints: &mut BTreeMap<String, Checkpoint>,
    ) -> Result<()> {
        self.commit_product_group_mode(job, product, entries, checkpoints, true)
    }
    fn commit_availability_product(
        &self,
        job: &Value,
        product: &str,
        entries: &[ProductPublication],
        checkpoints: &BTreeMap<String, Checkpoint>,
    ) -> Result<()> {
        for _ in 0..4 {
            let mut batch = super::batch::RecordBatch::new(&self.inner.catalogue);
            let current = batch
                .get("job", field(job, "id")?)?
                .ok_or_else(|| anyhow::anyhow!("JOB_UNAVAILABLE"))?;
            if current["state"] == "cancelled" {
                bail!("CANCELLED");
            }
            for entry in entries {
                let checkpoint = checkpoints
                    .get(&entry.checkpoint_key)
                    .ok_or_else(|| anyhow::anyhow!("CHECKPOINT_INVALID"))?;
                if entry.product != product
                    || entry.source != "free"
                    || checkpoint.account_id != entry.account
                    || checkpoint.source != "free"
                    || entry.rows.is_empty()
                    || entry.needs_enrichment
                {
                    bail!("CHECKPOINT_INVALID");
                }
                for row in &entry.rows {
                    let route = entry
                        .routes
                        .get(&row.source_record_id)
                        .ok_or_else(|| anyhow::anyhow!("CHECKPOINT_INVALID"))?;
                    if row.product_id.as_deref() != Some(product)
                        || row.unavailable
                        || row.error_code.is_some()
                        || safety::free_item_id(route) != Some(product)
                    {
                        bail!("CHECKPOINT_INVALID");
                    }
                    self.plan_row_mode(
                        &mut batch,
                        &entry.account,
                        "free",
                        row,
                        route,
                        false,
                        true,
                        None,
                    )?;
                }
                let mut draft = batch
                    .get("product", product)?
                    .ok_or_else(|| anyhow::anyhow!("PRODUCT_INVALID"))?;
                let patch = public_patch(&draft, &entry.draft);
                draft
                    .as_object_mut()
                    .ok_or_else(|| anyhow::anyhow!("PRODUCT_INVALID"))?
                    .extend(patch.as_object().unwrap().clone());
                batch.stage("product", product, draft)?;
            }
            if batch.try_commit()?.is_some() {
                return Ok(());
            }
        }
        bail!("CONCURRENT_UPDATE")
    }
    fn commit_product_group_mode(
        &self,
        job: &Value,
        product: &str,
        entries: &[ProductPublication],
        checkpoints: &mut BTreeMap<String, Checkpoint>,
        authoritative: bool,
    ) -> Result<()> {
        for _ in 0..4 {
            let mut batch = super::batch::RecordBatch::new(&self.inner.catalogue);
            let current = batch
                .get("job", field(job, "id")?)?
                .ok_or_else(|| anyhow::anyhow!("JOB_UNAVAILABLE"))?;
            if current["state"] == "cancelled" {
                bail!("CANCELLED");
            }
            let mut updated = BTreeSet::new();
            for entry in entries {
                if entry.product != product {
                    bail!("CHECKPOINT_INVALID");
                }
                if publication_entry_published(entry, checkpoints) {
                    // Keep the scope as evidence for other pending sources;
                    // its certified facts already committed atomically.
                    continue;
                }
                if job["type"] == "boothUpdate" {
                    ensure!(job["args"]["publicOnly"] != true, "ACCOUNT_UNAVAILABLE");
                    ensure!(job["accountId"].as_str().is_none_or(|account| account == entry.account), "ACCOUNT_UNAVAILABLE");
                    ensure!(batch.get("account", &entry.account)?.is_some_and(|account| account["store"] == "BOOTH"), "ACCOUNT_UNAVAILABLE");
                }
                if entry.needs_enrichment {
                    bail!("PUBLIC_METADATA_FAILED");
                }
                let entitlement = format!("{}:{}:{product}", entry.account, entry.source);
                let previous = batch.get("entitlement", &entitlement)?;
                for row in &entry.rows {
                    self.plan_row_mode(
                        &mut batch,
                        &entry.account,
                        &entry.source,
                        row,
                        entry
                            .routes
                            .get(&row.source_record_id)
                            .ok_or_else(|| anyhow::anyhow!("CHECKPOINT_INVALID"))?,
                        entry.force_verify,
                        false,
                        Some(field(job, "id")?),
                    )?;
                }
                let mut draft = batch
                    .get("product", product)?
                    .unwrap_or_else(|| entry.draft.clone());
                let patch = public_patch(&draft, &entry.draft);
                draft
                    .as_object_mut()
                    .ok_or_else(|| anyhow::anyhow!("PRODUCT_INVALID"))?
                    .extend(patch.as_object().unwrap().clone());
                batch.stage("product", product, draft)?;
                let mut variant_ids: BTreeSet<String> = entry
                    .rows
                    .iter()
                    .flat_map(|r| &r.files)
                    .filter_map(|f| f.variant_id.as_deref())
                    .filter(|id| safety::numeric(id))
                    .map(str::to_owned)
                    .collect();
                let mut files = entry.files.clone();
                if !authoritative {
                    if let Some(previous) = &previous {
                        files.extend(
                            previous["fileIds"]
                                .as_array()
                                .into_iter()
                                .flatten()
                                .filter_map(Value::as_str)
                                .map(str::to_owned),
                        );
                        variant_ids.extend(
                            previous["variantIds"]
                                .as_array()
                                .into_iter()
                                .flatten()
                                .filter_map(Value::as_str)
                                .map(str::to_owned),
                        );
                    }
                }
                // A complete receipt proves this grant, not that a later order
                // cannot add another owned variant. Retain a previously certified
                // unchanged membership; certify new membership only at closure.
                let unchanged = previous.as_ref().is_some_and(|value| {
                    let saved = |field: &str| -> BTreeSet<String> {
                        value[field]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .filter_map(Value::as_str)
                            .map(str::to_owned)
                            .collect()
                    };
                    value["manifestComplete"] == true
                        && saved("fileIds") == files
                        && saved("variantIds") == variant_ids
                });
                let exact_free = entry.source == "free"
                    && !entry.rows.is_empty()
                    && entry.rows.iter().all(|row| row.manifest_complete);
                let complete = authoritative || unchanged || exact_free;
                batch.stage("entitlement",&entitlement,json!({"id":entitlement,"productId":product,"accountId":entry.account,"source":entry.source,"fileIds":files,"variantIds":variant_ids,"manifestComplete":complete,"currentManifest":true,"manifestBasis":if authoritative{"verifiedAccountProductFileList"}else{"completeObservedGrants"},"observedAt":now()}))?;
                if !authoritative {
                    continue;
                }
                let checkpoint = checkpoints
                    .get(&entry.checkpoint_key)
                    .ok_or_else(|| anyhow::anyhow!("CHECKPOINT_INVALID"))?;
                if checkpoint.account_id != entry.account || checkpoint.source != entry.source {
                    bail!("CHECKPOINT_INVALID");
                }
                let draft_id = format!("booth-product:{}:{product}", checkpoint.run_id);
                let signature = publication_manifest(
                    &entry.account,
                    &entry.source,
                    product,
                    &entry.rows,
                    &entry.routes,
                    &entry.files,
                )?;
                // The small current-product marker commits with all published
                // facts and the cancellation guard. The source/page checkpoint
                // is unchanged until finalization, avoiding a whole-source copy
                // for every product. Restart verifies this marker against the
                // durable journals and actual entitlement before skipping work.
                batch.stage(
                    "checkpoint",
                    &draft_id,
                    json!({"id":draft_id,
                    "accountId":entry.account,"source":entry.source,"runId":checkpoint.run_id,
                    "productId":product,"product":entry.draft,"enriched":true,
                    "published":true,"publishedManifest":signature}),
                )?;
                updated.insert(entry.checkpoint_key.clone());
            }
            if batch.try_commit()?.is_some() {
                for key in updated {
                    let checkpoint = checkpoints.get_mut(&key).unwrap();
                    checkpoint.published_products.insert(product.into());
                    checkpoint.enrichment_pending.remove(product);
                }
                return Ok(());
            }
        }
        bail!("CONCURRENT_UPDATE")
    }
    #[cfg(test)]
    fn publish_product(
        &self,
        job: &Value,
        account: &str,
        source: &str,
        product: &str,
        rows: &[&parser::Row],
        routes: &BTreeMap<String, String>,
        draft: &Value,
        files: &BTreeSet<String>,
        key: &str,
        checkpoint: &mut Checkpoint,
        force_verify: bool,
    ) -> Result<()> {
        let entry = ProductPublication {
            account: account.into(),
            source: source.into(),
            product: product.into(),
            rows: rows.iter().map(|r| (*r).clone()).collect(),
            routes: routes.clone(),
            draft: draft.clone(),
            files: files.clone(),
            checkpoint_key: key.into(),
            force_verify,
            needs_enrichment: false,
        };
        let mut checkpoints = BTreeMap::from([(key.to_owned(), checkpoint.clone())]);
        self.commit_product_group(job, product, &[entry], &mut checkpoints)?;
        *checkpoint = checkpoints.remove(key).unwrap();
        Ok(())
    }
    fn save_checkpoint(&self, key: &str, c: &Checkpoint) -> Result<()> {
        let mut value = serde_json::to_value(c)?;
        value["id"] = json!(key);
        self.inner.catalogue.put("checkpoint", key, value)
    }
    fn save_product_draft(&self, id: &str, value: Value) -> Result<()> {
        for _ in 0..4 {
            let mut batch = super::batch::RecordBatch::new(&self.inner.catalogue);
            let mut value = value.clone();
            if let Some(saved) = batch.get("checkpoint", id)? {
                if saved["published"] == true
                    && saved["publishedManifest"].is_string()
                    && product_draft_scope(
                        &saved,
                        field(&value, "accountId")?,
                        field(&value, "source")?,
                        field(&value, "runId")?,
                        field(&value, "productId")?,
                    )
                {
                    value["published"] = json!(true);
                    value["publishedManifest"] = saved["publishedManifest"].clone();
                }
            }
            // A late metadata callback cannot erase a concurrently committed
            // publication marker: it retries against the latest draft instead.
            batch.stage("checkpoint", id, value)?;
            if batch.try_commit()?.is_some() {
                return Ok(());
            }
        }
        bail!("CONCURRENT_UPDATE")
    }
    fn restore_product_publication(
        &self,
        product: &str,
        rows: &[&parser::Row],
        routes: &BTreeMap<String, String>,
        files: &BTreeSet<String>,
        manifest_complete: bool,
        saved: Option<&Value>,
        checkpoint: &mut Checkpoint,
    ) -> Result<bool> {
        if checkpoint.published_products.contains(product) {
            return Ok(true); // Existing validated current-run checkpoint proof.
        }
        let Some(saved) = saved.filter(|saved| {
            manifest_complete
                && saved["published"] == true
                && saved["enriched"] == true
                && product_draft_scope(
                    saved,
                    &checkpoint.account_id,
                    &checkpoint.source,
                    &checkpoint.run_id,
                    product,
                )
        }) else {
            return Ok(false);
        };
        let signature = publication_manifest(
            &checkpoint.account_id,
            &checkpoint.source,
            product,
            rows.iter().copied(),
            routes,
            files,
        )?;
        if saved["publishedManifest"] != signature {
            return Ok(false);
        }
        let entitlement_id = format!("{}:{}:{product}", checkpoint.account_id, checkpoint.source);
        let Some(entitlement) = self.inner.catalogue.get("entitlement", &entitlement_id)? else {
            return Ok(false);
        };
        let variant_ids: BTreeSet<_> = rows
            .iter()
            .flat_map(|row| &row.files)
            .filter_map(|file| file.variant_id.as_deref())
            .filter(|id| safety::numeric(id))
            .map(str::to_owned)
            .collect();
        if entitlement["accountId"] != checkpoint.account_id
            || entitlement["source"] != checkpoint.source
            || entitlement["productId"] != product
            || entitlement["manifestComplete"] != true
            || entitlement["currentManifest"] != true
            || entitlement["manifestBasis"] != "verifiedAccountProductFileList"
            || entitlement["fileIds"] != json!(files)
            || entitlement["variantIds"] != json!(variant_ids)
            || self.inner.catalogue.get("product", product)?.is_none()
        {
            return Ok(false);
        }
        checkpoint.published_products.insert(product.into());
        checkpoint.enrichment_pending.remove(product);
        Ok(true)
    }
    fn retain_published_context(
        &self,
        publications: &mut Publications,
        prefetch: Option<&PublicPrefetch>,
        entry: ProductPublication,
    ) -> Result<()> {
        if let Some(prefetch) = prefetch {
            let mut early = prefetch
                .early
                .lock()
                .map_err(|_| anyhow::anyhow!("STATE_LOCK_FAILED"))?;
            let checkpoint = early
                .publications
                .checkpoints
                .get_mut(&entry.checkpoint_key)
                .ok_or_else(|| anyhow::anyhow!("CHECKPOINT_INVALID"))?;
            checkpoint.published_products.insert(entry.product.clone());
            checkpoint.enrichment_pending.remove(&entry.product);
            early.published.insert(entry.product.clone());
            remember_product_context(&mut early.publications, entry.clone());
        }
        remember_product_context(publications, entry);
        Ok(())
    }
    #[cfg(test)]
    pub(super) fn record_row(
        &self,
        account: &str,
        source: &str,
        row: &parser::Row,
        route: &str,
        force_verify: bool,
    ) -> Result<()> {
        for _ in 0..4 {
            let mut batch = super::batch::RecordBatch::new(&self.inner.catalogue);
            self.plan_row(&mut batch, account, source, row, route, force_verify)?;
            if batch.try_commit()?.is_some() {
                return Ok(());
            }
        }
        bail!("CONCURRENT_UPDATE")
    }
    pub(super) fn plan_row(
        &self,
        batch: &mut super::batch::RecordBatch<'_>,
        account: &str,
        source: &str,
        row: &parser::Row,
        route: &str,
        force_verify: bool,
    ) -> Result<()> {
        self.plan_row_mode(
            batch,
            account,
            source,
            row,
            route,
            force_verify,
            false,
            None,
        )
    }
    fn plan_row_mode(
        &self,
        batch: &mut super::batch::RecordBatch<'_>,
        account: &str,
        source: &str,
        row: &parser::Row,
        route: &str,
        force_verify: bool,
        history_only: bool,
        observation_job: Option<&str>,
    ) -> Result<()> {
        let canonical_row = if batch
            .get(
                "acquisition",
                &format!("{account}:{}", row.source_record_id),
            )?
            .is_none()
            && parser::product_field_file_keys(row).is_some()
        {
            let previous = batch.related("acquisition", row.product_id.as_deref().unwrap())?;
            unique_previous_acquisition_record(&previous, account, source, row).map(
                |source_record_id| {
                    let mut canonical = row.clone();
                    canonical.source_record_id = source_record_id;
                    canonical
                },
            )
        } else {
            None
        };
        let row = canonical_row.as_ref().unwrap_or(row);
        let acquisition_id = format!("{account}:{}", row.source_record_id);
        let previous_acquisition = batch.get("acquisition", &acquisition_id)?;
        let digital_product_id = if source == "purchase" && row.files.is_empty() {
            None
        } else {
            row.product_id.as_ref()
        };
        {
            let mut acquisition = json!({"id":acquisition_id,"sourceRecordId":row.source_record_id,"productId":digital_product_id,"observedProductId":row.product_id,"accountId":account,"source":source,"orderId":row.order_id,"acquiredDateText":row.acquired_date_text,"paidAmountText":row.paid_amount_text,"variantName":row.variant_name,"unavailable":row.unavailable,"errorCode":row.error_code,"observedAt":now()});
            let variant_ids: BTreeSet<_> = row
                .files
                .iter()
                .filter_map(|f| f.variant_id.as_deref())
                .filter(|id| safety::numeric(id))
                .collect();
            acquisition["variantIds"] = json!(variant_ids);
            if let Some(previous) = previous_acquisition
                .as_ref()
                .filter(|previous| same_acquisition_binding(previous, account, source, row))
            {
                // Sparse refreshes do not erase historical facts or their binding.
                for key in [
                    "acquiredDateText",
                    "paidAmountText",
                    "orderId",
                    "observedProductId",
                ] {
                    if acquisition[key]
                        .as_str()
                        .is_none_or(|value| value.trim().is_empty())
                        && previous[key]
                            .as_str()
                            .is_some_and(|value| !value.trim().is_empty())
                    {
                        acquisition[key] = previous[key].clone();
                    }
                }
                if source == "purchase"
                    && row.files.is_empty()
                    && previous["productId"].as_str().is_some_and(safety::numeric)
                {
                    acquisition["productId"] = previous["productId"].clone();
                }
                if previous["paidCurrency"] == "JPY"
                    && previous["paidPriceBasis"] == "productLineTotal"
                    && previous["paidPrice"].is_number()
                    && metadata::parse_jpy_price(&previous["paidPrice"]).is_some()
                {
                    for key in ["paidPrice", "paidCurrency", "paidPriceBasis"] {
                        acquisition[key] = previous[key].clone();
                    }
                }
                if variant_ids.is_empty() && row.files.is_empty() {
                    acquisition["variantIds"] = previous["variantIds"].clone();
                    acquisition["variantPayments"] = previous["variantPayments"].clone();
                } else if previous["variantIds"] == acquisition["variantIds"] {
                    acquisition["variantPayments"] = previous["variantPayments"].clone();
                }
            }
            if let Some(amount) = native_product_line_total(row, source, route) {
                acquisition["paidPrice"] = json!(amount);
                acquisition["paidCurrency"] = json!("JPY");
                acquisition["paidPriceBasis"] = json!("productLineTotal");
                if variant_ids.len() == 1 && row.files.iter().all(|f| f.variant_id.is_some()) {
                    acquisition["variantPayments"] = json!([{"variantId":variant_ids.iter().next().unwrap(),"name":row.variant_name,"price":amount,"currency":"JPY"}]);
                }
            }
            batch.stage("acquisition", &acquisition_id, acquisition)?;
        }
        let Some(product_id) = &row.product_id else {
            return Ok(());
        };
        if row.files.is_empty() && source == "purchase" {
            return Ok(());
        }
        let mut product=batch.get("product",product_id)?.unwrap_or_else(||json!({"id":product_id,"store":"BOOTH","title":row.name,"sourceTitle":row.name,"name":row.name,"author":row.shop,"shop":row.shop,"sourceShop":{"name":row.shop,"url":row.shop_url},"previewUrl":row.preview_url,"previews":row.preview_url.iter().collect::<Vec<_>>(),"sourceUrl":format!("https://booth.pm/ja/items/{product_id}"),"sourceTags":[],"tags":[],"sourceCategory":null}));
        // Sparse account observations must not replace richer last reliable public metadata.
        for (key, value) in [
            ("title", row.name.as_ref()),
            ("sourceTitle", row.name.as_ref()),
            ("name", row.name.as_ref()),
            ("shop", row.shop.as_ref()),
            ("author", row.shop.as_ref()),
            ("previewUrl", row.preview_url.as_ref()),
        ] {
            if product[key].is_null() {
                if let Some(value) = value {
                    product[key] = json!(value);
                }
            }
        }
        product["observedAt"] = json!(now());
        metadata::ensure_source_fields(&mut product)?;
        batch.stage("product", product_id, product)?;
        if history_only {
            return Ok(());
        }
        let entitlement_id = format!("{account}:{source}:{product_id}");
        let mut file_ids = vec![];
        for f in &row.files {
            let file_id = format!(
                "{account}:{}:{}",
                f.id,
                f.variant_id.as_deref().unwrap_or("")
            );
            file_ids.push(file_id.clone());
            let existing = batch.get("remote_file", &file_id)?;
            let observation = observation_job
                .map(|job| source_file_observation(job, account, product_id, f))
                .transpose()?;
            // A later grant or closing certification can replay a descriptor
            // already refreshed and positively verified during this same job.
            // New jobs and changed source facts must still force verification.
            let already_observed = observation.as_ref().is_some_and(|token| {
                existing.as_ref().is_some_and(|saved| {
                    saved["sourceObservationToken"].as_str() == Some(token.as_str())
                        && saved["accountId"] == account
                        && saved["productId"] == product_id.as_str()
                })
            });
            let (revision, basis, verify) =
                revision_observation(existing.as_ref(), f, force_verify && !already_observed);
            let name = f.name.clone().or_else(|| {
                existing
                    .as_ref()
                    .and_then(|e| e["name"].as_str())
                    .map(str::to_owned)
            });
            let size = f
                .size_bytes
                .or_else(|| existing.as_ref().and_then(|e| e["sizeBytes"].as_u64()));
            let mut remote = json!({"id":file_id,"productId":product_id,"accountId":account,"sourceFileId":f.id,"variantId":f.variant_id,"variantName":row.variant_name,"name":name,"sizeBytes":size,"revision":revision,"revisionBasis":basis,"requiresContentVerification":verify,"sourceUrl":format!("https://booth.pm/downloadables/{}",f.id),"resolverPage":route,"source":source,"observedAt":now()});
            if let Some(token) = observation {
                remote["sourceObservationToken"] = json!(token);
            }
            batch.stage("remote_file", &file_id, remote)?;
        }
        let existing = batch.get("entitlement", &entitlement_id)?;
        let mut all: BTreeSet<String> = existing
            .iter()
            .flat_map(|e| e["fileIds"].as_array().into_iter().flatten())
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect();
        all.extend(file_ids);
        let fresh = json!({"id":entitlement_id,"productId":product_id,"accountId":account,"source":source,"fileIds":all,"manifestComplete":false,"currentManifest":true,"observedAt":now(),"unavailable":row.unavailable});
        let mut entitlement = existing.unwrap_or_else(|| json!({}));
        entitlement
            .as_object_mut()
            .ok_or_else(|| anyhow::anyhow!("ENTITLEMENT_INVALID"))?
            .extend(fresh.as_object().unwrap().clone());
        batch.stage("entitlement", &entitlement_id, entitlement)
    }
    async fn enrich(&self, client: &reqwest::Client, id: &str) -> Result<()> {
        let mut product = self
            .inner
            .catalogue
            .get("product", id)?
            .unwrap_or_else(|| json!({"id":id}));
        self.enrich_into(client, id, &mut product, None).await?;
        self.inner.catalogue.put("product", id, product)
    }
    async fn enrich_into(
        &self,
        client: &reqwest::Client,
        id: &str,
        product: &mut Value,
        support_pages: Option<&Arc<Mutex<SupportPages>>>,
    ) -> Result<()> {
        if !safety::numeric(id) {
            bail!("INVALID_PRODUCT_ID");
        }
        let (status, body) = self
            .inner
            .http
            .get(
                client,
                url::Url::parse(&format!("https://booth.pm/ja/items/{id}.json")).unwrap(),
            )
            .await?;
        if status.as_u16() == 404 || status.as_u16() == 410 {
            metadata::apply_public_response(product, id, status.as_u16(), None)?;
            return Ok(());
        }
        if !status.is_success() {
            bail!("PUBLIC_METADATA_FAILED");
        }
        let value: Value =
            serde_json::from_str(&body).map_err(|_| anyhow::anyhow!("PUBLIC_METADATA_INVALID"))?;
        metadata::apply_public_response(product, id, status.as_u16(), Some(&value))?;
        // The JSON description can omit the dedicated support section. Only
        // supplement products whose digital variants do not establish it.
        if metadata::needs_public_support_html(product) {
            if let Some(support_pages) = support_pages {
                if support_pages
                    .lock()
                    .map_err(|_| anyhow::anyhow!("STATE_LOCK_FAILED"))?
                    .apply(id, product)
                {
                    return Ok(());
                }
            }
            let url = url::Url::parse(&format!("https://booth.pm/ja/items/{id}"))?;
            let support = self.inner.http.get(client, url).await;
            match support {
                Ok((status, html)) if status.is_success() => {
                    metadata::apply_public_support_html(product, &html)?
                }
                Ok(_) => {
                    product["publicMetadataState"] = json!("incomplete");
                    product["publicMetadataErrorCode"] = json!("PUBLIC_METADATA_FAILED");
                    bail!("PUBLIC_METADATA_FAILED");
                }
                Err(error) => {
                    // Keep newly obtained price/title fields even when the
                    // required support request needs a later retry.
                    product["publicMetadataState"] = json!("incomplete");
                    product["publicMetadataErrorCode"] = json!(super::code(&error));
                    return Err(error);
                }
            }
        }
        Ok(())
    }
}
fn same_acquisition_binding(
    previous: &Value,
    account: &str,
    source: &str,
    row: &parser::Row,
) -> bool {
    if row.source_record_id.is_empty()
        || previous["accountId"] != account
        || previous["source"] != source
        || previous["sourceRecordId"] != row.source_record_id
    {
        return false;
    }
    let known = |value: Option<&str>| {
        value
            .filter(|value| !value.trim().is_empty())
            .map(str::to_owned)
    };
    let orders: BTreeSet<_> = [
        known(previous["orderId"].as_str()),
        known(row.order_id.as_deref()),
    ]
    .into_iter()
    .flatten()
    .collect();
    let products: BTreeSet<_> = [
        known(previous["productId"].as_str()),
        known(previous["observedProductId"].as_str()),
        known(row.product_id.as_deref()),
    ]
    .into_iter()
    .flatten()
    .collect();
    orders.len() <= 1 && products.len() <= 1
}
fn unique_previous_acquisition_record(
    previous: &[Value],
    account: &str,
    source: &str,
    row: &parser::Row,
) -> Option<String> {
    let product = row.product_id.as_deref().filter(|id| safety::numeric(id))?;
    let order = row.order_id.as_deref().unwrap_or("");
    if source == "purchase" && !safety::numeric(order) {
        return None;
    }
    if !matches!(source, "purchase" | "library" | "gift" | "free") {
        return None;
    }
    let valid_date = |date: &str| {
        date.is_empty()
            || parser::slash_date_shape(date)
            || chrono::DateTime::parse_from_rfc3339(date).is_ok()
            || chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d").is_ok()
    };
    let fresh_date = row.acquired_date_text.as_deref().unwrap_or("");
    if !valid_date(fresh_date) {
        return None;
    }
    let record_id = |variant: &str, date: &str| {
        format!(
            "{source}:{}",
            safety::hash(format!("{order}:{product}:{variant}:{date}:"))
        )
    };
    let mut variants = BTreeSet::from([String::new()]);
    variants.extend(row.files.iter().filter_map(|file| file.variant_id.clone()));
    // Explicit line IDs and occurrence suffixes are not date-based identities.
    let bound_variants: Vec<_> = variants
        .into_iter()
        .filter(|variant| {
            row.source_record_id == record_id(variant, fresh_date)
                || row.source_record_id == record_id(variant, "")
        })
        .collect();
    if bound_variants.len() != 1 {
        return None;
    }
    let variant = &bound_variants[0];
    let matching: Vec<_> = previous
        .iter()
        .filter(|saved| {
            if saved["accountId"] != account
                || saved["source"] != source
                || saved["productId"] != product
                || saved["orderId"].as_str().unwrap_or("") != order
                || saved["variantName"].as_str() != row.variant_name.as_deref()
            {
                return false;
            }
            let old_date = saved["acquiredDateText"].as_str().unwrap_or("");
            if !valid_date(old_date)
                || (source != "purchase"
                    && !old_date.is_empty()
                    && !fresh_date.is_empty()
                    && old_date != fresh_date)
            {
                return false;
            }
            let Some(old_record) = saved["sourceRecordId"].as_str() else {
                return false;
            };
            saved["id"] == format!("{account}:{old_record}")
                && (old_record == record_id(variant, old_date)
                    || old_record == record_id(variant, ""))
        })
        .collect();
    // Every same-binding saved native record counts toward uniqueness, including
    // explicit lines/occurrences which cannot be collapsed into a fallback row.
    let bound_count = previous
        .iter()
        .filter(|saved| {
            saved["accountId"] == account
                && saved["source"] == source
                && saved["productId"] == product
                && saved["orderId"].as_str().unwrap_or("") == order
                && saved["variantName"].as_str() == row.variant_name.as_deref()
        })
        .count();
    (matching.len() == 1 && bound_count == 1)
        .then(|| matching[0]["sourceRecordId"].as_str().unwrap().to_owned())
}
fn selected_purchase_routes(
    acquisitions: &[Value],
    remote: &[Value],
    account: &str,
    selected: &BTreeSet<String>,
) -> Option<BTreeMap<String, BTreeSet<String>>> {
    let mut routes: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for product in selected {
        let mut found = BTreeSet::new();
        for acquisition in acquisitions.iter().filter(|a| {
            a["accountId"] == account
                && a["source"] == "purchase"
                && (a["productId"] == *product || a["observedProductId"] == *product)
        }) {
            if let Some(order) = acquisition["orderId"]
                .as_str()
                .filter(|id| safety::numeric(id))
            {
                found.insert(format!("/orders/{order}"));
            }
        }
        for file in remote.iter().filter(|file| {
            file["accountId"] == account
                && file["source"] == "purchase"
                && file["productId"] == *product
        }) {
            if let Some(route) = file["resolverPage"]
                .as_str()
                .and_then(|route| safety::route(route, "purchase").ok())
            {
                if route.query().is_none()
                    && route
                        .path()
                        .strip_prefix("/orders/")
                        .is_some_and(safety::numeric)
                {
                    found.insert(route.path().to_owned());
                }
            }
        }
        if found.is_empty() {
            return None;
        }
        for route in found {
            routes.entry(route).or_default().insert(product.clone());
        }
    }
    (!routes.is_empty()).then_some(routes)
}
fn native_product_line_total(row: &parser::Row, source: &str, route: &str) -> Option<u64> {
    if source != "purchase"
        || row.source_record_id.is_empty()
        || !row.order_id.as_deref().is_some_and(safety::numeric)
        || parser::product_field_file_keys(row).is_none()
    {
        return None;
    }
    let url = safety::route(route, "purchase").ok()?;
    if url.path().strip_prefix("/orders/") != row.order_id.as_deref() || url.query().is_some() {
        return None;
    }
    row.product_line_total_jpy
        .filter(|amount| *amount <= 9_007_199_254_740_991)
}
#[cfg(test)]
fn patched_product_fields(
    existing: &Value,
    account: &str,
    row: &parser::Row,
    route: &str,
) -> Option<Value> {
    let amount = native_product_line_total(row, "purchase", route)?;
    let product = row.product_id.as_deref()?;
    if !same_acquisition_binding(existing, account, "purchase", row)
        || existing["id"] != format!("{account}:{}", row.source_record_id)
        || existing["orderId"].as_str() != row.order_id.as_deref()
        || existing["productId"].as_str() != Some(product)
    {
        return None;
    }
    let mut updated = existing.clone();
    updated["paidPrice"] = json!(amount);
    updated["paidCurrency"] = json!("JPY");
    updated["paidPriceBasis"] = json!("productLineTotal");
    if let Some(date) = row
        .acquired_date_text
        .as_deref()
        .filter(|date| !date.trim().is_empty())
    {
        updated["acquiredDateText"] = json!(date);
    }
    Some(updated)
}
fn saved_product_line_binding(saved: &[parser::Row], fresh: &parser::Row) -> bool {
    let Some(keys) = parser::diagnostic_digital_file_keys(fresh) else {
        return false;
    };
    let bound: Vec<_> = saved
        .iter()
        .filter(|row| row.source_record_id == fresh.source_record_id)
        .collect();
    bound.len() == 1
        && bound[0].product_id == fresh.product_id
        && bound[0].order_id == fresh.order_id
        && parser::diagnostic_digital_file_keys(bound[0]).as_ref() == Some(&keys)
}
fn select_acquisition_diagnostic_detail(
    checkpoint: &Checkpoint,
    journals: &[Value],
    account: &str,
) -> Result<(String, Vec<parser::Row>)> {
    acquisition_diagnostic_details(checkpoint, journals, account)?
        .into_iter()
        .next()
        .ok_or_else(|| anyhow::anyhow!("DIAGNOSTIC_ENTRY_NOT_FOUND"))
}
fn select_targeted_receipt_proof(
    checkpoint: &Checkpoint,
    journals: &[Value],
    account: &str,
    receipt_id: &str,
) -> Result<Option<Vec<parser::Row>>> {
    if !safety::numeric(receipt_id) {
        bail!("INVALID_DIAGNOSTIC_ARGUMENTS");
    }
    let target = format!("/orders/{receipt_id}");
    // The existing validator processes ALL journals, never just the target.
    Ok(
        acquisition_diagnostic_details(checkpoint, journals, account)?
            .into_iter()
            .find(|(route, _)| *route == target)
            .map(|(_, proof)| proof),
    )
}
fn acquisition_diagnostic_details(
    checkpoint: &Checkpoint,
    journals: &[Value],
    account: &str,
) -> Result<Vec<(String, Vec<parser::Row>)>> {
    if checkpoint.version != 1
        || checkpoint.account_id != account
        || checkpoint.source != "purchase"
        || checkpoint.run_id.is_empty()
        || journals.len() != checkpoint.journal_ids.len()
    {
        bail!("CHECKPOINT_INVALID");
    }
    let mut witnessed = BTreeSet::new();
    let mut pages = Vec::new();
    for (id, value) in checkpoint.journal_ids.iter().zip(journals) {
        if value["id"] != id.as_str() {
            bail!("CHECKPOINT_INVALID");
        }
        let page = checked_journal_page(value, account, "purchase", &checkpoint.run_id)?;
        if checkpoint.committed.get(&page.route) != Some(&page.fingerprint) {
            bail!("CHECKPOINT_INVALID");
        }
        let url = safety::route(&page.route, "purchase")
            .map_err(|_| anyhow::anyhow!("CHECKPOINT_INVALID"))?;
        if page.is_detail != (url.path() != "/orders") {
            bail!("CHECKPOINT_INVALID");
        }
        if !page.is_detail {
            for route in &page.details {
                let url = safety::route(route, "purchase")
                    .map_err(|_| anyhow::anyhow!("CHECKPOINT_INVALID"))?;
                if url.path() == "/orders" {
                    bail!("CHECKPOINT_INVALID");
                }
                witnessed.insert(route.clone());
            }
        }
        pages.push(page);
    }
    let mut result = Vec::new();
    let mut seen = BTreeSet::new();
    for page in pages {
        if !page.is_detail || !witnessed.contains(&page.route) {
            continue;
        }
        let receipt_id = page
            .route
            .strip_prefix("/orders/")
            .ok_or_else(|| anyhow::anyhow!("CHECKPOINT_INVALID"))?;
        let proof: Vec<_> = page
            .rows
            .into_iter()
            .filter(|row| {
                row.order_id.as_deref() == Some(receipt_id)
                    && parser::diagnostic_digital_file_keys(row).is_some()
            })
            .collect();
        if !proof.is_empty() && seen.insert(page.route.clone()) {
            result.push((page.route, proof));
        }
    }
    Ok(result)
}
#[cfg(test)]
fn select_entry_diagnostic_route(
    checkpoint: &Checkpoint,
    journals: &[Value],
    account: &str,
) -> Result<String> {
    select_free_item_diagnostic_route(checkpoint, journals, account, FreeItemDiagnostic::Entry)
}
fn select_free_item_diagnostic_route(
    checkpoint: &Checkpoint,
    journals: &[Value],
    account: &str,
    issue: FreeItemDiagnostic,
) -> Result<String> {
    if checkpoint.version != 1
        || checkpoint.free_item_adapter_version != FREE_ITEM_ADAPTER_VERSION
        || checkpoint.account_id != account
        || checkpoint.source != "free"
        || checkpoint.run_id.is_empty()
        || journals.len() != checkpoint.journal_ids.len()
    {
        bail!("CHECKPOINT_INVALID");
    }
    let mut history = BTreeMap::new();
    let mut pages = vec![];
    for (id, value) in checkpoint.journal_ids.iter().zip(journals) {
        if value["id"] != id.as_str() {
            bail!("CHECKPOINT_INVALID");
        }
        let page = checked_journal_page(value, account, "free", &checkpoint.run_id)?;
        if checkpoint.committed.get(&page.route) != Some(&page.fingerprint) {
            bail!("CHECKPOINT_INVALID");
        }
        remember_free_history(&page, &mut history);
        pages.push(page);
    }
    pages
        .into_iter()
        .find_map(|page| {
            page.free_item
                .filter(|e| {
                    e.error_code.as_deref() == Some(issue.error_code())
                        && history.contains_key(&e.product_id)
                })
                .map(|_| page.route)
        })
        .ok_or_else(|| anyhow::anyhow!("DIAGNOSTIC_ENTRY_NOT_FOUND"))
}
fn source_coverage_record(
    selected: Option<&BTreeSet<String>>,
    id: &str,
    value: Value,
) -> Option<(String, String, Value)> {
    selected
        .is_none()
        .then(|| ("source_coverage".into(), id.into(), value))
}
fn checked_journal_page(
    value: &Value,
    account: &str,
    source: &str,
    run: &str,
) -> Result<parser::Page> {
    if value["accountId"] != account || value["source"] != source || value["runId"] != run {
        bail!("CHECKPOINT_INVALID");
    }
    let page: parser::Page = serde_json::from_value(value["page"].clone())
        .map_err(|_| anyhow::anyhow!("CHECKPOINT_INVALID"))?;
    if page.source != source {
        bail!("CHECKPOINT_INVALID");
    }
    if let Some(evidence) = &page.free_item {
        if source != "free"
            || !page.is_detail
            || !page.rows.is_empty()
            || page.terminal
            || !page.next.is_empty()
            || !page.details.is_empty()
            || safety::free_item_id(&page.route) != Some(evidence.product_id.as_str())
            || (evidence.manifest_complete
                && (evidence.files.is_empty()
                    || evidence.unavailable
                    || evidence.error_code.is_some()))
        {
            bail!("CHECKPOINT_INVALID");
        }
    }
    Ok(page)
}
fn eligible_free_rows(page: &parser::Page) -> impl Iterator<Item = &parser::Row> {
    page.rows.iter().filter(|row| {
        page.source == "free"
            && !page.is_detail
            && page.free_item.is_none()
            && safety::route(&page.route, "free").is_ok()
            && !row.source_record_id.is_empty()
            && !row.unavailable
            && row.error_code.is_none()
            && row.product_id.as_deref().is_some_and(safety::numeric)
    })
}
fn remember_free_history(page: &parser::Page, history: &mut BTreeMap<String, Vec<parser::Row>>) {
    for row in eligible_free_rows(page) {
        let rows = history.entry(row.product_id.clone().unwrap()).or_default();
        if !rows
            .iter()
            .any(|old| old.source_record_id == row.source_record_id)
        {
            rows.push(row.clone());
        }
    }
}
fn augment_free_row(row: &mut parser::Row, evidence: &free_item::Evidence) {
    if row.product_id.as_deref() == Some(evidence.product_id.as_str()) {
        row.manifest_complete = false;
    }
    if row.product_id.as_deref() == Some(evidence.product_id.as_str())
        && !row.unavailable
        && row.error_code.is_none()
        && evidence.manifest_complete
        && !evidence.unavailable
        && evidence.error_code.is_none()
        && !evidence.files.is_empty()
    {
        row.files = evidence.files.clone();
        row.manifest_complete = true;
    }
}
fn settled_free_absence(page: &parser::Page, history: &BTreeMap<String, Vec<parser::Row>>) -> bool {
    let Some(evidence) = &page.free_item else {
        return false;
    };
    page.source == "free"
        && page.is_detail
        && page.unresolved == 1
        && safety::free_item_id(&page.route) == Some(evidence.product_id.as_str())
        && evidence.unavailable
        && !evidence.manifest_complete
        && evidence.files.is_empty()
        && matches!(
            evidence.error_code.as_deref(),
            Some("FREE_OFFER_UNAVAILABLE" | "PUBLIC_METADATA_UNAVAILABLE")
        )
        && history.get(&evidence.product_id).is_some_and(|rows| {
            !rows.is_empty()
                && rows.iter().all(|row| {
                    row.product_id.as_deref() == Some(evidence.product_id.as_str())
                        && !row.source_record_id.is_empty()
                        && !row.unavailable
                        && row.error_code.is_none()
                })
        })
}
fn collection_settled(
    checkpoint: &Checkpoint,
    complete: bool,
    closed: bool,
    unbound: bool,
    availability_warnings: usize,
) -> bool {
    complete
        || (checkpoint.source == "free"
            && closed
            && !unbound
            && checkpoint.pending.is_empty()
            && checkpoint.terminal
            && !checkpoint.head.is_empty()
            && availability_warnings > 0
            && checkpoint.unresolved == availability_warnings)
}
fn record_source_page(
    checkpoint: &mut Checkpoint,
    source: &str,
    route: &str,
    page: &parser::Page,
    selected: Option<&BTreeSet<String>>,
) -> Result<()> {
    if page.route == format!("{}?page=1", safety::source_path(source)?) {
        checkpoint.head = page.fingerprint.clone();
    }
    if !page.is_detail {
        checkpoint.boundary = Some(page.route.clone());
        if checkpoint.pages == 0 {
            checkpoint.ordering = page.ordering;
        } else if checkpoint.ordering != page.ordering {
            checkpoint.ordering = parser::Ordering::Unknown;
        }
    }
    checkpoint
        .committed
        .insert(page.route.clone(), page.fingerprint.clone());
    checkpoint.pending.retain(|p| p != route);
    for next in page.next.iter().chain(page.details.iter()) {
        if safety::free_item_id(next)
            .is_some_and(|id| selected.is_some_and(|ids| !ids.contains(id)))
        {
            continue;
        }
        if !checkpoint.committed.contains_key(next) && !checkpoint.pending.contains(next) {
            checkpoint.pending.push(next.clone());
        }
    }
    checkpoint.terminal |= page.terminal;
    checkpoint.unresolved += page.unresolved;
    if page.is_detail {
        checkpoint.details_completed += 1;
    } else {
        checkpoint.pages += 1;
    }
    Ok(())
}
fn limit_listing_pages(checkpoint: &mut Checkpoint, limit: usize) {
    if checkpoint.pages >= limit {
        let before = checkpoint.pending.len();
        // Drain receipts/item endpoints and their metadata without following
        // another listing page or pretending the source reached its end.
        checkpoint.pending.retain(|route| detail_route(route));
        if checkpoint.pending.len() != before {
            checkpoint.terminal = false;
            checkpoint.unresolved += 1;
        }
    }
}
fn collection_membership_complete(
    checkpoint: &Checkpoint,
    incomplete_scopes: &BTreeSet<(String, String, String)>,
) -> bool {
    checkpoint.pending.is_empty()
        && checkpoint.terminal
        && checkpoint.unresolved == 0
        && !source_has_incomplete_product(checkpoint, incomplete_scopes)
}
fn public_patch(_before: &Value, after: &Value) -> Value {
    // Every source draft receives the same complete public observation. A
    // field matching the first draft can still differ on a receipt draft.
    let mut patch: serde_json::Map<String, Value> = after
        .as_object()
        .expect("product object")
        .iter()
        .filter(|(key, _)| public_field(key))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    if after.get("publicMetadataErrorCode").is_none() {
        patch.insert("publicMetadataErrorCode".into(), Value::Null);
    }
    Value::Object(patch)
}
fn public_field(key: &str) -> bool {
    matches!(
        key,
        "store"
            | "title"
            | "sourceTitle"
            | "name"
            | "description"
            | "sourceDescription"
            | "publicPageAvailability"
            | "publishedAt"
            | "listingPriceText"
            | "price"
            | "isAdult"
            | "sensitive"
            | "sourceShop"
            | "shop"
            | "author"
            | "sourceCategory"
            | "sourceTags"
            | "tags"
            | "publicVariants"
            | "previews"
            | "previewUrl"
            | "sourceUrl"
            | "metadataUnavailable"
            | "observedAt"
            | "publicMetadataObservedAt"
            | "publicMetadataState"
            | "publicMetadataErrorCode"
            | "publicSupportAvatars"
            | "publicSupportObservedAt"
    )
}
fn detail_route(route: &str) -> bool {
    safety::free_item_id(route).is_some()
        || route.strip_prefix("/orders/").is_some_and(safety::numeric)
}
#[cfg(test)]
fn next_routes(pending: &[String]) -> Result<Vec<String>> {
    available_routes(pending, &BTreeSet::new())
}
fn available_routes(pending: &[String], active: &BTreeSet<String>) -> Result<Vec<String>> {
    let listing: Vec<_> = pending
        .iter()
        .filter(|r| !detail_route(r) && !active.contains(*r))
        .cloned()
        .collect();
    let details: Vec<_> = pending
        .iter()
        .filter(|r| detail_route(r) && !active.contains(*r))
        .take(2usize.saturating_sub(active.iter().filter(|r| detail_route(r)).count()))
        .cloned()
        .collect();
    let mut routes = Vec::new();
    if !listing.is_empty() && !active.iter().any(|r| !detail_route(r)) {
        routes.push(next_route(&listing)?);
    }
    routes.extend(details);
    if routes.is_empty() && active.is_empty() {
        bail!("CHECKPOINT_INVALID");
    }
    Ok(routes)
}
fn next_route(pending: &[String]) -> Result<String> {
    pending
        .iter()
        .min_by_key(|route| {
            if route.contains('?') {
                route
                    .rsplit('=')
                    .next()
                    .and_then(|v| v.parse::<u32>().ok())
                    .unwrap_or(u32::MAX)
            } else {
                u32::MAX
            }
        })
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("CHECKPOINT_INVALID"))
}
fn overlap_changed(c: &Checkpoint, head: &parser::Page, boundary: Option<&parser::Page>) -> bool {
    if head.fingerprint != c.head {
        return true;
    }
    if let Some(route) = c.boundary.as_deref().filter(|r| *r != head.route) {
        return boundary
            .is_none_or(|p| p.route != route || c.committed.get(route) != Some(&p.fingerprint));
    }
    false
}
fn source_file_observation(
    job: &str,
    account: &str,
    product: &str,
    file: &parser::File,
) -> Result<String> {
    Ok(safety::hash(serde_json::to_vec(&(
        job,
        account,
        product,
        &file.id,
        &file.variant_id,
        &file.name,
        file.size_bytes,
        &file.revision,
    ))?))
}
fn revision_observation(
    existing: Option<&Value>,
    file: &parser::File,
    force: bool,
) -> (String, String, bool) {
    if file.revision != "unknown" {
        return (file.revision.clone(), "source".into(), false);
    }
    let unchanged = existing.is_some_and(|e| {
        e["sourceFileId"] == file.id
            && e["variantId"].as_str() == file.variant_id.as_deref()
            && file.name.as_ref().is_none_or(|n| e["name"] == n.as_str())
            && file
                .size_bytes
                .is_none_or(|n| e["sizeBytes"].as_u64().is_none_or(|old| old == n))
    });
    if unchanged {
        if let Some(old) = existing
            .and_then(|e| e["revision"].as_str())
            .filter(|r| !r.is_empty() && *r != "unknown")
        {
            let basis = existing
                .and_then(|e| e["revisionBasis"].as_str())
                .unwrap_or("source")
                .to_owned();
            let pending = existing.is_some_and(|e| e["requiresContentVerification"] == true);
            return (old.to_owned(), basis, force || pending);
        }
    }
    ("unknown".into(), "unknown".into(), true)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_source_local_failures_allow_other_source_scans_to_continue() {
        for code in ["COLLECTOR_UNSUPPORTED", "SOURCE_UNSUPPORTED", "CHECKPOINT_INVALID",
            "NO_RECOGNIZED_ROWS", "SOURCE_MARKER_UNKNOWN", "DIGITAL_PROOF_UNPROVEN",
            "SOURCE_TRAVERSAL_LIMIT", "METADATA_TOO_LARGE"] {
            assert!(source_local_failure(code));
        }
        for code in ["AUTH_REQUIRED", "AUTH_CHALLENGE", "PERMISSION_DENIED", "RATE_LIMITED",
            "NETWORK_UNAVAILABLE", "NETWORK_TIMEOUT", "SOURCE_HTTP_FAILED", "CANCELLED",
            "ACCOUNT_UNAVAILABLE", "CONCURRENT_UPDATE", "catalogue.lock"] {
            assert!(!source_local_failure(code));
        }
    }
    #[test]
    fn selected_update_cannot_recreate_a_removed_accounts_grants() {
        let path = std::env::temp_dir().join(format!("nna-booth-removed-account-{}", uuid::Uuid::new_v4()));
        let catalogue = crate::catalogue::Catalogue::open(&path).unwrap();
        let service = BoothService::new(catalogue.clone()).unwrap();
        let account = "10000000-0000-4000-8000-000000000001";
        let job = json!({"id":"authored-update","type":"boothUpdate","accountId":account,"state":"running","args":{"accountId":account,"productIds":["900001"]}});
        catalogue.put("job", "authored-update", job.clone()).unwrap();
        let entry = ProductPublication {
            account:account.into(), source:"library".into(), product:"900001".into(),
            rows:vec![], routes:BTreeMap::new(), draft:json!({"id":"900001"}),
            files:BTreeSet::new(), checkpoint_key:"authored-source".into(),
            force_verify:true, needs_enrichment:false,
        };
        let mut checkpoints = BTreeMap::from([("authored-source".to_owned(), Checkpoint::fresh(account, "library", "full").unwrap())]);
        assert_eq!(service.commit_product_group_mode(&job, "900001", &[entry], &mut checkpoints, true).unwrap_err().to_string(), "ACCOUNT_UNAVAILABLE");
        for kind in ["product", "acquisition", "entitlement", "remote_file"] {
            assert!(catalogue.list(kind).unwrap().is_empty());
        }
        drop(service);
        drop(catalogue);
        std::fs::remove_dir_all(path).unwrap();
    }

    #[tokio::test]
    async fn source_diagnostics_keep_checked_journal_reasons_and_changed_head() {
        let page = parser::Page {
            source: "purchase".into(), route: "/orders/910001".into(),
            rows: vec![parser::Row {
                source_record_id: "AUTHORED_SOURCE_MARKER".into(),
                product_id: Some("900001".into()),
                name: Some("AUTHORED_TITLE_MARKER".into()),
                error_code: Some("DIGITAL_CONTROLS_UNRESOLVED".into()),
                ..Default::default()
            }],
            next: vec![], details: vec![], terminal: false, unresolved: 1,
            fingerprint: "AUTHORED_CHANGED_HEAD".into(), is_detail: true,
            ordering: parser::Ordering::Unknown, free_item: None,
        };
        let mut checkpoint = Checkpoint::fresh("AUTHORED_ACCOUNT_MARKER", "purchase", "forced").unwrap();
        checkpoint.head = "AUTHORED_ORIGINAL_HEAD".into();
        let (_, trace) = crate::support_trace::capture(async {
            assert!(!trace_checked_journal(&page, false));
            // A resumed journal observation is counted again, not deduplicated
            // into an invented number of unique orders.
            assert!(!trace_checked_journal(&page, false));
            record_source_head(&mut checkpoint, &page);
            let mut missing_terminal = page.clone();
            missing_terminal.source = "library".into();
            missing_terminal.is_detail = false;
            missing_terminal.rows.clear();
            missing_terminal.unresolved = 0;
            assert!(trace_checked_journal(&missing_terminal, true));
            let free = free_item::Evidence::unknown("900002", "FREE_FILE_CONTROLS_UNAVAILABLE", false)
                .page().unwrap();
            trace_checked_journal(&free, false);
        }).await;
        assert_eq!(checkpoint.unresolved, 1);
        assert_eq!(trace["sources"]["purchase"]["reasons"]["DIGITAL_CONTROLS_UNRESOLVED"], 2);
        assert_eq!(trace["sources"]["purchase"]["reasons"]["SOURCE_HEAD_CHANGED"], 1);
        assert_eq!(trace["sources"]["library"]["reasons"]["SOURCE_TERMINAL_UNPROVEN"], 1);
        assert_eq!(trace["sources"]["library"]["reasons"]["SOURCE_MEMBERSHIP_UNBOUND"], 1);
        assert_eq!(trace["sources"]["free"]["reasons"]["FREE_FILE_CONTROLS_UNAVAILABLE"], 1);
        let text = crate::support::report(&json!({"type":"boothSync","supportDiagnostics":trace}), &[], &[]);
        assert!(text.contains("checkedJournalAndAssemblyObservationsThisAttempt"));
        for marker in ["AUTHORED_", "/orders/", "900001", "900002", "910001"] {
            assert!(!text.contains(marker));
        }
    }
    #[tokio::test]
    async fn source_diagnostics_refresh_closure_without_recounting_incomplete_manifests() {
        let account = "AUTHORED_ACCOUNT_MARKER";
        let mut checkpoint = Checkpoint::fresh(account, "purchase", "forced").unwrap();
        checkpoint.unresolved = 3;
        checkpoint.enrichment_pending.extend(["900001".into(), "900002".into()]);
        let mut publications = Publications::default();
        publications.source_closures.insert((account.into(), "purchase".into()), (true, false));
        publications.incomplete_scopes.extend([
            (account.into(), "purchase".into(), "900001".into()),
            (account.into(), "purchase".into(), "900002".into()),
            (account.into(), "gift".into(), "900003".into()),
            ("AUTHORED_OTHER_ACCOUNT".into(), "purchase".into(), "900004".into()),
        ]);
        let before = serde_json::to_value(&checkpoint).unwrap();
        let (_, trace) = crate::support_trace::capture(async {
            trace_source_assembly(&checkpoint, &publications, false);
            assert_eq!(serde_json::to_value(&checkpoint).unwrap(), before);
            checkpoint.enrichment_pending.clear();
            trace_source_closure(&checkpoint, &publications);
        }).await;
        let source = &trace["sources"]["purchase"];
        assert_eq!(source["reasons"]["SOURCE_MANIFEST_INCOMPLETE"], 2);
        assert_eq!(source["reasons"]["SOURCE_TERMINAL_UNPROVEN"], 1);
        assert_eq!(source["lastClosure"], json!({
            "scope":"latestObservedSourceClosure", "terminalSeen":false,
            "pendingRequests":1,"unresolvedRows":3,"enrichmentPending":0,
            "incompleteProducts":2,"membershipUnbound":true,"headStable":false
        }));
        assert!(source.get("state").is_none());
        let text = serde_json::to_string(&trace).unwrap();
        for marker in ["AUTHORED_", "900001", "900002", "900003", "900004"] {
            assert!(!text.contains(marker));
        }
    }
    #[test]
    fn verification_prefix_drains_details_without_certifying_source_end() {
        let mut checkpoint = Checkpoint::fresh("authored-account", "purchase", "forced").unwrap();
        checkpoint.pages = 4;
        checkpoint.pending = vec!["/orders?page=5".into(), "/orders/900001".into()];
        limit_listing_pages(&mut checkpoint, 5);
        assert_eq!(checkpoint.pending.len(), 2);
        checkpoint.pages = 5;
        checkpoint.pending[0] = "/orders?page=6".into();
        limit_listing_pages(&mut checkpoint, 5);
        assert_eq!(checkpoint.pending, vec!["/orders/900001"]);
        assert!(!checkpoint.terminal);
        assert_eq!(checkpoint.unresolved, 1);
        assert!(!collection_membership_complete(
            &checkpoint,
            &BTreeSet::new()
        ));

        let mut natural_end = Checkpoint::fresh("authored-account", "purchase", "forced").unwrap();
        natural_end.pages = 2;
        natural_end.terminal = true;
        natural_end.pending.clear();
        limit_listing_pages(&mut natural_end, 5);
        assert!(collection_membership_complete(
            &natural_end,
            &BTreeSet::new()
        ));
    }
    fn support_page_fixture(description: &str) -> String {
        format!("<html><head><link rel='canonical' href='https://booth.pm/ja/items/900001'><meta property='og:url' content='https://booth.pm/ja/items/900001'></head><body><div id='items' class='market' data-product-id='900001'><ul id='variations'><li class='variation-item'><div class='cart-button-wrap'><a title='authored.zip' href='https://booth.pm/downloadables/700001?variation_id=800001'><span class='cmd-label'>無料ダウンロード</span></a><div class='text-14 text-text-default font-normal text-left mb-8'>authored.zip</div></div></li></ul>{description}</div><p>PRIVATE_AUTH_BODY_SENTINEL</p></body></html>")
    }
    #[test]
    fn free_support_page_reuse_is_product_scoped_and_keeps_only_public_fields() {
        let body = support_page_fixture("<section class='shop__text'><h2>対応アバター</h2><p>しなの https://ponderogen.booth.pm/items/6106863</p></section>");
        let evidence = free_item::parse(&body, "900001").unwrap();
        assert!(evidence.manifest_complete);
        let original_files = evidence.files.clone();
        let mut pages = SupportPages::default();
        pages.remember(&evidence, &body).unwrap();
        let mut product =
            json!({"id":"900001","note":"Human note","publicSupportAvatars":[{"id":"old"}]});
        assert!(!pages.apply("900002", &mut product));
        assert!(pages.apply("900001", &mut product));
        assert_eq!(product["publicSupportAvatars"][0]["id"], "booth:6106863");
        assert_eq!(
            product["publicSupportAvatars"][0]["entitlementEstablished"],
            false
        );
        assert_eq!(product["note"], "Human note");
        assert_eq!(evidence.files, original_files);
        assert_eq!(pages.reused, 1);
        assert!(!SupportPages::default().apply("900001", &mut product));
        let observation = &pages.observations["900001"];
        assert_eq!(observation.as_object().unwrap().len(), 2);
        let serialized = serde_json::to_string(observation).unwrap();
        for private in [
            "PRIVATE_AUTH_BODY_SENTINEL",
            "700001",
            "authored.zip",
            "<html",
            "entitlementId",
        ] {
            assert!(!serialized.contains(private), "{private}");
        }
    }
    #[test]
    fn free_support_reuse_distinguishes_observed_empty_from_unproven_pages() {
        let empty = support_page_fixture(
            "<section class='shop__text'><p>No supported avatars listed</p></section>",
        );
        let mut pages = SupportPages::default();
        pages
            .remember(&free_item::parse(&empty, "900001").unwrap(), &empty)
            .unwrap();
        let mut product = json!({"id":"900001","publicSupportAvatars":[{"id":"prior"}]});
        assert!(pages.apply("900001", &mut product));
        assert_eq!(product["publicSupportAvatars"], json!([]));
        for body in [
            support_page_fixture(""),
            empty.replace("900001", "900002"),
            empty.replace("<ul id='variations'>", "<ul id='unknown'>"),
        ] {
            let evidence = free_item::parse(&body, "900001").unwrap();
            let mut pages = SupportPages::default();
            pages.remember(&evidence, &body).unwrap();
            let mut product = json!({"id":"900001","publicSupportAvatars":[{"id":"prior"}]});
            assert!(!pages.apply("900001", &mut product));
            assert_eq!(product["publicSupportAvatars"][0]["id"], "prior");
            if !evidence.manifest_complete {
                assert!(evidence.files.is_empty());
                assert_eq!(evidence.page().unwrap().unresolved, 1);
            }
        }
        for marker in [
            "<form action='/sign_in'><input type='password'></form>",
            "<div id='challenge-form'></div>",
        ] {
            assert!(free_item::parse(
                &empty.replace("<body>", &format!("<body>{marker}")),
                "900001"
            )
            .is_err());
        }
    }
    #[tokio::test]
    async fn receipt_page_pipeline_publishes_before_next_listing_and_merges_later_variant() {
        let path =
            std::env::temp_dir().join(format!("booth-page-pipeline-{}", uuid::Uuid::new_v4()));
        let catalogue = crate::catalogue::Catalogue::open(&path).unwrap();
        let service = BoothService::new(catalogue.clone()).unwrap();
        catalogue.put("account", "authored-account", json!({"id":"authored-account","store":"BOOTH","state":"connected","identityVerified":true})).unwrap();
        let job = json!({"id":"authored-page-job","type":"boothSync","state":"running"});
        catalogue
            .put("job", "authored-page-job", job.clone())
            .unwrap();
        let mut checkpoint = Checkpoint::fresh("authored-account", "purchase", "full").unwrap();
        let (requests, _receiver) = mpsc::unbounded_channel();
        let mut early = EarlyPublications {
            remaining_sources: BTreeSet::from(["library".into(), "purchase".into()]),
            ..Default::default()
        };
        let mut public = metadata::public_metadata(&json!({
            "id":900001,"name":"Authored product","description":"Authored clothing description",
            "shop":{"name":"Authored shop","url":"https://invented.booth.pm"},
            "category":{"id":1,"name":"3D衣装"},
            "tags":[{"name":"VRChat"},{"name":"Authored source tag"},{"name":"Authored rejected tag"}],
            "images":[],"price":"¥ 50~","variations":[
                {"id":800001,"name":null,"type":"digital","price":700},
                {"id":800002,"name":"Second authored variant","type":"digital","price":900},
                {"id":800099,"name":"Unowned listing minimum","type":"digital","price":50}
            ]}), "900001").unwrap();
        public["publicMetadataState"] = json!("complete");
        public["publicMetadataObservedAt"] = json!("2026-10-12T00:00:00Z");
        early.observations.insert("900001".into(), Ok(public));
        let prefetch = PublicPrefetch {
            requests,
            enqueued: Default::default(),
            support_pages: Default::default(),
            early: std::sync::Arc::new(Mutex::new(early)),
        };
        let receipt = |order: &str, file: &str, variant: &str, amount: u64| {
            let dropdown = serde_json::to_string(&json!([
                {"deeplinkDownloadableUrl":format!("https://booth.pm/downloadables/{file}/deeplink?variation_id={variant}&client=booth-library-manager")},
                {"deeplinkDownloadableUrl":format!("https://booth.pm/downloadables/{file}/deeplink?variation_id={variant}&client=vroid&type=zip")}
            ])).unwrap();
            parser::parse(&format!("<main><div class='sheet' data-product-id='900001'><section><div><div><b><a href='https://invented.booth.pm/items/900001'>Authored product</a></b></div><div>¥ {amount}</div><div><i class='icon-boost'></i><span>0</span></div></div></section><div class='legacy-list-item'><div class='min-w-0'><b>authored-{file}.zip</b></div><a href='https://booth.pm/downloadables/{file}'>Download</a><button data-test='other-downloads-button' data-dropdown-items='{dropdown}'></button></div></div><div class='sheet'><div><span>注文番号</span><span>{order}</span></div><div><span>注文日時</span><span>2026/10/10 09:30</span></div></div></main>"),"purchase",&format!("/orders/{order}")).unwrap()
        };
        let first_listing = parser::parse("<main><h1>Orders</h1><a href='/orders/910001'><div class='sheet'>Authored order</div></a><nav class='pagination'><a aria-current='page' href='/orders?page=1'>1</a><a href='/orders?page=2'>2</a></nav></main>","purchase","/orders?page=1").unwrap();
        let consume = |page: &parser::Page, checkpoint: &mut Checkpoint| {
            record_source_page(checkpoint, "purchase", &page.route, page, None).unwrap();
            for row in &page.rows {
                if let Some(id) = &row.product_id {
                    checkpoint.product_ids.insert(id.clone());
                    checkpoint.enrichment_pending.insert(id.clone());
                }
            }
            let id = format!("authored-journal:{}", safety::hash(&page.route));
            checkpoint.journal_ids.push(id.clone());
            let journal = json!({"id":id,"accountId":"authored-account","source":"purchase","runId":checkpoint.run_id,"page":page});
            service
                .commit_source_page(&id, &journal, "authored-checkpoint", checkpoint)
                .unwrap();
            service.stage_discovered_page(
                &job,
                "authored-account",
                "purchase",
                "full",
                "authored-checkpoint",
                checkpoint,
                page,
                &prefetch,
            )
        };
        assert!(consume(&first_listing, &mut checkpoint).unwrap().is_empty());
        let routes = available_routes(&checkpoint.pending, &BTreeSet::new()).unwrap();
        assert_eq!(routes, vec!["/orders?page=2", "/orders/910001"]);
        let (release_next, wait_next) = tokio::sync::oneshot::channel::<()>();
        let mut wait_next = Some(wait_next);
        let mut reads = FuturesUnordered::new();
        for route in routes {
            let blocked = route.contains('?').then(|| wait_next.take().unwrap());
            let page = receipt("910001", "700001", "800001", 100);
            reads.push(async move {
                if let Some(blocked) = blocked {
                    blocked.await.unwrap();
                    None
                } else {
                    Some(page)
                }
            });
        }
        let page = reads.next().await.unwrap().unwrap();
        assert_eq!(consume(&page, &mut checkpoint).unwrap(), vec!["900001"]);
        assert_eq!((checkpoint.pages, checkpoint.details_completed), (1, 1));
        assert!(checkpoint.pending.contains(&"/orders?page=2".into()));
        assert!(!checkpoint.complete());
        {
            let early = prefetch.early.lock().unwrap();
            let aggregate = publication_progress(&early.publications, "finalizing", 1, 1);
            assert_eq!(aggregate["pages"], 1);
            assert_eq!(aggregate["detailsCompleted"], 1);
            assert_eq!(aggregate["requestsCompleted"], 2);
            assert_eq!(aggregate["products"], 1);
            assert_eq!(aggregate["metadataCompleted"], 1);
            assert_eq!(aggregate["published"], 1);
        }
        assert!(!prefetch.early.lock().unwrap().remaining_sources.is_empty());
        assert_eq!(catalogue.related("remote_file", "900001").unwrap().len(), 1);
        let publication_draft = format!("booth-product:{}:900001", checkpoint.run_id);
        assert!(catalogue
            .get("checkpoint", &publication_draft)
            .unwrap()
            .is_none_or(|draft| draft["published"] != true));
        assert_eq!(
            catalogue.related("acquisition", "900001").unwrap()[0]["paidPrice"],
            100
        );
        let source_details = catalogue
            .command("product_details", json!({"id":"900001"}))
            .unwrap();
        assert!(source_details["tags"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tag| tag == "Authored source tag"));
        catalogue
            .command(
                "overlay",
                json!({"ids":["900001"],"patch":{
                    "title":"Human product name","note":"Human note","category":"Hair",
                    "tags":["Authored manual tag"],"rejectedTags":["Authored rejected tag"]
                }}),
            )
            .unwrap();
        let first_details = catalogue
            .command("product_details", json!({"id":"900001"}))
            .unwrap();
        assert_eq!(first_details["price"], 700.0);
        assert_eq!(
            first_details["ownedCurrentPrices"][0]["variantId"],
            "800001"
        );
        assert_eq!(first_details["ownedPaidPrices"][0]["price"], 100.0);
        let mut first_file = catalogue
            .related("remote_file", "900001")
            .unwrap()
            .remove(0);
        first_file["revision"] = json!("verified-A");
        first_file["revisionBasis"] = json!("contentHash");
        first_file["requiresContentVerification"] = json!(false);
        catalogue
            .put(
                "remote_file",
                first_file["id"].as_str().unwrap(),
                first_file.clone(),
            )
            .unwrap();
        catalogue.put("artifact", "authored-A", json!({"id":"authored-A","productId":"900001","fileId":first_file["id"],"revision":first_file["revision"],"status":"ready","hash":"authored-hash-A","relativePath":"authored-A.zip","originalAvailable":true,"extractedPath":"authored-A","extractedInventory":[{"path":"Authored-A.unitypackage","size":10,"hash":"authored-member-A"}]})).unwrap();
        let details = || {
            catalogue
                .command("product_details", json!({"id":"900001"}))
                .unwrap()
        };
        assert_eq!(details()["files"][0]["status"], "ready");
        assert_eq!(details()["manifestComplete"], false);
        assert_eq!(details()["status"], "ready");
        assert_ne!(details()["status"], "attention");
        release_next.send(()).unwrap();
        assert!(reads.next().await.unwrap().is_none());
        let second = receipt("910002", "700002", "800002", 200);
        assert_eq!(consume(&second, &mut checkpoint).unwrap(), vec!["900001"]);
        let replayed_first = catalogue
            .get("remote_file", first_file["id"].as_str().unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(replayed_first["revision"], "verified-A");
        assert_eq!(replayed_first["requiresContentVerification"], false);
        assert_eq!(
            replayed_first["sourceObservationToken"],
            first_file["sourceObservationToken"]
        );
        let entitlement = catalogue
            .related("entitlement", "900001")
            .unwrap()
            .remove(0);
        assert_eq!(entitlement["fileIds"].as_array().unwrap().len(), 2);
        assert_eq!(entitlement["variantIds"].as_array().unwrap().len(), 2);
        assert_eq!(details()["status"], "partial");
        let paid: u64 = catalogue
            .related("acquisition", "900001")
            .unwrap()
            .iter()
            .filter_map(|row| row["paidPrice"].as_u64())
            .sum();
        assert_eq!(paid, 300);
        let assert_facts = |catalogue: &crate::catalogue::Catalogue| {
            let value = catalogue
                .command("product_details", json!({"id":"900001"}))
                .unwrap();
            assert!(value["price"].is_null()); // No listing minimum or multi-variant allocation.
            let current = value["ownedCurrentPrices"].as_array().unwrap();
            assert_eq!(current.len(), 2);
            for (variant, amount, paid) in [("800001", 700.0, 100.0), ("800002", 900.0, 200.0)] {
                let quote = current.iter().find(|v| v["variantId"] == variant).unwrap();
                assert_eq!(quote["price"], amount);
                assert_eq!(quote["currency"], "JPY");
                assert_eq!(quote["accountIds"], json!(["authored-account"]));
                let payment = value["ownedPaidPrices"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|v| v["variantId"] == variant)
                    .unwrap();
                assert_eq!(payment["price"], paid);
                assert_eq!(payment["accountId"], "authored-account");
            }
            assert!(value["paidPrice"].is_null()); // Distinct actual charges remain separate.
            assert_eq!(value["accountIds"], json!(["authored-account"]));
            assert_eq!(
                value["sourceTags"],
                json!(["VRChat", "Authored source tag", "Authored rejected tag"])
            );
            assert!(value["tags"]
                .as_array()
                .unwrap()
                .iter()
                .any(|tag| tag == "Authored manual tag"));
            assert!(!value["tags"]
                .as_array()
                .unwrap()
                .iter()
                .any(|tag| tag == "Authored rejected tag"));
            let rejected: BTreeSet<_> = value["rejectedTags"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(Value::as_str)
                .collect();
            assert_eq!(
                rejected,
                BTreeSet::from(["Authored rejected tag", "Authored source tag", "VRChat"])
            );
            assert_eq!(value["title"], "Human product name");
            assert_eq!(value["sourceTitle"], "Authored product");
            assert_eq!(value["note"], "Human note");
            assert_eq!(value["category"], "Hair");
            let acquisitions = catalogue.related("acquisition", "900001").unwrap();
            assert_eq!(acquisitions.len(), 2);
            assert!(acquisitions
                .iter()
                .all(|row| row["accountId"] == "authored-account"
                    && row["paidPriceBasis"] == "productLineTotal"
                    && row["acquiredDateText"] == "2026/10/10 09:30"));
        };
        assert_facts(&catalogue);
        assert_eq!(details()["manifestComplete"], false);
        assert_ne!(details()["status"], "ready");
        // Verified closure certifies the same cumulative facts atomically.
        let entries = prefetch.early.lock().unwrap().publications.products["900001"].clone();
        let mut closed_checkpoints = prefetch
            .early
            .lock()
            .unwrap()
            .publications
            .checkpoints
            .clone();
        service
            .commit_product_group(&job, "900001", &entries, &mut closed_checkpoints)
            .unwrap();
        assert_eq!(details()["manifestComplete"], true);
        assert_ne!(details()["status"], "ready");
        assert_facts(&catalogue);
        let second_file = catalogue
            .related("remote_file", "900001")
            .unwrap()
            .into_iter()
            .find(|row| row["id"] != first_file["id"])
            .unwrap();
        assert_eq!(second_file["requiresContentVerification"], true);
        let mut second_file = second_file;
        second_file["revision"] = json!("verified-B");
        second_file["revisionBasis"] = json!("etag");
        second_file["requiresContentVerification"] = json!(false);
        catalogue
            .put(
                "remote_file",
                second_file["id"].as_str().unwrap(),
                second_file.clone(),
            )
            .unwrap();
        catalogue.put("artifact", "authored-B", json!({"id":"authored-B","productId":"900001","fileId":second_file["id"],"revision":second_file["revision"],"status":"ready","hash":"authored-hash-B","relativePath":"authored-B.zip"})).unwrap();
        service
            .commit_product_group(&job, "900001", &entries, &mut closed_checkpoints)
            .unwrap();
        assert_eq!(details()["manifestComplete"], true);
        assert_eq!(details()["status"], "ready");
        assert_facts(&catalogue);
        // A different full refresh must verify even these unchanged descriptors.
        let next_job = json!({"id":"authored-next-job","type":"boothSync","state":"running"});
        catalogue
            .put("job", "authored-next-job", next_job.clone())
            .unwrap();
        let mut next_checkpoints = BTreeMap::from([(
            entries[0].checkpoint_key.clone(),
            Checkpoint::fresh("authored-account", "purchase", "full").unwrap(),
        )]);
        service
            .commit_product_group_mode(&next_job, "900001", &entries, &mut next_checkpoints, false)
            .unwrap();
        let next_first = catalogue
            .get("remote_file", first_file["id"].as_str().unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(next_first["requiresContentVerification"], true);
        assert_ne!(
            next_first["sourceObservationToken"],
            first_file["sourceObservationToken"]
        );
        assert_ne!(details()["status"], "ready");
        service
            .commit_product_group_mode(&next_job, "900001", &entries, &mut next_checkpoints, false)
            .unwrap();
        assert_eq!(
            catalogue
                .get("remote_file", first_file["id"].as_str().unwrap())
                .unwrap()
                .unwrap()["requiresContentVerification"],
            true
        );
        // Positive verification during that job cannot mask changed source facts.
        let mut verified = next_first;
        verified["requiresContentVerification"] = json!(false);
        catalogue
            .put("remote_file", first_file["id"].as_str().unwrap(), verified)
            .unwrap();
        let mut changed = entries.clone();
        changed[0].rows[0].files[0].size_bytes = Some(99);
        service
            .commit_product_group_mode(&next_job, "900001", &changed, &mut next_checkpoints, false)
            .unwrap();
        let changed_first = catalogue
            .get("remote_file", first_file["id"].as_str().unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(changed_first["requiresContentVerification"], true);
        assert_eq!(changed_first["sizeBytes"], 99);
        assert_facts(&catalogue);
        // A genuinely changed descriptor requires a new positive verification.
        // These authored artifact records stand for storage's verified extraction;
        // this source test does not perform archive/filesystem verification.
        for (label, original) in [("A2", &first_file), ("B2", &second_file)] {
            let mut remote = catalogue
                .get("remote_file", original["id"].as_str().unwrap())
                .unwrap()
                .unwrap();
            remote["revision"] = json!(format!("verified-{label}"));
            remote["revisionBasis"] = json!("contentHash");
            remote["requiresContentVerification"] = json!(false);
            let artifact_id = format!("authored-{label}");
            catalogue
                .put(
                    "remote_file",
                    remote["id"].as_str().unwrap(),
                    remote.clone(),
                )
                .unwrap();
            catalogue.put("artifact", &artifact_id, json!({
                "id":artifact_id,"productId":"900001","fileId":remote["id"],
                "revision":remote["revision"],"status":"ready","hash":format!("authored-hash-{label}"),
                "relativePath":format!("authored-{label}.zip"),"originalAvailable":true,
                "extractedPath":format!("authored-{label}"),
                "extractedInventory":[{"path":format!("Authored-{label}.unitypackage"),"size":10,"hash":format!("authored-member-{label}")}]
            })).unwrap();
        }
        service
            .commit_product_group(&next_job, "900001", &changed, &mut next_checkpoints)
            .unwrap();
        assert_eq!(details()["status"], "ready");
        assert_facts(&catalogue);
        // A sparse incremental observation keeps previously proven financial
        // facts, public quotes/tags and unchanged verified local revisions.
        let incremental_job =
            json!({"id":"authored-incremental-job","type":"boothSync","state":"running"});
        catalogue
            .put("job", "authored-incremental-job", incremental_job.clone())
            .unwrap();
        let mut incremental_entries = changed.clone();
        for entry in &mut incremental_entries {
            entry.force_verify = false;
            entry.draft = json!({"id":"900001","publicMetadataState":"complete"});
            for row in &mut entry.rows {
                row.product_line_total_jpy = None;
                row.paid_amount_text = None;
                row.acquired_date_text = None;
            }
        }
        let mut incremental_checkpoints = BTreeMap::from([(
            entries[0].checkpoint_key.clone(),
            Checkpoint::fresh("authored-account", "purchase", "incremental").unwrap(),
        )]);
        service
            .commit_product_group_mode(
                &incremental_job,
                "900001",
                &incremental_entries,
                &mut incremental_checkpoints,
                false,
            )
            .unwrap();
        assert_eq!(details()["status"], "ready");
        service
            .commit_product_group(
                &incremental_job,
                "900001",
                &incremental_entries,
                &mut incremental_checkpoints,
            )
            .unwrap();
        assert_eq!(details()["manifestComplete"], true);
        assert_eq!(details()["status"], "ready");
        assert_facts(&catalogue);
        let retained_artifact = catalogue.get("artifact", "authored-A2").unwrap().unwrap();
        catalogue
            .put(
                "job",
                "authored-page-job",
                json!({"id":"authored-page-job","state":"cancelled"}),
            )
            .unwrap();
        let third = receipt("910003", "700003", "800003", 300);
        assert_eq!(
            consume(&third, &mut checkpoint).unwrap_err().to_string(),
            "CANCELLED"
        );
        assert_eq!(catalogue.related("remote_file", "900001").unwrap().len(), 2);
        drop(service);
        drop(catalogue);
        let reopened = crate::catalogue::Catalogue::open(&path).unwrap();
        assert_facts(&reopened);
        let reopened_details = reopened
            .command("product_details", json!({"id":"900001"}))
            .unwrap();
        assert_eq!(reopened_details["manifestComplete"], true);
        assert_eq!(reopened_details["status"], "ready");
        assert_eq!(
            reopened.get("artifact", "authored-A2").unwrap().unwrap(),
            retained_artifact
        );
        drop(reopened);
        std::fs::remove_dir_all(path).unwrap();
    }
    #[test]
    fn authored_complete_product_publishes_before_unrelated_drafts_and_public_details() {
        let root = std::env::var("NNA_STORE_EVIDENCE_DIR")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|_| std::env::temp_dir());
        let path = root.join(format!("booth-early-{}", uuid::Uuid::new_v4()));
        let catalogue = crate::catalogue::Catalogue::open(&path).unwrap();
        let service = BoothService::new(catalogue.clone()).unwrap();
        let job =
            json!({"id":"authored-early-job","type":"boothSync","state":"running","progress":{}});
        catalogue
            .put("job", "authored-early-job", job.clone())
            .unwrap();
        let entry = |product: &str, source: &str| {
            let row = parser::Row {
                source_record_id: format!("{source}:{product}"),
                product_id: Some(product.into()),
                order_id: (source == "purchase").then(|| "910001".into()),
                name: Some(format!("Authored {product}")),
                manifest_complete: true,
                product_line_total_jpy: (source == "purchase").then_some(100),
                files: vec![parser::File {
                    id: format!("7{product}"),
                    variant_id: Some("800001".into()),
                    name: Some("authored.zip".into()),
                    size_bytes: Some(10),
                    source_url: format!(
                        "https://booth.pm/downloadables/7{product}?variation_id=800001"
                    ),
                    revision: "authored".into(),
                }],
                ..Default::default()
            };
            ProductPublication {
                account: "authored-account".into(),
                source: source.into(),
                product: product.into(),
                rows: vec![row.clone()],
                routes: BTreeMap::from([(
                    row.source_record_id,
                    if source == "purchase" {
                        "/orders/910001".into()
                    } else {
                        "/library?page=1".into()
                    },
                )]),
                draft: json!({"id":product,"store":"BOOTH","title":format!("Authored {product}")}),
                files: BTreeSet::from([format!("authored-account:7{product}:800001")]),
                checkpoint_key: source.into(),
                force_verify: true,
                needs_enrichment: true,
            }
        };
        let mut early = EarlyPublications {
            remaining_sources: BTreeSet::from(["purchase".into()]),
            ..Default::default()
        };
        for source in ["library", "purchase"] {
            early.publications.checkpoints.insert(
                source.into(),
                Checkpoint::fresh("authored-account", source, "full").unwrap(),
            );
        }
        early
            .publications
            .products
            .insert("900001".into(), vec![entry("900001", "library")]);
        early.observations.insert(
            "900001".into(),
            Ok(json!({"publicMetadataState":"complete","sourceTitle":"Authored complete"})),
        );
        assert!(service
            .publish_early_records(&job, &mut early, None)
            .unwrap()
            .is_empty());
        assert!(service
            .publish_early_records(&job, &mut early, None)
            .unwrap()
            .is_empty()); // receipt grant still absent, discovery remains open
        let began = std::time::Instant::now();
        early
            .publications
            .products
            .get_mut("900001")
            .unwrap()
            .push(entry("900001", "purchase"));
        let first = service
            .publish_early_records(&job, &mut early, None)
            .unwrap();
        assert_eq!(first, vec!["900001"]);
        let first_ms = began.elapsed().as_millis();
        assert!(catalogue.get("product", "900001").unwrap().is_some());
        assert_eq!(catalogue.related("entitlement", "900001").unwrap().len(), 2);
        assert_eq!(catalogue.related("remote_file", "900001").unwrap().len(), 1);
        assert!(catalogue
            .related("acquisition", "900001")
            .unwrap()
            .iter()
            .any(|row| row["paidPrice"] == 100));
        assert!(catalogue.get("product", "900002").unwrap().is_none());
        // Unrelated product source preparation continues after the first atomic commit.
        early.publications.products.insert(
            "900002".into(),
            vec![entry("900002", "library"), entry("900002", "purchase")],
        );
        assert!(service
            .publish_early_records(&job, &mut early, None)
            .unwrap()
            .is_empty()); // public details still pending
        early.observations.insert(
            "900002".into(),
            Ok(json!({"publicMetadataState":"complete"})),
        );
        assert_eq!(
            service
                .publish_early_records(&job, &mut early, None)
                .unwrap(),
            vec!["900002"]
        );
        assert!(service
            .publish_early_records(&job, &mut early, None)
            .unwrap()
            .is_empty()); // no duplicate commit
        if let Ok(root) = std::env::var("NNA_STORE_EVIDENCE_DIR") {
            std::fs::write(std::path::Path::new(&root).join("booth-publication-timing.json"),serde_json::to_vec_pretty(&json!({"firstPublishMs":first_ms,"secondPublishMs":began.elapsed().as_millis(),"firstPublicationProducts":1,"firstPublicationEntitlements":2,"unrelatedSourceDraftsPendingAtFirstPublish":true,"unrelatedPublicDetailsPendingAtFirstPublish":true,"identityEnumerationClosedBeforeFirstPublish":false,"realStoreTiming":false})).unwrap()).unwrap();
        }
        drop(service);
        drop(catalogue);
    }
    #[test]
    fn single_candidate_publication_does_not_revisit_unrelated_observations() {
        let path = std::env::temp_dir().join(format!("nna-booth-scoped-{}", uuid::Uuid::new_v4()));
        let catalogue = crate::catalogue::Catalogue::open(&path).unwrap();
        let service = BoothService::new(catalogue.clone()).unwrap();
        let job =
            json!({"id":"authored-scoped-job","type":"boothSync","state":"running","progress":{}});
        catalogue
            .put("job", "authored-scoped-job", job.clone())
            .unwrap();
        let mut early = EarlyPublications::default();
        early.publications.checkpoints.insert(
            "purchase".into(),
            Checkpoint::fresh("authored-account", "purchase", "full").unwrap(),
        );
        for product in ["900001", "900002"] {
            let row = parser::Row {
                source_record_id: format!("purchase:{product}"),
                product_id: Some(product.into()),
                order_id: Some("910001".into()),
                name: Some("Authored product".into()),
                manifest_complete: true,
                product_line_total_jpy: Some(100),
                ..Default::default()
            };
            early.publications.products.insert(
                product.into(),
                vec![ProductPublication {
                    account: "authored-account".into(),
                    source: "purchase".into(),
                    product: product.into(),
                    routes: BTreeMap::from([(
                        row.source_record_id.clone(),
                        "/orders/910001".into(),
                    )]),
                    rows: vec![row],
                    draft: json!({"id":product,"store":"BOOTH","title":"Authored product"}),
                    files: BTreeSet::new(),
                    checkpoint_key: "purchase".into(),
                    force_verify: true,
                    needs_enrichment: true,
                }],
            );
            early.observations.insert(
                product.into(),
                Ok(json!({"publicMetadataState":"complete"})),
            );
        }
        let selected = BTreeSet::from(["900001".into()]);
        assert_eq!(
            service
                .publish_early_records(&job, &mut early, Some(&selected))
                .unwrap(),
            vec!["900001"]
        );
        assert!(early.publications.products["900002"][0].needs_enrichment);
        assert!(early.publications.additive_versions.get("900002").is_none());
        assert!(catalogue.get("product", "900002").unwrap().is_none());
        // An unrelated completed observation is not reconsidered on this callback.
        early
            .observations
            .insert("900002".into(), Err("PUBLIC_METADATA_FAILED".into()));
        assert!(service
            .publish_early_records(&job, &mut early, Some(&selected))
            .unwrap()
            .is_empty());
        assert!(early.publications.incomplete_scopes.is_empty());
        drop(service);
        drop(catalogue);
        std::fs::remove_dir_all(path).unwrap();
    }
    #[test]
    fn product_draft_restart_and_cancel_preserve_published_snapshot_until_atomic_publish() {
        let path = std::env::temp_dir().join(format!("nna-booth-draft-{}", uuid::Uuid::new_v4()));
        let catalogue = crate::catalogue::Catalogue::open(&path).unwrap();
        let service = BoothService::new(catalogue.clone()).unwrap();
        let account = "synthetic-account";
        let job = json!({"id":"synthetic-job","type":"boothSync","accountId":account,"state":"running","progress":{}});
        catalogue.put("job", "synthetic-job", job.clone()).unwrap();
        let prior = json!({"id":"900001","store":"BOOTH","title":"Previous published","price":50,"sourceTags":["previous"]});
        let old_entitlement = json!({"id":"synthetic-account:purchase:900001","productId":"900001","accountId":account,"source":"purchase","fileIds":["old-file"],"manifestComplete":true});
        catalogue.put("product", "900001", prior.clone()).unwrap();
        catalogue
            .put(
                "entitlement",
                old_entitlement["id"].as_str().unwrap(),
                old_entitlement.clone(),
            )
            .unwrap();
        catalogue
            .put(
                "overlay",
                "900001",
                json!({"id":"900001","title":"Manual title","note":"Manual note"}),
            )
            .unwrap();
        let row = parser::Row {
            source_record_id: "purchase:synthetic-variant".into(),
            product_id: Some("900001".into()),
            order_id: Some("910001".into()),
            name: Some("New source title".into()),
            manifest_complete: true,
            product_line_total_jpy: Some(0),
            acquired_date_text: Some("2026/10/11 09:30".into()),
            files: vec![parser::File {
                id: "700001".into(),
                variant_id: Some("800001".into()),
                name: Some("authored.zip".into()),
                size_bytes: Some(10),
                source_url: "https://booth.pm/downloadables/700001?variation_id=800001".into(),
                revision: "authored".into(),
            }],
            ..Default::default()
        };
        let mut checkpoint = Checkpoint::fresh(account, "purchase", "full").unwrap();
        checkpoint.pending.clear();
        checkpoint.terminal = true;
        checkpoint.head = "authored-head".into();
        checkpoint.product_ids.insert("900001".into());
        checkpoint.enrichment_pending.insert("900001".into());
        let key = "booth:synthetic-account:purchase";
        service
            .commit_source_page(
                "synthetic-page",
                &json!({"id":"synthetic-page","rows":[row]}),
                key,
                &checkpoint,
            )
            .unwrap();
        let collected_checkpoint = catalogue.get("checkpoint", key).unwrap().unwrap();
        let publication_draft = format!("booth-product:{}:900001", checkpoint.run_id);
        let draft = json!({"id":"900001","store":"BOOTH","title":"Enriched source","price":100,"sourceTags":["complete"],"publicMetadataState":"complete"});
        catalogue
            .put(
                "checkpoint",
                "synthetic-draft",
                json!({"id":"synthetic-draft","product":draft,"enriched":true}),
            )
            .unwrap();
        assert_eq!(
            catalogue.get("product", "900001").unwrap(),
            Some(prior.clone())
        );
        assert_eq!(
            catalogue
                .get("entitlement", old_entitlement["id"].as_str().unwrap())
                .unwrap(),
            Some(old_entitlement.clone())
        );
        assert!(catalogue.list("acquisition").unwrap().is_empty());
        assert!(catalogue.list("remote_file").unwrap().is_empty());
        drop(service);
        drop(catalogue);
        let catalogue = crate::catalogue::Catalogue::open(&path).unwrap();
        let service = BoothService::new(catalogue.clone()).unwrap();
        checkpoint =
            serde_json::from_value(catalogue.get("checkpoint", key).unwrap().unwrap()).unwrap();
        assert!(checkpoint.published_products.is_empty());
        let routes = BTreeMap::from([(row.source_record_id.clone(), "/orders/910001".into())]);
        let files = BTreeSet::from(["synthetic-account:700001:800001".into()]);
        let mut cancelled = job.clone();
        cancelled["state"] = json!("cancelled");
        catalogue.put("job", "synthetic-job", cancelled).unwrap();
        assert_eq!(
            service
                .publish_product(
                    &job,
                    account,
                    "purchase",
                    "900001",
                    &[&row],
                    &routes,
                    &draft,
                    &files,
                    key,
                    &mut checkpoint,
                    true
                )
                .unwrap_err()
                .to_string(),
            "CANCELLED"
        );
        assert_eq!(
            catalogue.get("product", "900001").unwrap(),
            Some(prior.clone())
        );
        assert_eq!(
            catalogue
                .get("entitlement", old_entitlement["id"].as_str().unwrap())
                .unwrap(),
            Some(old_entitlement.clone())
        );
        assert!(catalogue
            .get("checkpoint", &publication_draft)
            .unwrap()
            .is_none());
        catalogue.put("job", "synthetic-job", job.clone()).unwrap();
        let mut library_row = row.clone();
        library_row.source_record_id = "library:synthetic-variant".into();
        library_row.order_id = None;
        library_row.product_line_total_jpy = None;
        let library_key = "booth:synthetic-account:library";
        let mut library_checkpoint = Checkpoint::fresh(account, "library", "full").unwrap();
        library_checkpoint.pending.clear();
        library_checkpoint.terminal = true;
        library_checkpoint.head = "authored-library-head".into();
        library_checkpoint.product_ids.insert("900001".into());
        library_checkpoint
            .enrichment_pending
            .insert("900001".into());
        let mut checkpoints = BTreeMap::from([
            (key.to_owned(), checkpoint.clone()),
            (library_key.to_owned(), library_checkpoint),
        ]);
        let entries = vec![
            ProductPublication {
                account: account.into(),
                source: "library".into(),
                product: "900001".into(),
                rows: vec![library_row.clone()],
                routes: BTreeMap::from([(
                    library_row.source_record_id.clone(),
                    "/library?page=1".into(),
                )]),
                draft: draft.clone(),
                files: files.clone(),
                checkpoint_key: library_key.into(),
                force_verify: true,
                needs_enrichment: false,
            },
            ProductPublication {
                account: account.into(),
                source: "purchase".into(),
                product: "900001".into(),
                rows: vec![row.clone()],
                routes: routes.clone(),
                draft: draft.clone(),
                files: files.clone(),
                checkpoint_key: key.into(),
                force_verify: true,
                needs_enrichment: false,
            },
        ];
        let free_gap = BTreeSet::from([(account.to_owned(), "free".to_owned())]);
        assert!(!publication_membership_proven(&entries, &free_gap));
        let mut endpoint_entries = entries;
        endpoint_entries.push(ProductPublication {
            account: account.into(),
            source: "free".into(),
            product: "900001".into(),
            rows: vec![],
            routes: BTreeMap::new(),
            draft: draft.clone(),
            files: files.clone(),
            checkpoint_key: "authored-endpoint".into(),
            force_verify: false,
            needs_enrichment: false,
        });
        assert!(publication_membership_proven(&endpoint_entries, &free_gap));
        assert!(!publication_membership_proven(
            &endpoint_entries,
            &BTreeSet::from([(account.to_owned(), "purchase".to_owned())])
        ));
        assert!(publication_membership_proven(
            &endpoint_entries,
            &BTreeSet::from([("different-account".into(), "purchase".into())])
        ));
        endpoint_entries.pop();
        let mut entries = endpoint_entries;
        entries[0].needs_enrichment = true;
        assert_eq!(
            service
                .commit_product_group(&job, "900001", &entries, &mut checkpoints)
                .unwrap_err()
                .to_string(),
            "PUBLIC_METADATA_FAILED"
        );
        assert_eq!(catalogue.get("product", "900001").unwrap().unwrap(), prior);
        assert_eq!(
            catalogue
                .get("entitlement", old_entitlement["id"].as_str().unwrap())
                .unwrap()
                .unwrap(),
            old_entitlement
        );
        entries[0].needs_enrichment = false;
        service
            .commit_product_group(&job, "900001", &entries, &mut checkpoints)
            .unwrap();
        checkpoint = checkpoints.remove(key).unwrap();
        assert_eq!(catalogue.list("acquisition").unwrap().len(), 2);
        assert!(catalogue
            .list("entitlement")
            .unwrap()
            .iter()
            .all(|value| value["manifestComplete"] == true));
        assert_eq!(
            catalogue.get("product", "900001").unwrap().unwrap()["sourceTags"],
            json!(["complete"])
        );
        let entitlement = catalogue
            .get("entitlement", "synthetic-account:purchase:900001")
            .unwrap()
            .unwrap();
        assert_eq!(entitlement["manifestComplete"], true);
        assert_eq!(
            entitlement["fileIds"],
            json!(["synthetic-account:700001:800001"])
        );
        let acquired = catalogue
            .list("acquisition")
            .unwrap()
            .into_iter()
            .find(|value| value["source"] == "purchase")
            .unwrap();
        assert_eq!(acquired["variantIds"], json!(["800001"]));
        assert_eq!(acquired["variantPayments"][0]["price"], 0);
        assert_eq!(catalogue.list("remote_file").unwrap().len(), 1);
        assert_eq!(
            catalogue.get("overlay", "900001").unwrap().unwrap()["note"],
            "Manual note"
        );
        assert!(checkpoint.published_products.contains("900001"));
        assert!(checkpoint.enrichment_pending.is_empty());
        // Publication no longer rewrites the large source checkpoint per item.
        assert_eq!(
            catalogue.get("checkpoint", key).unwrap().unwrap(),
            collected_checkpoint
        );
        let marker = catalogue
            .get("checkpoint", &publication_draft)
            .unwrap()
            .unwrap();
        assert_eq!(marker["published"], true);
        assert_eq!(
            marker["publishedManifest"],
            publication_manifest(account, "purchase", "900001", [&row], &routes, &files).unwrap()
        );
        // A later metadata callback keeps the certified marker, with its read
        // participating in the same expected-value transaction.
        let mut later_draft = marker.clone();
        later_draft.as_object_mut().unwrap().remove("published");
        later_draft
            .as_object_mut()
            .unwrap()
            .remove("publishedManifest");
        later_draft["product"]["title"] = json!("Later metadata draft");
        service
            .save_product_draft(&publication_draft, later_draft)
            .unwrap();
        let marker = catalogue
            .get("checkpoint", &publication_draft)
            .unwrap()
            .unwrap();
        assert_eq!(marker["published"], true);
        assert_eq!(marker["product"]["title"], "Later metadata draft");
        drop(service);
        drop(catalogue);
        let catalogue = crate::catalogue::Catalogue::open(&path).unwrap();
        let service = BoothService::new(catalogue.clone()).unwrap();
        let mut resumed: Checkpoint =
            serde_json::from_value(catalogue.get("checkpoint", key).unwrap().unwrap()).unwrap();
        assert!(resumed.published_products.is_empty());
        assert!(resumed.enrichment_pending.contains("900001"));
        let restore = |checkpoint: &mut Checkpoint, saved: &Value, complete| {
            service
                .restore_product_publication(
                    "900001",
                    &[&row],
                    &routes,
                    &files,
                    complete,
                    Some(saved),
                    checkpoint,
                )
                .unwrap()
        };
        assert!(!restore(&mut resumed, &marker, false));
        for key in [
            "accountId",
            "source",
            "runId",
            "productId",
            "publishedManifest",
        ] {
            let mut other = marker.clone();
            other[key] = json!("other");
            assert!(!restore(&mut resumed, &other, true), "{key}");
        }
        let mut prepared_only = marker.clone();
        prepared_only["published"] = json!(false);
        assert!(!restore(&mut resumed, &prepared_only, true));
        let mut mismatched_product = marker.clone();
        mismatched_product["product"]["id"] = json!("900002");
        assert!(!restore(&mut resumed, &mismatched_product, true));
        let mut changed_row = row.clone();
        changed_row.product_line_total_jpy = Some(100);
        assert!(!service
            .restore_product_publication(
                "900001",
                &[&changed_row],
                &routes,
                &files,
                true,
                Some(&marker),
                &mut resumed
            )
            .unwrap());
        let mut fresh_forced = Checkpoint::fresh(account, "purchase", "forced").unwrap();
        assert!(!restore(&mut fresh_forced, &marker, true));
        let mut changed_entitlement = entitlement.clone();
        changed_entitlement["manifestComplete"] = json!(false);
        catalogue
            .put(
                "entitlement",
                &format!("{account}:purchase:900001"),
                changed_entitlement,
            )
            .unwrap();
        assert!(!restore(&mut resumed, &marker, true));
        catalogue
            .put(
                "entitlement",
                &format!("{account}:purchase:900001"),
                entitlement,
            )
            .unwrap();
        assert!(restore(&mut resumed, &marker, true));
        assert!(resumed.published_products.contains("900001"));
        assert!(resumed.enrichment_pending.is_empty());
        assert_eq!(catalogue.list("acquisition").unwrap().len(), 2);
        assert_eq!(catalogue.list("remote_file").unwrap().len(), 1);
        assert_eq!(
            catalogue.get("checkpoint", key).unwrap().unwrap(),
            collected_checkpoint
        );
        // Restoring one source omits only its already certified entry. A later
        // publication for the other source must retain those committed facts.
        let recovered_entitlement = catalogue
            .get("entitlement", &format!("{account}:purchase:900001"))
            .unwrap()
            .unwrap();
        let mut next_library_row = library_row.clone();
        next_library_row.files[0].id = "700002".into();
        next_library_row.files[0].variant_id = Some("800002".into());
        next_library_row.files[0].source_url =
            "https://booth.pm/downloadables/700002?variation_id=800002".into();
        let mut next_library = Checkpoint::fresh(account, "library", "full").unwrap();
        next_library.pending.clear();
        next_library.terminal = true;
        next_library.product_ids.insert("900001".into());
        next_library.enrichment_pending.insert("900001".into());
        let mut next_checkpoints = BTreeMap::from([(library_key.to_owned(), next_library)]);
        let next_entry = ProductPublication {
            account: account.into(),
            source: "library".into(),
            product: "900001".into(),
            routes: BTreeMap::from([(
                next_library_row.source_record_id.clone(),
                "/library?page=1".into(),
            )]),
            rows: vec![next_library_row],
            draft: draft.clone(),
            files: BTreeSet::from(["synthetic-account:700002:800002".into()]),
            checkpoint_key: library_key.into(),
            force_verify: true,
            needs_enrichment: false,
        };
        service
            .commit_product_group(&job, "900001", &[next_entry], &mut next_checkpoints)
            .unwrap();
        assert_eq!(
            catalogue
                .get("entitlement", &format!("{account}:purchase:900001"))
                .unwrap()
                .unwrap(),
            recovered_entitlement
        );
        assert!(resumed.complete());
        assert!(next_checkpoints[library_key].complete());
        assert!(restore(&mut resumed, &marker, true));
        assert_eq!(catalogue.list("remote_file").unwrap().len(), 2);
        assert_eq!(catalogue.list("acquisition").unwrap().len(), 2);
        drop(service);
        drop(catalogue);
        std::fs::remove_dir_all(path).unwrap();
    }
    #[test]
    fn restored_free_scope_preserves_unbound_exception_and_completed_totals_without_replay() {
        let path =
            std::env::temp_dir().join(format!("booth-restored-free-{}", uuid::Uuid::new_v4()));
        let catalogue = crate::catalogue::Catalogue::open(&path).unwrap();
        let service = BoothService::new(catalogue.clone()).unwrap();
        let account = "authored-account";
        let job = json!({"id":"authored-restored-job","type":"boothSync","state":"running","progress":{}});
        catalogue
            .put("job", "authored-restored-job", job.clone())
            .unwrap();
        let entry = |source: &str| {
            let file = if source == "free" { "700001" } else { "700002" };
            let row = parser::Row {
                source_record_id: format!("{source}:900001"),
                product_id: Some("900001".into()),
                order_id: (source == "purchase").then(|| "910001".into()),
                product_line_total_jpy: (source == "purchase").then_some(100),
                manifest_complete: true,
                files: vec![parser::File {
                    id: file.into(),
                    variant_id: None,
                    name: Some("authored.zip".into()),
                    size_bytes: Some(10),
                    source_url: format!("https://booth.pm/downloadables/{file}"),
                    revision: "authored".into(),
                }],
                ..Default::default()
            };
            ProductPublication {
                account: account.into(),
                source: source.into(),
                product: "900001".into(),
                routes: BTreeMap::from([(
                    row.source_record_id.clone(),
                    if source == "free" {
                        safety::free_item_route("900001").unwrap()
                    } else {
                        "/orders/910001".into()
                    },
                )]),
                rows: vec![row],
                draft: json!({"id":"900001","store":"BOOTH","title":"Authored complete","publicMetadataState":"complete"}),
                files: BTreeSet::from([format!("{account}:{file}:")]),
                checkpoint_key: source.into(),
                force_verify: true,
                needs_enrichment: false,
            }
        };
        let free_entry = entry("free");
        let purchase_entry = entry("purchase");
        let mut checkpoints = BTreeMap::new();
        for source in ["free", "purchase"] {
            let mut checkpoint = Checkpoint::fresh(account, source, "full").unwrap();
            checkpoint.pending.clear();
            checkpoint.terminal = true;
            checkpoint.head = format!("authored-{source}-head");
            checkpoint.product_ids.insert("900001".into());
            checkpoint.enrichment_pending.insert("900001".into());
            // The exact free endpoint proves this product; an unrelated
            // unbound historical row still keeps source-wide coverage incomplete.
            checkpoint.unresolved = if source == "free" { 1 } else { 0 };
            service.save_checkpoint(source, &checkpoint).unwrap();
            checkpoints.insert(source.into(), checkpoint);
        }
        service
            .commit_product_group(
                &job,
                "900001",
                std::slice::from_ref(&free_entry),
                &mut checkpoints,
            )
            .unwrap();
        let free_entitlement = catalogue
            .get("entitlement", &format!("{account}:free:900001"))
            .unwrap()
            .unwrap();
        let draft_id = format!("booth-product:{}:900001", checkpoints["free"].run_id);
        let marker = catalogue.get("checkpoint", &draft_id).unwrap().unwrap();
        let mut resumed: Checkpoint =
            serde_json::from_value(catalogue.get("checkpoint", "free").unwrap().unwrap()).unwrap();
        assert!(resumed.published_products.is_empty());
        let mut publications = Publications {
            checkpoints: BTreeMap::from([
                ("free".into(), resumed.clone()),
                ("purchase".into(), checkpoints.remove("purchase").unwrap()),
            ]),
            unbound_sources: BTreeSet::from([(account.into(), "free".into())]),
            ..Default::default()
        };
        let (requests, _receiver) = mpsc::unbounded_channel();
        let prefetch = PublicPrefetch {
            requests,
            enqueued: Default::default(),
            support_pages: Default::default(),
            early: std::sync::Arc::new(Mutex::new(EarlyPublications {
                publications: Publications {
                    checkpoints: publications.checkpoints.clone(),
                    unbound_sources: publications.unbound_sources.clone(),
                    ..Default::default()
                },
                ..Default::default()
            })),
        };
        assert!(service
            .restore_product_publication(
                "900001",
                &[&free_entry.rows[0]],
                &free_entry.routes,
                &free_entry.files,
                true,
                Some(&marker),
                &mut resumed
            )
            .unwrap());
        publications
            .checkpoints
            .insert("free".into(), resumed.clone());
        service
            .retain_published_context(&mut publications, Some(&prefetch), free_entry.clone())
            .unwrap();
        remember_product_context(&mut publications, purchase_entry.clone());
        remember_product_context(
            &mut prefetch.early.lock().unwrap().publications,
            purchase_entry.clone(),
        );
        assert!(publication_membership_proven(
            &publications.products["900001"],
            &publications.unbound_sources
        ));
        assert!(!publication_membership_proven(
            std::slice::from_ref(&purchase_entry),
            &publications.unbound_sources
        ));
        assert!(!publication_membership_proven(
            &publications.products["900001"],
            &BTreeSet::from([(account.into(), "purchase".into())])
        ));
        assert_eq!(published_product_count(&publications), 0); // Other scope still awaits certification.
        assert_eq!(
            publication_progress(&publications, "enriching", 1, 0)["metadataTotal"],
            1
        );
        let entries = publications.products["900001"].clone();
        service
            .commit_product_group(&job, "900001", &entries, &mut publications.checkpoints)
            .unwrap();
        assert_eq!(
            catalogue
                .get("entitlement", &format!("{account}:free:900001"))
                .unwrap()
                .unwrap(),
            free_entitlement
        );
        assert_eq!(
            catalogue.get("checkpoint", &draft_id).unwrap().unwrap(),
            marker
        );
        assert!(!publications.checkpoints["free"].complete());
        assert!(publications.checkpoints["purchase"].complete());
        assert_eq!(published_product_count(&publications), 1);
        // This also covers the subsequent resume through published_products,
        // rather than through a newly hydrated marker.
        assert!(service
            .restore_product_publication(
                "900001",
                &[&free_entry.rows[0]],
                &free_entry.routes,
                &free_entry.files,
                true,
                None,
                &mut resumed
            )
            .unwrap());
        let mut early = prefetch.early.lock().unwrap();
        early.publications.checkpoints = publications.checkpoints.clone();
        let revision = catalogue.revision().unwrap();
        assert!(service
            .publish_early_records(&job, &mut early, None)
            .unwrap()
            .is_empty());
        assert!(service
            .publish_early_records(&job, &mut early, None)
            .unwrap()
            .is_empty());
        service
            .commit_product_group(&job, "900001", &entries, &mut publications.checkpoints)
            .unwrap();
        assert_eq!(catalogue.revision().unwrap(), revision);
        let progress = publication_progress(
            &early.publications,
            "enriching",
            1,
            published_product_count(&early.publications),
        );
        assert_eq!(progress["products"], 1);
        assert_eq!(progress["metadataTotal"], 1);
        assert_eq!(progress["metadataCompleted"], 1);
        assert_eq!(progress["published"], 1);
        assert!(early.published.contains("900001"));
        assert_eq!(
            catalogue
                .get("entitlement", &format!("{account}:free:900001"))
                .unwrap()
                .unwrap()["manifestBasis"],
            "verifiedAccountProductFileList"
        );
        drop(early);
        drop(service);
        drop(catalogue);
        std::fs::remove_dir_all(path).unwrap();
    }
    #[test]
    fn discovery_interleaves_listing_and_details_and_public_patch_keeps_manual_fields() {
        let mut checkpoint = Checkpoint::fresh("synthetic", "purchase", "full").unwrap();
        checkpoint.pending.clear();
        checkpoint.terminal = true;
        checkpoint.product_ids.insert("900001".into());
        checkpoint.enrichment_pending.insert("900001".into());
        assert!(collection_membership_complete(
            &checkpoint,
            &BTreeSet::new()
        ));
        assert!(!checkpoint.complete()); // collected does not mean published
        assert!(!collection_membership_complete(
            &checkpoint,
            &BTreeSet::from([("synthetic".into(), "purchase".into(), "900001".into())])
        ));
        assert!(collection_membership_complete(
            &checkpoint,
            &BTreeSet::from([("synthetic".into(), "gift".into(), "900001".into())])
        ));
        let incomplete = BTreeSet::from([("synthetic".into(), "purchase".into(), "900001".into())]);
        assert!(checkpoint_finished_after_collection(
            &checkpoint,
            false,
            &incomplete
        ));
        checkpoint.pending.push("/orders?page=2".into());
        assert!(!checkpoint_finished_after_collection(
            &checkpoint,
            false,
            &incomplete
        ));
        checkpoint.pending.clear();
        checkpoint.enrichment_pending.clear();
        checkpoint.published_products.insert("900001".into());
        assert!(checkpoint.complete());
        let pending = vec![
            "/orders?page=3".into(),
            "/orders/910001".into(),
            "/orders?page=2".into(),
            "/orders/910002".into(),
        ];
        assert_eq!(
            next_routes(&pending).unwrap(),
            vec!["/orders?page=2", "/orders/910001", "/orders/910002"]
        );
        assert_eq!(next_routes(&pending[3..]).unwrap(), vec!["/orders/910002"]);
        let free = vec![
            "/library/free_downloads?page=2".into(),
            safety::free_item_route("900001").unwrap(),
        ];
        assert_eq!(next_routes(&free).unwrap(), free);
        let before = json!({"id":"900001","note":"Human note","sourceTags":["old"],"tags":["old"],"price":10});
        let mut after = before.clone();
        after["sourceTags"] = json!(["new"]);
        after["price"] = json!(0);
        let patch = public_patch(&before, &after);
        assert!(patch.get("note").is_none());
        assert!(patch.get("id").is_none());
        assert_eq!(patch["price"], 0);
        let mut newer = before;
        newer["note"] = json!("Edited while fetching");
        newer
            .as_object_mut()
            .unwrap()
            .extend(patch.as_object().unwrap().clone());
        assert_eq!(newer["note"], "Edited while fetching");
        assert_eq!(newer["sourceTags"], json!(["new"]));
        let request = PublicRequest {
            product: "900001".into(),
            draft: newer.clone(),
            account: "synthetic".into(),
            source: "purchase".into(),
            run_id: "current-run".into(),
        };
        let mut saved = json!({"accountId":"synthetic","source":"purchase","runId":"current-run",
            "productId":"900001","product":newer,"enriched":true});
        assert!(request.saved_product(&saved).is_some());
        for key in ["accountId", "source", "runId", "productId"] {
            let mut other = saved.clone();
            other[key] = json!("other");
            assert!(request.saved_product(&other).is_none(), "{key}");
        }
        saved["enriched"] = json!(false);
        assert!(request.saved_product(&saved).is_none());
        let before =
            json!({"note":"Current human note","publicMetadataErrorCode":"PUBLIC_METADATA_FAILED"});
        let after = json!({"note":"Old human note","publicMetadataState":"complete"});
        let patch = public_patch(&before, &after);
        assert!(patch.get("note").is_none());
        assert_eq!(patch.get("publicMetadataErrorCode"), Some(&Value::Null));
        let public = json!({"id":"900001","sourceTitle":"Current public title",
            "title":"Current public title","previewUrl":"https://booth.pximg.net/current.png",
            "publicMetadataState":"complete","price":null,"publicVariants":[]});
        let mut receipt = json!({"id":"900001","sourceTitle":"Older receipt title",
            "title":"Older receipt title","previewUrl":"https://booth.pximg.net/old.png",
            "note":"Human note","publicMetadataErrorCode":"PUBLIC_METADATA_FAILED"});
        let observation = public_patch(&public, &public);
        receipt
            .as_object_mut()
            .unwrap()
            .extend(observation.as_object().unwrap().clone());
        assert_eq!(receipt["sourceTitle"], public["sourceTitle"]);
        assert_eq!(receipt["title"], public["title"]);
        assert_eq!(receipt["previewUrl"], public["previewUrl"]);
        assert!(receipt["price"].is_null());
        assert_eq!(receipt["publicVariants"], json!([]));
        assert!(receipt["publicMetadataErrorCode"].is_null());
        assert_eq!(receipt["note"], "Human note");
    }
    #[test]
    fn unresolved_digital_candidates_prevent_manifest_certification() {
        let mut checkpoint = Checkpoint {
            terminal: true,
            head: "same-head".into(),
            ..Default::default()
        };
        let head = parser::parse(
            "<main><h1>Library</h1><div class='empty-state'>No items</div></main>",
            "library",
            "/library?page=1",
        )
        .unwrap();
        checkpoint.head = head.fingerprint.clone();
        assert!(source_traversal_verified(&checkpoint, &head));
        checkpoint.unresolved = 1;
        assert!(!source_traversal_verified(&checkpoint, &head));
        checkpoint.unresolved = 0;
        checkpoint.pending.push("/library?page=2".into());
        assert!(!source_traversal_verified(&checkpoint, &head));
    }
    #[test]
    fn acquisition_identity_reconciliation_rejects_ambiguous_or_foreign_bindings() {
        let mut row = parser::Row {
            product_id: Some("900001".into()),
            order_id: Some("910001".into()),
            source_record_id: format!(
                "purchase:{}",
                safety::hash("910001:900001::2026/10/10 09:30:")
            ),
            acquired_date_text: Some("2026/10/10 09:30".into()),
            ..Default::default()
        };
        let old_record = format!("purchase:{}", safety::hash("910001:900001:::"));
        let saved = json!({"id":format!("a:{old_record}"),"sourceRecordId":old_record,"accountId":"a","source":"purchase","productId":"900001","orderId":"910001","variantName":null,"acquiredDateText":"2026/10/10 09:30"});
        assert_eq!(
            unique_previous_acquisition_record(std::slice::from_ref(&saved), "a", "purchase", &row),
            Some(old_record.clone())
        );
        assert!(unique_previous_acquisition_record(
            &[saved.clone(), saved.clone()],
            "a",
            "purchase",
            &row
        )
        .is_none());
        assert!(unique_previous_acquisition_record(
            std::slice::from_ref(&saved),
            "b",
            "purchase",
            &row
        )
        .is_none());
        row.variant_name = Some("Different variant".into());
        assert!(unique_previous_acquisition_record(
            std::slice::from_ref(&saved),
            "a",
            "purchase",
            &row
        )
        .is_none());
        row.variant_name = None;
        row.source_record_id = format!("purchase:{}", safety::hash("line:920001"));
        assert!(unique_previous_acquisition_record(
            std::slice::from_ref(&saved),
            "a",
            "purchase",
            &row
        )
        .is_none());
    }
    #[test]
    fn shelf_date_supplementation_preserves_unique_fallback_but_not_distinct_dated_events() {
        for source in ["library", "gift", "free"] {
            let old_record = format!("{source}:{}", safety::hash(":900001:::"));
            let mut row = parser::Row {
                product_id: Some("900001".into()),
                source_record_id: format!(
                    "{source}:{}",
                    safety::hash(":900001::2026/10/10 09:30:")
                ),
                acquired_date_text: Some("2026/10/10 09:30".into()),
                ..Default::default()
            };
            let mut saved = json!({"id":format!("a:{old_record}"),"sourceRecordId":old_record,"accountId":"a","source":source,"productId":"900001","orderId":null,"variantName":null,"acquiredDateText":null});
            assert_eq!(
                unique_previous_acquisition_record(std::slice::from_ref(&saved), "a", source, &row),
                Some(old_record)
            );
            saved["acquiredDateText"] = json!("2026/10/09 09:30");
            let dated_record = format!("{source}:{}", safety::hash(":900001::2026/10/09 09:30:"));
            saved["sourceRecordId"] = json!(dated_record);
            saved["id"] = json!(format!("a:{dated_record}"));
            assert!(unique_previous_acquisition_record(
                std::slice::from_ref(&saved),
                "a",
                source,
                &row
            )
            .is_none());
            row.source_record_id.push_str(":occurrence:1");
            assert!(unique_previous_acquisition_record(
                std::slice::from_ref(&saved),
                "a",
                source,
                &row
            )
            .is_none());
        }
    }
    #[test]
    fn selected_receipts_use_unique_bound_routes_and_fallback_when_unknown() {
        let acquisitions = vec![
            json!({"accountId":"a","source":"purchase","productId":"900001","orderId":"910001"}),
            json!({"accountId":"a","source":"purchase","productId":"900001","orderId":"910001"}),
            json!({"accountId":"b","source":"purchase","productId":"900001","orderId":"910999"}),
            json!({"accountId":"a","source":"gift","productId":"900001","orderId":"910998"}),
        ];
        let remote = vec![
            json!({"accountId":"a","source":"purchase","productId":"900002","resolverPage":"/orders/910001"}),
            json!({"accountId":"a","source":"purchase","productId":"900002","resolverPage":"/orders?page=1"}),
        ];
        let selected = BTreeSet::from(["900001".into(), "900002".into()]);
        assert_eq!(
            selected_purchase_routes(&acquisitions, &remote, "a", &selected),
            Some(BTreeMap::from([(
                "/orders/910001".into(),
                selected.clone()
            )]))
        );
        assert!(selected_purchase_routes(&acquisitions, &remote, "b", &selected).is_none());
        assert!(selected_purchase_routes(&[], &[], "a", &selected).is_none());
    }
    #[test]
    fn targeted_paid_patch_preserves_all_other_fields_and_records() {
        let path =
            std::env::temp_dir().join(format!("nna-paid-patch-synthetic-{}", uuid::Uuid::new_v4()));
        let catalogue = crate::catalogue::Catalogue::open(&path).unwrap();
        let mut row = parser::Row {
            source_record_id: "purchase:authored-product".into(),
            product_id: Some("900001".into()),
            order_id: Some("910001".into()),
            name: Some("Authored product".into()),
            manifest_complete: true,
            product_line_total_jpy: Some(0),
            acquired_date_text: Some("2026/10/10 09:30".into()),
            files: vec![parser::File {
                id: "700001".into(),
                variant_id: None,
                name: Some("authored.zip".into()),
                size_bytes: None,
                source_url: "https://booth.pm/downloadables/700001".into(),
                revision: "authored-revision".into(),
            }],
            ..Default::default()
        };
        let existing = json!({"id":"synthetic-account:purchase:authored-product", "accountId":"synthetic-account", "source":"purchase",
            "sourceRecordId":row.source_record_id, "orderId":"910001", "productId":"900001", "observedProductId":"900001",
            "acquiredDateText":"authored-prior-date", "paidAmountText":"authored-prior-text", "note":"authored-overlay", "observedAt":"authored-time", "extra":{"keep":true}});
        catalogue
            .put(
                "acquisition",
                existing["id"].as_str().unwrap(),
                existing.clone(),
            )
            .unwrap();
        let protected = [
            "entitlement",
            "product",
            "remote_file",
            "artifact",
            "checkpoint",
            "source_coverage",
        ];
        for kind in protected {
            catalogue.put(kind, "authored-protected", json!({"id":"authored-protected", "manifestComplete":true, "authoredKeep":kind})).unwrap();
        }
        let before: Vec<_> = protected
            .iter()
            .map(|kind| catalogue.list(kind).unwrap())
            .collect();
        assert!(saved_product_line_binding(std::slice::from_ref(&row), &row));
        let patched =
            patched_product_fields(&existing, "synthetic-account", &row, "/orders/910001").unwrap();
        let mut expected = existing.clone();
        expected["paidPrice"] = json!(0);
        expected["paidCurrency"] = json!("JPY");
        expected["paidPriceBasis"] = json!("productLineTotal");
        expected["acquiredDateText"] = json!("2026/10/10 09:30");
        assert_eq!(patched, expected);
        let mut batch = super::super::batch::RecordBatch::new(&catalogue);
        batch
            .stage(
                "acquisition",
                existing["id"].as_str().unwrap(),
                patched.clone(),
            )
            .unwrap();
        assert_eq!(batch.try_commit().unwrap(), Some(1));
        assert_eq!(
            catalogue.list("acquisition").unwrap(),
            vec![patched.clone()]
        );
        for (kind, prior) in protected.iter().zip(before) {
            assert_eq!(catalogue.list(kind).unwrap(), prior);
        }
        for key in [
            "id",
            "accountId",
            "source",
            "sourceRecordId",
            "orderId",
            "productId",
            "observedProductId",
        ] {
            let mut conflict = existing.clone();
            conflict[key] = json!("authored-conflict");
            assert!(
                patched_product_fields(&conflict, "synthetic-account", &row, "/orders/910001")
                    .is_none()
            );
        }
        let mut stale = row.clone();
        stale.files[0].id = "700002".into();
        assert!(!saved_product_line_binding(
            std::slice::from_ref(&row),
            &stale
        ));
        assert!(!saved_product_line_binding(
            &[row.clone(), row.clone()],
            &row
        ));
        row.acquired_date_text = Some(" ".into());
        let unchanged =
            patched_product_fields(&patched, "synthetic-account", &row, "/orders/910001").unwrap();
        assert_eq!(unchanged, patched);
        let mut replay = super::super::batch::RecordBatch::new(&catalogue);
        replay
            .stage("acquisition", existing["id"].as_str().unwrap(), unchanged)
            .unwrap();
        assert_eq!(replay.try_commit().unwrap(), Some(0));
        drop(catalogue);
        assert!(path.starts_with(std::env::temp_dir()));
        std::fs::remove_dir_all(path).unwrap();
    }
    fn diagnostic_sample_fixture(count: usize) -> (Checkpoint, Vec<Value>) {
        let mut checkpoint = Checkpoint::fresh("synthetic-account", "purchase", "full").unwrap();
        let mut details = Vec::new();
        for index in 0..count {
            let page = parser::parse(&format!("<main><div class='sheet'><a href='https://invented.booth.pm/items/{}'>Authored</a><div data-test='downloadable-list'><b data-test='filename'>authored.zip</b><a href='https://booth.pm/downloadables/{}'>Download</a></div></div></main>", 900001 + index, 700001 + index), "purchase", &format!("/orders/{}", 910001 + index)).unwrap();
            details.push(page);
        }
        let mut listing = details[0].clone();
        listing.route = "/orders?page=1".into();
        listing.is_detail = false;
        listing.rows.clear();
        listing.details = details.iter().map(|page| page.route.clone()).collect();
        listing.fingerprint = "synthetic-listing".into();
        let mut journals = Vec::new();
        for (index, page) in std::iter::once(listing).chain(details).enumerate() {
            let id = format!("journal-{index}");
            checkpoint.journal_ids.push(id.clone());
            checkpoint
                .committed
                .insert(page.route.clone(), page.fingerprint.clone());
            journals.push(json!({"id":id,"accountId":checkpoint.account_id,"source":"purchase","runId":checkpoint.run_id,"page":page}));
        }
        (checkpoint, journals)
    }
    #[test]
    fn targeted_receipt_selection_is_exact_and_validates_other_journals() {
        let (checkpoint, journals) = diagnostic_sample_fixture(3);
        let selected =
            select_targeted_receipt_proof(&checkpoint, &journals, "synthetic-account", "910003")
                .unwrap()
                .unwrap();
        assert!(selected
            .iter()
            .all(|row| row.order_id.as_deref() == Some("910003")));
        assert!(select_targeted_receipt_proof(
            &checkpoint,
            &journals,
            "synthetic-account",
            "910999"
        )
        .unwrap()
        .is_none());
        for index in 0..journals.len() {
            let mut invalid = journals.clone();
            invalid[index]["runId"] = json!("other-run");
            assert_eq!(
                select_targeted_receipt_proof(&checkpoint, &invalid, "synthetic-account", "910003")
                    .unwrap_err()
                    .to_string(),
                "CHECKPOINT_INVALID"
            );
        }
        assert_eq!(
            select_targeted_receipt_proof(&checkpoint, &journals, "different-account", "910003")
                .unwrap_err()
                .to_string(),
            "CHECKPOINT_INVALID"
        );
        assert_eq!(
            select_targeted_receipt_proof(
                &checkpoint,
                &journals,
                "synthetic-account",
                "910003?x=1"
            )
            .unwrap_err()
            .to_string(),
            "INVALID_DIAGNOSTIC_ARGUMENTS"
        );
        let mut unwitnessed = journals.clone();
        unwitnessed[0]["page"]["details"] = json!(["/orders/910001"]);
        assert!(select_targeted_receipt_proof(
            &checkpoint,
            &unwitnessed,
            "synthetic-account",
            "910003"
        )
        .unwrap()
        .is_none());
    }
    #[test]
    fn acquisition_diagnostic_detail_is_current_run_and_listing_witness_bound() {
        let detail=parser::parse("<main><div class='sheet'><a href='https://invented.booth.pm/items/900001'>Authored</a><div data-test='downloadable-list'><b data-test='filename'>authored.zip</b><a href='https://booth.pm/downloadables/700001'>Download</a></div></div></main>","purchase","/orders/910001").unwrap();
        let mut listing = detail.clone();
        listing.route = "/orders?page=1".into();
        listing.is_detail = false;
        listing.rows.clear();
        listing.details = vec![detail.route.clone()];
        listing.fingerprint = "synthetic-listing".into();
        let mut checkpoint = Checkpoint::fresh("synthetic-account", "purchase", "full").unwrap();
        checkpoint.journal_ids = vec!["listing".into(), "detail".into()];
        checkpoint
            .committed
            .insert(listing.route.clone(), listing.fingerprint.clone());
        checkpoint
            .committed
            .insert(detail.route.clone(), detail.fingerprint.clone());
        let journals = vec![
            json!({"id":"listing","accountId":checkpoint.account_id,"source":"purchase","runId":checkpoint.run_id,"page":listing}),
            json!({"id":"detail","accountId":checkpoint.account_id,"source":"purchase","runId":checkpoint.run_id,"page":detail}),
        ];
        let (route, rows) =
            select_acquisition_diagnostic_detail(&checkpoint, &journals, "synthetic-account")
                .unwrap();
        assert_eq!(route, "/orders/910001");
        assert_eq!(rows.len(), 1);
        let mut rejected = journals.clone();
        rejected[1]["runId"] = json!("old-run");
        assert_eq!(
            select_acquisition_diagnostic_detail(&checkpoint, &rejected, "synthetic-account")
                .unwrap_err()
                .to_string(),
            "CHECKPOINT_INVALID"
        );
        let mut rejected = journals.clone();
        rejected[0]["page"]["details"] = json!([]);
        assert_eq!(
            select_acquisition_diagnostic_detail(&checkpoint, &rejected, "synthetic-account")
                .unwrap_err()
                .to_string(),
            "DIAGNOSTIC_ENTRY_NOT_FOUND"
        );
        let mut rejected = journals.clone();
        rejected[1]["page"]["rows"][0]["manifestComplete"] = json!(false);
        assert_eq!(
            select_acquisition_diagnostic_detail(&checkpoint, &rejected, "synthetic-account")
                .unwrap_err()
                .to_string(),
            "DIAGNOSTIC_ENTRY_NOT_FOUND"
        );
        let mut rejected = checkpoint.clone();
        rejected.committed.clear();
        assert_eq!(
            select_acquisition_diagnostic_detail(&rejected, &journals, "synthetic-account")
                .unwrap_err()
                .to_string(),
            "CHECKPOINT_INVALID"
        );
    }
    #[test]
    fn diagnostic_reservation_atomically_blocks_duplicates_and_account_jobs() {
        let running = Mutex::new(BTreeSet::new());
        let first = DiagnosticReservation::acquire(&running, "synthetic-account").unwrap();
        assert_eq!(
            *running.lock().unwrap(),
            BTreeSet::from([
                "booth-global".to_owned(),
                "account:synthetic-account".to_owned()
            ])
        );
        for account in ["synthetic-account", "other-account"] {
            assert_eq!(
                DiagnosticReservation::acquire(&running, account)
                    .err()
                    .unwrap()
                    .to_string(),
                "ACCOUNT_BUSY"
            );
        }
        drop(first);
        assert!(running.lock().unwrap().is_empty());
        running
            .lock()
            .unwrap()
            .insert("account:other-account".into());
        assert_eq!(
            DiagnosticReservation::acquire(&running, "synthetic-account")
                .err()
                .unwrap()
                .to_string(),
            "ACCOUNT_BUSY"
        );
        assert_eq!(
            *running.lock().unwrap(),
            BTreeSet::from(["account:other-account".to_owned()])
        );
    }
    #[test]
    fn diagnostic_reservation_releases_its_keys_on_error() {
        let running = Mutex::new(BTreeSet::new());
        let result: Result<()> = (|| {
            let _reservation = DiagnosticReservation::acquire(&running, "synthetic-account")?;
            running.lock().unwrap().insert("unrelated-key".into());
            bail!("SYNTHETIC_EARLY_ERROR");
        })();
        assert_eq!(result.unwrap_err().to_string(), "SYNTHETIC_EARLY_ERROR");
        assert_eq!(
            *running.lock().unwrap(),
            BTreeSet::from(["unrelated-key".to_owned()])
        );
        running.lock().unwrap().clear();
        let next = DiagnosticReservation::acquire(&running, "synthetic-account").unwrap();
        drop(next);
        assert!(running.lock().unwrap().is_empty());
    }
    #[tokio::test]
    async fn cancelling_pending_diagnostic_future_releases_both_keys() {
        let running = Mutex::new(BTreeSet::new());
        let mut future = Box::pin(async {
            let _reservation =
                DiagnosticReservation::acquire(&running, "synthetic-account").unwrap();
            std::future::pending::<()>().await;
        });
        std::future::poll_fn(|context| {
            assert!(std::future::Future::poll(future.as_mut(), context).is_pending());
            std::task::Poll::Ready(())
        })
        .await;
        assert_eq!(running.lock().unwrap().len(), 2);
        drop(future);
        assert!(running.lock().unwrap().is_empty());
        let next = DiagnosticReservation::acquire(&running, "other-account").unwrap();
        drop(next);
        assert!(running.lock().unwrap().is_empty());
    }
    #[test]
    fn identity_diagnostic_selection_requires_its_fixed_issue_and_current_history() {
        let mut checkpoint = Checkpoint::fresh("synthetic-account", "free", "full").unwrap();
        let history = free_history_page();
        let identity = free_item::Evidence::unknown("900001", "FREE_ITEM_IDENTITY_UNPROVEN", false)
            .page()
            .unwrap();
        checkpoint.journal_ids = vec!["synthetic-history".into(), "synthetic-identity".into()];
        checkpoint
            .committed
            .insert(history.route.clone(), history.fingerprint.clone());
        checkpoint
            .committed
            .insert(identity.route.clone(), identity.fingerprint.clone());
        let journals = vec![
            json!({"id":"synthetic-history","accountId":checkpoint.account_id,"source":"free","runId":checkpoint.run_id,"page":history}),
            json!({"id":"synthetic-identity","accountId":checkpoint.account_id,"source":"free","runId":checkpoint.run_id,"page":identity}),
        ];
        assert_eq!(
            select_free_item_diagnostic_route(
                &checkpoint,
                &journals,
                "synthetic-account",
                FreeItemDiagnostic::Identity
            )
            .unwrap(),
            "https://booth.pm/ja/items/900001"
        );
        assert_eq!(
            select_entry_diagnostic_route(&checkpoint, &journals, "synthetic-account")
                .unwrap_err()
                .to_string(),
            "DIAGNOSTIC_ENTRY_NOT_FOUND"
        );
        for (index, field, value) in [
            (1, "runId", "old-run"),
            (0, "accountId", "other-account"),
            (1, "source", "purchase"),
        ] {
            let mut rejected = journals.clone();
            rejected[index][field] = json!(value);
            assert_eq!(
                select_free_item_diagnostic_route(
                    &checkpoint,
                    &rejected,
                    "synthetic-account",
                    FreeItemDiagnostic::Identity
                )
                .unwrap_err()
                .to_string(),
                "CHECKPOINT_INVALID"
            );
        }
        for (index, field, value) in [
            (0, "unavailable", json!(true)),
            (0, "errorCode", json!("ROW_UNPROVEN")),
            (1, "errorCode", json!("FREE_ITEM_ENTRY_UNPROVEN")),
        ] {
            let mut rejected = journals.clone();
            if index == 0 {
                rejected[0]["page"]["rows"][0][field] = value;
            } else {
                rejected[1]["page"]["freeItem"][field] = value;
            }
            assert_eq!(
                select_free_item_diagnostic_route(
                    &checkpoint,
                    &rejected,
                    "synthetic-account",
                    FreeItemDiagnostic::Identity
                )
                .unwrap_err()
                .to_string(),
                "DIAGNOSTIC_ENTRY_NOT_FOUND"
            );
        }
    }
    #[test]
    fn entry_diagnostic_selection_is_current_account_run_and_history_bound() {
        let mut checkpoint = Checkpoint::fresh("synthetic-account", "free", "full").unwrap();
        let history = free_history_page();
        let entry = free_item::Evidence::unknown("900001", "FREE_ITEM_ENTRY_UNPROVEN", false)
            .page()
            .unwrap();
        checkpoint.journal_ids = vec!["synthetic-history".into(), "synthetic-entry".into()];
        checkpoint
            .committed
            .insert(history.route.clone(), history.fingerprint.clone());
        checkpoint
            .committed
            .insert(entry.route.clone(), entry.fingerprint.clone());
        let journals = vec![
            json!({"id":"synthetic-history","accountId":checkpoint.account_id,"source":"free","runId":checkpoint.run_id,"page":history}),
            json!({"id":"synthetic-entry","accountId":checkpoint.account_id,"source":"free","runId":checkpoint.run_id,"page":entry}),
        ];
        assert_eq!(
            select_entry_diagnostic_route(&checkpoint, &journals, "synthetic-account").unwrap(),
            "https://booth.pm/ja/items/900001"
        );
        assert_eq!(
            select_entry_diagnostic_route(&checkpoint, &journals, "different-account")
                .unwrap_err()
                .to_string(),
            "CHECKPOINT_INVALID"
        );
        for (index, field, value) in [
            (0, "accountId", "other-account"),
            (1, "runId", "old-run"),
            (1, "source", "purchase"),
            (1, "id", "unlisted-entry"),
        ] {
            let mut rejected = journals.clone();
            rejected[index][field] = json!(value);
            assert_eq!(
                select_entry_diagnostic_route(&checkpoint, &rejected, "synthetic-account")
                    .unwrap_err()
                    .to_string(),
                "CHECKPOINT_INVALID"
            );
        }
        let mut no_history = journals.clone();
        no_history[0]["page"]["rows"][0]["unavailable"] = json!(true);
        assert_eq!(
            select_entry_diagnostic_route(&checkpoint, &no_history, "synthetic-account")
                .unwrap_err()
                .to_string(),
            "DIAGNOSTIC_ENTRY_NOT_FOUND"
        );
        let mut wrong_reason = journals.clone();
        wrong_reason[1]["page"]["freeItem"]["errorCode"] = json!("FREE_ITEM_ROLE_UNPROVEN");
        assert_eq!(
            select_entry_diagnostic_route(&checkpoint, &wrong_reason, "synthetic-account")
                .unwrap_err()
                .to_string(),
            "DIAGNOSTIC_ENTRY_NOT_FOUND"
        );
        for issue in [FreeItemDiagnostic::Role, FreeItemDiagnostic::Controls] {
            let mut bound = journals.clone();
            bound[1]["page"]["freeItem"]["errorCode"] = json!(issue.error_code());
            assert_eq!(
                select_free_item_diagnostic_route(&checkpoint, &bound, "synthetic-account", issue,)
                    .unwrap(),
                "https://booth.pm/ja/items/900001"
            );
            bound[0]["page"]["rows"][0]["unavailable"] = json!(true);
            assert_eq!(
                select_free_item_diagnostic_route(&checkpoint, &bound, "synthetic-account", issue,)
                    .unwrap_err()
                    .to_string(),
                "DIAGNOSTIC_ENTRY_NOT_FOUND"
            );
        }
        for version in [0, 1] {
            let mut old = checkpoint.clone();
            old.free_item_adapter_version = version;
            assert_eq!(
                select_entry_diagnostic_route(&old, &journals, "synthetic-account")
                    .unwrap_err()
                    .to_string(),
                "CHECKPOINT_INVALID"
            );
        }
        let mut old = checkpoint.clone();
        old.committed.clear();
        assert_eq!(
            select_entry_diagnostic_route(&old, &journals, "synthetic-account")
                .unwrap_err()
                .to_string(),
            "CHECKPOINT_INVALID"
        );
    }
    #[test]
    fn selected_free_update_preserves_global_coverage_and_unselected_product() {
        let account = "10000000-0000-4000-8000-000000000001";
        let coverage_id = format!("{account}:free");
        let selected = BTreeSet::from(["900001".to_owned()]);
        for existing_coverage in [true, false] {
            let path = std::env::temp_dir().join(format!(
                "nna-booth-synthetic-selected-{}",
                uuid::Uuid::new_v4()
            ));
            let catalogue = crate::catalogue::Catalogue::open(&path).unwrap();
            let original = json!({"id":coverage_id,"accountId":account,"source":"free","status":"incomplete","coverageComplete":false,"productCount":2,"unresolvedRows":1,"completedAt":"authored-prior-proof"});
            if existing_coverage {
                catalogue
                    .put("source_coverage", &coverage_id, original.clone())
                    .unwrap();
            }
            let mut history = free_history_page();
            let mut unselected = history.rows[0].clone();
            unselected.source_record_id = "free:synthetic-other".into();
            unselected.product_id = Some("900002".into());
            history.rows.push(unselected);
            assert_eq!(eligible_free_rows(&history).count(), 2);
            let a_id = format!("{account}:free:900001");
            let b_id = format!("{account}:free:900002");
            let a_before = json!({"id":a_id,"productId":"900001","accountId":account,"source":"free","fileIds":[],"manifestComplete":false});
            let b_before = json!({"id":b_id,"productId":"900002","accountId":account,"source":"free","fileIds":[],"manifestComplete":false});
            catalogue
                .put_many(vec![
                    ("entitlement".into(), a_id.clone(), a_before),
                    ("entitlement".into(), b_id.clone(), b_before.clone()),
                ])
                .unwrap();
            let before = catalogue.get("source_coverage", &coverage_id).unwrap();
            let running =
                json!({"id":coverage_id,"accountId":account,"source":"free","status":"running"});
            if let Some((kind, id, value)) =
                source_coverage_record(Some(&selected), &coverage_id, running)
            {
                catalogue.put(&kind, &id, value).unwrap();
            }
            assert_eq!(
                catalogue.get("source_coverage", &coverage_id).unwrap(),
                before
            );
            let mut a_updated = catalogue.get("entitlement", &a_id).unwrap().unwrap();
            a_updated["manifestComplete"] = json!(true);
            a_updated["fileIds"] = json!([format!("{account}:700001:800001")]);
            let complete = json!({"id":coverage_id,"accountId":account,"source":"free","status":"complete","coverageComplete":true,"productCount":2});
            let mut records = vec![("entitlement".into(), a_id.clone(), a_updated.clone())];
            records.extend(source_coverage_record(
                Some(&selected),
                &coverage_id,
                complete.clone(),
            ));
            catalogue.put_many(records).unwrap();
            assert_eq!(
                catalogue.get("source_coverage", &coverage_id).unwrap(),
                before
            );
            assert_eq!(
                catalogue.get("entitlement", &a_id).unwrap(),
                Some(a_updated)
            );
            assert_eq!(catalogue.get("entitlement", &b_id).unwrap(), Some(b_before));
            let unrestricted =
                source_coverage_record(None, &coverage_id, complete.clone()).unwrap();
            catalogue.put_many(vec![unrestricted]).unwrap();
            assert_eq!(
                catalogue.get("source_coverage", &coverage_id).unwrap(),
                Some(complete)
            );
            drop(catalogue);
            std::fs::remove_dir_all(path).unwrap();
        }
    }
    fn free_history_page() -> parser::Page {
        parser::Page {
            source: "free".into(),
            route: "/library/free_downloads?page=1".into(),
            rows: vec![parser::Row {
                source_record_id: "free:synthetic-original".into(),
                product_id: Some("900001".into()),
                name: Some("Authored history name".into()),
                acquired_date_text: Some("synthetic-original-date".into()),
                ..parser::Row::default()
            }],
            next: vec![],
            details: vec![],
            terminal: true,
            unresolved: 0,
            fingerprint: "synthetic".into(),
            is_detail: false,
            ordering: parser::Ordering::DownloadedNewestFirst,
            free_item: None,
        }
    }
    #[test]
    fn settled_free_absence_requires_bound_history_and_preserves_incomplete_coverage() {
        let mut history = BTreeMap::new();
        remember_free_history(&free_history_page(), &mut history);
        for code in ["FREE_OFFER_UNAVAILABLE", "PUBLIC_METADATA_UNAVAILABLE"] {
            let page = free_item::Evidence::unknown("900001", code, true)
                .page()
                .unwrap();
            assert!(settled_free_absence(&page, &history));
            assert!(!settled_free_absence(&page, &BTreeMap::new()));
            let mut wrong = page.clone();
            wrong.route = safety::free_item_route("900002").unwrap();
            assert!(!settled_free_absence(&wrong, &history));
            let mut wrong = history.clone();
            wrong.get_mut("900001").unwrap()[0].product_id = Some("900002".into());
            assert!(!settled_free_absence(&page, &wrong));
            let journal =
                json!({"accountId":"authored","source":"free","runId":"current","page":page});
            assert!(checked_journal_page(&journal, "authored", "free", "current").is_ok());
            for (account, source, run) in [
                ("other", "free", "current"),
                ("authored", "library", "current"),
                ("authored", "free", "old"),
            ] {
                assert!(checked_journal_page(&journal, account, source, run).is_err());
            }
        }
        for (code, unavailable) in [
            ("FREE_FILE_CONTROLS_UNAVAILABLE", false),
            ("FREE_OFFER_UNAVAILABLE", false),
            ("SOURCE_HTTP_FAILED", true),
            ("AUTH_REQUIRED", true),
            ("RATE_LIMITED", true),
        ] {
            assert!(!settled_free_absence(
                &free_item::Evidence::unknown("900001", code, unavailable)
                    .page()
                    .unwrap(),
                &history
            ));
        }
        let mut checkpoint = Checkpoint::fresh("authored", "free", "full").unwrap();
        checkpoint.pending.clear();
        checkpoint.terminal = true;
        checkpoint.head = "authored-head".into();
        checkpoint.unresolved = 1;
        assert!(!checkpoint.complete());
        assert!(!collection_membership_complete(
            &checkpoint,
            &BTreeSet::new()
        ));
        assert!(collection_settled(&checkpoint, false, true, false, 1));
        assert!(!collection_settled(&checkpoint, false, false, false, 1));
        assert!(!collection_settled(&checkpoint, false, true, true, 1));
        checkpoint.unresolved = 2;
        assert!(!collection_settled(&checkpoint, false, true, false, 1));
        checkpoint.unresolved = 1;
        checkpoint
            .pending
            .push(safety::free_item_route("900002").unwrap());
        assert!(!collection_settled(&checkpoint, false, true, false, 1));
    }
    #[test]
    fn settled_free_history_publication_preserves_downloads_and_visible_ownership() {
        let account = "10000000-0000-4000-8000-000000000001";
        for existing_grant in [false, true] {
            let path = std::env::temp_dir()
                .join(format!("booth-settled-history-{}", uuid::Uuid::new_v4()));
            let catalogue = crate::catalogue::Catalogue::open(&path).unwrap();
            let service = BoothService::new(catalogue.clone()).unwrap();
            let mut job = json!({"id":"authored-warning-job","type":"boothSync","accountId":account,"state":"running"});
            catalogue
                .put("job", "authored-warning-job", job.clone())
                .unwrap();
            catalogue
                .put(
                    "account",
                    account,
                    json!({"id":account,"provider":"booth","name":"Authored account"}),
                )
                .unwrap();
            let entitlement_id = format!("{account}:free:900001");
            let remote_id = format!("{account}:700001:");
            let old_entitlement = json!({"id":entitlement_id,"productId":"900001","accountId":account,"source":"free","fileIds":[remote_id],"manifestComplete":true,"manifestBasis":"verifiedAccountProductFileList"});
            let old_file = json!({"id":remote_id,"productId":"900001","accountId":account,"source":"free","sourceFileId":"700001","name":"authored-owned.zip","revision":"owned-v1"});
            let old_artifact = json!({"id":"authored-ready","productId":"900001","fileId":remote_id,"revision":"owned-v1","status":"ready"});
            if existing_grant {
                catalogue
                    .put_many(vec![
                        (
                            "entitlement".into(),
                            entitlement_id.clone(),
                            old_entitlement.clone(),
                        ),
                        ("remote_file".into(), remote_id.clone(), old_file.clone()),
                        (
                            "artifact".into(),
                            "authored-ready".into(),
                            old_artifact.clone(),
                        ),
                    ])
                    .unwrap();
            }
            let mut row = free_history_page().rows.remove(0);
            // Even historical file hints must not update a current certified grant.
            row.files.push(parser::File {
                id: "700001".into(),
                variant_id: None,
                name: Some("obsolete-hint.zip".into()),
                size_bytes: Some(20),
                source_url: "https://booth.pm/downloadables/700001".into(),
                revision: "obsolete-hint".into(),
            });
            let mut draft = json!({"id":"900001","store":"BOOTH","title":"Authored history name","sourceTags":[],"sourceCategory":null});
            if !existing_grant {
                metadata::apply_public_response(&mut draft, "900001", 404, None).unwrap();
            }
            let entry = ProductPublication {
                account: account.into(),
                source: "free".into(),
                product: "900001".into(),
                routes: BTreeMap::from([(
                    row.source_record_id.clone(),
                    safety::free_item_route("900001").unwrap(),
                )]),
                rows: vec![row],
                draft,
                files: BTreeSet::new(),
                checkpoint_key: "free".into(),
                force_verify: true,
                needs_enrichment: false,
            };
            let mut checkpoint = Checkpoint::fresh(account, "free", "full").unwrap();
            checkpoint.pending.clear();
            checkpoint.terminal = true;
            checkpoint.head = "authored-head".into();
            checkpoint.unresolved = 1;
            let mut publications = Publications {
                availability: BTreeMap::from([("900001".into(), vec![entry.clone()])]),
                checkpoints: BTreeMap::from([("free".into(), checkpoint)]),
                ..Default::default()
            };
            assert_eq!(published_product_count(&publications), 0);
            let mut not_ready = entry.clone();
            not_ready.needs_enrichment = true;
            assert!(service
                .commit_availability_product(
                    &job,
                    "900001",
                    &[not_ready],
                    &publications.checkpoints
                )
                .is_err());
            assert!(catalogue.get("product", "900001").unwrap().is_none());
            service
                .commit_availability_product(
                    &job,
                    "900001",
                    std::slice::from_ref(&entry),
                    &publications.checkpoints,
                )
                .unwrap();
            publications.availability_published.insert("900001".into());
            assert_eq!(published_product_count(&publications), 1);
            let details = catalogue
                .command("product_details", json!({"id":"900001"}))
                .unwrap();
            assert_eq!(details["accountIds"], json!([account]));
            let query = catalogue
                .command(
                    "query",
                    json!({"accountIds":[account],"category":details["category"]}),
                )
                .unwrap();
            assert_eq!(query["matchedIds"], json!(["900001"]));
            let acquisition = catalogue
                .get(
                    "acquisition",
                    &format!("{account}:{}", entry.rows[0].source_record_id),
                )
                .unwrap()
                .unwrap();
            assert_eq!(acquisition["acquiredDateText"], "synthetic-original-date");
            assert_eq!(acquisition["unavailable"], false);
            if existing_grant {
                assert_eq!(
                    catalogue.get("entitlement", &entitlement_id).unwrap(),
                    Some(old_entitlement)
                );
                assert_eq!(
                    catalogue.get("remote_file", &remote_id).unwrap(),
                    Some(old_file)
                );
                assert_eq!(
                    catalogue.get("artifact", "authored-ready").unwrap(),
                    Some(old_artifact)
                );
                assert_eq!(details["manifestComplete"], true);
                assert_eq!(details["status"], "ready");
            } else {
                assert!(catalogue.list("entitlement").unwrap().is_empty());
                assert!(catalogue.list("remote_file").unwrap().is_empty());
                assert!(catalogue.list("artifact").unwrap().is_empty());
                assert_eq!(details["manifestComplete"], false);
                assert_eq!(details["status"], "notDownloaded");
                assert_eq!(details["publicPageAvailability"], "unavailable");
                assert!(details["sourceCategory"].is_null());
            }
            assert!(catalogue.list("checkpoint").unwrap().is_empty());
            assert!(publications.checkpoints["free"]
                .published_products
                .is_empty());
            let coverage_id = format!("{account}:free");
            let coverage = json!({"id":coverage_id,"accountId":account,"source":"free","status":"incomplete","coverageComplete":false,"unresolvedRows":1,"availabilityWarnings":1});
            catalogue
                .put("source_coverage", &coverage_id, coverage.clone())
                .unwrap();
            job["progress"] = publication_progress(&publications, "finalizing", 1, 1);
            catalogue
                .put("job", "authored-warning-job", job.clone())
                .unwrap();
            service.finish_job(&job, None).unwrap();
            let completed = catalogue
                .get("job", "authored-warning-job")
                .unwrap()
                .unwrap();
            assert_eq!(completed["state"], "completed");
            assert_eq!(completed["progress"]["availabilityWarnings"], 1);
            assert_eq!(
                catalogue.get("source_coverage", &coverage_id).unwrap(),
                Some(coverage.clone())
            );
            let selected = BTreeSet::from(["900001".into()]);
            assert!(source_coverage_record(Some(&selected), &coverage_id, coverage).is_none());
            let cancelled = json!({"id":"authored-cancelled","state":"cancelled"});
            catalogue
                .put("job", "authored-cancelled", cancelled.clone())
                .unwrap();
            let before = catalogue.get("product", "900001").unwrap();
            assert_eq!(
                service
                    .commit_availability_product(
                        &cancelled,
                        "900001",
                        &[entry],
                        &publications.checkpoints
                    )
                    .unwrap_err()
                    .to_string(),
                "CANCELLED"
            );
            assert_eq!(catalogue.get("product", "900001").unwrap(), before);
            drop(service);
            drop(catalogue);
            std::fs::remove_dir_all(path).unwrap();
        }
    }
    #[test]
    fn settled_free_offer_does_not_block_other_source_or_close_unbound_membership() {
        let path = std::env::temp_dir().join(format!(
            "booth-settled-cross-source-{}",
            uuid::Uuid::new_v4()
        ));
        let catalogue = crate::catalogue::Catalogue::open(&path).unwrap();
        let service = BoothService::new(catalogue.clone()).unwrap();
        let job = json!({"id":"authored-cross-source","type":"boothSync","state":"running"});
        catalogue
            .put("job", "authored-cross-source", job.clone())
            .unwrap();
        let account = "authored-account";
        let mut free = Checkpoint::fresh(account, "free", "full").unwrap();
        free.unresolved = 1;
        let (requests, _receiver) = mpsc::unbounded_channel();
        let prefetch = PublicPrefetch {
            requests,
            enqueued: Default::default(),
            support_pages: Default::default(),
            early: Default::default(),
        };
        let mut history = free_history_page();
        history.rows[0].files.push(parser::File {
            id: "700001".into(),
            variant_id: None,
            name: Some("historical-hint.zip".into()),
            size_bytes: None,
            source_url: "https://booth.pm/downloadables/700001".into(),
            revision: "historical".into(),
        });
        // Pending historical controls and confirmed absent offers cannot poison
        // the whole product before a different source's grant is published.
        service
            .stage_discovered_page(
                &job, account, "free", "full", "free", &free, &history, &prefetch,
            )
            .unwrap();
        let mut absent = free_item::Evidence::unknown("900001", "FREE_OFFER_UNAVAILABLE", true)
            .page()
            .unwrap();
        absent.rows = history.rows.clone();
        service
            .stage_discovered_page(
                &job, account, "free", "full", "free", &free, &absent, &prefetch,
            )
            .unwrap();
        assert!(prefetch
            .early
            .lock()
            .unwrap()
            .publications
            .incomplete_scopes
            .is_empty());
        let mut publications = Publications {
            checkpoints: BTreeMap::from([("free".into(), free)]),
            ..Default::default()
        };
        let history_row = history.rows.remove(0);
        let availability = ProductPublication {
            account: account.into(),
            source: "free".into(),
            product: "900001".into(),
            routes: BTreeMap::from([(history_row.source_record_id.clone(), absent.route.clone())]),
            rows: vec![history_row],
            draft: json!({"id":"900001","store":"BOOTH","title":"Authored history","publicMetadataState":"complete"}),
            files: BTreeSet::new(),
            checkpoint_key: "free".into(),
            force_verify: true,
            needs_enrichment: false,
        };
        publications
            .availability
            .insert("900001".into(), vec![availability.clone()]);
        let purchase_row = parser::Row {
            source_record_id: "purchase:910001".into(),
            product_id: Some("900001".into()),
            order_id: Some("910001".into()),
            product_line_total_jpy: Some(100),
            manifest_complete: true,
            files: vec![parser::File {
                id: "700002".into(),
                variant_id: None,
                name: Some("purchased.zip".into()),
                size_bytes: Some(10),
                source_url: "https://booth.pm/downloadables/700002".into(),
                revision: "purchased".into(),
            }],
            ..Default::default()
        };
        let purchase = ProductPublication {
            account: account.into(),
            source: "purchase".into(),
            product: "900001".into(),
            routes: BTreeMap::from([(
                purchase_row.source_record_id.clone(),
                "/orders/910001".into(),
            )]),
            rows: vec![purchase_row],
            draft: availability.draft.clone(),
            files: BTreeSet::from([format!("{account}:700002:")]),
            checkpoint_key: "purchase".into(),
            force_verify: true,
            needs_enrichment: false,
        };
        publications.checkpoints.insert(
            "purchase".into(),
            Checkpoint::fresh(account, "purchase", "full").unwrap(),
        );
        remember_product_context(&mut publications, purchase.clone());
        assert!(publication_membership_proven(
            &publications.products["900001"],
            &BTreeSet::new()
        ));
        assert!(!publication_membership_proven(
            &publications.products["900001"],
            &BTreeSet::from([(account.into(), "free".into())])
        ));
        assert_eq!(
            publication_progress(&publications, "enriching", 1, 0)["metadataTotal"],
            1
        );
        assert_eq!(
            publication_progress(&publications, "enriching", 1, 0)["availabilityWarnings"],
            1
        );
        service
            .commit_availability_product(&job, "900001", &[availability], &publications.checkpoints)
            .unwrap();
        publications.availability_published.insert("900001".into());
        assert_eq!(published_product_count(&publications), 0); // Purchase still pending.
        service
            .commit_product_group(&job, "900001", &[purchase], &mut publications.checkpoints)
            .unwrap();
        assert_eq!(published_product_count(&publications), 1);
        assert!(catalogue
            .get("entitlement", &format!("{account}:free:900001"))
            .unwrap()
            .is_none());
        assert_eq!(
            catalogue
                .get("entitlement", &format!("{account}:purchase:900001"))
                .unwrap()
                .unwrap()["manifestComplete"],
            true
        );
        assert!(catalogue
            .get("remote_file", &format!("{account}:700001:"))
            .unwrap()
            .is_none());
        assert!(publications.checkpoints["free"]
            .published_products
            .is_empty());
        let free_draft = format!(
            "booth-product:{}:900001",
            publications.checkpoints["free"].run_id
        );
        assert!(catalogue.get("checkpoint", &free_draft).unwrap().is_none());
        let mut unknown = absent;
        unknown.free_item = Some(free_item::Evidence::unknown(
            "900001",
            "FREE_FILE_CONTROLS_UNAVAILABLE",
            false,
        ));
        service
            .stage_discovered_page(
                &job,
                account,
                "free",
                "full",
                "free",
                &publications.checkpoints["free"],
                &unknown,
                &prefetch,
            )
            .unwrap();
        assert!(prefetch
            .early
            .lock()
            .unwrap()
            .publications
            .incomplete_scopes
            .contains(&(account.into(), "free".into(), "900001".into())));
        drop(prefetch);
        drop(service);
        drop(catalogue);
        std::fs::remove_dir_all(path).unwrap();
    }
    #[test]
    fn incomplete_gift_grant_does_not_hide_paid_receipt_in_either_arrival_order() {
        for gift_first in [true, false] {
            let path = std::env::temp_dir()
                .join(format!("booth-independent-grants-{}", uuid::Uuid::new_v4()));
            let catalogue = crate::catalogue::Catalogue::open(&path).unwrap();
            let service = BoothService::new(catalogue.clone()).unwrap();
            let account = "authored-account";
            let job =
                json!({"id":"authored-independent-grants","type":"boothSync","state":"running"});
            catalogue
                .put("job", "authored-independent-grants", job.clone())
                .unwrap();
            let (requests, _receiver) = mpsc::unbounded_channel();
            let prefetch = PublicPrefetch {
                requests,
                enqueued: Default::default(),
                support_pages: Default::default(),
                early: Default::default(),
            };
            let gift = Checkpoint::fresh(account, "gift", "full").unwrap();
            let purchase = Checkpoint::fresh(account, "purchase", "full").unwrap();
            let file = |id: &str, variant: &str| parser::File {
                id: id.into(),
                variant_id: Some(variant.into()),
                name: Some(format!("authored-{id}.zip")),
                size_bytes: Some(10),
                source_url: format!("https://booth.pm/downloadables/{id}?variation_id={variant}"),
                revision: "authored".into(),
            };
            let gift_row = parser::Row {
                source_record_id: "gift:910002".into(),
                product_id: Some("900001".into()),
                name: Some("Authored product".into()),
                files: vec![file("700001", "800001")],
                manifest_complete: false,
                error_code: Some("DIGITAL_CONTROLS_UNRESOLVED".into()),
                ..Default::default()
            };
            let complete_gift_row = parser::Row {
                source_record_id: "gift:910003".into(),
                product_id: Some("900001".into()),
                name: Some("Authored product".into()),
                files: vec![file("700003", "800003")],
                manifest_complete: true,
                ..Default::default()
            };
            let purchase_row = parser::Row {
                source_record_id: "purchase:910001".into(),
                product_id: Some("900001".into()),
                order_id: Some("910001".into()),
                name: Some("Authored product".into()),
                acquired_date_text: Some("2026/10/10 09:30".into()),
                product_line_total_jpy: Some(900),
                files: vec![file("700002", "800002")],
                manifest_complete: true,
                ..Default::default()
            };
            let page = |source: &str, route: &str, row: parser::Row| parser::Page {
                source: source.into(),
                route: route.into(),
                rows: vec![row],
                next: vec![],
                details: vec![],
                terminal: true,
                unresolved: 0,
                fingerprint: "authored".into(),
                is_detail: true,
                ordering: parser::Ordering::Unknown,
                free_item: None,
            };
            let mut bad = page("gift", "/gifts?page=1", gift_row.clone());
            bad.rows.push(complete_gift_row);
            let paid = page("purchase", "/orders/910001", purchase_row);
            let stage = |source: &str, checkpoint: &Checkpoint, page: &parser::Page| {
                service
                    .stage_discovered_page(
                        &job, account, source, "full", source, checkpoint, page, &prefetch,
                    )
                    .unwrap();
            };
            if gift_first {
                stage("gift", &gift, &bad);
                stage("purchase", &purchase, &paid);
            } else {
                stage("purchase", &purchase, &paid);
                stage("gift", &gift, &bad);
            }
            let paid_acquisition = catalogue
                .get("acquisition", "authored-account:purchase:910001")
                .unwrap()
                .unwrap();
            assert_eq!(paid_acquisition["paidPrice"], 900);
            assert_eq!(paid_acquisition["acquiredDateText"], "2026/10/10 09:30");
            assert_eq!(paid_acquisition["variantIds"], json!(["800002"]));
            assert!(catalogue
                .get("remote_file", "authored-account:700002:800002")
                .unwrap()
                .is_some());
            assert!(catalogue
                .get("remote_file", "authored-account:700001:800001")
                .unwrap()
                .is_none());
            assert!(catalogue
                .get("remote_file", "authored-account:700003:800003")
                .unwrap()
                .is_some());
            assert_eq!(
                catalogue
                    .get("entitlement", "authored-account:gift:900001")
                    .unwrap()
                    .unwrap()["manifestComplete"],
                false
            );
            assert!(prefetch
                .early
                .lock()
                .unwrap()
                .publications
                .incomplete_scopes
                .contains(&(account.into(), "gift".into(), "900001".into())));
            {
                let early = prefetch.early.lock().unwrap();
                let mut closed_purchase = purchase.clone();
                closed_purchase.pending.clear();
                closed_purchase.terminal = true;
                let final_scope = Publications {
                    checkpoints: BTreeMap::from([
                        ("purchase".into(), closed_purchase),
                        ("gift".into(), gift.clone()),
                    ]),
                    incomplete_scopes: early.publications.incomplete_scopes.clone(),
                    ..Default::default()
                };
                let entries = &early.publications.products["900001"];
                assert!(publication_entry_certifiable(
                    entries
                        .iter()
                        .find(|entry| entry.source == "purchase")
                        .unwrap(),
                    &final_scope,
                ));
                assert!(!publication_entry_certifiable(
                    entries.iter().find(|entry| entry.source == "gift").unwrap(),
                    &final_scope,
                ));
            }
            let mut complete_gift = gift_row;
            complete_gift.manifest_complete = true;
            complete_gift.error_code = None;
            stage("gift", &gift, &page("gift", "/gifts?page=1", complete_gift));
            assert_eq!(catalogue.related("acquisition", "900001").unwrap().len(), 3);
            assert_eq!(catalogue.related("remote_file", "900001").unwrap().len(), 3);
            assert_eq!(
                catalogue
                    .get("acquisition", "authored-account:purchase:910001")
                    .unwrap()
                    .unwrap()["paidPrice"],
                900
            );
            drop(prefetch);
            drop(service);
            drop(catalogue);
            std::fs::remove_dir_all(path).unwrap();
        }
    }
    #[test]
    fn late_metadata_keeps_complete_receipt_from_mixed_incomplete_product() {
        let path =
            std::env::temp_dir().join(format!("booth-partial-purchase-{}", uuid::Uuid::new_v4()));
        let catalogue = crate::catalogue::Catalogue::open(&path).unwrap();
        let service = BoothService::new(catalogue.clone()).unwrap();
        let job = json!({"id":"authored-partial-purchase","type":"boothSync","state":"running"});
        catalogue
            .put("job", "authored-partial-purchase", job.clone())
            .unwrap();
        let account = "authored-account";
        let key = format!("booth:{account}:purchase");
        let mut checkpoint = Checkpoint::fresh(account, "purchase", "full").unwrap();
        checkpoint.pending.clear();
        checkpoint.terminal = true;
        checkpoint.unresolved = 1;
        checkpoint.product_ids.insert("900002".into());
        let incomplete_row = parser::Row {
            source_record_id: "purchase:910001".into(),
            product_id: Some("900002".into()),
            order_id: Some("910001".into()),
            files: vec![parser::File {
                id: "700001".into(),
                variant_id: Some("800001".into()),
                name: Some("unresolved.zip".into()),
                size_bytes: None,
                source_url: "https://booth.pm/downloadables/700001?variation_id=800001".into(),
                revision: "unknown".into(),
            }],
            manifest_complete: false,
            error_code: Some("DIGITAL_CONTROLS_UNRESOLVED".into()),
            ..Default::default()
        };
        let row = parser::Row {
            source_record_id: "purchase:910002".into(),
            product_id: Some("900002".into()),
            order_id: Some("910002".into()),
            name: Some("Authored paid item".into()),
            acquired_date_text: Some("2026/10/10 09:30".into()),
            product_line_total_jpy: Some(900),
            files: vec![parser::File {
                id: "700002".into(),
                variant_id: Some("800002".into()),
                name: Some("authored.zip".into()),
                size_bytes: Some(10),
                source_url: "https://booth.pm/downloadables/700002?variation_id=800002".into(),
                revision: "authored".into(),
            }],
            manifest_complete: true,
            ..Default::default()
        };
        let ancillary_row = parser::Row {
            source_record_id: "purchase:910003".into(),
            product_id: Some("900002".into()),
            order_id: Some("910003".into()),
            files: vec![],
            manifest_complete: false,
            ..Default::default()
        };
        let observed = observed_grant_rows(
            "purchase",
            &[&incomplete_row, &row, &ancillary_row],
            false,
            false,
        );
        assert_eq!(observed.len(), 2);
        assert!(observed
            .iter()
            .any(|row| row.source_record_id == "purchase:910002"));
        assert!(observed
            .iter()
            .any(|row| row.source_record_id == "purchase:910003"));
        let entry = ProductPublication {
            account: account.into(),
            source: "purchase".into(),
            product: "900002".into(),
            rows: observed.into_iter().cloned().collect(),
            routes: BTreeMap::from([
                ("purchase:910002".into(), "/orders/910002".into()),
                ("purchase:910003".into(), "/orders/910003".into()),
            ]),
            draft: json!({"id":"900002","store":"BOOTH","title":"Authored paid item"}),
            files: BTreeSet::from([format!("{account}:700002:800002")]),
            checkpoint_key: key.clone(),
            force_verify: true,
            needs_enrichment: true,
        };
        let mut publications = Publications {
            products: BTreeMap::from([("900002".into(), vec![entry.clone()])]),
            checkpoints: BTreeMap::from([(key.clone(), checkpoint)]),
            incomplete_scopes: BTreeSet::from([(
                account.into(),
                "purchase".into(),
                "900002".into(),
            )]),
            ..Default::default()
        };
        let (authoritative, additive) = ready_product_entries(&publications, "900002");
        assert!(authoritative.is_empty() && additive.is_empty());
        publications.products.get_mut("900002").unwrap()[0].draft["publicMetadataState"] =
            json!("complete");
        publications.products.get_mut("900002").unwrap()[0].needs_enrichment = false;
        let (authoritative, additive) = ready_product_entries(&publications, "900002");
        assert!(authoritative.is_empty());
        assert_eq!(additive.len(), 1);
        service
            .commit_product_group_mode(
                &job,
                "900002",
                &additive,
                &mut publications.checkpoints,
                false,
            )
            .unwrap();
        assert_eq!(
            catalogue
                .get("acquisition", "authored-account:purchase:910002")
                .unwrap()
                .unwrap()["paidPrice"],
            900
        );
        assert!(catalogue
            .get("acquisition", "authored-account:purchase:910003")
            .unwrap()
            .is_some());
        assert!(catalogue
            .get("remote_file", "authored-account:700002:800002")
            .unwrap()
            .is_some());
        assert_eq!(
            catalogue
                .get("entitlement", "authored-account:purchase:900002")
                .unwrap()
                .unwrap()["manifestComplete"],
            false
        );
        assert!(catalogue
            .get("remote_file", "authored-account:700001:800001")
            .unwrap()
            .is_none());
        assert!(publications.checkpoints[&key].published_products.is_empty());
        let prior = additive_version(&additive).unwrap();
        let mut changed = additive[0].clone();
        changed
            .routes
            .insert("purchase:910002".into(), "/orders/910003".into());
        assert_ne!(prior, additive_version(&[changed.clone()]).unwrap());
        changed = additive[0].clone();
        changed.files.insert(format!("{account}:700003:800003"));
        assert_ne!(prior, additive_version(&[changed]).unwrap());
        drop(service);
        drop(catalogue);
        std::fs::remove_dir_all(path).unwrap();
    }
    #[test]
    fn free_history_witness_and_augmented_rows_preserve_original_acquisition_facts() {
        let page = free_history_page();
        let mut history = BTreeMap::new();
        remember_free_history(&page, &mut history);
        assert_eq!(history.len(), 1);
        assert_eq!(history["900001"].len(), 1);
        remember_free_history(&page, &mut history);
        assert_eq!(history["900001"].len(), 1);
        let mut row = page.rows[0].clone();
        let before = serde_json::to_value(&row).unwrap();
        let evidence = free_item::Evidence {
            product_id: "900001".into(),
            files: vec![parser::File {
                id: "700001".into(),
                variant_id: Some("800001".into()),
                name: Some("authored.zip".into()),
                size_bytes: None,
                source_url: "https://booth.pm/downloadables/700001?variation_id=800001".into(),
                revision: "unknown".into(),
            }],
            manifest_complete: true,
            unavailable: false,
            error_code: None,
        };
        augment_free_row(&mut row, &evidence);
        assert!(row.manifest_complete);
        assert_eq!(row.files.len(), 1);
        let after = serde_json::to_value(&row).unwrap();
        for (key, value) in before.as_object().unwrap() {
            if !matches!(key.as_str(), "files" | "manifestComplete") {
                assert_eq!(&after[key], value);
            }
        }
        augment_free_row(
            &mut row,
            &free_item::Evidence::unknown("900001", "FREE_FILE_CONTROLS_UNAVAILABLE", false),
        );
        assert!(!row.manifest_complete);
        assert_eq!(row.files.len(), 1);
        let mut mismatched = page.rows[0].clone();
        augment_free_row(
            &mut mismatched,
            &free_item::Evidence {
                product_id: "900002".into(),
                ..evidence
            },
        );
        assert!(mismatched.files.is_empty());
        let mut rejected = page.clone();
        rejected.rows[0].error_code = Some("SYNTHETIC_UNSUPPORTED".into());
        assert_eq!(eligible_free_rows(&rejected).count(), 0);
        let mut rejected = page.clone();
        rejected.rows[0].unavailable = true;
        assert_eq!(eligible_free_rows(&rejected).count(), 0);
        let mut rejected = page.clone();
        rejected.source = "library".into();
        assert_eq!(eligible_free_rows(&rejected).count(), 0);
        let mut rejected = page.clone();
        rejected.is_detail = true;
        assert_eq!(eligible_free_rows(&rejected).count(), 0);
    }
    #[test]
    fn current_free_checkpoint_roundtrips_and_does_not_upgrade_missing_version() {
        assert_eq!(FREE_ITEM_ADAPTER_VERSION, 3);
        let checkpoint = Checkpoint::fresh("synthetic", "free", "full").unwrap();
        assert_eq!(
            checkpoint.free_item_adapter_version,
            FREE_ITEM_ADAPTER_VERSION
        );
        let mut old = serde_json::to_value(&checkpoint).unwrap();
        old.as_object_mut()
            .unwrap()
            .remove("freeItemAdapterVersion");
        assert!(serde_json::from_value::<Checkpoint>(old).is_err());
        let mut legacy = serde_json::to_value(&checkpoint).unwrap();
        legacy["freeItemAdapterVersion"] = json!(1);
        let restored: Checkpoint = serde_json::from_value(legacy).unwrap();
        assert_eq!(restored.free_item_adapter_version, 1);
        assert_ne!(
            restored.free_item_adapter_version,
            FREE_ITEM_ADAPTER_VERSION
        );
        assert_eq!(
            Checkpoint::fresh("synthetic", "library", "full")
                .unwrap()
                .free_item_adapter_version,
            0
        );
        let mut old = serde_json::to_value(free_history_page()).unwrap();
        old.as_object_mut().unwrap().remove("freeItem");
        assert!(serde_json::from_value::<parser::Page>(old)
            .unwrap()
            .free_item
            .is_none());
        let pending = vec![
            safety::free_item_route("900001").unwrap(),
            "/library/free_downloads?page=2".into(),
        ];
        assert_eq!(
            next_route(&pending).unwrap(),
            "/library/free_downloads?page=2"
        );
    }
    #[test]
    fn free_history_journals_cannot_cross_account_source_or_current_run() {
        let journal = json!({"accountId":"synthetic-account","source":"free","runId":"synthetic-run","page":free_history_page()});
        assert!(
            checked_journal_page(&journal, "synthetic-account", "free", "synthetic-run").is_ok()
        );
        for (account, source, run) in [
            ("other-account", "free", "synthetic-run"),
            ("synthetic-account", "library", "synthetic-run"),
            ("synthetic-account", "free", "old-run"),
        ] {
            assert_eq!(
                checked_journal_page(&journal, account, source, run)
                    .unwrap_err()
                    .to_string(),
                "CHECKPOINT_INVALID"
            );
        }
    }
    #[test]
    fn checkpoint_completion_requires_terminal_and_all_rows() {
        let mut c = Checkpoint::fresh("synthetic", "free", "full").unwrap();
        assert!(!c.complete());
        c.pending.clear();
        c.terminal = true;
        assert!(c.complete());
        c.unresolved = 1;
        assert!(!c.complete());
        c.unresolved = 0;
        c.enrichment_pending.insert("900001".into());
        assert!(!c.complete());
    }
    #[test]
    fn source_queue_numeric_not_lexical() {
        assert_eq!(
            next_route(&vec![
                "/library?page=10".into(),
                "/library?page=2".into(),
                "/orders/1".into()
            ])
            .unwrap(),
            "/library?page=2"
        );
    }
    #[test]
    fn resume_overlap_guards_shifted_head_and_boundary() {
        let head = parser::Page {
            source: "library".into(),
            route: "/library?page=1".into(),
            rows: vec![],
            next: vec![],
            details: vec![],
            terminal: false,
            unresolved: 0,
            fingerprint: "head".into(),
            is_detail: false,
            ordering: parser::Ordering::Unknown,
            free_item: None,
        };
        let mut boundary = head.clone();
        boundary.route = "/library?page=2".into();
        boundary.fingerprint = "boundary".into();
        let mut c = Checkpoint::fresh("synthetic", "library", "full").unwrap();
        c.head = "head".into();
        c.boundary = Some(boundary.route.clone());
        c.committed
            .insert(boundary.route.clone(), boundary.fingerprint.clone());
        assert!(!overlap_changed(&c, &head, Some(&boundary)));
        assert!(overlap_changed(&c, &head, None));
        boundary.fingerprint = "shifted".into();
        assert!(overlap_changed(&c, &head, Some(&boundary)));
        c.head = "old-head".into();
        assert!(overlap_changed(&c, &head, Some(&boundary)));
    }
    #[test]
    fn revision_is_content_evidence_not_signed_url_or_clock() {
        let mut f = parser::File {
            id: "700001".into(),
            variant_id: None,
            name: Some("authored.zip".into()),
            size_bytes: Some(9),
            source_url: "https://booth.pm/downloadables/700001".into(),
            revision: "unknown".into(),
        };
        let old = json!({"sourceFileId":"700001","variantId":null,"name":"authored.zip","sizeBytes":9,"revision":"sha256:positive-content","revisionBasis":"contentHash"});
        assert_eq!(
            revision_observation(Some(&old), &f, false),
            (
                "sha256:positive-content".into(),
                "contentHash".into(),
                false
            )
        );
        assert!(revision_observation(Some(&old), &f, true).2);
        f.size_bytes = Some(10);
        assert_eq!(revision_observation(Some(&old), &f, false).0, "unknown");
        f.revision = "source:actual-provider-revision".into();
        assert!(!revision_observation(Some(&old), &f, true).2);
    }
}
