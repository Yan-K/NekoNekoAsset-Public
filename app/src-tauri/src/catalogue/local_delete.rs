use super::*;

impl Catalogue {
    /// Storage has staged the checked, exclusively owned directories first.
    /// Tombstones, dependent records, collection membership and its cleanup job
    /// commit together, so a database failure leaves every product intact.
    pub(crate) fn delete_local_products(&self, ids: &[String], job: Value) -> Result<()> {
        validate("job", text(&job, "id"), &job)?;
        let removed: BTreeSet<_> = ids.iter().cloned().collect();
        ensure!(
            !removed.is_empty() && removed.len() == ids.len(),
            "invalid_arguments"
        );
        self.mutate(|tx, projections| {
            for id in &removed {
                let product =
                    raw(tx, "product", id)?.ok_or_else(|| anyhow::anyhow!("product_missing"))?;
                ensure!(product["provider"] == "local", "local_product_required");
            }
            for id in &removed {
                Self::write(tx, projections, "product", id, None)?;
                Self::write(tx, projections, "overlay", id, None)?;
                for kind in [
                    "acquisition",
                    "entitlement",
                    "remote_file",
                    "artifact",
                    "download_intent",
                ] {
                    for record in related(tx, kind, id)? {
                        Self::write(tx, projections, kind, text(&record, "id"), None)?;
                    }
                }
                Self::write(tx, projections, "download_intent", id, None)?;
            }
            for mut collection in rows(tx, "collection")? {
                let members = strings(&collection["productIds"]);
                let retained: Vec<_> = members
                    .iter()
                    .filter(|id| !removed.contains(*id))
                    .cloned()
                    .collect();
                if retained.len() != members.len() {
                    collection["productIds"] = json!(retained);
                    Self::write(
                        tx,
                        projections,
                        "collection",
                        &text(&collection, "id").to_owned(),
                        Some(collection),
                    )?;
                }
            }
            // A later retry of a partial batch import must not recreate a deleted
            // product. Keep the surviving groups and their completion indexes.
            for mut import in rows(tx, "job")?
                .into_iter()
                .filter(|record| record["type"] == "storage:import_local")
            {
                let product_ids = strings(&import["productIds"]);
                let Some(groups) = import["localPlan"]["groups"].as_array() else {
                    continue;
                };
                let completed = import["completedGroupIndexes"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default();
                let mut retained_groups = Vec::new();
                let mut retained_ids = Vec::new();
                let mut retained_completed = Vec::new();
                for (index, group) in groups.iter().enumerate() {
                    if removed.contains(text(group, "product_id"))
                        || product_ids
                            .get(index)
                            .is_some_and(|id| removed.contains(id))
                    {
                        continue;
                    }
                    if completed.contains(&json!(index)) {
                        retained_completed.push(json!(retained_groups.len()));
                    }
                    retained_groups.push(group.clone());
                    if let Some(id) = product_ids.get(index) {
                        retained_ids.push(id.clone());
                    }
                }
                if retained_groups.len() == groups.len() {
                    continue;
                }
                let import_id = text(&import, "id").to_owned();
                if retained_groups.is_empty() {
                    Self::write(tx, projections, "job", &import_id, None)?;
                } else {
                    import["localPlan"]["groups"] = json!(retained_groups);
                    import["productIds"] = json!(retained_ids);
                    import["completedProductIds"] = json!(strings(&import["completedProductIds"])
                        .into_iter()
                        .filter(|id| !removed.contains(id))
                        .collect::<Vec<_>>());
                    import["completedGroupIndexes"] = json!(retained_completed);
                    Self::write(tx, projections, "job", &import_id, Some(import))?;
                }
            }
            Self::write(
                tx,
                projections,
                "job",
                &text(&job, "id").to_owned(),
                Some(job),
            )?;
            Ok(())
        })
    }
}
