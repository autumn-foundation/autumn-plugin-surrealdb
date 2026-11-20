#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::HashMap;

use autumn_web::config::DatabaseConfig;
use autumn_web::db::fleet_replication::FleetReplication;
use autumn_web::db::fleet_tombstones::FleetTombstones;
use autumn_web::db::{RuntimeConnection, create_pool};
use autumn_web::plugin_conformance::CheckStatus;
use autumn_web::plugin_conformance::fleet::{FleetFixture, run_fleet_conformance};
use autumn_web::reexports::diesel_async::pooled_connection::deadpool::Pool;
use autumn_web::replication::{FileDestination, ReplicaDestination, ReplicationSettings};

use super::*;

/// The kit fixture. With `replicated`, generations go to a local directory.
struct Fixture {
    replicated: bool,
}

/// The fixture with no replica.
const PLAIN: Fixture = Fixture { replicated: false };

fn control_pool(dir: &Path) -> Pool<RuntimeConnection> {
    create_pool(&DatabaseConfig {
        url: Some(format!("sqlite://{}", dir.join("control.db").display())),
        ..Default::default()
    })
    .unwrap()
    .unwrap()
}

fn test_engine() -> SurrealKvEngine {
    SurrealKvEngine::new()
        .resident_bytes(1024)
        .ship_every(Duration::from_hours(1))
        .migrations([Migration::new(
            "0001_entry",
            "DEFINE TABLE IF NOT EXISTS entry SCHEMALESS",
        )])
}

impl FleetFixture for Fixture {
    type Engine = SurrealKvEngine;

    async fn fleet(
        &self,
        config: DatabaseFleetConfig,
    ) -> Result<DatabaseFleet<SurrealKvEngine>, String> {
        let scratch = Path::new(&config.root)
            .parent()
            .ok_or("the root has no parent")?
            .to_path_buf();
        let fleet =
            DatabaseFleet::with_engine(&config, test_engine()).map_err(|e| e.to_string())?;
        if self.replicated {
            let destination: Arc<dyn ReplicaDestination> = Arc::new(
                FileDestination::new(scratch.join("replicas")).map_err(|e| e.to_string())?,
            );
            let clock = Arc::new(autumn_web::time::SystemClock);
            let tombstones = Arc::new(FleetTombstones::new(
                control_pool(&scratch),
                Arc::clone(&clock) as _,
            ));
            let replication = Arc::new(
                FleetReplication::new(
                    destination,
                    ReplicationSettings {
                        database_path: PathBuf::new(),
                        root: "app/kit".to_owned(),
                        sync_interval: Duration::from_secs(1),
                        snapshot_interval: Duration::from_hours(1),
                        max_wal_bytes: 4 * 1024 * 1024,
                        retention: Duration::from_hours(24),
                        verify_interval: None,
                    },
                    Duration::from_secs(30),
                    false,
                    clock,
                )
                .with_tombstones(tombstones),
            );
            fleet
                .install_replication(replication)
                .map_err(|_| "replication is installed twice".to_owned())?;
        }
        Ok(fleet)
    }

    async fn restore(
        &self,
        fleet: &DatabaseFleet<SurrealKvEngine>,
        key: &FleetDbKey,
    ) -> Option<Result<(), FleetError>> {
        if self.replicated {
            Some(fleet.restore_latest(key).await)
        } else {
            None
        }
    }

