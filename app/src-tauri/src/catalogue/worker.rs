//! A single owner for causal history; local catalogue commands only enqueue SQL intents.
use super::*;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::time::Duration;

type Task = Box<dyn FnOnce(&Catalogue) + Send>;
enum Request {
    Wake,
    Task(Task),
    Stop,
}
#[derive(Default)]
pub(super) struct Worker {
    sender: Mutex<Option<mpsc::Sender<Request>>>,
    wake_pending: AtomicBool,
    stopping: AtomicBool,
    thread: Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl Drop for Catalogue {
    fn drop(&mut self) {
        if !self.is_sync_worker && Arc::strong_count(&self.inner) == 2 {
            if let Ok(sender) = self.worker.sender.lock() {
                let Some(sender) = sender.as_ref() else {
                    return;
                };
                self.worker.stopping.store(true, Ordering::Release);
                let _ = sender.send(Request::Stop);
            }
            if let Ok(mut thread) = self.worker.thread.lock() {
                if let Some(thread) = thread.take() {
                    // The application can close while a cold decoder finishes.
                    // Tests wait only to release their isolated temporary fixture.
                    #[cfg(test)]
                    let _ = thread.join();
                    #[cfg(not(test))]
                    drop(thread);
                }
            }
        }
    }
}

impl Catalogue {
    /// Starts one history worker for all clones. Repeated startup calls are harmless.
    pub fn start_sync_worker(&self) -> Result<()> {
        let mut sender = self
            .worker
            .sender
            .lock()
            .map_err(|_| anyhow::anyhow!("catalogue.lock"))?;
        if sender.is_some() {
            return Ok(());
        }
        let (send, receive) = mpsc::channel();
        let mut catalogue = self.clone();
        catalogue.is_sync_worker = true;
        let thread = std::thread::Builder::new().name("catalogue-sync".into()).spawn(move || {
            loop {
                if catalogue.worker.stopping.load(Ordering::Acquire) { return; }
                // Preparation/replay errors preserve the durable intents. A later explicit
                // barrier reports the error; SQLite reads and writes stay available.
                let result = catalogue.flush_sync_outbox();
                if let Ok(conn) = catalogue.inner.lock() {
                    let _ = conn.execute("INSERT INTO meta(key,value) VALUES('sync_worker_error',?1) ON CONFLICT(key) DO UPDATE SET value=excluded.value", [if result.is_ok() { "" } else { "catalogue.sync_history_unavailable" }]);
                }
                loop {
                    match receive.recv_timeout(Duration::from_secs(1)) {
                        Ok(Request::Wake) => { catalogue.worker.wake_pending.store(false, Ordering::Release); break; }
                        Ok(Request::Task(task)) => { task(&catalogue); break; },
                        Ok(Request::Stop) | Err(mpsc::RecvTimeoutError::Disconnected) => return,
                        Err(mpsc::RecvTimeoutError::Timeout) => {
                            if Arc::strong_count(&catalogue.inner) == 1 { return; }
                        }
                    }
                }
            }
        })?;
        *sender = Some(send);
        *self
            .worker
            .thread
            .lock()
            .map_err(|_| anyhow::anyhow!("catalogue.lock"))? = Some(thread);
        Ok(())
    }

    pub(super) fn wake_sync_worker(&self) {
        if self.worker.wake_pending.swap(true, Ordering::AcqRel) {
            return;
        }
        if let Ok(sender) = self.worker.sender.lock() {
            if let Some(sender) = sender.as_ref() {
                if sender.send(Request::Wake).is_ok() {
                    return;
                }
            }
        }
        self.worker.wake_pending.store(false, Ordering::Release);
    }

