//! Opt-in lead-only exact receipt audit. Authenticated values stay in memory/private storage.
use super::*;

#[test]
fn fresh_delisted_purchase_publishes_files_and_respects_vrchat_scope() {
    for status in [404, 410] {
        let root =
            std::env::temp_dir().join(format!("booth-delisted-visible-{}", uuid::Uuid::new_v4()));
        let catalogue = crate::catalogue::Catalogue::open(&root).unwrap();
        let service = BoothService::new(catalogue.clone()).unwrap();
        let account = "authored-account";
        let product = "900001";
        catalogue
            .put(
                "account",
                account,
                json!({"id":account,"store":"BOOTH","state":"connected","identityVerified":true}),
            )
            .unwrap();
        let job = json!({"id":"authored-delisted-job","type":"boothSync","state":"running"});
        catalogue
            .put("job", "authored-delisted-job", job.clone())
            .unwrap();
        let mut checkpoint = Checkpoint::fresh(account, "purchase", "full").unwrap();
        let mut patch = json!({});
        metadata::apply_public_response(&mut patch, product, status, None).unwrap();
        let (requests, _receiver) = mpsc::unbounded_channel();
        let prefetch = PublicPrefetch {
            requests,
            enqueued: Default::default(),
            support_pages: Default::default(),
            early: std::sync::Arc::new(Mutex::new(EarlyPublications {
                remaining_sources: BTreeSet::from(["purchase".into()]),
                observations: BTreeMap::from([(product.into(), Ok(patch))]),
                ..Default::default()
            })),
        };
        let page = |order: &str, variant: &str, ids: &[u32]| {
            let entries=ids.iter().map(|id|{
                let dropdown=serde_json::to_string(&json!([{"deeplinkDownloadableUrl":format!("https://booth.pm/downloadables/{id}/deeplink?variation_id={variant}&client=booth-library-manager")}])).unwrap();
                format!("<div class='legacy-list-item'><div class='min-w-0'><b>authored-{id}.zip</b></div><button data-href='https://booth.pm/downloadables/{id}'></button><button data-test='other-downloads-button' data-dropdown-items='{dropdown}'></button></div>")
            }).collect::<String>();
            parser::parse(&format!("<main><div class='sheet'><section><div><b><a class='nav' href='https://invented.booth.pm/items/900001'>Authored generic item</a></b><div>¥ 900</div></div></section><div class='list list--collapse'>{entries}</div></div></main>"),"purchase",&format!("/orders/{order}")).unwrap()
        };
        let consume = |page: &parser::Page, checkpoint: &mut Checkpoint| {
            record_source_page(checkpoint, "purchase", &page.route, page, None).unwrap();
            for row in &page.rows {
                if let Some(id) = &row.product_id {
                    checkpoint.product_ids.insert(id.clone());
                    checkpoint.enrichment_pending.insert(id.clone());
                }
            }
            let journal_id = format!("authored-delisted-journal:{}", safety::hash(&page.route));
            checkpoint.journal_ids.push(journal_id.clone());
            service.commit_source_page(&journal_id,&json!({"id":journal_id,"accountId":account,"source":"purchase","runId":checkpoint.run_id,"page":page}),"authored-delisted-checkpoint",checkpoint).unwrap();
            service
                .stage_discovered_page(
                    &job,
                    account,
                    "purchase",
                    "full",
                    "authored-delisted-checkpoint",
                    checkpoint,
                    page,
                    &prefetch,
                )
                .unwrap()
        };
        let first = page("910001", "800001", &[700001, 700002, 700003]);
        assert_eq!(first.rows.len(), 1);
        assert!(first.rows[0].manifest_complete);
        assert_eq!(first.rows[0].files.len(), 3);
        assert_eq!(consume(&first, &mut checkpoint), vec![product]);
        let detail = catalogue
            .command("product_details", json!({"id":product}))
            .unwrap();
        assert_eq!(detail["publicPageAvailability"], "unavailable");
        assert_eq!(detail["category"], "NonVRChat");
        assert_eq!(detail["categoryInference"]["basis"], "fallback");
        assert_eq!(detail["categoryInference"]["scope"], "unknown");
        assert_eq!(detail["total"], 3);
        assert_eq!(detail["knownFileCount"], 3);
        let ids: BTreeSet<_> = detail["files"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| f["id"].as_str().unwrap())
            .collect();
        assert_eq!(ids.len(), 3);
        for query in [json!({}), json!({"accountIds":[account]})] {
            let result = catalogue.command("query", query).unwrap();
            assert_eq!(result["matched"], 0);
        }
        assert_eq!(
            catalogue
                .command("query", json!({"category":"NonVRChat"}))
                .unwrap()["matched"],
            1
        );
        assert_eq!(
            catalogue
                .command("query", json!({"quick":"total","accountIds":[account]}))
                .unwrap()["matched"],
            1
        );
        catalogue
            .command(
                "overlay",
                json!({"ids":[product],"patch":{"title":"Human title","note":"Human note","category":"Hair"}}),
            )
            .unwrap();
        assert_eq!(
            consume(&page("910002", "800002", &[700004]), &mut checkpoint),
            vec![product]
        );
        let merged = catalogue
            .command("product_details", json!({"id":product}))
            .unwrap();
        assert_eq!(merged["title"], "Human title");
        assert_eq!(merged["note"], "Human note");
        assert_eq!(merged["total"], 4);
        assert_eq!(merged["files"].as_array().unwrap().len(), 4);
        assert_eq!(merged["category"], "Hair");
        assert_eq!(catalogue.command("query", json!({})).unwrap()["matched"], 1);
        catalogue
            .command(
                "overlay",
                json!({"ids":[product],"patch":{"category":"NonVRChat"}}),
            )
            .unwrap();
        assert_eq!(catalogue.command("query", json!({})).unwrap()["matched"], 0);
        drop(prefetch);
        drop(service);
        drop(catalogue);
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[tokio::test]
#[ignore = "Requires explicit receipt authorization and protected session; no listing or asset GETs"]
async fn authorized_receipt_publication_audit() {
    let source = std::path::PathBuf::from(
        std::env::var_os("NNA_VERIFY_SESSION_SOURCE").expect("SESSION_SOURCE_REQUIRED"),
    );
    let root = std::path::PathBuf::from(
        std::env::var_os("NNA_VERIFY_RECEIPT_ROOT").expect("ISOLATED_ROOT_REQUIRED"),
    );
    let receipt = std::env::var("NNA_VERIFY_RECEIPT_ID").expect("RECEIPT_AUTHORIZATION_REQUIRED");
    assert!(safety::numeric(&receipt), "INVALID_RECEIPT");
    assert!(!root.exists(), "FRESH_ISOLATED_ROOT_REQUIRED");
    let bindings: Value = serde_json::from_slice(
        &std::fs::read(source.join("account-ids.json")).expect("BINDINGS_UNAVAILABLE"),
    )
    .expect("BINDINGS_INVALID");
    let account = bindings["BOOTH"]
        .as_str()
        .expect("BOOTH_BINDING_UNAVAILABLE");
    let provider = crate::stores::StoreProvider::Booth;
    let profile = crate::stores::session::profile_path(&source.join("profile"), provider, account)
        .expect("PROFILE_INVALID");
    let cookies = crate::stores::session_cache::load(&profile, provider, account)
        .expect("PROTECTED_SESSION_UNAVAILABLE");
    assert!(!cookies.is_empty(), "PROTECTED_SESSION_EMPTY");
    let jar = std::sync::Arc::new(reqwest::cookie::Jar::default());
    for cookie in cookies {
        if cookie
            .domain()
            .is_some_and(|d| safety::booth_host(d.trim_start_matches('.')))
        {
            for origin in ["https://accounts.booth.pm/", "https://booth.pm/"] {
                jar.add_cookie_str(&cookie.to_string(), &url::Url::parse(origin).unwrap());
            }
        }
    }
    let client = reqwest::Client::builder()
        .cookie_provider(jar)
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            if attempt.previous().len() < 5
                && attempt.url().scheme() == "https"
                && attempt.url().host_str().is_some_and(safety::booth_host)
            {
                attempt.follow()
            } else {
                attempt.stop()
            }
        }))
        .build()
        .expect("CLIENT_UNAVAILABLE");
    let route = format!("/orders/{receipt}");
    let response = client
        .get(safety::route(&route, "purchase").unwrap())
        .send()
        .await
        .unwrap_or_else(|_| panic!("RECEIPT_REQUEST_FAILED"));
    assert!(
        response.status().is_success()
            && response.url().host_str() == Some("accounts.booth.pm")
            && response.url().path() == route,
        "RECEIPT_AUTH_FAILED"
    );
    let body = response.text().await.expect("RECEIPT_BODY_FAILED");
    assert!(body.len() <= 4_000_000, "RECEIPT_BODY_LIMIT");
    let page = parser::parse(&body, "purchase", &route).expect("RECEIPT_PARSE_FAILED");
    drop(body);
    let products: BTreeSet<_> = page
        .rows
        .iter()
        .filter_map(|r| r.product_id.clone())
        .collect();
    let mut report = json!({"authenticatedReceipt":true,"parsedRows":page.rows.len(),"unresolvedRows":page.unresolved,"products":products.len(),"parsedFiles":page.rows.iter().map(|r|r.files.len()).sum::<usize>(),"completeRows":page.rows.iter().filter(|r|r.manifest_complete).count(),"namedRows":page.rows.iter().filter(|r|r.name.is_some()).count(),"unavailableRows":page.rows.iter().filter(|r|r.unavailable).count(),"errorRows":page.rows.iter().filter(|r|r.error_code.is_some()).count(),"assetDownloadRequests":0,"rawBodySaved":false});
    assert!(products.len() == 1, "EXPECTED_ONE_AUTHORIZED_PRODUCT");
    let product = products.iter().next().unwrap();
    let public = reqwest::Client::new()
        .get(format!("https://booth.pm/ja/items/{product}.json"))
        .send()
        .await
        .unwrap_or_else(|_| panic!("PUBLIC_REQUEST_FAILED"));
    let status = public.status().as_u16();
    report["publicStatus"] = json!(status);
    assert!(
        matches!(status, 404 | 410),
        "EXPECTED_UNAVAILABLE_PUBLIC_PAGE"
    );
    drop(public);
    let catalogue = crate::catalogue::Catalogue::open(&root).expect("ISOLATED_CATALOGUE_FAILED");
    let service = BoothService::new(catalogue.clone()).expect("SERVICE_FAILED");
    catalogue
        .put(
            "account",
            account,
            json!({"id":account,"store":"BOOTH","state":"connected","identityVerified":true}),
        )
        .unwrap();
    let job = json!({"id":"authorized-targeted-receipt","type":"boothSync","state":"running"});
    catalogue
        .put("job", "authorized-targeted-receipt", job.clone())
        .unwrap();
    let mut checkpoint = Checkpoint::fresh(account, "purchase", "full").unwrap();
    record_source_page(&mut checkpoint, "purchase", &route, &page, None).unwrap();
    for row in &page.rows {
        if let Some(id) = &row.product_id {
            checkpoint.product_ids.insert(id.clone());
            checkpoint.enrichment_pending.insert(id.clone());
        }
    }
    let journal_id = format!("targeted-journal:{}", safety::hash(&route));
    checkpoint.journal_ids.push(journal_id.clone());
    service.commit_source_page(&journal_id,&json!({"id":journal_id,"accountId":account,"source":"purchase","runId":checkpoint.run_id,"page":page}),"targeted-checkpoint",&checkpoint).unwrap();
    let mut patch = json!({});
    metadata::apply_public_response(&mut patch, product, status, None).unwrap();
    let (requests, _receiver) = mpsc::unbounded_channel();
    let prefetch = PublicPrefetch {
        requests,
        enqueued: Default::default(),
        support_pages: Default::default(),
        early: std::sync::Arc::new(Mutex::new(EarlyPublications {
            remaining_sources: BTreeSet::from(["purchase".into()]),
            observations: BTreeMap::from([(product.clone(), Ok(patch))]),
            ..Default::default()
        })),
    };
    let published = service
        .stage_discovered_page(
            &job,
            account,
            "purchase",
            "full",
            "targeted-checkpoint",
            &checkpoint,
            &page,
            &prefetch,
        )
        .unwrap();
    report["publishedProducts"] = json!(published.len());
    report["storedProduct"] = json!(catalogue.get("product", product).unwrap().is_some());
    if report["storedProduct"] == true {
        let detail = catalogue
            .command("product_details", json!({"id":product}))
            .unwrap();
        let files = detail["files"].as_array().unwrap();
        let unique: BTreeSet<_> = files.iter().map(|f| f["id"].as_str().unwrap()).collect();
        report["rawDetailFiles"] = json!(files.len());
        report["uniqueDetailFiles"] = json!(unique.len());
        report["storedTotalFiles"] = detail["total"].clone();
        report["storedKnownFiles"] = detail["knownFileCount"].clone();
        report["fallbackCategory"] = json!(detail["categoryInference"]["basis"] == "fallback");
        report["unknownScope"] = json!(detail["categoryInference"]["scope"] == "unknown");
        report["nonVRChatCategory"] = json!(detail["category"] == "NonVRChat");
        report["publicUnavailable"] = json!(detail["publicPageAvailability"] == "unavailable");
        for (key, args) in [
            ("totalQuery", json!({"quick":"total","limit":1})),
            ("defaultQuery", json!({"limit":1})),
            ("accountQuery", json!({"accountIds":[account],"limit":1})),
            ("nonVRChatQuery", json!({"category":"NonVRChat","limit":1})),
        ] {
            report[key] = catalogue.command("query", args).unwrap()["matched"].clone();
        }
        report["sourceFileListExact"] = json!(
            files.len() == page.rows.iter().map(|r| r.files.len()).sum::<usize>()
                && unique.len() == files.len()
                && detail["total"].as_u64() == Some(files.len() as u64)
                && detail["knownFileCount"].as_u64() == Some(files.len() as u64)
        );
    }
    let report_path = std::path::PathBuf::from(
        std::env::var_os("NNA_VERIFY_RECEIPT_REPORT").expect("AGGREGATE_REPORT_REQUIRED"),
    );
    std::fs::write(report_path, serde_json::to_vec_pretty(&report).unwrap()).unwrap();
    println!("TARGETED_RECEIPT_AGGREGATE {}", report);
    assert!(
        report["storedProduct"] == true && report["sourceFileListExact"] == true,
        "PUBLICATION_OR_FILES_FAILED"
    );
}
