use super::*;

#[test]
fn queued_job_query_filters_history_and_preserves_exact_scope_and_order() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch("CREATE TABLE entities(kind TEXT NOT NULL,id TEXT NOT NULL,product_id TEXT NOT NULL DEFAULT '',body TEXT NOT NULL,PRIMARY KEY(kind,id));").unwrap();
    conn.execute_batch("WITH RECURSIVE n(value) AS (VALUES(1) UNION ALL SELECT value+1 FROM n WHERE value<10000) INSERT INTO entities(kind,id,body) SELECT 'job',printf('history-%05d',value),json_object('id',printf('history-%05d',value),'type','download','state','completed') FROM n;").unwrap();
    for (id, kind, state, job_type, store) in [
        ("b-download", "job", "queued", "download", "BOOTH"),
        ("a-login", "job", "queued", "jinxxyLogin", "Jinxxy"),
        ("c-local", "job", "queued", "storage:import_local", ""),
        ("d-sync", "job", "queued", "jinxxySync", "Jinxxy"),
        ("e-running", "job", "running", "download", "Jinxxy"),
        ("f-failed", "job", "failed", "download", "Jinxxy"),
        ("g-other-kind", "product", "queued", "download", "Jinxxy"),
    ] {
        set_raw(
            &conn,
            kind,
            id,
            &json!({"id":id,"state":state,"type":job_type,"store":store}),
        )
        .unwrap();
    }
    let ids = |job_type, store| {
        queued_jobs(&conn, job_type, store)
            .unwrap()
            .iter()
            .map(|job| text(job, "id").to_owned())
            .collect::<Vec<_>>()
    };
    assert_eq!(
        ids(None, None),
        ["a-login", "b-download", "c-local", "d-sync"]
    );
    assert_eq!(ids(Some("download"), None), ["b-download"]);
    assert_eq!(ids(Some("storage:import_local"), None), ["c-local"]);
    assert_eq!(ids(None, Some("Jinxxy")), ["a-login", "d-sync"]);
    assert_eq!(ids(Some("jinxxySync"), Some("Jinxxy")), ["d-sync"]);
    assert!(ids(Some("download"), Some("Jinxxy")).is_empty());
    assert!(ids(Some("download' OR 1=1 --"), None).is_empty());
    assert_eq!(
        conn.query_row("SELECT COUNT(*) FROM entities", [], |row| row
            .get::<_, i64>(0))
            .unwrap(),
        10007
    );
}
