//! Explicit selected-product spending, derived only from proven acquisition facts.
use super::*;
use std::collections::{BTreeMap, BTreeSet};

pub(super) fn fraction_digits(currency: &str) -> Option<u32> {
    Some(match currency {
        "BIF" | "CLP" | "DJF" | "GNF" | "ISK" | "JPY" | "KMF" | "KRW" | "PYG" | "RWF" | "UGX"
        | "UYI" | "VND" | "VUV" | "XAF" | "XOF" | "XPF" => 0,
        "BHD" | "IQD" | "JOD" | "KWD" | "LYD" | "OMR" | "TND" => 3,
        "CLF" | "UYW" => 4,
        "AED" | "AFN" | "ALL" | "AMD" | "ANG" | "AOA" | "ARS" | "AUD" | "AWG" | "AZN" | "BAM"
        | "BBD" | "BDT" | "BGN" | "BMD" | "BND" | "BOB" | "BOV" | "BRL" | "BSD" | "BTN" | "BWP"
        | "BYN" | "BZD" | "CAD" | "CDF" | "CHE" | "CHF" | "CHW" | "CNY" | "COP" | "COU" | "CRC"
        | "CUP" | "CVE" | "CZK" | "DKK" | "DOP" | "DZD" | "EGP" | "ERN" | "ETB" | "EUR" | "FJD"
        | "FKP" | "GBP" | "GEL" | "GHS" | "GIP" | "GMD" | "GTQ" | "GYD" | "HKD" | "HNL" | "HTG"
        | "HUF" | "IDR" | "ILS" | "INR" | "IRR" | "JMD" | "KES" | "KGS" | "KHR" | "KPW" | "KYD"
        | "KZT" | "LAK" | "LBP" | "LKR" | "LRD" | "LSL" | "MAD" | "MDL" | "MGA" | "MKD" | "MMK"
        | "MNT" | "MOP" | "MRU" | "MUR" | "MVR" | "MWK" | "MXN" | "MXV" | "MYR" | "MZN" | "NAD"
        | "NGN" | "NIO" | "NOK" | "NPR" | "NZD" | "PAB" | "PEN" | "PGK" | "PHP" | "PKR" | "PLN"
        | "QAR" | "RON" | "RSD" | "RUB" | "SAR" | "SBD" | "SCR" | "SDG" | "SEK" | "SGD" | "SHP"
        | "SLE" | "SOS" | "SRD" | "SSP" | "STN" | "SVC" | "SYP" | "SZL" | "THB" | "TJS" | "TMT"
        | "TOP" | "TRY" | "TTD" | "TWD" | "TZS" | "UAH" | "USD" | "USN" | "UYU" | "UZS" | "VED"
        | "VES" | "WST" | "XCD" | "XCG" | "YER" | "ZAR" | "ZMW" | "ZWG" => 2,
        _ => return None,
    })
}

// Convert the JSON number's decimal representation, without floating-point sums
// or rounding away unsupported fractional minor units.
fn minor_units(value: &Value, currency: &str) -> Option<i128> {
    let fraction = fraction_digits(currency)? as i32;
    let Value::Number(number) = value else {
        return None;
    };
    let raw = number.to_string();
    if raw.starts_with('-') {
        return None;
    }
    let (mantissa, exponent) = raw
        .split_once(['e', 'E'])
        .map_or(Some((raw.as_str(), 0)), |(m, e)| {
            Some((m, e.parse::<i32>().ok()?))
        })?;
    let (whole, decimal) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    let mut digits = format!("{whole}{decimal}");
    if digits.len() > 38 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let shift = exponent
        .checked_add(fraction)?
        .checked_sub(decimal.len() as i32)?;
    if !(-38..=38).contains(&shift) {
        return None;
    }
    if shift < 0 {
        let keep = digits.len().checked_sub((-shift) as usize)?;
        if !digits[keep..].bytes().all(|b| b == b'0') {
            return None;
        }
        digits.truncate(keep);
    } else {
        digits.extend(std::iter::repeat_n('0', shift as usize));
    }
    if digits.is_empty() {
        Some(0)
    } else {
        digits.parse().ok()
    }
}

