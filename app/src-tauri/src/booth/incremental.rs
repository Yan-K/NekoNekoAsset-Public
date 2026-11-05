//! Provider-ordered acquisition overlap. All stored facts are digital projections.
use super::{
    parser::{Ordering, Page, Row},
    safety,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Baseline {
    pub version: u32,
    pub account_id: String,
    pub source: String,
    pub complete: bool,
    pub ordering: Ordering,
    pub rows: Vec<Row>,
    pub full_traversal_at: String,
    /// Receipt routes this complete pass read, including receipts without digital rows.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub details: BTreeSet<String>,
    /// The rows that led the updated order last time, by `content`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub updated_head: Vec<String>,
}
impl Baseline {
    /// An incremental purchase scan still lists every order, but a receipt this
    /// complete pass already read supplies its rows instead of a new request.
    /// Orders are immutable; Full rereads every receipt.
    pub fn known_receipts(&self, account: &str, source: &str) -> BTreeMap<String, Vec<Row>> {
        let mut known: BTreeMap<String, Vec<Row>> = BTreeMap::new();
        if source != "purchase"
            || self.version != 1
            || self.account_id != account
            || self.source != source
            || !self.complete
            || !unique(&self.rows)
            || !self.rows.iter().all(stable)
        {
            return known;
        }
        for row in &self.rows {
            let Some(order) = row.order_id.as_deref().filter(|id| safety::numeric(id)) else {
                // A row that cannot name its receipt cannot be reused safely.
                return BTreeMap::new();
            };
            known.entry(format!("/orders/{order}")).or_default().push(row.clone());
        }
        for route in &self.details {
            if route.strip_prefix("/orders/").is_some_and(safety::numeric) {
                known.entry(route.clone()).or_default();
            }
        }
        known
    }
    pub fn usable(&self, account: &str, source: &str) -> bool {
        self.version == 1
            && self.account_id == account
            && self.source == source
            && self.complete
            && self.ordering == Ordering::AcquiredNewestFirst
            && !self.rows.is_empty()
            && self.rows.iter().all(|row| stable(row) || withheld(row))
    }
    /// What neither order brings to the top, like a delisted row or a removed file, waits for
    /// the next pass that reads every page. This keeps that wait short.
    pub fn recently_read_in_full(&self, now: chrono::DateTime<chrono::Utc>) -> bool {
        chrono::DateTime::parse_from_rfc3339(&self.full_traversal_at)
            .is_ok_and(|at| now.signed_duration_since(at) < chrono::Duration::days(FULL_PASS_DAYS))
    }
}
const FULL_PASS_DAYS: i64 = 7;
fn stable(row: &Row) -> bool {
    !row.source_record_id.is_empty() && row.product_id.is_some() && row.error_code.is_none()
}
/// A delisted row names no product, and its record ID moves with its place in the list.
fn withheld(row: &Row) -> bool {
    row.product_id.is_none() && row.error_code.as_deref() == Some("BOOTH_WITHHELD_PRODUCT_ID")
}
pub(super) fn unique(rows: &[Row]) -> bool {
    let mut ids = BTreeSet::new();
    rows.iter().all(|r| ids.insert(&r.source_record_id))
}
/// What a row says about its product. A record ID stays out: the same row takes an
/// `:occurrence:` suffix or not depending on which page it falls on.
fn content(row: &Row) -> String {
    safety::hash(
        serde_json::to_vec(&serde_json::json!({
            "productId":row.product_id,"files":row.files,
            "manifestComplete":row.manifest_complete,"unavailable":row.unavailable,
            "errorCode":row.error_code
        }))
        .expect("digital row serialization"),
    )
}
/// One stable row of the previous pass. `position` counts only such rows, so a delisted row
/// between two others does not read as a gap; `index` is its place in the baseline.
struct KnownRow {
    position: usize,
    index: usize,
    content: String,
}
pub(super) struct Tracker {
    ordering: Ordering,
    /// By product: a product bought more than once can list several rows.
    known: BTreeMap<String, Vec<KnownRow>>,
    read: BTreeMap<String, BTreeSet<String>>,
    last_known: Option<usize>,
    matched: Option<usize>,
    matched_index: Option<usize>,
    matched_pages: usize,
    disabled: bool,
}
impl Tracker {
    pub fn new(base: &Baseline) -> Self {
        let mut known: BTreeMap<String, Vec<KnownRow>> = BTreeMap::new();
        let rows = base.rows.iter().enumerate().filter(|(_, row)| !withheld(row));
        for (position, (index, row)) in rows.enumerate() {
            known
                .entry(row.product_id.clone().unwrap_or_default())
                .or_default()
                .push(KnownRow {
                    position,
                    index,
                    content: content(row),
                });
        }
        Self {
            ordering: base.ordering,
            known,
            read: BTreeMap::new(),
            last_known: None,
            matched: None,
            matched_index: None,
            matched_pages: 0,
            disabled: !base.complete || base.ordering == Ordering::Unknown,
        }
    }
    /// A changed known row is refreshed, then requires a new two-page overlap.
    /// Unknown rows after the old boundary or reversed old identities invalidate order.
    pub fn observe(&mut self, page: &Page) -> Option<usize> {
        if self.disabled {
            return None;
        }
        if page.is_detail
            || page.ordering != self.ordering
            || page.unresolved != 0
            || !page.details.is_empty()
        {
            self.disabled = true;
            return None;
        }
        let mut matched_this_page = false;
        for row in page.rows.iter().filter(|row| !withheld(row)) {
            let Some(product) = row.product_id.as_ref().filter(|_| stable(row)) else {
                self.disabled = true;
                return None;
            };
            let current = content(row);
            self.read
                .entry(product.clone())
                .or_default()
                .insert(current.clone());
            let Some(entries) = self.known.get(product) else {
                if self.last_known.is_some() {
                    self.disabled = true;
                    return None;
                }
                self.matched = None;
                self.matched_pages = 0;
                continue;
            };
            let last = self.last_known;
            let mut later = entries
                .iter()
                .filter(|entry| last.is_none_or(|last| entry.position > last));
            let same = later.clone().find(|entry| entry.content == current);
            let Some(entry) = same.or_else(|| later.next()) else {
                self.disabled = true;
                return None;
            };
            let (position, index, changed) =
                (entry.position, entry.index, entry.content != current);
            self.last_known = Some(position);
            if changed {
                self.matched = None;
                self.matched_pages = 0;
                matched_this_page = false;
                continue;
            }
            if self.matched.is_none_or(|last| position != last + 1) {
                self.matched_pages = 0;
                matched_this_page = false;
            }
            self.matched = Some(position);
            self.matched_index = Some(index);
            if !matched_this_page {
                self.matched_pages += 1;
                matched_this_page = true;
            }
        }
        if page.rows.is_empty() {
            return None;
        }
        (self.matched_pages >= 2 && matched_this_page)
            .then_some(self.matched_index?)
            .filter(|_| !self.disabled)
    }
    /// Stopping at the overlap leaves rows further down unread, and a seller can change the
    /// files on any of them. BOOTH's "recently updated first" order brings those to the top.
    pub fn updated(&self, head: &[String]) -> UpdatedTracker {
        UpdatedTracker {
            known: self
                .known
                .iter()
                .map(|(product, rows)| {
                    (product.clone(), rows.iter().map(|row| row.content.clone()).collect())
                })
                .collect(),
            read: self.read.clone(),
            head: head.to_vec(),
            last_head: None,
            head_run: 0,
            quiet_pages: 0,
            settled: false,
        }
    }
}
/// A page's worth of the last read's rows, in order, shows the read has passed everything
/// BOOTH moved since. The next pass keeps enough rows to find that after some have moved.
const HEAD_RUN: usize = 10;
const HEAD_KEPT: usize = 40;
#[derive(Debug, PartialEq)]
pub(super) enum UpdatedRead {
    Reading,
    Settled,
    /// Something this order should not show: the App reads every page instead.
    Broken,
}
pub(super) struct UpdatedTracker {
    known: BTreeMap<String, Vec<String>>,
    read: BTreeMap<String, BTreeSet<String>>,
    head: Vec<String>,
    last_head: Option<usize>,
    head_run: usize,
    quiet_pages: usize,
    settled: bool,
}
impl UpdatedTracker {
    /// Whether this row says what neither the last pass nor this pass's plain pages did.
    pub fn changes(&self, row: &Row) -> bool {
        let Some(product) = row.product_id.as_ref() else {
            return false;
        };
        let current = content(row);
        !self.known.get(product).is_some_and(|rows| rows.contains(&current))
            && !self.read.get(product).is_some_and(|rows| rows.contains(&current))
    }
    /// A row BOOTH moved sits above every row it did not, whether or not anything this account
    /// sees changed, so an unchanged row alone proves nothing. Reaching the rows that led this
    /// order last time does. Before any such read, two pages of the last pass's rows have to do.
    pub fn observe(&mut self, page: &Page) -> UpdatedRead {
        if page.is_detail
            || page.ordering != Ordering::FileUpdatedNewestFirst
            || page.unresolved != 0
            || !page.details.is_empty()
            || page.rows.is_empty()
        {
            return UpdatedRead::Broken;
        }
        let (mut quiet, mut rows) = (true, 0);
        for row in page.rows.iter().filter(|row| !withheld(row)) {
            let Some(product) = row.product_id.as_ref().filter(|_| stable(row)) else {
                return UpdatedRead::Broken;
            };
            rows += 1;
            let current = content(row);
            let known = self.known.get(product);
            let unchanged = known.is_some_and(|rows| rows.contains(&current));
            if !unchanged && !self.read.get(product).is_some_and(|rows| rows.contains(&current)) {
                // A product no list has shown was acquired out of order or just now, and among
                // several rows of one product, which one changed is unknown.
                if known.is_none_or(|rows| rows.len() != 1) {
                    return UpdatedRead::Broken;
                }
            }
            quiet &= unchanged;
            self.follow_head(&current, unchanged);
        }
        // A page of delisted rows says nothing either way.
        if rows > 0 {
            self.quiet_pages = if quiet { self.quiet_pages + 1 } else { 0 };
        }
        if self.settled || (self.head.is_empty() && self.quiet_pages >= 2) {
            UpdatedRead::Settled
        } else if page.terminal || !page.next.is_empty() {
            UpdatedRead::Reading
        } else {
            UpdatedRead::Broken
        }
    }
    fn follow_head(&mut self, current: &str, unchanged: bool) {
        let place = |from: usize| {
            self.head[from..]
                .iter()
                .position(|row| row == current)
                .map(|place| place + from)
                .filter(|_| unchanged)
        };
        if let Some(next) = self.last_head.and_then(|last| place(last + 1)) {
            self.last_head = Some(next);
            self.head_run += 1;
        } else {
            self.last_head = place(0);
            self.head_run = usize::from(self.last_head.is_some());
        }
        self.settled |= !self.head.is_empty() && self.head_run >= HEAD_RUN.min(self.head.len());
    }
}
/// What the next pass looks for: this read's rows, then the last read's that are below them.
pub(super) fn next_head(updated: &[Row], previous: &[String]) -> Vec<String> {
    let mut head: Vec<_> = updated
        .iter()
        .filter(|row| !withheld(row))
        .map(content)
        .collect();
    for row in previous {
        if !head.contains(row) {
            head.push(row.clone());
        }
    }
    head.truncate(HEAD_KEPT);
    head
}
pub(super) struct Merged {
    pub rows: Vec<Row>,
    /// Rows read in updated order that say something new.
    pub changed: Vec<Row>,
}
/// The rows read in acquired order, the unread tail of the previous pass after them, and each
/// changed row read in updated order in its product's place. `UpdatedTracker` lets a change
/// through only for a product with one row.
pub(super) fn merged_rows(
    base: &Baseline,
    prefix: &[Row],
    overlap_end: usize,
    updated: &[Row],
) -> Option<Merged> {
    if overlap_end >= base.rows.len() {
        return None;
    }
    let mut rows = prefix.to_vec();
    rows.extend(base.rows[overlap_end + 1..].iter().filter(|row| !withheld(row)).cloned());
    let mut places = BTreeMap::new();
    for (place, row) in rows.iter().enumerate() {
        if let Some(product) = &row.product_id {
            places.entry(product.clone()).or_insert(place);
        }
    }
    let known: BTreeSet<_> = base.rows.iter().chain(prefix).map(content).collect();
    let mut changed = vec![];
    for row in updated.iter().filter(|row| !withheld(row) && !known.contains(&content(row))) {
        let Some(product) = &row.product_id else {
            continue;
        };
        changed.push(row.clone());
        if let Some(place) = places.get(product) {
            rows[*place] = row.clone();
        } else {
            places.insert(product.clone(), rows.len());
            rows.push(row.clone());
        }
    }
    Some(Merged { rows, changed })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn row(id: usize) -> Row {
        Row {
            source_record_id: format!("synthetic:{id}"),
            product_id: Some(id.to_string()),
            manifest_complete: true,
            ..Row::default()
        }
    }
    fn base() -> Baseline {
        Baseline {
            version: 1,
            account_id: "synthetic".into(),
            source: "library".into(),
            complete: true,
            ordering: Ordering::AcquiredNewestFirst,
            rows: (0..10000).map(row).collect(),
            full_traversal_at: "previous-full-pass".into(),
            details: BTreeSet::new(),
            updated_head: vec![],
        }
    }
    fn page(rows: Vec<Row>) -> Page {
        Page {
            source: "library".into(),
            route: "/library?page=1".into(),
            rows,
            next: vec![],
            details: vec![],
            terminal: false,
            unresolved: 0,
            fingerprint: "synthetic".into(),
            is_detail: false,
            ordering: Ordering::AcquiredNewestFirst,
            free_item: None,
        }
    }
    #[test]
    fn known_receipts_come_only_from_a_complete_purchase_pass() {
        let receipt = |id: usize, order: &str| Row {
            order_id: Some(order.into()),
            ..row(id)
        };
        let mut pass = Baseline {
            version: 1,
            account_id: "synthetic".into(),
            source: "purchase".into(),
            complete: true,
            ordering: Ordering::Unknown,
            rows: vec![receipt(1, "910001"), receipt(2, "910001"), receipt(3, "910002")],
            full_traversal_at: "previous-full-pass".into(),
            details: BTreeSet::from(["/orders/910003".into()]),
            updated_head: vec![],
        };
        let known = pass.known_receipts("synthetic", "purchase");
        assert_eq!(known.keys().collect::<Vec<_>>(), ["/orders/910001", "/orders/910002", "/orders/910003"]);
        assert_eq!(known["/orders/910001"].len(), 2);
        assert!(known["/orders/910003"].is_empty());
        assert!(pass.known_receipts("other", "purchase").is_empty());
        pass.complete = false;
        assert!(pass.known_receipts("synthetic", "purchase").is_empty());
        pass.complete = true;
        pass.rows[0].error_code = Some("DIGITAL_CONTROLS_UNRESOLVED".into());
        assert!(pass.known_receipts("synthetic", "purchase").is_empty());
        pass.rows[0].error_code = None;
        pass.rows[2].order_id = None;
        assert!(pass.known_receipts("synthetic", "purchase").is_empty());
        let mut library = base();
        library.details = BTreeSet::from(["/orders/910003".into()]);
        assert!(library.known_receipts("synthetic", "library").is_empty());
        // Baselines saved before receipt routes were recorded still load.
        let mut saved = serde_json::to_value(base()).unwrap();
        saved.as_object_mut().unwrap().remove("details");
        assert!(serde_json::from_value::<Baseline>(saved).unwrap().details.is_empty());
    }
    #[test]
    fn moved_page_boundary_stops_by_stable_records_and_preserves_tail() {
        let b = base();
        let mut t = Tracker::new(&b);
        let first = page(vec![row(20000), row(20001), row(0)]);
        assert_eq!(t.observe(&first), None);
        let second = page(vec![row(1), row(2), row(3)]);
        assert_eq!(t.observe(&second), Some(3));
        let prefix = [first.rows, second.rows].concat();
        let all = merged_rows(&b, &prefix, 3, &[]).unwrap().rows;
        assert_eq!(all.len(), 10002);
        assert_eq!(all.last().unwrap().source_record_id, "synthetic:9999");
    }
    #[test]
    fn arbitrarily_long_new_prefix_has_no_fixed_page_stop() {
        let mut t = Tracker::new(&base());
        for i in 0..150 {
            assert_eq!(t.observe(&page(vec![row(20000 + i)])), None);
        }
        assert_eq!(t.observe(&page(vec![row(0), row(1)])), None);
        assert_eq!(t.observe(&page(vec![row(2), row(3)])), Some(3));
    }
    #[test]
    fn changed_known_file_resets_two_page_overlap() {
        let mut t = Tracker::new(&base());
        assert_eq!(t.observe(&page(vec![row(0), row(1)])), None);
        let mut changed = row(2);
        changed.files.push(super::super::parser::File {
            id: "700001".into(),
            variant_id: Some("800001".into()),
            name: Some("new-file.zip".into()),
            size_bytes: Some(10),
            source_url: "https://booth.pm/downloadables/700001".into(),
            revision: "source:changed".into(),
            format: None,
        });
        assert_eq!(t.observe(&page(vec![changed, row(3)])), None);
        assert_eq!(t.observe(&page(vec![row(4), row(5)])), Some(5));
    }
    #[test]
    fn unknown_order_partial_baseline_and_interleaving_never_shortcut() {
        let mut b = base();
        b.complete = false;
        assert!(!b.usable("synthetic", "library"));
        assert_eq!(Tracker::new(&b).observe(&page(vec![row(0)])), None);
        b.complete = true;
        for rows in [
            vec![row(1), row(0)],
            vec![row(0), row(20000)],
            vec![row(0), row(0)],
        ] {
            let mut t = Tracker::new(&b);
            assert_eq!(t.observe(&page(rows)), None);
            assert_eq!(t.observe(&page(vec![row(2), row(3)])), None);
        }
        let mut t = Tracker::new(&b);
        let mut p = page(vec![row(0)]);
        p.ordering = Ordering::Unknown;
        assert_eq!(t.observe(&p), None);
        assert_eq!(t.observe(&page(vec![row(1), row(2)])), None);
    }
    #[test]
    fn resumed_prefix_reconstructs_proof_without_page_number_assumptions() {
        let b = base();
        let prefix = page(vec![row(20000), row(0)]);
        let mut resumed = Tracker::new(&b);
        assert_eq!(resumed.observe(&prefix), None);
        assert_eq!(resumed.observe(&page(vec![row(1), row(2)])), Some(2));
    }
    fn updated(rows: Vec<Row>) -> Page {
        Page {
            ordering: Ordering::FileUpdatedNewestFirst,
            next: vec![safety::updated_route(9)],
            ..page(rows)
        }
    }
    fn with_file(id: usize, file: &str) -> Row {
        let mut changed = row(id);
        changed.files.push(super::super::parser::File {
            id: file.into(),
            variant_id: None,
            name: Some("new-file.zip".into()),
            size_bytes: None,
            source_url: format!("https://booth.pm/downloadables/{file}"),
            revision: "unknown".into(),
            format: None,
        });
        changed
    }
    fn overlap(base: &Baseline, pages: Vec<Vec<Row>>) -> Tracker {
        let mut tracker = Tracker::new(base);
        let last = pages.len() - 1;
        for (index, rows) in pages.into_iter().enumerate() {
            let proof = tracker.observe(&page(rows));
            assert_eq!(proof.is_some(), index == last);
        }
        tracker
    }
    #[test]
    fn a_product_listed_twice_is_matched_by_position() {
        // Two rows of one product, whatever page each falls on.
        let mut b = base();
        b.rows[5] = with_file(1, "700001");
        assert!(b.usable("synthetic", "library"));
        let same_order = vec![row(0), row(1), row(2), row(3), row(4), with_file(1, "700001"), row(6)];
        assert_eq!(Tracker::new(&b).observe(&page(same_order[..4].to_vec())), None);
        let mut t = Tracker::new(&b);
        assert_eq!(t.observe(&page(same_order[..3].to_vec())), None);
        assert_eq!(t.observe(&page(same_order[3..].to_vec())), Some(6));
        // A row with one place cannot appear twice.
        let mut t = Tracker::new(&b);
        assert_eq!(t.observe(&page(vec![row(0), row(0)])), None);
        assert_eq!(t.observe(&page(vec![row(1), row(2)])), None);
        // An earlier place never answers for a later row.
        let mut t = Tracker::new(&b);
        assert_eq!(t.observe(&page(vec![with_file(1, "700001"), row(1)])), None);
        assert_eq!(t.observe(&page(vec![row(6), row(7)])), None);
        // The suffix a row takes from sharing a page says nothing about the row.
        let mut split = row(1);
        split.source_record_id = "synthetic:1:occurrence:1".into();
        let mut t = Tracker::new(&base());
        assert_eq!(t.observe(&page(vec![row(0), split])), None);
        assert_eq!(t.observe(&page(vec![row(2)])), Some(2));
    }
    #[test]
    fn a_delisted_row_neither_blocks_the_overlap_nor_leaves_a_gap() {
        let delisted = |route: &str| Row {
            source_record_id: format!("library:{route}"),
            unavailable: true,
            error_code: Some("BOOTH_WITHHELD_PRODUCT_ID".into()),
            ..Row::default()
        };
        let mut b = base();
        b.rows[2] = delisted("old-place");
        b.rows[5] = delisted("other-old-place");
        assert!(b.usable("synthetic", "library"));
        let mut t = Tracker::new(&b);
        assert_eq!(t.observe(&page(vec![row(0), row(1), delisted("new-place")])), None);
        assert_eq!(t.observe(&page(vec![row(3), row(4)])), Some(4));
        // It stays out of what is carried forward, or it would be there again next time.
        let prefix = [row(0), row(1), delisted("new-place"), row(3), row(4)];
        let all = merged_rows(&b, &prefix, 4, &[]).unwrap().rows;
        assert_eq!(all.iter().filter(|row| withheld(row)).count(), 1);
        let mut other = base();
        other.rows[2].error_code = Some("PRODUCT_ID_UNRESOLVED".into());
        assert!(!other.usable("synthetic", "library"));
    }
    #[test]
    fn updated_order_settles_after_two_quiet_pages_and_keeps_changes() {
        let b = base();
        // A new purchase, then the overlap; the updated order shows the rest.
        let t = overlap(&b, vec![vec![row(20000), row(0)], vec![row(1), row(2)]]);
        let mut u = t.updated(&[]);
        assert!(u.changes(&with_file(9000, "700001")));
        assert!(!u.changes(&row(20000)) && !u.changes(&row(9000)));
        // A new file on a row far down the list, and the new purchase, then two quiet pages.
        assert_eq!(u.observe(&updated(vec![with_file(9000, "700001"), row(20000)])), UpdatedRead::Reading);
        assert_eq!(u.observe(&updated(vec![row(5000), row(5001)])), UpdatedRead::Reading);
        assert_eq!(u.observe(&updated(vec![row(4000), row(3000)])), UpdatedRead::Settled);
        let prefix = [row(20000), row(0), row(1), row(2)];
        let read = [with_file(9000, "700001"), row(20000), row(5000)];
        let merged = merged_rows(&b, &prefix, 2, &read).unwrap();
        assert_eq!(merged.changed.len(), 1);
        let all = merged.rows;
        assert_eq!(all.len(), 10001);
        assert_eq!(all[0].source_record_id, "synthetic:20000");
        assert_eq!(all.iter().filter(|r| r.source_record_id == "synthetic:9000").count(), 1);
        let place = all.iter().position(|r| r.source_record_id == "synthetic:9000").unwrap();
        assert_eq!(all[place].files.len(), 1);
        assert_eq!(place, 9000 + 1);
        // A change resets the count, a new purchase does not count, and neither does a page
        // of delisted rows, which says nothing.
        let mut u = t.updated(&[]);
        assert_eq!(u.observe(&updated(vec![row(7000)])), UpdatedRead::Reading);
        assert_eq!(u.observe(&updated(vec![with_file(7001, "700002")])), UpdatedRead::Reading);
        assert_eq!(u.observe(&updated(vec![row(7002)])), UpdatedRead::Reading);
        assert_eq!(u.observe(&updated(vec![row(20000)])), UpdatedRead::Reading);
        assert_eq!(u.observe(&updated(vec![row(7003)])), UpdatedRead::Reading);
        let delisted = Row {
            source_record_id: "library:delisted".into(),
            error_code: Some("BOOTH_WITHHELD_PRODUCT_ID".into()),
            ..Row::default()
        };
        assert_eq!(u.observe(&updated(vec![delisted])), UpdatedRead::Reading);
        assert_eq!(u.observe(&updated(vec![row(7004)])), UpdatedRead::Settled);
    }
    #[test]
    fn updated_order_reads_past_rows_moved_without_a_visible_change() {
        let b = base();
        let t = overlap(&b, vec![vec![row(0)], vec![row(1)]]);
        let head: Vec<_> = (100..140).map(|id| content(&row(id))).collect();
        let leading = |from: usize, to: usize| (from..to).map(row).collect::<Vec<_>>();
        // Thirty rows BOOTH moved show nothing new here, then a real change, then the old order.
        let mut u = t.updated(&head);
        for start in [500, 510, 520] {
            assert_eq!(u.observe(&updated(leading(start, start + 10))), UpdatedRead::Reading, "{start}");
        }
        assert_eq!(u.observe(&updated(vec![with_file(9000, "700001"), row(100), row(101)])), UpdatedRead::Reading);
        // A row of the old order that moved is simply absent from its place.
        assert_eq!(u.observe(&updated(vec![row(102), row(103), row(105), row(106), row(107), row(108), row(109)])), UpdatedRead::Reading);
        assert_eq!(u.observe(&updated(vec![row(110)])), UpdatedRead::Settled);
        // Rows out of the old order start the count again.
        let mut u = t.updated(&head);
        let mut scrambled = leading(100, 105);
        scrambled.reverse();
        assert_eq!(u.observe(&updated(scrambled)), UpdatedRead::Reading);
        assert_eq!(u.observe(&updated(leading(105, 113))), UpdatedRead::Reading);
        assert_eq!(u.observe(&updated(leading(113, 115))), UpdatedRead::Settled);
        // A row of the old order with new files is a change, not a witness.
        let mut u = t.updated(&head);
        assert_eq!(u.observe(&updated(leading(100, 109))), UpdatedRead::Reading);
        assert_eq!(u.observe(&updated(vec![with_file(109, "700002")])), UpdatedRead::Reading);
        assert_eq!(u.observe(&updated(leading(110, 119))), UpdatedRead::Reading);
        assert_eq!(u.observe(&updated(leading(119, 120))), UpdatedRead::Settled);
        // The next pass looks for what was read now, then what fell below it, within a bound.
        let read = leading(500, 503);
        let next = next_head(&read, &head);
        assert_eq!(next[..3], [content(&read[0]), content(&read[1]), content(&read[2])]);
        assert_eq!(next[3], head[0]);
        assert_eq!(next.len(), HEAD_KEPT);
        assert_eq!(next_head(&[], &head), head);
    }
    #[test]
    fn updated_order_gives_up_when_anything_does_not_fit() {
        let b = base();
        let t = overlap(&b, vec![vec![row(0)], vec![row(1)]]);
        let unknown = vec![row(20000)];
        let mut unstated = updated(vec![row(5)]);
        unstated.ordering = Ordering::AcquiredNewestFirst;
        let mut unresolved = updated(vec![row(5)]);
        unresolved.unresolved = 1;
        let mut unstable = row(5);
        unstable.error_code = Some("PRODUCT_TITLE_UNRESOLVED".into());
        let mut dead_end = updated(vec![row(5)]);
        dead_end.next.clear();
        for bad in [updated(unknown), unstated, unresolved, updated(vec![unstable]), updated(vec![]), dead_end] {
            assert_eq!(t.updated(&[]).observe(&bad), UpdatedRead::Broken);
        }
        // The list's own end is not a dead end, and a page that settles needs no next page.
        let mut last = updated(vec![row(5)]);
        last.next.clear();
        last.terminal = true;
        assert_eq!(t.updated(&[]).observe(&last), UpdatedRead::Reading);
        let head = [content(&row(5))];
        let mut settling = updated(vec![row(5)]);
        settling.next.clear();
        assert_eq!(t.updated(&head).observe(&settling), UpdatedRead::Settled);
        // Two different rows of one product: a new file cannot be pinned on either.
        let mut twice = base();
        twice.rows[5] = with_file(1, "700001");
        let t = overlap(&twice, vec![vec![row(0)], vec![row(1)]]);
        assert_eq!(t.updated(&[]).observe(&updated(vec![with_file(1, "700002")])), UpdatedRead::Broken);
        assert_eq!(t.updated(&[]).observe(&updated(vec![with_file(1, "700001")])), UpdatedRead::Reading);
        // Nor on either of two identical rows.
        let mut same = base();
        same.rows[5] = row(1);
        let t = overlap(&same, vec![vec![row(0)], vec![row(1)]]);
        assert_eq!(t.updated(&[]).observe(&updated(vec![with_file(1, "700002")])), UpdatedRead::Broken);
    }
    #[test]
    fn a_row_keeps_its_files_whichever_record_id_its_page_gives_it() {
        // A product bought twice with different files, listed together in one order and apart
        // in the other, so their record IDs differ between the two.
        let mut b = base();
        let mut second = with_file(1, "700009");
        second.source_record_id = "synthetic:1:occurrence:1".into();
        b.rows[2] = second;
        let first = with_file(1, "700001");
        b.rows[1] = first.clone();
        let t = overlap(&b, vec![vec![row(0)], vec![first.clone()]]);
        let mut apart = with_file(1, "700009");
        apart.source_record_id = "synthetic:1".into();
        let mut u = t.updated(&[]);
        assert_eq!(u.observe(&updated(vec![first.clone()])), UpdatedRead::Reading);
        assert_eq!(u.observe(&updated(vec![apart.clone()])), UpdatedRead::Settled);
        let merged = merged_rows(&b, &[row(0), first], 1, &[apart]).unwrap();
        assert!(merged.changed.is_empty());
        let names: Vec<_> = merged.rows[..3].iter().map(|row| row.files.first().map(|file| file.id.clone())).collect();
        assert_eq!(names, [None, Some("700001".to_owned()), Some("700009".to_owned())]);
    }
    #[test]
    fn only_an_order_the_page_stated_and_a_recent_full_read_allow_stopping() {
        let mut b = base();
        for ordering in [Ordering::Unknown, Ordering::FileUpdatedNewestFirst, Ordering::DownloadedNewestFirst] {
            b.ordering = ordering;
            assert!(!b.usable("synthetic", "library"), "{ordering:?}");
        }
        let now = chrono::DateTime::parse_from_rfc3339("2026-11-05T12:00:00+00:00")
            .unwrap()
            .with_timezone(&chrono::Utc);
        for (at, recent) in [
            ("2026-11-05T11:00:00+00:00", true),
            ("2026-10-30T12:00:01+00:00", true),
            ("2026-10-29T12:00:00+00:00", false),
            ("previous-full-pass", false),
            ("", false),
        ] {
            b.full_traversal_at = at.into();
            assert_eq!(b.recently_read_in_full(now), recent, "{at}");
        }
    }
}
