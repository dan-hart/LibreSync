#![cfg(feature = "managed")]
use sp_p2p::managed::ManagedMomentum;
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
#[test]
fn actual_momentum_action_snapshot_applied_after_durable_app_save() {
    let a_dir = tempfile::tempdir().unwrap();
    let b_dir = tempfile::tempdir().unwrap();
    let a = ManagedMomentum::open(
        a_dir.path(),
        "Phone",
        Arc::new(libresync::MemoryKeyStore::new()),
    )
    .unwrap();
    let b = ManagedMomentum::open(
        b_dir.path(),
        "Computer",
        Arc::new(libresync::MemoryKeyStore::new()),
    )
    .unwrap();
    a.session.start().unwrap();
    b.session.start().unwrap();
    let action = sp_oplog::Action::AddTask {
        task: sp_model::Task::new("Managed sync task", "INBOX_PROJECT"),
        bottom: true,
    };
    let op = action.to_op("phone", &mut Default::default());
    a.publish(&op, &action).unwrap();
    let mut initial = sp_model::AppData::fresh();
    sp_oplog::apply(&mut initial, &action);
    a.publish_snapshot(&serde_json::to_vec(&initial).unwrap())
        .unwrap();
    b.session
        .connect(
            &a.session
                .create_invitation(Duration::from_secs(120))
                .unwrap(),
        )
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let inbox = b.inbox().unwrap();
        if inbox.ops.len() == 1 && inbox.snapshot.is_some() {
            let received = sp_model::AppData::from_backup(
                serde_json::from_slice(&inbox.snapshot.as_ref().unwrap().json().unwrap()).unwrap(),
            )
            .unwrap();
            assert_eq!(received.task.entities.len(), 1);
            assert_eq!(inbox.ops[0].decode().unwrap().0.id, op.id);
            assert_eq!(a.session.snapshot().unwrap().peers[0].applied.sequence, 0);
            let save = b_dir.path().join("app-transaction.json");
            std::fs::write(&save, serde_json::to_vec(&received).unwrap()).unwrap();
            std::fs::File::open(&save).unwrap().sync_all().unwrap();
            b.acknowledge_committed(&inbox).unwrap();
            break;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(20));
    }
    let deadline = Instant::now() + Duration::from_secs(10);
    while a.session.snapshot().unwrap().peers[0].applied.sequence == 0 {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(20));
    }
}