fn totals(rows: &BTreeMap<String, i128>) -> Vec<Value> {
    rows.iter().map(|(currency,minor)| {
        let fraction = fraction_digits(currency).unwrap();
        let scale = 10i128.pow(fraction);
        let amount = if fraction == 0 { minor.to_string() } else {
            format!("{}.{:0width$}",minor/scale,minor%scale,width=fraction as usize)
        };
        json!({"currency":currency,"amount":amount,"minorUnits":minor.to_string(),"fractionDigits":fraction})
    }).collect()
}

#[derive(Default)]
struct Fact {
    products: BTreeSet<String>,
    amounts: BTreeSet<(String, i128)>,
}

pub(super) fn summary(conn: &Connection, args: &Value) -> Result<Value> {
    let selected = args["productIds"]
        .as_array()
        .context("catalogue.invalid_product_ids")?;
    ensure!(
        selected.len() <= 10_000
            && selected
                .iter()
                .all(|v| v.as_str().is_some_and(|s| !s.is_empty())),
        "catalogue.invalid_product_ids"
    );
    let accounts = match args.get("accountIds") {
        None | Some(Value::Null) => None,
        Some(Value::Array(values)) if values.iter().all(Value::is_string) => Some(values),
        _ => anyhow::bail!("catalogue.invalid_account_ids"),
    };
    let ids: BTreeSet<_> = selected.iter().filter_map(Value::as_str).collect();
    let mut products = BTreeMap::<String, (String, BTreeMap<String, i128>, usize)>::new();
    let mut facts = BTreeMap::<(String, String, String), Fact>::new();
    for id in ids {
        let body: Option<String> = conn
            .query_row("SELECT summary FROM products WHERE id=?1", [id], |row| {
                row.get(0)
            })
            .optional()?;
        let Some(body) = body else {
            continue;
        };
        let product: Value = serde_json::from_str(&body)?;
        products.insert(
            id.into(),
            (text(&product, "title").into(), BTreeMap::new(), 0),
        );
        if text(&product, "provider") == "local" {
            continue;
        }
        for row in related(conn, "acquisition", id)? {
            let account = text(&row, "accountId");
            if accounts.is_some_and(|ids| !ids.iter().any(|id| id.as_str() == Some(account))) {
                continue;
            }
            if row["observationKind"] == "localImport" {
                continue;
            }
            // Plain library/tag/grant observations are not separate purchases.
            if row["paidPriceBasis"] != "productLineTotal"
                && text(&row, "source") != "purchase"
                && text(&row, "orderId").is_empty()
            {
                continue;
            }
            let provider = text(&product, "provider").to_owned();
            let source_id = if text(&row, "sourceRecordId").is_empty() {
                text(&row, "id")
            } else {
                text(&row, "sourceRecordId")
            };
            let fact = facts
                .entry((provider, account.into(), source_id.into()))
                .or_default();
            fact.products.insert(id.into());
            if row["paidPriceBasis"] == "productLineTotal" {
                let currency = text(&row, "paidCurrency");
                if let Some(minor) = minor_units(&row["paidPrice"], currency) {
                    fact.amounts.insert((currency.into(), minor));
                }
            }
        }
    }
    for fact in facts.values() {
        if fact.products.len() == 1 && fact.amounts.len() == 1 {
            let product = products.get_mut(fact.products.first().unwrap()).unwrap();
            let (currency, minor) = fact.amounts.first().unwrap();
            let total = product.1.entry(currency.clone()).or_default();
            *total = total
                .checked_add(*minor)
                .context("catalogue.spending_overflow")?;
        } else {
            for id in &fact.products {
                products.get_mut(id).unwrap().2 += 1;
            }
        }
    }
    let mut aggregate = BTreeMap::<String, i128>::new();
    let mut skipped = 0usize;
    let mut output = Vec::new();
    for (id, (title, paid, mut unknown)) in products {
        if paid.is_empty() {
            skipped += 1;
            unknown = unknown.max(1);
        }
        for (currency, minor) in &paid {
            let total = aggregate.entry(currency.clone()).or_default();
            *total = total
                .checked_add(*minor)
                .context("catalogue.spending_overflow")?;
        }
        output.push(json!({"productId":id,"title":title,"totals":totals(&paid),"unknownPaymentCount":unknown}));
    }
    Ok(json!({"totals":totals(&aggregate),"products":output,"skippedCount":skipped}))
}
