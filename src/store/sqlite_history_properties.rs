use super::*;
use proptest::prelude::*;

#[derive(Debug)]
enum Op { Open { indexed: bool }, Count, Checkpoint(u64), Close(u64) }

#[derive(Debug, Default, Clone, PartialEq)]
struct Pass { indexed: bool, counted: bool, ms: u64, closed: bool }

fn apply(model: &mut Vec<Pass>, op: &Op) {
    match *op {
        Op::Open { indexed } => model.push(Pass { indexed, ..Pass::default() }),
        Op::Count => model.last_mut().unwrap().counted = true,
        Op::Checkpoint(ms) => model.last_mut().unwrap().ms = ms,
        Op::Close(ms) => { let p = model.last_mut().unwrap(); p.ms = ms; p.closed = true; }
    }
}

fn observe(db: &Database, store: &Store, model: &[Pass], writes: usize) {
    let actual = db.query("SELECT track_id IS NOT NULL,counted,listened_ms,ended_at IS NOT NULL FROM play_history ORDER BY id", |r|Ok(Pass {
        indexed:r.get(0)?, counted:r.get(1)?, ms:r.get(2)?, closed:r.get(3)?
    }));
    assert_eq!(actual, model);
    let counted: Vec<_> = model.iter().enumerate().filter(|(_,p)|p.counted).collect();
    let stats: Vec<_> = counted.iter().filter(|(_,p)|p.indexed).collect();
    let expected = stats.last().map(|(i,_)|(3, stats.len() as u64, Some(*i as i64+100)));
    assert_eq!(db.stats(), expected.into_iter().collect::<Vec<_>>());
    let page = store.history_page(0, 50).unwrap();
    assert_eq!(page.revision, counted.len() as u64);
    assert_eq!(page.entries.iter().map(|p|p.played_at).collect::<Vec<_>>(),
        counted.iter().rev().map(|(i,_)| *i as i64+100).collect::<Vec<_>>());
    assert_eq!(store.stats_revision().unwrap(), writes as u64);
}

proptest! {
    #[test]
    fn sqlite_failure_leaves_committed_prefix_then_retries_once(
        passes in prop::collection::vec((any::<bool>(),any::<bool>(),any::<bool>(),1..10000u64),1..8),
        fail_at in 0..32usize, deferred in any::<bool>(),
    ) {
        let (db, mut store) = Database::open();
        let mut pending = PendingHistory::default();
        let mut ops = Vec::new();
        for (i,(indexed,counted,early_count,ms)) in passes.into_iter().enumerate() {
            let id = i as u64+1;
            pending.push(HistoryEvent::Open { play:id, record:play(id,indexed.then_some(3),"Pass",i as i64+100) });
            ops.push(Op::Open { indexed });
            if counted && early_count { pending.push(HistoryEvent::Count { play:id }); ops.push(Op::Count); }
            pending.push(HistoryEvent::Checkpoint {play:id,listened_ms:ms/2}); ops.push(Op::Checkpoint(ms/2));
            if counted && !early_count { pending.push(HistoryEvent::Count { play:id }); ops.push(Op::Count); }
            pending.push(HistoryEvent::Close {play:id,listened_ms:ms,ended_at:i as i64+120,reason:EndReason::Stopped}); ops.push(Op::Close(ms));
        }
        // Every statement modifying a play has run when the revision write fails.
        // Even multi-table Count must leave precisely the previous prefix visible.
        let k = fail_at % ops.len();
        // A deferred foreign key lets all SQL statements succeed and fails COMMIT itself.
        store.connection.execute_batch("CREATE TEMP TABLE parent(id INTEGER PRIMARY KEY);
            CREATE TEMP TABLE child(id INTEGER REFERENCES parent(id) DEFERRABLE INITIALLY DEFERRED)").unwrap();
        let inject = if deferred { "INSERT INTO child VALUES(1)" } else { "SELECT RAISE(ABORT,'injected failure')" };
        store.connection.execute_batch(&format!("CREATE TEMP TRIGGER fail_write BEFORE UPDATE ON history_control WHEN OLD.writes={k} BEGIN {inject}; END")).unwrap();
        let stopped = pending.write(&mut store);
        let error = stopped.error.unwrap().to_string();
        prop_assert!(error.contains(if deferred { "FOREIGN KEY constraint failed" } else { "injected failure" }), "{error}");
        prop_assert_eq!(pending.len(), ops.len()-k);
        let mut model = Vec::new();
        for op in &ops[..k] { apply(&mut model,op); }
        observe(&db,&store,&model,k);
        store.connection.execute_batch("DROP TRIGGER fail_write").unwrap();
        let resumed = pending.write(&mut store);
        prop_assert!(resumed.error.is_none() && pending.is_empty());
        for op in &ops[k..] { apply(&mut model,op); }
        observe(&db,&store,&model,ops.len());
    }
}
