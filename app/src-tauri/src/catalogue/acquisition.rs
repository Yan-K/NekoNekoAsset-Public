//! Derived acquisition summaries. Raw provider records and identities stay intact.
use super::*;

pub(super) fn date_key(value: &str) -> Option<String> {
    let value = value.trim();
    if let Ok(date) = chrono::DateTime::parse_from_rfc3339(value) {
        return Some(
            date.with_timezone(&chrono::Utc)
                .format("%Y-%m-%dT%H:%M:%S%.9fZ")
                .to_string(),
        );
    }
    for format in ["%Y/%m/%d %H:%M:%S", "%Y/%m/%d %H:%M"] {
        if let Ok(date) = chrono::NaiveDateTime::parse_from_str(value, format) {
            // This is a comparison key, not a claim that an unzoned provider
            // timestamp was UTC. Keep the provider's original display text.
            return Some(date.format("%Y-%m-%dT%H:%M:%S%.9fZ").to_string());
        }
    }
    for format in ["%Y-%m-%d", "%Y/%m/%d", "%Y年%m月%d日"] {
        if let Ok(date) = chrono::NaiveDate::parse_from_str(value, format) {
            return Some(format!("{}T00:00:00.000000000Z", date.format("%Y-%m-%d")));
        }
    }
    None
}

fn nonempty(value: &Value) -> Option<&str> {
    value.as_str().filter(|text| !text.trim().is_empty())
}

pub(super) fn money_amount(value: &Value, currency: &str) -> Option<f64> {
    if currency.len() != 3 || !currency.bytes().all(|c| c.is_ascii_uppercase()) {
        return None;
    }
    let amount = value.as_f64()?;
    if !amount.is_finite()
        || amount < 0.0
        || (currency == "JPY" && amount.fract() != 0.0)
        || amount > 9_007_199_254_740_991.0
    {
        return None;
    }
    Some(if amount == 0.0 { 0.0 } else { amount })
}

fn paid_observation(row: &Value) -> Option<(f64, &str)> {
    if row["paidPriceBasis"] != "productLineTotal" {
        return None;
    }
    let currency = row["paidCurrency"].as_str()?;
    money_amount(&row["paidPrice"], currency).map(|amount| (amount, currency))
}

pub(super) fn normalize_provenance(acquisitions: &[Value]) -> Vec<Value> {
    acquisitions
        .iter()
        .map(|raw| {
            let mut row = raw.clone();
            let display = ["acquiredDateText", "acquired", "acquiredAt"]
                .into_iter()
                .find_map(|field| nonempty(&raw[field]))
                .unwrap_or("");
            let key = ["acquiredAt", "acquired", "acquiredDateText"]
                .into_iter()
                .find_map(|field| nonempty(&raw[field]).and_then(date_key));
            row["acquiredDisplay"] = json!(display);
            row["acquiredSortKey"] = json!(key);
            let paid = paid_observation(raw);
            row["paidPrice"] = json!(paid.map(|(amount, _)| amount));
            row["paidCurrency"] = json!(paid.map(|(_, currency)| currency));
            row
        })
        .collect()
}

fn in_scope(row: &Value, accounts: Option<&Vec<Value>>) -> bool {
    accounts.is_none_or(|accounts| {
        row["accountId"]
            .as_str()
            .is_some_and(|id| accounts.iter().any(|account| account.as_str() == Some(id)))
    })
}

/// Used while materializing the default/all-account product summary.
pub(super) fn apply_default(product: &mut Value, source_acquired: &str) {
    summarize(product, None, Some(source_acquired));
}

/// Absent/null scope uses the materialized all-account summary. An explicit
/// account selection recomputes only these facts, retaining full provenance.
pub(super) fn apply_scope(product: &mut Value, accounts: Option<&Vec<Value>>) {
    // Local products have no store account ownership; retain their existing facts.
    if product["provider"] == "local" {
        return;
    }
    if accounts.is_some() {
        summarize(product, accounts, None);
        scope_owned_prices(product, accounts);
    }
}