    pub(super) fn on_sync_worker<T: Send + 'static>(
        &self,
        run: impl FnOnce(&Catalogue) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        self.start_sync_worker()?;
        let (send, receive) = mpsc::sync_channel(1);
        self.worker
            .sender
            .lock()
            .map_err(|_| anyhow::anyhow!("catalogue.lock"))?
            .as_ref()
            .context("catalogue.sync_worker_unavailable")?
            .send(Request::Task(Box::new(move |catalogue| {
                let _ = send.send(run(catalogue));
            })))
            .map_err(|_| anyhow::anyhow!("catalogue.sync_worker_unavailable"))?;
        receive
            .recv()
            .map_err(|_| anyhow::anyhow!("catalogue.sync_worker_unavailable"))?
    }

    /// Replay immutable before/after intent in order, outside the SQLite mutex.
    /// Delta persistence and acknowledgement share a transaction, so a crash
    /// either replays the same intent or observes both its history and its ack.
    pub(super) fn flush_sync_outbox(&self) -> Result<()> {
        self.ensure_document()?;
        loop {
            if self.is_sync_worker && self.worker.stopping.load(Ordering::Acquire) {
                return Ok(());
            }
            let batch = {
                let conn = self
                    .inner
                    .lock()
                    .map_err(|_| anyhow::anyhow!("catalogue.lock"))?;
                let mut stmt = conn.prepare("SELECT seq,kind,id,before_json,after_json FROM sync_outbox ORDER BY seq LIMIT 128")?;
                let records = stmt
                    .query_map([], |row| {
                        Ok((
                            row.get::<_, i64>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, String>(2)?,
                            row.get::<_, Option<String>>(3)?,
                            row.get::<_, Option<String>>(4)?,
                        ))
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                records
            };
            if batch.is_empty() {
                return Ok(());
            }
            let mut doc = self
                .document_cache
                .lock()
                .map_err(|_| anyhow::anyhow!("catalogue.lock"))?
                .take()
                .context("catalogue.sync_document_unavailable")?;
            let heads = doc.get_heads();
            for (_, kind, id, before, after) in &batch {
                let before: Option<Value> = before
                    .as_ref()
                    .map(|body| serde_json::from_str(body))
                    .transpose()?;
                let after: Option<Value> = after
                    .as_ref()
                    .map(|body| serde_json::from_str(body))
                    .transpose()?;
                sync::write_entity(&mut doc, kind, id, before.as_ref(), after.as_ref())?;
            }
            let bytes = doc.save_after(&heads);
            let mut conn = self
                .inner
                .lock()
                .map_err(|_| anyhow::anyhow!("catalogue.lock"))?;
            let tx = conn.transaction()?;
            if !bytes.is_empty() {
                Self::persist_delta(&tx, &bytes)?;
                tx.execute("INSERT INTO meta(key,value) VALUES('sync_revision',1) ON CONFLICT(key) DO UPDATE SET value=CAST(value AS INTEGER)+1", [])?;
            }
            let conflicts = self.current_conflicts(&tx)?;
            let remaining = sync::remaining_conflicts(&doc, &conflicts)?;
            if remaining != conflicts {
                Self::save_conflicts(&tx, &remaining)?;
                Self::bump_revision(&tx)?;
            }
            tx.execute(
                "DELETE FROM sync_outbox WHERE seq<=?1",
                [batch.last().unwrap().0],
            )?;
            tx.commit()?;
            *self
                .document_cache
                .lock()
                .map_err(|_| anyhow::anyhow!("catalogue.lock"))? = Some(doc);
        }
    }

    pub(super) fn digest(bytes: &[u8]) -> String {
        use sha2::{Digest, Sha256};
        format!("{:x}", Sha256::digest(bytes))
    }
    pub(super) fn persist_delta(conn: &Connection, bytes: &[u8]) -> Result<()> {
        conn.execute("INSERT INTO am_changes(body) VALUES(?1)", [bytes])?;
        let seq = conn.last_insert_rowid();
        conn.execute(
            "INSERT OR REPLACE INTO sync_trust(key,digest) VALUES(?1,?2)",
            params![format!("delta:{seq}"), Self::digest(bytes)],
        )?;
        Ok(())
    }
}
