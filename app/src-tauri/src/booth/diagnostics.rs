//! Closed structural counts only. This does not change or certify source ordering.
use scraper::{ElementRef, Html, Selector};
use serde::Serialize;

const ACQUIRED: &str = "入手が新しい順";
const UPDATED: &str = "ファイル更新が新しい順";
/// Independent failure counters overlap: one row may fail several gates.
/// Every value is a fixed number/boolean; source values stay inside the parser.
#[derive(Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ManifestDiagnostics {
    version: u32,
    rows: usize,
    gate_evaluated_rows: usize,
    rows_complete: usize,
    rows_missing_explicit_list: usize,
    rows_without_files: usize,
    rows_with_error: usize,
    rows_unavailable: usize,
    rows_invalid_controls: usize,
    rows_missing_filename: usize,
    raw_controls: usize,
    valid_controls: usize,
    duplicate_controls: usize,
    invalid_controls: usize,
    files: usize,
    files_with_variant: usize,
    files_named: usize,
    files_unnamed: usize,
    files_size_known: usize,
    files_positive_revision: usize,
    unresolved_rows: usize,
    detail_routes: usize,
    next_routes: usize,
    terminal_seen: bool,
}
impl ManifestDiagnostics {
    pub(super) fn record(
        &mut self,
        explicit_list: bool,
        files: &[super::parser::File],
        error_free: bool,
        unavailable: bool,
        raw: usize,
        valid: usize,
        complete: bool,
    ) {
        self.gate_evaluated_rows += 1;
        self.rows_complete += usize::from(complete);
        self.rows_missing_explicit_list += usize::from(!explicit_list);
        self.rows_without_files += usize::from(files.is_empty());
        self.rows_with_error += usize::from(!error_free);
        self.rows_unavailable += usize::from(unavailable);
        self.rows_invalid_controls += usize::from(raw != valid);
        self.rows_missing_filename += usize::from(files.iter().any(|f| f.name.is_none()));
        self.raw_controls += raw;
        self.valid_controls += valid;
        self.duplicate_controls += valid.saturating_sub(files.len());
        self.invalid_controls += raw.saturating_sub(valid);
        self.files += files.len();
        self.files_with_variant += files.iter().filter(|f| f.variant_id.is_some()).count();
        self.files_named += files.iter().filter(|f| f.name.is_some()).count();
        self.files_unnamed += files.iter().filter(|f| f.name.is_none()).count();
        self.files_size_known += files.iter().filter(|f| f.size_bytes.is_some()).count();
        self.files_positive_revision += files
            .iter()
            .filter(|f| !f.revision.is_empty() && f.revision != "unknown")
            .count();
    }
    pub(super) fn finish(&mut self, page: &super::parser::Page) {
        self.version = 1;
        self.rows = page.rows.len();
        self.unresolved_rows = page.unresolved;
        self.detail_routes = page.details.len();
        self.next_routes = page.next.len();
        self.terminal_seen = page.terminal;
    }
}
fn sel(value: &str) -> Selector {
    Selector::parse(value).expect("fixed selector")
}
fn text(node: ElementRef<'_>) -> String {
    super::safety::short(&node.text().collect::<String>())
}
#[derive(Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct LabelCounts {
    acquired_exact: u32,
    acquired_containing_only: u32,
    file_updated_exact: u32,
    file_updated_containing_only: u32,
}
impl LabelCounts {
    fn add(&mut self, label: &str) {
        if label == ACQUIRED {
            self.acquired_exact += 1;
        } else if label.contains(ACQUIRED) {
            self.acquired_containing_only += 1;
        }
        if label == UPDATED {
            self.file_updated_exact += 1;
        } else if label.contains(UPDATED) {
            self.file_updated_containing_only += 1;
        }
    }
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
enum Language {
    Ja,
    En,
    Ko,
    ZhHans,
    ZhHant,
    Unknown,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct OrderingDiagnostics {
    version: u32,
    language: Language,
    search_value_nonempty: bool,
    selects: LabelCounts,
    options: LabelCounts,
    buttons: LabelCounts,
    links: LabelCounts,
    radios: LabelCounts,
    selected_attribute: LabelCounts,
    aria_pressed: LabelCounts,
    aria_selected: LabelCounts,
    aria_current: LabelCounts,
    aria_expanded_present: LabelCounts,
    radio_checked: LabelCounts,
    implicit_single_select_first: LabelCounts,
}
pub(super) fn inspect(body: &str) -> OrderingDiagnostics {
    let doc = Html::parse_document(body);
    let language = match doc
        .select(&sel("html"))
        .next()
        .and_then(|e| e.value().attr("lang"))
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "ja" | "ja-jp" => Language::Ja,
        "en" | "en-us" | "en-gb" => Language::En,
        "ko" | "ko-kr" => Language::Ko,
        "zh-hans" | "zh-cn" => Language::ZhHans,
        "zh-hant" | "zh-tw" | "zh-hk" => Language::ZhHant,
        _ => Language::Unknown,
    };
    let mut result = OrderingDiagnostics {
        version: 1,
        language,
        search_value_nonempty: doc
            .select(&sel(
                "input[type='search'][value],input[name='q'][value],input[name='keyword'][value]",
            ))
            .any(|e| {
                e.value()
                    .attr("value")
                    .is_some_and(|v| !v.trim().is_empty())
            }),
        selects: Default::default(),
        options: Default::default(),
        buttons: Default::default(),
        links: Default::default(),
        radios: Default::default(),
        selected_attribute: Default::default(),
        aria_pressed: Default::default(),
        aria_selected: Default::default(),
        aria_current: Default::default(),
        aria_expanded_present: Default::default(),
        radio_checked: Default::default(),
        implicit_single_select_first: Default::default(),
    };
    for node in doc.select(&sel("select,option,button,a,input[type='radio'],[aria-pressed],[aria-selected],[aria-current],[aria-expanded]")) {
        let is_radio = node.value().name() == "input"
            && node.value().attr("type").is_some_and(|v| v.eq_ignore_ascii_case("radio"));
        let label = if is_radio {
            // Only a native radio's associated label is examined; no attribute
            // values or text are ever returned, even for unknown controls.
            node.ancestors().filter_map(ElementRef::wrap).find(|p| p.value().name() == "label").map(text)
                .or_else(|| node.value().attr("id").and_then(|id| doc.select(&sel("label[for]")).find(|l| l.value().attr("for") == Some(id)).map(text)))
                .unwrap_or_default()
        } else { text(node) };
        match node.value().name() {
            "select" => result.selects.add(&label),
            "option" => result.options.add(&label),
            "button" => result.buttons.add(&label),
            "a" => result.links.add(&label),
            "input" if is_radio => { result.radios.add(&label); if node.value().attr("checked").is_some() { result.radio_checked.add(&label); } },
            _ => (),
        }
        if node.value().name() == "option" && node.value().attr("selected").is_some() { result.selected_attribute.add(&label); }
        if node.value().attr("aria-pressed") == Some("true") { result.aria_pressed.add(&label); }
        if node.value().attr("aria-selected") == Some("true") { result.aria_selected.add(&label); }
        if node.value().attr("aria-current").is_some_and(|v| !v.is_empty() && v != "false") { result.aria_current.add(&label); }
        if node.value().attr("aria-expanded").is_some() { result.aria_expanded_present.add(&label); }
        if node.value().name() == "select"
            && node.value().attr("multiple").is_none()
            && node.value().attr("disabled").is_none()
            && node.value().attr("size").is_none_or(|v| v == "1")
            && node.select(&sel("option[selected]")).next().is_none()
        {
            if let Some(first) = node.select(&sel("option")).next() {
                let disabled_group = first.ancestors().filter_map(ElementRef::wrap).take_while(|p| p.id() != node.id()).any(|p| p.value().name() == "optgroup" && p.value().attr("disabled").is_some());
                if first.value().attr("disabled").is_none() && !disabled_group { result.implicit_single_select_first.add(&text(first)); }
            }
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    const MANIFEST_HTML: &str = r#"<main><h1>Library</h1><article data-test='library-item'><a href='https://synthetic.booth.pm/items/900001'>PRIVATE_TITLE</a><div data-test='downloadable-list'><div><b data-test='filename'>PRIVATE_FILE.psd</b><a data-test='downloadable' href='https://booth.pm/downloadables/700001?variation_id=800001'>Download</a></div></div></article><nav class='pagination'><a aria-current='page' href='/library?page=1'>1</a></nav></main>"#;
    fn manifest_value(html: &str) -> serde_json::Value {
        serde_json::to_value(
            super::super::parser::diagnose_manifests(html, "library", "/library?page=1").unwrap(),
        )
        .unwrap()
    }
    #[test]
    fn manifest_diagnostic_counts_exact_parser_gates_without_source_values() {
        let value = manifest_value(MANIFEST_HTML);
        assert_eq!(value["rows"], 1);
        assert_eq!(value["rowsComplete"], 1);
        assert_eq!(value["filesWithVariant"], 1);
        assert_eq!(value["filesNamed"], 1);
        assert_eq!(value["filesSizeKnown"], 0);
        assert_eq!(value["filesPositiveRevision"], 0);
        assert_eq!(value["terminalSeen"], true);
        let allowed = [
            "version",
            "rows",
            "gateEvaluatedRows",
            "rowsComplete",
            "rowsMissingExplicitList",
            "rowsWithoutFiles",
            "rowsWithError",
            "rowsUnavailable",
            "rowsInvalidControls",
            "rowsMissingFilename",
            "rawControls",
            "validControls",
            "duplicateControls",
            "invalidControls",
            "files",
            "filesWithVariant",
            "filesNamed",
            "filesUnnamed",
            "filesSizeKnown",
            "filesPositiveRevision",
            "unresolvedRows",
            "detailRoutes",
            "nextRoutes",
            "terminalSeen",
        ];
        assert_eq!(value.as_object().unwrap().len(), allowed.len());
        for (key, count) in value.as_object().unwrap() {
            assert!(allowed.contains(&key.as_str()));
            assert!(if key == "terminalSeen" {
                count.is_boolean()
            } else {
                count.as_u64().is_some()
            });
        }
        let encoded = value.to_string();
        for private in ["PRIVATE", "900001", "700001", "800001", "http", "synthetic"] {
            assert!(!encoded.contains(private));
        }
        let positive = manifest_value(
            &MANIFEST_HTML
                .replace(
                    "data-test='filename'",
                    "data-test='filename' data-size-bytes='128'",
                )
                .replace(
                    "data-test='downloadable'",
                    "data-test='downloadable' data-revision='synthetic-v1'",
                ),
        );
        assert_eq!(positive["filesSizeKnown"], 1);
        assert_eq!(positive["filesPositiveRevision"], 1);
        for (html, gate) in [
            (
                MANIFEST_HTML.replace("data-test='downloadable-list'", ""),
                "rowsMissingExplicitList",
            ),
            (
                MANIFEST_HTML.replace("<b data-test='filename'>PRIVATE_FILE.psd</b>", ""),
                "rowsMissingFilename",
            ),
            (
                MANIFEST_HTML.replace(
                    "<a data-test='downloadable'",
                    "<i data-test='downloadable'></i><a data-test='downloadable'",
                ),
                "rowsInvalidControls",
            ),
            (
                MANIFEST_HTML.replace(
                    "<article data-test='library-item'>",
                    "<article data-test='library-item'><i data-test='item-unavailable'></i>",
                ),
                "rowsUnavailable",
            ),
            (
                MANIFEST_HTML.replace(
                    "https://synthetic.booth.pm/items/900001",
                    "/unknown-synthetic-route",
                ),
                "rowsWithError",
            ),
            (
                MANIFEST_HTML.replace(
                    "https://booth.pm/downloadables/700001?variation_id=800001",
                    "/unknown-synthetic-download",
                ),
                "rowsWithoutFiles",
            ),
        ] {
            let diagnostic = manifest_value(&html);
            let page = super::super::parser::parse(&html, "library", "/library?page=1").unwrap();
            assert_eq!(diagnostic[gate], 1, "{gate}");
            assert_eq!(diagnostic["rowsComplete"], 0, "{gate}");
            assert!(!page.rows[0].manifest_complete, "{gate}");
        }
    }
    #[test]
    fn duplicate_controls_are_distinct_from_invalid_controls() {
        let html = MANIFEST_HTML.replace("</div></div></article>", "<a href='https://booth.pm/downloadables/700001?variation_id=800001'>PRIVATE_AGAIN</a></div></div></article>");
        let value = manifest_value(&html);
        assert_eq!(value["rawControls"], 2);
        assert_eq!(value["validControls"], 2);
        assert_eq!(value["duplicateControls"], 1);
        assert_eq!(value["invalidControls"], 0);
        assert_eq!(value["files"], 1);
        assert_eq!(value["rowsComplete"], 1);
    }
    fn structure_value(html: &str) -> serde_json::Value {
        serde_json::to_value(
            super::super::parser::diagnose_file_structure(html, "library", "/library?page=1")
                .unwrap(),
        )
        .unwrap()
    }
    fn modern_structure() -> String {
        MANIFEST_HTML.replace("data-test='downloadable-list'", "class='synthetic-file-container'").replace("<b data-test='filename'>PRIVATE_FILE.psd</b>", "<div class='min-w-0 break-words whitespace-pre-line'><div class='text-14'>PRIVATE_FILE.psd</div></div>")
    }
    #[test]
    fn file_structure_probe_requires_recognized_entries_and_proper_file_only_container() {
        let html = modern_structure();
        let value = structure_value(&html);
        assert_eq!(value["modernNameControls"], 1);
        assert_eq!(value["recognizedEntries"], 1);
        assert_eq!(value["rowsAllEntriesRecognized"], 1);
        assert_eq!(value["rowsFileOnlyContainer"], 1);
        // The authorized production witness matches this proven modern structure.
        assert!(
            super::super::parser::parse(&html, "library", "/library?page=1")
                .unwrap()
                .rows[0]
                .manifest_complete
        );
        let card_only = html
            .replace("<div class='synthetic-file-container'>", "")
            .replace("</div></div></article>", "</div></article>");
        assert_eq!(structure_value(&card_only)["rowsFileOnlyContainer"], 0);
        assert_eq!(structure_value(&card_only)["rowsCardOnly"], 1);
        let shared=html.replace("<div class='synthetic-file-container'>","<div class='synthetic-file-container'><a href='https://synthetic.booth.pm/items/900002'>PRIVATE_SECOND</a>");
        assert_eq!(structure_value(&shared)["rowsFileOnlyContainer"], 0);
        let ambiguous = html.replace(
            "PRIVATE_FILE.psd</div>",
            "PRIVATE_FILE.psd</div><div class='text-14'>PRIVATE_OTHER.zip</div>",
        );
        assert_eq!(structure_value(&ambiguous)["rowsAllEntriesRecognized"], 0);
        assert_eq!(structure_value(&ambiguous)["unrecognizedEntryControls"], 1);
        let encoded = value.to_string();
        for private in ["PRIVATE", "synthetic", "http", "900001", "700001", "800001"] {
            assert!(!encoded.contains(private));
        }
        let allowed = [
            "version",
            "rows",
            "validControls",
            "modernNameControls",
            "testNameControls",
            "legacyNameControls",
            "trackNameControls",
            "unrecognizedEntryControls",
            "recognizedEntries",
            "rowsAllEntriesRecognized",
            "rowsFileOnlyContainer",
            "rowsCardOnly",
            "orphanModernNames",
            "rowsWithOrphanModernName",
            "rowsConflictingEntryScopes",
            "rowsOtherDownloadMethods",
            "rowsExpandOrLoadMore",
            "rowsVariantSelector",
            "rowsUnknownDropdown",
            "rowsAdvertisedCount",
            "rowsAdvertisedCountMatches",
            "rowsAdvertisedCountMismatch",
            "rowsAdvertisedCountInvalid",
        ];
        assert_eq!(value.as_object().unwrap().len(), allowed.len());
        assert!(value
            .as_object()
            .unwrap()
            .iter()
            .all(|(key, value)| allowed.contains(&key.as_str()) && value.as_u64().is_some()));
    }
    #[test]
    fn file_structure_probe_distinguishes_alternate_methods_from_incomplete_list_hints() {
        let html = modern_structure();
        let methods=html.replace("</article>","<button data-test='other-downloads-button' aria-expanded='false' data-dropdown-items='PRIVATE_METHODS'>PRIVATE_METHOD</button></article>");
        let value = structure_value(&methods);
        assert_eq!(value["rowsOtherDownloadMethods"], 1);
        assert_eq!(value["rowsExpandOrLoadMore"], 0);
        assert_eq!(value["rowsUnknownDropdown"], 0);
        let limited=html.replace("<article data-test='library-item'>","<article data-test='library-item' data-file-count='2'>").replace("</article>","<button data-test='show-more'>PRIVATE_MORE</button><select name='variation_id'></select><button data-dropdown-items='PRIVATE_UNKNOWN'></button></article>");
        let value = structure_value(&limited);
        assert_eq!(value["rowsExpandOrLoadMore"], 1);
        assert_eq!(value["rowsVariantSelector"], 1);
        assert_eq!(value["rowsUnknownDropdown"], 1);
        assert_eq!(value["rowsAdvertisedCountMismatch"], 1);
        assert_eq!(
            structure_value(&html.replace(
                "<article data-test='library-item'>",
                "<article data-test='library-item' data-file-count='1'>"
            ))["rowsAdvertisedCountMatches"],
            1
        );
        assert_eq!(
            structure_value(&html.replace(
                "<article data-test='library-item'>",
                "<article data-test='library-item' data-file-count='PRIVATE_COUNT'>"
            ))["rowsAdvertisedCountInvalid"],
            1
        );
    }
    #[test]
    fn modern_shelf_manifest_preserves_all_other_gates_and_rejects_partial_structures() {
        let html = modern_structure();
        let complete = |body: &str, source: &str, route: &str| {
            super::super::parser::parse(body, source, route)
                .unwrap()
                .rows[0]
                .manifest_complete
        };
        assert!(complete(&html, "library", "/library?page=1"));
        for (source, route) in [
            ("gift", "/library/gifts?page=1"),
            ("free", "/library/free_downloads?page=1"),
        ] {
            assert!(complete(
                &html.replace("<h1>Library</h1>", "<h1>Gift Free downloads</h1>"),
                source,
                route
            ));
        }
        // Purchase has not established this modern shelf witness.
        assert!(!complete(
            &html.replace("data-test='library-item'", "class='sheet'"),
            "purchase",
            "/orders/900009"
        ));
        let alternate=html.replace("</article>","<button data-test='other-downloads-button' aria-expanded='false' data-dropdown-items='PRIVATE_METHODS'>PRIVATE_METHOD</button></article>");
        assert!(complete(&alternate, "library", "/library?page=1"));
        let signed = html.replace(
            "variation_id=800001",
            "variation_id=800001&amp;signature=PRIVATE_TOKEN",
        );
        let page = super::super::parser::parse(&signed, "library", "/library?page=1").unwrap();
        assert!(page.rows[0].manifest_complete);
        let duplicate_action = html.replace("</div></div></article>", "<a href='https://booth.pm/downloadables/700001?variation_id=800001'>PRIVATE_AGAIN</a></div></div></article>");
        assert!(complete(&duplicate_action, "library", "/library?page=1"));
        assert_eq!(
            structure_value(&duplicate_action)["rowsConflictingEntryScopes"],
            0
        );
        assert!(!page.rows[0].files[0].source_url.contains("PRIVATE_TOKEN"));
        let multiple=html.replace("</div></div></article>","</div><div><div class='min-w-0 break-words whitespace-pre-line'><div class='text-14'>PRIVATE_DOC.zip</div></div><a data-test='downloadable' href='https://booth.pm/downloadables/700002'>Download</a></div></div></article>");
        let page = super::super::parser::parse(&multiple, "library", "/library?page=1").unwrap();
        assert_eq!(page.rows[0].files.len(), 2);
        assert!(page.rows[0].manifest_complete);
        let orphan = html.replace("</div></div></article>", "</div><div class='min-w-0 break-words whitespace-pre-line'><div class='text-14'>PRIVATE_ORPHAN.zip</div></div></div></article>");
        let conflicting = html.replace("</div></div></article>", "</div><div><div class='min-w-0 break-words whitespace-pre-line'><div class='text-14'>PRIVATE_CONFLICT.zip</div></div><a data-test='downloadable' href='https://booth.pm/downloadables/700001?variation_id=800001'>Download</a></div></div></article>");
        let nested_orphan=html.replace("<div class='synthetic-file-container'>","<div class='synthetic-file-container'><div class='synthetic-inner'>").replace("</div></div></article>","</div></div><div class='min-w-0 break-words whitespace-pre-line'><div class='text-14'>PRIVATE_NESTED_ORPHAN.zip</div></div></div></article>");
        assert_eq!(structure_value(&orphan)["orphanModernNames"], 1);
        assert_eq!(structure_value(&orphan)["rowsWithOrphanModernName"], 1);
        assert_eq!(
            structure_value(&conflicting)["rowsConflictingEntryScopes"],
            1
        );
        assert_eq!(structure_value(&nested_orphan)["orphanModernNames"], 1);
        let linked_outer_orphan = html.replace("<div class='synthetic-file-container'>", "<div class='synthetic-outer'><a href='https://synthetic.booth.pm/items/900001'>PRIVATE_SAME_PRODUCT</a><div class='synthetic-file-container'>").replace("</div></div></article>", "</div></div><div class='min-w-0 break-words whitespace-pre-line'><div class='text-14'>PRIVATE_OUTSIDE_ORPHAN.zip</div></div></div></article>");
        let linked_page =
            super::super::parser::parse(&linked_outer_orphan, "library", "/library?page=1")
                .unwrap();
        assert_eq!(linked_page.unresolved, 0);
        assert!(linked_page.rows[0].error_code.is_none());
        assert_eq!(
            structure_value(&linked_outer_orphan)["rowsFileOnlyContainer"],
            1
        );
        assert_eq!(
            structure_value(&linked_outer_orphan)["orphanModernNames"],
            1
        );
        assert!(!linked_page.rows[0].manifest_complete);
        for body in [&orphan, &conflicting, &nested_orphan] {
            assert!(!complete(body, "library", "/library?page=1"));
        }
        let missing_list = MANIFEST_HTML.replace("data-test='downloadable-list'", "");
        assert!(!complete(&missing_list, "library", "/library?page=1"));
        for (body, reason) in [
            (html.replace("</article>","<button data-test='show-more'>PRIVATE_MORE</button></article>"),"load more"),
            (html.replace("</article>","<button aria-expanded='false'>PRIVATE_EXPAND</button></article>"),"collapsed control"),
            (html.replace("</article>","<select name='variation_id'></select></article>"),"variant selector"),
            (html.replace("</article>","<button data-dropdown-items='PRIVATE_UNKNOWN'></button></article>"),"unknown dropdown"),
            (html.replace("<article data-test='library-item'>","<article data-test='library-item' data-file-count='2'>"),"advertised mismatch"),
            (html.replace("<article data-test='library-item'>","<article data-test='library-item' data-file-count='PRIVATE_COUNT'>"),"advertised invalid"),
            (html.replace("PRIVATE_FILE.psd</div>","PRIVATE_FILE.psd</div><div class='text-14'>PRIVATE_OTHER.zip</div>"),"ambiguous name"),
            (html.replace("<div class='synthetic-file-container'>","").replace("</div></div></article>","</div></article>"),"broad card only"),
            (html.replace("<div class='synthetic-file-container'>","<div class='synthetic-file-container'><a href='https://synthetic.booth.pm/items/900002'>PRIVATE_SECOND</a>"),"multiple products"),
            (html.replace("<a data-test='downloadable'","<i data-test='downloadable'></i><a data-test='downloadable'"),"invalid resolver"),
            (html.replace("variation_id=800001","variation_id=PRIVATE_INVALID"),"invalid variant"),
            (html.replace("<article data-test='library-item'>","<article data-test='library-item'><i data-test='item-unavailable'></i>"),"unavailable"),
        ] { assert!(!complete(&body,"library","/library?page=1"),"{reason}"); }
        assert!(complete(
            &html.replace(
                "<article data-test='library-item'>",
                "<article data-test='library-item' data-file-count='1'>"
            ),
            "library",
            "/library?page=1"
        ));
    }
    #[test]
    fn ordering_diagnostic_is_closed_counts_and_never_serializes_source_values() {
        let html = r#"<html lang='ja'><body><h1>PRIVATE_TITLE</h1><input type='search' value='PRIVATE_QUERY'><select name='PRIVATE_CONTROL'><option value='PRIVATE_VALUE'>入手が新しい順</option><option>ファイル更新が新しい順</option></select><select><option selected>ファイル更新が新しい順</option></select><button aria-expanded='false' class='PRIVATE_CLASS'>並び替え 入手が新しい順</button><button aria-pressed='true'>入手が新しい順</button><a href='https://PRIVATE_HOST/?token=PRIVATE_TOKEN' aria-current='true'>入手が新しい順</a><label for='PRIVATE_ID'>入手が新しい順</label><input type='radio' checked id='PRIVATE_ID' value='PRIVATE_RADIO'><div role='option' aria-selected='true'>ファイル更新が新しい順</div></body></html>"#;
        let value = serde_json::to_value(inspect(html)).unwrap();
        assert_eq!(value["language"], "ja");
        assert_eq!(value["searchValueNonempty"], true);
        assert_eq!(value["implicitSingleSelectFirst"]["acquiredExact"], 1);
        assert_eq!(value["selectedAttribute"]["fileUpdatedExact"], 1);
        assert_eq!(value["buttons"]["acquiredContainingOnly"], 1);
        assert_eq!(value["ariaPressed"]["acquiredExact"], 1);
        assert_eq!(value["ariaExpandedPresent"]["acquiredContainingOnly"], 1);
        assert_eq!(value["ariaSelected"]["fileUpdatedExact"], 1);
        assert_eq!(value["radioChecked"]["acquiredExact"], 1);
        let encoded = value.to_string();
        assert!(!encoded.contains("PRIVATE"));
        assert!(!encoded.contains("https"));
        let allowed = [
            "version",
            "language",
            "searchValueNonempty",
            "selects",
            "options",
            "buttons",
            "links",
            "radios",
            "selectedAttribute",
            "ariaPressed",
            "ariaSelected",
            "ariaCurrent",
            "ariaExpandedPresent",
            "radioChecked",
            "implicitSingleSelectFirst",
        ];
        assert_eq!(value.as_object().unwrap().len(), allowed.len());
        assert!(value
            .as_object()
            .unwrap()
            .keys()
            .all(|k| allowed.contains(&k.as_str())));
        for key in &allowed[3..] {
            assert_eq!(value[*key].as_object().unwrap().len(), 4);
            assert!(value[*key]
                .as_object()
                .unwrap()
                .values()
                .all(|v| v.as_u64().is_some()));
        }
    }
    #[test]
    fn implicit_selection_counts_exclude_multiple_disabled_size_and_selected_controls() {
        let option = "<option>入手が新しい順</option><option>ファイル更新が新しい順</option>";
        for control in [format!("<select multiple>{option}</select>"),format!("<select size='2'>{option}</select>"),format!("<select disabled>{option}</select>"),"<select><option disabled>入手が新しい順</option></select>".into(),"<select><optgroup disabled><option>入手が新しい順</option></optgroup></select>".into(),"<select><option>入手が新しい順</option><option selected>ファイル更新が新しい順</option></select>".into()] {
            let value=serde_json::to_value(inspect(&control)).unwrap();
            assert_eq!(value["implicitSingleSelectFirst"]["acquiredExact"],0);
        }
        let unknown = serde_json::to_value(inspect(
            "<html lang='PRIVATE_LANG'><button>PRIVATE_CAPTION</button></html>",
        ))
        .unwrap();
        assert_eq!(unknown["language"], "unknown");
        assert_eq!(unknown["buttons"]["acquiredExact"], 0);
        let non_radio = serde_json::to_value(inspect(
            "<label for='synthetic-check'>入手が新しい順</label><input type='checkbox' id='synthetic-check' checked aria-pressed='true'>",
        )).unwrap();
        assert_eq!(non_radio["radios"]["acquiredExact"], 0);
        assert_eq!(non_radio["radioChecked"]["acquiredExact"], 0);
    }
}