    async fn write(&self, handle: &SurrealDb, value: &str) -> Result<(), String> {
        let mut vars = serde_json::Map::new();
        vars.insert("v".to_owned(), value.into());
        handle
            .query_json(
                "CREATE entry SET v = $v, n = array::len((SELECT id FROM entry))",
                &vars,
            )
            .await
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    async fn read(&self, handle: &SurrealDb) -> Result<Vec<String>, String> {
        let rows = handle
            .query_json("SELECT v, n FROM entry ORDER BY n", &serde_json::Map::new())
            .await
            .map_err(|e| e.to_string())?;
        rows.first().map_or_else(
            || Ok(Vec::new()),
            |rows| {
                rows.iter()
                    .map(|row| {
                        row.get("v")
                            .and_then(serde_json::Value::as_str)
                            .map(str::to_owned)
                            .ok_or_else(|| format!("no value in {row}"))
                    })
                    .collect()
            },
        )
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_engine_passes_the_fleet_conformance_kit() {
    let scratch = tempfile::tempdir().unwrap();
    let report = run_fleet_conformance("surrealkv", &PLAIN, scratch.path()).await;
    assert!(report.passed(), "{}", report.to_text_report());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_replicated_engine_passes_every_check() {
    let scratch = tempfile::tempdir().unwrap();
    let fixture = Fixture { replicated: true };
    let report = run_fleet_conformance("surrealkv", &fixture, scratch.path()).await;
    assert!(report.passed(), "{}", report.to_text_report());
    for check in &report.checks {
        assert_eq!(
            check.status,
            CheckStatus::Pass,
            "{}: {}",
            check.name,
            check.message
        );
    }
}

fn fleet_config(root: &Path) -> DatabaseFleetConfig {
    let mut table = toml::Table::new();
    table.insert("mode".to_owned(), "tenant".into());
    table.insert("root".to_owned(), root.display().to_string().into());
    table.insert("create_on_demand".to_owned(), true.into());
    toml::Value::Table(table).try_into().unwrap()
}

fn key(fleet: &DatabaseFleet<SurrealKvEngine>, tenant: &str) -> FleetDbKey {
    fleet.key_for(tenant).unwrap()
}

fn snap_dir(scratch: &Path, name: &str) -> PathBuf {
    let dir = scratch.join(name);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unchanged_database_snapshots_to_the_same_bytes() {
    let scratch = tempfile::tempdir().unwrap();
    let fleet = PLAIN
        .fleet(fleet_config(&scratch.path().join("fleet")))
        .await
        .unwrap();
    let key = key(&fleet, "acme");
    let db = fleet.open(&key).await.unwrap();
    PLAIN.write(db.handle(), "one").await.unwrap();
    let path = fleet.path_of(&key);
    let engine = fleet.engine();
    let first = engine
        .snapshot(db.handle(), &path, &snap_dir(scratch.path(), "a"))
        .await
        .unwrap();
    let second = engine
        .snapshot(db.handle(), &path, &snap_dir(scratch.path(), "b"))
        .await
        .unwrap();
    assert_eq!(first.len(), 1);
    assert!(first[0].ends_with(SNAPSHOT_FILE));
    assert_eq!(
        std::fs::read(&first[0]).unwrap(),
        std::fs::read(&second[0]).unwrap()
    );
    PLAIN.write(db.handle(), "two").await.unwrap();
    let third = engine
        .snapshot(db.handle(), &path, &snap_dir(scratch.path(), "c"))
        .await
        .unwrap();
    assert_ne!(
        std::fs::read(&first[0]).unwrap(),
        std::fs::read(&third[0]).unwrap()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_closed_database_snapshots_its_data() {
    let scratch = tempfile::tempdir().unwrap();
    let fleet = PLAIN
        .fleet(fleet_config(&scratch.path().join("fleet")))
        .await
        .unwrap();
    let key = key(&fleet, "acme");
    let db = fleet.open(&key).await.unwrap();
    PLAIN.write(db.handle(), "kept").await.unwrap();
    let open = fleet
        .engine()
        .snapshot(
            db.handle(),
            &fleet.path_of(&key),
            &snap_dir(scratch.path(), "open"),
        )
        .await
        .unwrap();
    let handle = db.handle().clone();
    drop(db);
    fleet.close(&key).await;
    assert!(handle.is_closed());
    assert!(!fleet.engine().in_use(&handle));
    let err = handle.version().await.unwrap_err();
    assert!(matches!(err, SurrealDbError::Closed), "{err}");
    let closed = fleet
        .engine()
        .snapshot(
            &handle,
            &fleet.path_of(&key),
            &snap_dir(scratch.path(), "closed"),
        )
        .await
        .unwrap();
    assert_eq!(
        std::fs::read(&open[0]).unwrap(),
        std::fs::read(&closed[0]).unwrap()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_restored_export_imports_at_the_next_open() {
    let scratch = tempfile::tempdir().unwrap();
    let fleet = PLAIN
        .fleet(fleet_config(&scratch.path().join("fleet")))
        .await
        .unwrap();
    let source = key(&fleet, "acme");
    let db = fleet.open(&source).await.unwrap();
    PLAIN.write(db.handle(), "kept").await.unwrap();
    let files = fleet
        .engine()
        .snapshot(
            db.handle(),
            &fleet.path_of(&source),
            &snap_dir(scratch.path(), "s"),
        )
        .await
        .unwrap();
    drop(db);
    fleet.close(&source).await;
    // Put the export where a restore puts it, under the same key.
    let path = fleet.path_of(&source);
    std::fs::remove_dir_all(&path).unwrap();
    std::fs::create_dir_all(&path).unwrap();
    std::fs::copy(&files[0], path.join(SNAPSHOT_FILE)).unwrap();
    assert!(fleet.engine().exists(&path));
    assert_eq!(fleet.engine().epoch_stamp(&source, &path).unwrap(), None);
    // A closed snapshot of a database that no open imported copies the export.
    let copy = fleet
        .engine()
        .snapshot(&closed_handle(), &path, &snap_dir(scratch.path(), "copy"))
        .await
        .unwrap();
    assert_eq!(
        std::fs::read(&files[0]).unwrap(),
        std::fs::read(&copy[0]).unwrap()
    );
    let db = fleet.open(&source).await.unwrap();
    assert_eq!(PLAIN.read(db.handle()).await.unwrap(), ["kept"]);
    assert!(!path.join(SNAPSHOT_FILE).exists());
    assert!(!path.join(IMPORT_DIR).exists());
    drop(db);
    fleet.close_all().await;
}

/// A handle that is closed, for the snapshot of a database with no handle.
fn closed_handle() -> SurrealDb {
    let runtime = tokio::runtime::Handle::current();
    let scratch = tempfile::tempdir().unwrap();
    let store = scratch.path().join("x");
    let db = tokio::task::block_in_place(|| {
        runtime.block_on(SurrealDb::open_local(&store, SurrealDbConfig::default()))
    })
    .unwrap();
    db.close_now();
    db
}

#[tokio::test(flavor = "multi_thread")]
async fn the_epoch_stamp_is_the_stamp_of_the_open() {
    let scratch = tempfile::tempdir().unwrap();
    let fleet = DatabaseFleet::with_engine(&fleet_config(&scratch.path().join("f")), test_engine())
        .unwrap();
    let engine = fleet.engine();
    let key = key(&fleet, "acme");
    let path = fleet.path_of(&key);
    assert_eq!(engine.epoch_stamp(&key, &path).unwrap(), None);
    let token = FencingToken::try_from(7_i64).unwrap();
    let db = engine.open(&key, &path, Some(token)).await.unwrap();
    assert_eq!(engine.migrations_applied(), 1);
    engine.close(&db);
    drop(db);
    assert_eq!(engine.epoch_stamp(&key, &path).unwrap(), Some(7));
    let other = fleet.key_for("other").unwrap();
    assert_eq!(engine.epoch_stamp(&other, &path).unwrap(), None);
    let older = FencingToken::try_from(3_i64).unwrap();
    let err = engine.open(&key, &path, Some(older)).await.unwrap_err();
    assert!(
        matches!(err, FleetError::NewerEpoch { found: 7, .. }),
        "{err}"
    );
    // An open with no epoch keeps the stamp, and the migration runs once.
    let db = engine.open(&key, &path, None).await.unwrap();
    assert_eq!(engine.migrations_applied(), 1);
    assert_eq!(read_stamp(&db).await.unwrap(), Some((key.name(), 7)));
    let busy = engine.remove(&key, &path).unwrap_err();
    assert!(matches!(busy, FleetError::InUseElsewhere { .. }), "{busy}");
    engine.close(&db);
    drop(db);
    wait_released(&path.join(STORE_DIR), Duration::from_secs(10))
        .await
        .unwrap();
    assert!(engine.remove(&key, &path).unwrap());
    assert!(!engine.remove(&key, &path).unwrap());
    assert!(!engine.exists(&path));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_bad_migration_fails_the_open_and_closes_the_store() {
    let scratch = tempfile::tempdir().unwrap();
    let engine = SurrealKvEngine::new()
        .timeout(Duration::from_secs(2))
        .migrations([Migration::owned("0001_bad", "THROW \"no such step\"")]);
    let fleet =
        DatabaseFleet::with_engine(&fleet_config(&scratch.path().join("f")), engine).unwrap();
    let key = key(&fleet, "acme");
    let path = fleet.path_of(&key);
    let err = fleet.engine().open(&key, &path, None).await.unwrap_err();
    assert!(err.to_string().contains("migrate"), "{err}");
    wait_released(&path.join(STORE_DIR), Duration::from_secs(10))
        .await
        .unwrap();
    assert_eq!(fleet.engine().migrations_applied(), 0);
}

#[test]
fn the_identity_file_round_trips() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(read_identity_file(dir.path()).unwrap(), None);
    write_identity_file(dir.path(), "acme", 9).unwrap();
    assert_eq!(
        read_identity_file(dir.path()).unwrap(),
        Some(("acme".to_owned(), 9))
    );
    std::fs::write(dir.path().join(IDENTITY_FILE), "acme\n").unwrap();
    assert_eq!(read_identity_file(dir.path()).unwrap(), None);
    std::fs::write(dir.path().join(IDENTITY_FILE), "acme\nnine\n").unwrap();
    assert_eq!(read_identity_file(dir.path()).unwrap(), None);
}

#[test]
fn a_migration_keeps_its_name_and_text() {
    let fixed = Migration::new("0001", "DEFINE TABLE a");
    let owned = Migration::owned("0001", "DEFINE TABLE a");
    assert_eq!(fixed, owned);
    assert_eq!(fixed.name(), "0001");
    assert_eq!(fixed.sql(), "DEFINE TABLE a");
}

fn table(text: &str) -> toml::Table {
    toml::from_str(text).unwrap()
}

#[test]
fn the_config_reads_the_fleet_and_the_engine_keys() {
    let config = SurrealDbFleetConfig::from_table(
        "surrealdb_fleet",
        table(
            r#"
            mode = "tenant"
            root = "data/tenants"
            max_resident_bytes = 4096
            namespace = "shop"
            database = "main"
            timeout_ms = 1234
            ship_every_secs = 5
            resident_bytes = 1024
            [ownership]
            owner = "host-a"
            "#,
        ),
    )
    .unwrap();
    assert_eq!(config.fleet.root, "data/tenants");
    assert_eq!(config.fleet.max_resident_bytes, 4096);
    assert_eq!(config.namespace, "shop");
    assert_eq!(config.database, "main");
    assert_eq!(config.timeout_ms, 1234);
    assert_eq!(config.ship_every, Duration::from_secs(5));
    assert_eq!(config.resident_bytes, 1024);
    assert_eq!(config.fleet.ownership.as_ref().unwrap().owner, "host-a");
    let engine = config.engine();
    let text = format!("{engine:?}");
    assert!(text.contains("shop") && text.contains("1.234s"), "{text}");
    assert_eq!(engine.ship_mode().every(), Some(Duration::from_secs(5)));
    let defaults = SurrealDbFleetConfig::new(fleet_config(Path::new("x")));
    assert_eq!(
        defaults.engine().ship_mode().every(),
        Some(DEFAULT_SHIP_EVERY)
    );
    defaults.validate().unwrap();
}

#[test]
fn the_config_refuses_bad_values() {
    let cases = [
        ("namespace = 3", "namespace"),
        ("namespace = \"a b\"", "namespace"),
        ("database = \"\"", "database"),
        ("timeout_ms = 0", "timeout_ms"),
        ("timeout_ms = \"x\"", "timeout_ms"),
        ("ship_every_secs = \"x\"", "ship_every_secs"),
        ("ship_every_secs = 0", "ship_every_secs"),
        ("resident_bytes = -1", "resident_bytes"),
        ("resident_bytes = 0", "resident_bytes"),
    ];
    for (extra, needle) in cases {
        let text = format!("mode = \"tenant\"\nroot = \"r\"\n{extra}");
        let err = SurrealDbFleetConfig::from_table("surrealdb_fleet", table(&text)).unwrap_err();
        assert!(err.to_string().contains(needle), "{text}: {err}");
    }
    let err = SurrealDbFleetConfig::from_table(
        "surrealdb_fleet",
        table("mode = \"tenant\"\nroot = \" \""),
    )
    .unwrap_err();
    assert!(err.to_string().contains("root"), "{err}");
}

/// An [`Env`] backed by a map.
#[derive(Default)]
struct MapEnv {
    vars: HashMap<String, String>,
}

impl Env for MapEnv {
    fn var(&self, key: &str) -> Result<String, std::env::VarError> {
        self.vars
            .get(key)
            .cloned()
            .ok_or(std::env::VarError::NotPresent)
    }
}

#[test]
fn variables_set_the_root_and_the_owner() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("autumn.toml"),
        "[surrealdb_fleet]\nmode = \"tenant\"\nroot = \"from-file\"\nnamespace = \"shop\"\n",
    )
    .unwrap();
    let mut env = MapEnv::default();
    for (key, value) in [
        ("AUTUMN_SURREALDB_FLEET__ROOT", "from-env"),
        ("AUTUMN_SURREALDB_FLEET__OWNERSHIP__OWNER", "host-b"),
        ("AUTUMN_MANIFEST_DIR", &dir.path().display().to_string()),
    ] {
        env.vars.insert(key.to_owned(), value.to_owned());
    }
    let config = SurrealDbFleetConfig::resolve_with_env("surrealdb_fleet", &env).unwrap();
    assert_eq!(config.fleet.root, "from-env");
    assert_eq!(config.namespace, "shop");
    assert_eq!(config.fleet.ownership.unwrap().owner, "host-b");
}

#[test]
fn the_plugin_resolves_an_explicit_config() {
    let good = SurrealDbFleetConfig::new(fleet_config(Path::new("data")));
    let explicit = |config: &SurrealDbFleetConfig| {
        SurrealDbFleetPlugin::resolve(&FleetSource::Explicit(Box::new(config.clone())))
    };
    assert!(explicit(&good).is_ok());
    let mut bad = good.clone();
    bad.resident_bytes = 0;
    assert!(explicit(&bad).is_err());
    let plugin = SurrealDbFleetPlugin::default()
        .config_section("tenants")
        .migrations([Migration::new("0001", "DEFINE TABLE a")]);
    let text = format!("{plugin:?}");
    assert!(
        text.contains("tenants") && text.contains("migrations: 1"),
        "{text}"
    );
    assert_eq!(plugin.name(), FLEET_PLUGIN_NAME);
    let explicit = SurrealDbFleetPlugin::new().config(good);
    assert!(format!("{explicit:?}").contains("(explicit)"));
}

#[tokio::test(flavor = "multi_thread")]
async fn the_runtime_closes_every_database_at_shutdown() {
    let scratch = tempfile::tempdir().unwrap();
    let config = SurrealDbFleetConfig::new(fleet_config(&scratch.path().join("fleet")));
    let state = AppState::for_test();
    let runtime = start(&state, &config, config.engine()).await.unwrap();
    let lease = FleetSurrealDb::for_key(&state, "acme").await.unwrap();
    lease.version().await.unwrap();
    let handle = (*lease).clone();
    assert_eq!(runtime.fleet.open_keys().len(), 1);
    drop(lease);
    runtime.shutdown().await;
    assert!(handle.is_closed());
}

#[tokio::test(flavor = "multi_thread")]
async fn ownership_needs_a_store() {
    let scratch = tempfile::tempdir().unwrap();
    let mut table = toml::Table::new();
    table.insert("mode".to_owned(), "tenant".into());
    table.insert(
        "root".to_owned(),
        scratch.path().display().to_string().into(),
    );
    let mut ownership = toml::Table::new();
    ownership.insert("owner".to_owned(), "host-a".into());
    table.insert("ownership".to_owned(), ownership.into());
    let config = SurrealDbFleetConfig::new(toml::Value::Table(table).try_into().unwrap());
    let state = AppState::for_test();
    let err = start(&state, &config, config.engine()).await.err().unwrap();
    assert!(err.contains("ownership"), "{err}");
}
