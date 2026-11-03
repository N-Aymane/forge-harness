//! Validate SQLite reservations across independent operating-system processes.
use forge_harness::storage::Store;

#[test]
#[ignore = "helper invoked by the parent test"]
fn reservation_worker() {
    let Ok(path) = std::env::var("FORGE_TEST_LEDGER") else {
        return;
    };
    let store = Store::open(std::path::Path::new(&path)).unwrap();
    std::process::exit(
        if store
            .reserve("worker", "project", 1, Some(60), None)
            .is_ok()
        {
            0
        } else {
            42
        },
    );
}

#[test]
fn two_processes_cannot_reserve_the_same_balance() {
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("ledger.db");
    let s = Store::open(&path).unwrap();
    s.set_budget("project", "project", Some(100)).unwrap();
    let spawn = || {
        std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--ignored", "--exact", "reservation_worker"])
            .env("FORGE_TEST_LEDGER", &path)
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap()
    };
    let mut a = spawn();
    let mut b = spawn();
    let mut codes = [
        a.wait().unwrap().code().unwrap(),
        b.wait().unwrap().code().unwrap(),
    ];
    codes.sort();
    assert_eq!(codes, [0, 42]);
    assert_eq!(s.ledger().unwrap().len(), 1);
}
