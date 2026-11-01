//! User-defined categories. Source inference and AI only produce the fixed keys,
//! so these IDs reach a product solely through its explicit category override.
use super::*;

const PREFIX: &str = "custom:";

/// Pure record validation accepts the stored shape; user writes also check existence.
pub(super) fn is_custom(category: &str) -> bool {
    category
        .strip_prefix(PREFIX)
        .is_some_and(|id| Uuid::parse_str(id).is_ok())
}

pub(super) fn missing(conn: &Connection, category: &str) -> Result<bool> {
    Ok(is_custom(category) && raw(conn, "custom_category", category)?.is_none())
}

impl Catalogue {
    pub(super) fn custom_category(&self, args: Value) -> Result<Value> {
        let op = text(&args, "op").to_owned();
        self.mutate(|tx, projections| {
            if op == "delete" {
                let id = text(&args, "id");
                ensure!(
                    raw(tx, "custom_category", id)?.is_some(),
                    "catalogue.entity_missing"
                );
                // Its products return to automatic classification, as clearing the override does.
                let products = tx
                    .prepare("SELECT id FROM entities WHERE kind='overlay' AND json_extract(body,'$.category')=?1")?
                    .query_map([id], |row| row.get::<_, String>(0))?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                for product in products {
                    let mut overlay = raw(tx, "overlay", &product)?.context("catalogue.entity_missing")?;
                    overlay.as_object_mut().unwrap().remove("category");
                    Self::write(tx, projections, "overlay", &product, Some(overlay))?;
                }
                Self::write(tx, projections, "custom_category", id, None)?;
            } else {
                ensure!(op == "create" || op == "update", "catalogue.invalid_operation");
                let id = if op == "create" {
                    format!("{PREFIX}{}", Uuid::new_v4())
                } else {
                    text(&args, "id").to_owned()
                };
                ensure!(
                    op == "create" || raw(tx, "custom_category", &id)?.is_some(),
                    "catalogue.entity_missing"
                );
                let name = text(&args, "name").trim().to_owned();
                ensure!(!name.is_empty(), "catalogue.name_required");
                let key = crate::taxonomy::normalize(&name);
                ensure!(
                    !rows(tx, "custom_category")?.iter().any(|row| text(row, "id") != id
                        && crate::taxonomy::normalize(text(row, "name")) == key),
                    "catalogue.already_exists"
                );
                let value = json!({"id":id,"name":name});
                validate("custom_category", &id, &value)?;
                Self::write(tx, projections, "custom_category", &id, Some(value))?;
            }
            Ok(json!({"ok":true,"customCategories":rows(tx, "custom_category")?}))
        })
    }
}