/// BOOTH prices are joined by observed variant identity, never listing minima.
pub(super) fn owned_prices(
    product: &mut Value,
    source: &Value,
    acquisitions: &[Value],
    entitlements: &[Value],
    files: &[Value],
) {
    if !matches!(text(product, "provider"), "booth" | "") {
        return;
    }
    let variants: Vec<_> = source["publicVariants"]
        .as_array()
        .into_iter()
        .flatten()
        .collect();
    let mut owned: std::collections::BTreeMap<String, (String, BTreeSet<String>)> =
        Default::default();
    for record in acquisitions.iter().chain(entitlements).chain(files) {
        let account = text(record, "accountId");
        if account.is_empty() {
            continue;
        }
        let mut ids = strings(&record["variantIds"]);
        if let Some(id) = nonempty(&record["variantId"]) {
            ids.push(id.into());
        }
        if ids.is_empty() {
            let name = text(record, "variantName");
            let matches: Vec<_> = variants
                .iter()
                .filter(|v| !name.is_empty() && text(v, "name") == name)
                .collect();
            if matches.len() == 1 {
                ids.push(text(matches[0], "id").into());
            }
        }
        for id in ids.into_iter().filter(|id| !id.is_empty()) {
            let entry = owned
                .entry(id)
                .or_insert_with(|| (text(record, "variantName").into(), BTreeSet::new()));
            entry.1.insert(account.into());
        }
    }
    let current: Vec<_> = owned.iter().map(|(id,(name,accounts))| {
        let matches: Vec<_> = variants.iter().filter(|v| text(v,"id") == id).collect();
        let quote = (matches.len() == 1).then(|| matches[0]);
        let currency = quote.map(|v| text(v,"currency")).unwrap_or("JPY");
        let price = quote.and_then(|v| money_amount(&v["price"],currency));
        json!({"variantId":id,"name":quote.map(|v|text(v,"name")).filter(|v|!v.is_empty()).unwrap_or(name),"price":price,"currency":currency,"accountIds":accounts})
    }).collect();
    let mut paid = Vec::new();
    for row in acquisitions {
        let account = text(row, "accountId");
        let Some(proven_paid) = paid_observation(row) else {
            continue;
        };
        for payment in row["variantPayments"].as_array().into_iter().flatten() {
            let id = text(payment, "variantId");
            if !strings(&row["variantIds"])
                .iter()
                .any(|variant| variant == id)
                || !owned
                    .get(id)
                    .is_some_and(|(_, accounts)| accounts.contains(account))
            {
                continue;
            }
            let currency = text(payment, "currency");
            let Some(amount) = money_amount(&payment["price"], currency) else {
                continue;
            };
            // The current adapter proves only one variant for a product line.
            // Never allocate its aggregate to multiple owned variants.
            if strings(&row["variantIds"])
                .into_iter()
                .collect::<BTreeSet<_>>()
                .len()
                != 1
                || proven_paid != (amount, currency)
            {
                continue;
            }
            let mut value = json!({"variantId":id,"name":text(payment,"name"),"price":amount,"currency":currency,"accountId":account,"acquisitionKey":text(row,"id")});
            if let Some(quantity) = payment["quantity"].as_u64().filter(|n| *n > 0) {
                value["quantity"] = json!(quantity);
            }
            if !paid.contains(&value) {
                paid.push(value);
            }
        }
    }
    for variant in &current {
        for account in strings(&variant["accountIds"]) {
            if !paid
                .iter()
                .any(|row| row["variantId"] == variant["variantId"] && row["accountId"] == account)
            {
                paid.push(json!({"variantId":variant["variantId"],"name":variant["name"],"accountId":account,"price":null,"currency":null}));
            }
        }
    }
    product["ownedCurrentPrices"] = json!(current);
    product["ownedPaidPrices"] = json!(paid);
    scope_owned_prices(product, None);
}

fn scope_owned_prices(product: &mut Value, accounts: Option<&Vec<Value>>) {
    if !product["ownedCurrentPrices"].is_array() {
        return;
    }
    let mut current: Vec<Value> = product["ownedCurrentPrices"]
        .as_array()
        .unwrap()
        .iter()
        .cloned()
        .collect();
    if let Some(accounts) = accounts {
        for row in &mut current {
            row["accountIds"] = json!(strings(&row["accountIds"])
                .into_iter()
                .filter(|id| accounts.iter().any(|a| a.as_str() == Some(id)))
                .collect::<Vec<_>>());
        }
        current.retain(|row| row["accountIds"].as_array().is_some_and(|a| !a.is_empty()));
    }
    let paid: Vec<_> = product["ownedPaidPrices"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|row| in_scope(row, accounts))
        .cloned()
        .collect();
    let unique = (current.len() == 1).then(|| &current[0]);
    let amount = unique.and_then(|row| money_amount(&row["price"], text(row, "currency")));
    product["price"] = json!(amount);
    product["priceCurrency"] = json!(amount.and_then(|_| unique.map(|row| text(row, "currency"))));
    product["currentPriceText"] = json!(amount.map(|price| format!("¥ {price}")));
    product["ownedCurrentPrices"] = json!(current);
    product["ownedPaidPrices"] = json!(paid);
}

fn summarize(product: &mut Value, accounts: Option<&Vec<Value>>, fallback: Option<&str>) {
    let mut rows: Vec<_> = product["provenance"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|row| in_scope(row, accounts))
        .collect();
    rows.sort_by(|a, b| {
        text(a, "id")
            .cmp(text(b, "id"))
            .then_with(|| text(a, "rowId").cmp(text(b, "rowId")))
    });
    let earliest = rows
        .iter()
        .filter_map(|row| {
            row["acquiredSortKey"]
                .as_str()
                .map(|key| (key, text(row, "acquiredDisplay")))
        })
        .min_by(|a, b| a.0.cmp(b.0));
    let fallback = fallback.filter(|value| !value.trim().is_empty());
    let fallback_key = fallback.and_then(date_key);
    let (key, display) = match (earliest, fallback_key.as_deref()) {
        (Some((key, _)), Some(other)) if other < key => (Some(other), fallback.unwrap()),
        (Some((key, display)), _) => (Some(key), display),
        (None, Some(key)) => (Some(key), fallback.unwrap()),
        (None, None) => (
            None,
            rows.iter()
                .find_map(|row| nonempty(&row["acquiredDisplay"]))
                .or(fallback)
                .unwrap_or(""),
        ),
    };
    let acquired = display.to_owned();
    let sort_key = key.map(str::to_owned);
    let mut observations = Vec::new();
    for row in rows {
        if let Some(paid) = paid_observation(row) {
            if !observations.contains(&paid) {
                observations.push(paid);
            }
        }
    }
    // Only a unique item-level amount and currency is a truthful scalar. A second
    // amount/currency cannot be replaced by a sum, latest amount, or minimum.
    let paid = match observations.as_slice() {
        [(amount, currency)] => Some((*amount, (*currency).to_owned())),
        _ => None,
    };
    product["acquired"] = json!(acquired);
    product["acquiredSortKey"] = json!(sort_key);
    product["paidPrice"] = json!(paid.as_ref().map(|(amount, _)| amount));
    product["paidCurrency"] = json!(paid.map(|(_, currency)| currency));
}
