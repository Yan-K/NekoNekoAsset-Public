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
}
impl Baseline {
    pub fn usable(&self, account: &str, source: &str) -> bool {
        self.version == 1
            && self.account_id == account
            && self.source == source
            && self.complete
            && self.ordering != Ordering::Unknown
            && !self.rows.is_empty()
            && unique(&self.rows)
            && self.rows.iter().all(stable)
    }
}
fn stable(row: &Row) -> bool {
    !row.source_record_id.is_empty() && row.product_id.is_some() && row.error_code.is_none()
}
pub(super) fn unique(rows: &[Row]) -> bool {
    let mut ids = BTreeSet::new();
    rows.iter().all(|r| ids.insert(&r.source_record_id))
}
fn signature(row: &Row) -> String {
    safety::hash(
        serde_json::to_vec(&serde_json::json!({
            "id":row.source_record_id,"productId":row.product_id,"files":row.files,
            "manifestComplete":row.manifest_complete,"unavailable":row.unavailable,
            "errorCode":row.error_code
        }))
        .expect("digital row serialization"),
    )
}
pub(super) struct Tracker {
    ordering: Ordering,
    known: BTreeMap<String, (usize, String)>,
    seen: BTreeSet<String>,
    last_known: Option<usize>,
    matched: Option<usize>,
    matched_pages: usize,
    disabled: bool,
}
impl Tracker {
    pub fn new(base: &Baseline) -> Self {
        Self {
            ordering: base.ordering,
            known: base
                .rows
                .iter()
                .enumerate()
                .map(|(i, r)| (r.source_record_id.clone(), (i, signature(r))))
                .collect(),
            seen: BTreeSet::new(),
            last_known: None,
            matched: None,
            matched_pages: 0,
            disabled: !base.complete || base.ordering == Ordering::Unknown || !unique(&base.rows),
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
        for row in &page.rows {
            if !stable(row) || !self.seen.insert(row.source_record_id.clone()) {
                self.disabled = true;
                return None;
            }
            let Some((index, old_signature)) = self.known.get(&row.source_record_id) else {
                if self.last_known.is_some() {
                    self.disabled = true;
                    return None;
                }
                self.matched = None;
                self.matched_pages = 0;
                continue;
            };
            let index = *index;
            if self.last_known.is_some_and(|last| index <= last) {
                self.disabled = true;
                return None;
            }
            self.last_known = Some(index);
            if *old_signature != signature(row) {
                self.matched = None;
                self.matched_pages = 0;
                matched_this_page = false;
                continue;
            }
            if self.matched.is_none_or(|last| index != last + 1) {
                self.matched_pages = 0;
                matched_this_page = false;
            }
            self.matched = Some(index);
            if !matched_this_page {
                self.matched_pages += 1;
                matched_this_page = true;
            }
        }
        if page.rows.is_empty() {
            return None;
        }
        (self.matched_pages >= 2 && matched_this_page)
            .then_some(self.matched?)
            .filter(|_| !self.disabled)
    }
}
pub(super) fn merged_rows(base: &Baseline, prefix: &[Row], overlap_end: usize) -> Option<Vec<Row>> {
    if overlap_end >= base.rows.len() || !unique(prefix) {
        return None;
    }
    let mut rows = prefix.to_vec();
    rows.extend_from_slice(&base.rows[overlap_end + 1..]);
    unique(&rows).then_some(rows)
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
    fn moved_page_boundary_stops_by_stable_records_and_preserves_tail() {
        let b = base();
        let mut t = Tracker::new(&b);
        let first = page(vec![row(20000), row(20001), row(0)]);
        assert_eq!(t.observe(&first), None);
        let second = page(vec![row(1), row(2), row(3)]);
        assert_eq!(t.observe(&second), Some(3));
        let prefix = [first.rows, second.rows].concat();
        let all = merged_rows(&b, &prefix, 3).unwrap();
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
        let mut duplicate = b;
        duplicate.rows.push(row(0));
        assert!(!duplicate.usable("synthetic", "library"));
    }
}
