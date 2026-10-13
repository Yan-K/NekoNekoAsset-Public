//! Bounded native ingestion staging using the existing catalogue transaction.
use crate::catalogue::Catalogue;
use anyhow::Result;
use serde_json::Value;
use std::collections::BTreeMap;

pub(crate) struct RecordBatch<'a> {
    catalogue: &'a Catalogue,
    initial: BTreeMap<(String, String), Option<Value>>,
    staged: BTreeMap<(String, String), Value>,
}
impl<'a> RecordBatch<'a> {
    pub fn new(catalogue: &'a Catalogue) -> Self {
        Self {
            catalogue,
            initial: BTreeMap::new(),
            staged: BTreeMap::new(),
        }
    }
    pub fn get(&mut self, kind: &str, id: &str) -> Result<Option<Value>> {
        let key = (kind.to_owned(), id.to_owned());
        if let Some(value) = self.staged.get(&key) {
            return Ok(Some(value.clone()));
        }
        if let Some(value) = self.initial.get(&key) {
            return Ok(value.clone());
        }
        let value = self.catalogue.get(kind, id)?;
        self.initial.insert(key, value.clone());
        Ok(value)
    }
    pub fn stage(&mut self, kind: &str, id: &str, value: Value) -> Result<()> {
        self.get(kind, id)?;
        let key = (kind.to_owned(), id.to_owned());
        if self.initial.get(&key).and_then(Option::as_ref) == Some(&value) {
            self.staged.remove(&key);
        } else {
            self.staged.insert(key, value);
        }
        Ok(())
    }
    pub fn related(&mut self, kind: &str, product_id: &str) -> Result<Vec<Value>> {
        let mut result: BTreeMap<String, Value> = self
            .catalogue
            .related(kind, product_id)?
            .into_iter()
            .filter_map(|value| {
                value["id"]
                    .as_str()
                    .map(str::to_owned)
                    .map(|id| (id, value))
            })
            .collect();
        for ((staged_kind, id), value) in &self.staged {
            if staged_kind == kind && value["productId"] == product_id {
                result.insert(id.clone(), value.clone());
            }
        }
        Ok(result.into_values().collect())
    }
    pub fn len(&self) -> usize {
        self.staged.len()
    }
    /// Prepared catalogue facts only. Callers must not stage transient provider
    /// capabilities; this permits durable journals before visible publication.
    pub fn staged_records(&self) -> Vec<(String, String, Value)> {
        self.staged
            .iter()
            .map(|((kind, id), value)| (kind.clone(), id.clone(), value.clone()))
            .collect()
    }
    #[cfg(test)]
    pub fn commit(self) -> Result<usize> {
        self.try_commit()?
            .ok_or_else(|| anyhow::anyhow!("CONCURRENT_UPDATE"))
    }
    pub fn try_commit(self) -> Result<Option<usize>> {
        let count = self.staged.len();
        if count != 0 {
            let expected = self
                .initial
                .into_iter()
                .map(|((kind, id), value)| (kind, id, value))
                .collect();
            if !self.catalogue.put_many_if_unchanged(
                self.staged
                    .into_iter()
                    .map(|((kind, id), value)| (kind, id, value))
                    .collect(),
                expected,
            )? {
                return Ok(None);
            }
        }
        Ok(Some(count))
    }
}
