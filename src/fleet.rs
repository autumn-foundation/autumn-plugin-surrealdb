//! SurrealDB fleets: one embedded SurrealKV database for each tenant or slot (feature `fleet`).
//!
//! # Contract
//!
//! [`SurrealKvEngine`] is the SurrealKV engine of an Autumn [`DatabaseFleet`]. Autumn core does
//! the keys, the paths, the opens, the eviction, the memory budget, the ownership and the fence.
//! The engine does the rest:
//!
//! - The database path is a directory. The SurrealKV store is in `store/`.
//! - `open` opens the store and applies the migrations. The record
//!   `autumn_fleet_identity:stamp` holds the key name and the epoch. SurrealKV locks the store
//!   for one handle, so the check and the stamp have no other writer.
//! - `open` refuses a store of another key ([`FleetError::Misplaced`]) and a newer epoch
//!   ([`FleetError::NewerEpoch`]). It never lowers the stamp.
//! - `close` refuses new calls. The store closes when the last running call ends.
//!   With no running call, `close` waits up to the timeout for the store to close.
//!   `in_use` counts the running calls.
//! - `snapshot` writes a SurrealQL export to `dir/<file name>/snapshot.surql`. The export of
//!   unchanged data has the same bytes. A closed database opens again for the export.
//! - A restore puts `snapshot.surql` in the database directory. The next `open` imports it into
//!   a new store, and then deletes it.
//! - `remove` deletes the directory. It refuses a store that another handle holds.
//! - The file `identity` beside the store repeats the key and the epoch for `epoch_stamp`.
//! - The ship mode is [`ShipMode::Generations`].
//! - [`SurrealDbFleetPlugin`] reads `[surrealdb_fleet]`, makes the fleet and puts it in the app
//!   state. [`FleetSurrealDb`] is the extractor. It opens the database of the request tenant.
//! - With `[surrealdb_fleet.ownership]`, the plugin claims its slot ranges before the first open.
//! - The plugin closes idle databases, and closes all databases at shutdown. This runs in each
//!   role: a role that opens a database must also close it.

use std::borrow::Cow;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use autumn_web::app::AppBuilder;
use autumn_web::config::{DatabaseFleetConfig, Env};
use autumn_web::db::fleet::{DatabaseFleet, FleetError};
use autumn_web::db::fleet_engine::{FleetEngine, ShipMode};
use autumn_web::db::fleet_ownership::{FleetOwnership, OWNERSHIP_INDICATOR, OwnerId, SlotRange};
use autumn_web::fleet::FleetLease;
use autumn_web::fleet_layout::FleetDbKey;
use autumn_web::lock::FencingToken;
use autumn_web::plugin::Plugin;
use autumn_web::reexports::tokio_util::sync::CancellationToken;
use autumn_web::{AppState, AutumnError};

use crate::client::SurrealDb;
use crate::config::{ConfigError, SurrealDbConfig, env_prefix, name_rule, section_table};
use crate::error::SurrealDbError;

/// The plugin name of [`SurrealDbFleetPlugin`] in Autumn diagnostics.
pub const FLEET_PLUGIN_NAME: &str = "autumn-plugin-surrealdb-fleet";

/// The default section of [`SurrealDbFleetConfig`].
pub const DEFAULT_FLEET_SECTION: &str = "surrealdb_fleet";

/// The default interval between two shipped generations.
pub const DEFAULT_SHIP_EVERY: Duration = Duration::from_mins(1);

/// The default memory that the fleet counts for one open database: 64 MiB.
pub const DEFAULT_RESIDENT_BYTES: u64 = 64 * 1024 * 1024;

/// The table of the key name and the epoch.
pub const IDENTITY_TABLE: &str = "autumn_fleet_identity";

/// The table of the applied migrations.
pub const MIGRATION_TABLE: &str = "autumn_fleet_migration";

/// The store directory in a database directory.
pub const STORE_DIR: &str = "store";

/// The export file of a snapshot.
pub const SNAPSHOT_FILE: &str = "snapshot.surql";

/// The file that repeats the key and the epoch.
const IDENTITY_FILE: &str = "identity";

/// The directory of an import that is not complete.
const IMPORT_DIR: &str = "store.import";

/// The record id of the stamp.
const STAMP_ID: &str = "stamp";

/// The pause between two lock checks.
const LOCK_POLL: Duration = Duration::from_millis(20);

/// One schema step. Each open applies the steps that the database does not have, in order.
///
/// The table `autumn_fleet_migration` records the applied names. Write each step so that a
/// second run does no harm, for example with `DEFINE TABLE IF NOT EXISTS`. A crash between the
/// step and its record runs the step again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Migration {
    name: Cow<'static, str>,
    sql: Cow<'static, str>,
}

impl Migration {
    /// Makes a step. The name is unique and stays the same.
    #[must_use]
    pub const fn new(name: &'static str, sql: &'static str) -> Self {
        Self {
            name: Cow::Borrowed(name),
            sql: Cow::Borrowed(sql),
        }
    }

    /// Makes a step from owned text.
    #[must_use]
    pub fn owned(name: impl Into<String>, sql: impl Into<String>) -> Self {
        Self {
            name: Cow::Owned(name.into()),
            sql: Cow::Owned(sql.into()),
        }
    }

    /// The name of the step.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The SurrealQL of the step.
    #[must_use]
    pub fn sql(&self) -> &str {
        &self.sql
    }
}

/// The SurrealKV engine of an Autumn fleet. See the module contract.
pub struct SurrealKvEngine {
    namespace: String,
    database: String,
    timeout: Duration,
    migrations: Vec<Migration>,
    applied: AtomicU64,
    ship_every: Duration,
    resident_bytes: u64,
}

impl Default for SurrealKvEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl SurrealKvEngine {
    /// Makes an engine with the namespace and the database `autumn`, and a 5 second timeout.
    #[must_use]
    pub fn new() -> Self {
        let defaults = SurrealDbConfig::default();
        Self {
            timeout: defaults.timeout(),
            namespace: defaults.namespace,
            database: defaults.database,
            migrations: Vec::new(),
            applied: AtomicU64::new(0),
            ship_every: DEFAULT_SHIP_EVERY,
            resident_bytes: DEFAULT_RESIDENT_BYTES,
        }
    }

    /// Sets the namespace and the database that each handle selects.
    #[must_use]
    pub fn select(mut self, namespace: impl Into<String>, database: impl Into<String>) -> Self {
        self.namespace = namespace.into();
        self.database = database.into();
        self
    }

    /// Sets the call timeout. The open, the export and the import also use it.
    #[must_use]
    pub const fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Sets the interval between two shipped generations.
    #[must_use]
    pub const fn ship_every(mut self, every: Duration) -> Self {
        self.ship_every = every;
        self
    }

    /// Sets the memory that the fleet counts for one open database.
    #[must_use]
    pub const fn resident_bytes(mut self, bytes: u64) -> Self {
        self.resident_bytes = bytes;
        self
    }

    /// Adds migrations. Each open applies the pending ones, in order.
    #[must_use]
    pub fn migrations(mut self, migrations: impl IntoIterator<Item = Migration>) -> Self {
        self.migrations.extend(migrations);
        self
    }

    fn handle_config(&self, store: &Path) -> SurrealDbConfig {
        SurrealDbConfig {
            endpoint: store.display().to_string(),
            namespace: self.namespace.clone(),
            database: self.database.clone(),
            timeout_ms: u64::try_from(self.timeout.as_millis()).unwrap_or(u64::MAX),
            health_check: false,
            ..SurrealDbConfig::default()
        }
    }

    /// Opens the store at `store`. It tries again until the timeout: a closed client lets go
    /// of the store in the background, and the store refuses an open until then.
    async fn open_store(&self, store: &Path) -> Result<SurrealDb, FleetError> {
        let deadline = Instant::now() + self.timeout;
        loop {
            match SurrealDb::open_local(store, self.handle_config(store)).await {
                Ok(db) => return Ok(db),
                Err(err) if Instant::now() < deadline => {
                    tracing::debug!("the SurrealKV store did not open, trying again: {err}");
                    tokio::time::sleep(LOCK_POLL).await;
                }
                Err(err) => return Err(engine_error("open database")(err)),
            }
        }
    }

    /// Imports `dump` into a new store at `store`, through a temporary directory.
    async fn import(&self, dump: &Path, store: &Path, staging: &Path) -> Result<(), FleetError> {
        remove_dir(staging).map_err(io_error("import snapshot"))?;
        let db = self.open_store(staging).await?;
        let imported = db.import_from(dump).await;
        db.close_now();
        drop(db);
        imported.map_err(engine_error("import snapshot"))?;
        wait_released(staging, self.timeout).await?;
        std::fs::rename(staging, store).map_err(io_error("import snapshot"))
    }

    /// Writes an export of the closed store at `store` to `file`.
    async fn export_closed(&self, store: &Path, file: &Path) -> Result<(), FleetError> {
        let db = self.open_store(store).await?;
        let exported = db.export_to(file).await;
        db.close_now();
        drop(db);
        exported.map_err(engine_error("snapshot database"))?;
        wait_released(store, self.timeout).await
    }

    async fn migrate(&self, db: &SurrealDb) -> Result<(), SurrealDbError> {
        let none = serde_json::Map::new();
        for migration in &self.migrations {
            if db
                .select_record(MIGRATION_TABLE, migration.name())
                .await?
                .is_some()
            {
                continue;
            }
            db.query_json(migration.sql(), &none).await?;
            db.create_record(
                MIGRATION_TABLE,
                Some(migration.name()),
                &serde_json::json!({ "name": migration.name() }),
            )
            .await?;
            self.applied.fetch_add(1, Ordering::Relaxed);
        }
        Ok(())
    }
}

impl std::fmt::Debug for SurrealKvEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SurrealKvEngine")
            .field("namespace", &self.namespace)
            .field("database", &self.database)
            .field("timeout", &self.timeout)
            .field("migrations", &self.migrations.len())
            .field("ship_every", &self.ship_every)
            .field("resident_bytes", &self.resident_bytes)
            .finish_non_exhaustive()
    }
}

fn engine_error(op: &'static str) -> impl FnOnce(SurrealDbError) -> FleetError {
    move |err| FleetError::Io {
        op,
        detail: err.to_string(),
    }
}

fn io_error(op: &'static str) -> impl FnOnce(std::io::Error) -> FleetError {
    move |err| FleetError::Io {
        op,
        detail: err.to_string(),
    }
}

/// Deletes a directory. A missing directory is not an error.
fn remove_dir(dir: &Path) -> std::io::Result<()> {
    match std::fs::remove_dir_all(dir) {
        Err(err) if err.kind() != std::io::ErrorKind::NotFound => Err(err),
        _ => Ok(()),
    }
}

/// Returns `true` when a handle holds the store at `store`.
fn locked(store: &Path) -> bool {
    let Ok(file) = std::fs::File::open(store.join("LOCK")) else {
        return false;
    };
    match file.try_lock() {
        Ok(()) => {
            let _ = file.unlock();
            false
        }
        Err(_) => true,
    }
}

/// Waits until no handle holds the store at `store`. A closed client lets go in the background.
async fn wait_released(store: &Path, limit: Duration) -> Result<(), FleetError> {
    let deadline = Instant::now() + limit;
    while locked(store) {
        if Instant::now() >= deadline {
            return Err(FleetError::Io {
                op: "close database",
                detail: "the SurrealKV store did not close in time".to_owned(),
            });
        }
        tokio::time::sleep(LOCK_POLL).await;
    }
    Ok(())
}

/// Reads the `identity` file: the key name and the epoch.
fn read_identity_file(dir: &Path) -> std::io::Result<Option<(String, u64)>> {
    let text = match std::fs::read_to_string(dir.join(IDENTITY_FILE)) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err),
    };
    let mut lines = text.lines();
    let (Some(name), Some(epoch)) = (lines.next(), lines.next()) else {
        return Ok(None);
    };
    Ok(epoch.parse().ok().map(|epoch| (name.to_owned(), epoch)))
}

/// Writes the `identity` file. A rename makes the write atomic.
fn write_identity_file(dir: &Path, name: &str, epoch: u64) -> std::io::Result<()> {
    let staged = dir.join(format!("{IDENTITY_FILE}.tmp"));
    std::fs::write(&staged, format!("{name}\n{epoch}\n"))?;
    std::fs::rename(staged, dir.join(IDENTITY_FILE))
}

/// Reads the stamp record: the key name and the epoch.
async fn read_stamp(db: &SurrealDb) -> Result<Option<(String, u64)>, SurrealDbError> {
    let Some(record) = db.select_record(IDENTITY_TABLE, STAMP_ID).await? else {
        return Ok(None);
    };
    let name = record
        .get("name")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let epoch = record
        .get("epoch")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    Ok(Some((name, epoch)))
}

/// Checks and writes the key and the epoch. Returns the stored epoch.
async fn stamp(
    db: &SurrealDb,
    name: &str,
    epoch: u64,
) -> Result<Result<u64, FleetError>, SurrealDbError> {
    // A read of a table that is not defined fails. `IF NOT EXISTS` writes nothing on a reopen.
    db.query_json(
        &format!(
            "DEFINE TABLE IF NOT EXISTS {IDENTITY_TABLE} SCHEMALESS; \
             DEFINE TABLE IF NOT EXISTS {MIGRATION_TABLE} SCHEMALESS;"
        ),
        &serde_json::Map::new(),
    )
    .await?;
    let Some((found, stored)) = read_stamp(db).await? else {
        db.create_record(
            IDENTITY_TABLE,
            Some(STAMP_ID),
            &serde_json::json!({ "name": name, "epoch": epoch }),
        )
        .await?;
        return Ok(Ok(epoch));
    };
    if found != name {
        return Ok(Err(FleetError::Misplaced {
            name: name.to_owned(),
            found,
        }));
    }
    if epoch > 0 && stored > epoch {
        return Ok(Err(FleetError::NewerEpoch {
            name: name.to_owned(),
            epoch,
            found: stored,
        }));
    }
    if epoch > stored {
        db.update_record(
            IDENTITY_TABLE,
            STAMP_ID,
            &serde_json::json!({ "epoch": epoch }),
        )
        .await?;
        return Ok(Ok(epoch));
    }
    Ok(Ok(stored))
}

impl FleetEngine for SurrealKvEngine {
    type Handle = SurrealDb;

    async fn open(
        &self,
        key: &FleetDbKey,
        path: &Path,
        epoch: Option<FencingToken>,
    ) -> Result<SurrealDb, FleetError> {
        std::fs::create_dir_all(path).map_err(io_error("open database"))?;
        let store = path.join(STORE_DIR);
        let dump = path.join(SNAPSHOT_FILE);
        if dump.is_file() {
            if !store.exists() {
                self.import(&dump, &store, &path.join(IMPORT_DIR)).await?;
            }
            std::fs::remove_file(&dump).map_err(io_error("import snapshot"))?;
        }
        let db = self.open_store(&store).await?;
        let name = key.name();
        let epoch = epoch.map_or(0, FencingToken::get);
        let stamped = match stamp(&db, &name, epoch).await {
            Ok(Ok(stored)) => write_identity_file(path, &name, stored)
                .map_err(io_error("stamp database"))
                .map(|()| db.clone()),
            Ok(Err(err)) => Err(err),
            Err(err) => Err(engine_error("stamp database")(err)),
        };
        let migrated = match stamped {
            Ok(db) => self
                .migrate(&db)
                .await
                .map_err(engine_error("migrate database")),
            Err(err) => Err(err),
        };
        if let Err(err) = migrated {
            db.close_now();
            drop(db);
            wait_released(&store, self.timeout).await?;
            return Err(err);
        }
        Ok(db)
    }

    fn close(&self, handle: &SurrealDb) {
        handle.close_now();
        if handle.is_busy() {
            // The store closes when the running calls end. The next open waits for it.
            return;
        }
        // The client lets go of the store in the background. Wait for it, so that a delete
        // or a copy after the close finds the store free.
        let store = PathBuf::from(&handle.config().endpoint);
        let deadline = Instant::now() + self.timeout;
        while locked(&store) && Instant::now() < deadline {
            std::thread::sleep(LOCK_POLL);
        }
    }

    fn in_use(&self, handle: &SurrealDb) -> bool {
        handle.is_busy()
    }

    async fn snapshot(
        &self,
        handle: &SurrealDb,
        path: &Path,
        dir: &Path,
    ) -> Result<Vec<PathBuf>, FleetError> {
        let name = path.file_name().ok_or_else(|| FleetError::Io {
            op: "snapshot database",
            detail: "the path has no file name".to_owned(),
        })?;
        let out = dir.join(name);
        std::fs::create_dir_all(&out).map_err(io_error("snapshot database"))?;
        let file = out.join(SNAPSHOT_FILE);
        let store = path.join(STORE_DIR);
        let dump = path.join(SNAPSHOT_FILE);
        if !handle.is_closed() {
            handle
                .export_to(&file)
                .await
                .map_err(engine_error("snapshot database"))?;
        } else if !store.exists() && dump.is_file() {
            // A restored database that no open has imported yet.
            std::fs::copy(&dump, &file).map_err(io_error("snapshot database"))?;
        } else {
            wait_released(&store, self.timeout).await?;
            self.export_closed(&store, &file).await?;
        }
        Ok(vec![file])
    }

    fn remove(&self, key: &FleetDbKey, path: &Path) -> Result<bool, FleetError> {
        if !path.exists() {
            return Ok(false);
        }
        if locked(&path.join(STORE_DIR)) {
            return Err(FleetError::InUseElsewhere { name: key.name() });
        }
        std::fs::remove_dir_all(path).map_err(io_error("delete database"))?;
        Ok(true)
    }

    fn resident_bytes(&self, _handle: &SurrealDb) -> u64 {
        self.resident_bytes
    }

    fn ship_mode(&self) -> ShipMode {
        ShipMode::Generations {
            every: self.ship_every,
        }
    }

    fn exists(&self, path: &Path) -> bool {
        path.join(STORE_DIR).is_dir() || path.join(SNAPSHOT_FILE).is_file()
    }

    fn migrations_applied(&self) -> u64 {
        self.applied.load(Ordering::Relaxed)
    }

    fn epoch_stamp(&self, key: &FleetDbKey, path: &Path) -> Result<Option<u64>, FleetError> {
        if !path.join(STORE_DIR).is_dir() {
            return Ok(None);
        }
        let found = read_identity_file(path).map_err(io_error("read epoch stamp"))?;
        Ok(found
            .filter(|(name, _)| *name == key.name())
            .map(|(_, epoch)| epoch))
    }
}

/// The SurrealDB database of the request tenant, from a [`SurrealDbFleetPlugin`] fleet.
///
/// It derefs to [`SurrealDb`]. The fleet keeps the database open while the value lives.
///
/// The rejection is the fleet error status: `400` with no tenant, `421` when another host owns the
/// slot, and `503` with no fleet.
#[derive(Debug, Clone)]
pub struct FleetSurrealDb(pub FleetLease<SurrealKvEngine>);

impl FleetSurrealDb {
    /// Opens the database of `key` with no request, for example in a job or a task.
    ///
    /// # Errors
    ///
    /// Returns the [`FleetError`] of [`autumn_web::fleet::resolve_key`].
    pub async fn for_key(state: &AppState, key: &str) -> Result<Self, FleetError> {
        autumn_web::fleet::resolve_key::<SurrealKvEngine>(state, key)
            .await
            .map(Self)
    }
}

impl std::ops::Deref for FleetSurrealDb {
    type Target = SurrealDb;

    fn deref(&self) -> &SurrealDb {
        self.0.handle()
    }
}

impl axum::extract::FromRequestParts<AppState> for FleetSurrealDb {
    type Rejection = AutumnError;

    async fn from_request_parts(
        parts: &mut http::request::Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        autumn_web::fleet::resolve::<SurrealKvEngine>(parts, state)
            .await
            .map(Self)
            .map_err(AutumnError::from)
    }
}

/// The configuration of a SurrealDB fleet.
///
/// ```toml
/// [surrealdb_fleet]
/// mode = "tenant"
/// root = "data/tenants"
/// create_on_demand = true
/// max_resident_bytes = 1073741824
/// namespace = "app"
/// database = "app"
/// timeout_ms = 5000
/// ship_every_secs = 60
/// resident_bytes = 67108864
///
/// [surrealdb_fleet.ownership]
/// owner = "host-a"
/// ```
///
/// The Autumn fleet keys (`[database.fleet]`) and the engine keys share the section.
/// The variables `AUTUMN_SURREALDB_FLEET__ROOT` and `AUTUMN_SURREALDB_FLEET__OWNERSHIP__OWNER`
/// set `root` and `ownership.owner`.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct SurrealDbFleetConfig {
    /// The Autumn fleet keys.
    pub fleet: DatabaseFleetConfig,
    /// The namespace that each handle selects.
    pub namespace: String,
    /// The database that each handle selects.
    pub database: String,
    /// The call timeout in milliseconds.
    pub timeout_ms: u64,
    /// The interval between two shipped generations.
    pub ship_every: Duration,
    /// The memory that the fleet counts for one open database.
    pub resident_bytes: u64,
}

impl SurrealDbFleetConfig {
    /// Makes a configuration with `fleet` and the default engine keys.
    #[must_use]
    pub fn new(fleet: DatabaseFleetConfig) -> Self {
        let defaults = SurrealDbConfig::default();
        Self {
            fleet,
            namespace: defaults.namespace,
            database: defaults.database,
            timeout_ms: defaults.timeout_ms,
            ship_every: DEFAULT_SHIP_EVERY,
            resident_bytes: DEFAULT_RESIDENT_BYTES,
        }
    }

    /// Reads `[section]` from the app files and the process environment.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError`] if a file is not valid TOML or a value is not valid.
    pub fn resolve(section: &str) -> Result<Self, ConfigError> {
        autumn_web::dotenv::os_env_with_dotenv().map_or_else(
            |_| Self::resolve_with_env(section, &autumn_web::config::OsEnv),
            |env| Self::resolve_with_env(section, &env),
        )
    }

    /// Reads `[section]` with `env` as the environment.
    ///
    /// # Errors
    ///
    /// See [`SurrealDbFleetConfig::resolve`].
    pub fn resolve_with_env(section: &str, env: &dyn Env) -> Result<Self, ConfigError> {
        let mut table = section_table(section, env)?;
        let prefix = env_prefix(section);
        if let Ok(root) = env.var(&format!("{prefix}ROOT")) {
            table.insert("root".to_owned(), toml::Value::String(root));
        }
        if let Ok(owner) = env.var(&format!("{prefix}OWNERSHIP__OWNER")) {
            let ownership = table
                .entry("ownership")
                .or_insert_with(|| toml::Value::Table(toml::Table::new()));
            if let toml::Value::Table(ownership) = ownership {
                ownership.insert("owner".to_owned(), toml::Value::String(owner));
            }
        }
        Self::from_table(section, table)
    }

    /// Reads a configuration from the TOML table of `[section]`.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError`] for an unknown key or a value that is not valid.
    pub fn from_table(section: &str, mut table: toml::Table) -> Result<Self, ConfigError> {
        let bad = |key: &str, rule: &str| ConfigError(format!("{section}.{key} {rule}"));
        let defaults = SurrealDbConfig::default();
        let text = |table: &mut toml::Table, key: &str, default: String| match table.remove(key) {
            None => Ok(default),
            Some(toml::Value::String(value)) => Ok(value),
            Some(_) => Err(bad(key, "must be text")),
        };
        let whole = |table: &mut toml::Table, key: &str, default: u64, rule: &str| {
            table.remove(key).map_or(Ok(default), |value| {
                value
                    .as_integer()
                    .and_then(|v| u64::try_from(v).ok())
                    .ok_or_else(|| bad(key, rule))
            })
        };
        let namespace = text(&mut table, "namespace", defaults.namespace)?;
        let database = text(&mut table, "database", defaults.database)?;
        let timeout_ms = whole(
            &mut table,
            "timeout_ms",
            defaults.timeout_ms,
            "must be a whole number of milliseconds",
        )?;
        let ship_every = Duration::from_secs(whole(
            &mut table,
            "ship_every_secs",
            DEFAULT_SHIP_EVERY.as_secs(),
            "must be a whole number of seconds",
        )?);
        let resident_bytes = whole(
            &mut table,
            "resident_bytes",
            DEFAULT_RESIDENT_BYTES,
            "must be a whole number",
        )?;
        let fleet: DatabaseFleetConfig = toml::Value::Table(table)
            .try_into()
            .map_err(|err| ConfigError(format!("[{section}]: {err}")))?;
        let config = Self {
            fleet,
            namespace,
            database,
            timeout_ms,
            ship_every,
            resident_bytes,
        };
        config.validate_section(section)?;
        Ok(config)
    }

    /// Checks each value.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError`] that names the first key that is not valid.
    pub fn validate(&self) -> Result<(), ConfigError> {
        self.validate_section(DEFAULT_FLEET_SECTION)
    }

    fn validate_section(&self, section: &str) -> Result<(), ConfigError> {
        let fail = |key: &str, rule: &str| Err(ConfigError(format!("{section}.{key} {rule}")));
        if self.fleet.root.trim().is_empty() {
            return fail("root", "must be a directory");
        }
        if let Some(rule) = name_rule(&self.namespace) {
            return fail("namespace", rule);
        }
        if let Some(rule) = name_rule(&self.database) {
            return fail("database", rule);
        }
        if !(1..=86_400_000).contains(&self.timeout_ms) {
            return fail("timeout_ms", "must be from 1 to 86400000 (one day)");
        }
        if self.resident_bytes == 0 {
            return fail("resident_bytes", "must be 1 or more");
        }
        if self.ship_every.is_zero() {
            return fail("ship_every_secs", "must be 1 or more");
        }
        Ok(())
    }

    /// Makes the engine of this configuration.
    #[must_use]
    pub fn engine(&self) -> SurrealKvEngine {
        SurrealKvEngine::new()
            .select(self.namespace.clone(), self.database.clone())
            .timeout(Duration::from_millis(self.timeout_ms))
            .ship_every(self.ship_every)
            .resident_bytes(self.resident_bytes)
    }
}

enum FleetSource {
    Section(String),
    Explicit(Box<SurrealDbFleetConfig>),
}

/// The running parts of a fleet: the idle sweeper and the ownership loop.
struct FleetRuntime {
    fleet: DatabaseFleet<SurrealKvEngine>,
    stop: CancellationToken,
    maintenance: tokio::task::JoinHandle<()>,
    ownership: Option<(CancellationToken, tokio::task::JoinHandle<()>)>,
}

impl FleetRuntime {
    async fn shutdown(self) {
        self.stop.cancel();
        let _ = self.maintenance.await;
        if let Some((stop, handle)) = self.ownership {
            stop.cancel();
            let _ = handle.await;
            if self.fleet.release_ownership().await {
                tracing::info!("the SurrealDB fleet released its slot ranges");
            }
        }
    }
}

/// Makes a fleet of embedded SurrealDB databases and puts it in the app state.
///
/// ```rust,no_run
/// use autumn_plugin_surrealdb::fleet::{FleetSurrealDb, Migration, SurrealDbFleetPlugin};
/// use autumn_plugin_surrealdb::SurrealDbResultExt as _;
///
/// #[autumn_web::get("/notes")]
/// async fn notes(db: FleetSurrealDb) -> autumn_web::AutumnResult<String> {
///     let rows = db.select_all("note").await.or_http()?;
///     Ok(rows.len().to_string())
/// }
///
/// # async fn run() {
/// autumn_web::app()
///     .plugin(SurrealDbFleetPlugin::new().migrations([Migration::new(
///         "0001_notes",
///         "DEFINE TABLE IF NOT EXISTS note SCHEMALESS",
///     )]))
///     .routes(autumn_web::routes![notes])
///     .run()
///     .await;
/// # }
/// ```
#[must_use]
pub struct SurrealDbFleetPlugin {
    source: FleetSource,
    migrations: Vec<Migration>,
}

impl Default for SurrealDbFleetPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl SurrealDbFleetPlugin {
    /// Makes a plugin that reads `[surrealdb_fleet]`.
    pub fn new() -> Self {
        Self {
            source: FleetSource::Section(DEFAULT_FLEET_SECTION.to_owned()),
            migrations: Vec::new(),
        }
    }

    /// Reads `[section]` instead of `[surrealdb_fleet]`.
    pub fn config_section(mut self, section: impl Into<String>) -> Self {
        self.source = FleetSource::Section(section.into());
        self
    }

    /// Uses `config` and reads no files or variables.
    pub fn config(mut self, config: SurrealDbFleetConfig) -> Self {
        self.source = FleetSource::Explicit(Box::new(config));
        self
    }

    /// Adds migrations. Each database open applies the pending ones, in order.
    pub fn migrations(mut self, migrations: impl IntoIterator<Item = Migration>) -> Self {
        self.migrations.extend(migrations);
        self
    }

    fn resolve(source: &FleetSource) -> Result<SurrealDbFleetConfig, ConfigError> {
        match source {
            FleetSource::Section(section) => SurrealDbFleetConfig::resolve(section),
            FleetSource::Explicit(config) => {
                config.validate()?;
                Ok((**config).clone())
            }
        }
    }
}

impl std::fmt::Debug for SurrealDbFleetPlugin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let source = match &self.source {
            FleetSource::Section(section) => section.as_str(),
            FleetSource::Explicit(_) => "(explicit)",
        };
        f.debug_struct("SurrealDbFleetPlugin")
            .field("config", &source)
            .field("migrations", &self.migrations.len())
            .finish_non_exhaustive()
    }
}

/// Starts the fleet: ownership first, then the idle sweeper.
async fn start(
    state: &AppState,
    config: &SurrealDbFleetConfig,
    engine: SurrealKvEngine,
) -> Result<FleetRuntime, String> {
    let fleet = DatabaseFleet::with_engine(&config.fleet, engine).map_err(|e| e.to_string())?;
    let ownership = match &config.fleet.ownership {
        None => None,
        Some(settings) => {
            let store = autumn_web::fleet::ownership_store(state)
                .ok_or("fleet ownership needs a control database or a fleet_ownership_store")?;
            store.ensure_schema().await.map_err(|e| e.to_string())?;
            let ranges = settings
                .ranges()?
                .into_iter()
                .map(|(start, end)| SlotRange::new(start, end))
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| e.to_string())?;
            let owner = OwnerId::new(settings.owner.as_str()).map_err(|e| e.to_string())?;
            let ownership = Arc::new(
                FleetOwnership::new(
                    store,
                    owner,
                    ranges,
                    Duration::from_secs(settings.lease_secs),
                )
                .map_err(|e| e.to_string())?,
            );
            fleet
                .start_ownership(Arc::clone(&ownership))
                .await
                .map_err(|e| e.to_string())?;
            if let Err(err) = state.health_indicator_registry().register(
                OWNERSHIP_INDICATOR,
                autumn_web::actuator::IndicatorGroup::HealthOnly,
                ownership.indicator(),
            ) {
                tracing::warn!("{err}");
            }
            let stop = CancellationToken::new();
            let handle = fleet.spawn_ownership(stop.clone());
            Some((stop, handle))
        }
    };
    let stop = CancellationToken::new();
    let maintenance = fleet.spawn_maintenance(stop.clone());
    state.insert_extension(fleet.clone());
    Ok(FleetRuntime {
        fleet,
        stop,
        maintenance,
        ownership,
    })
}

impl Plugin for SurrealDbFleetPlugin {
    fn name(&self) -> Cow<'static, str> {
        Cow::Borrowed(FLEET_PLUGIN_NAME)
    }

    fn build(self, app: AppBuilder) -> AppBuilder {
        let Self { source, migrations } = self;
        let mut app = app;
        if let FleetSource::Section(section) = &source {
            app = app.config_section(section.clone());
        }
        let source = Arc::new(source);
        let runtime: Arc<Mutex<Option<FleetRuntime>>> = Arc::default();
        let on_start = Arc::clone(&runtime);
        app.on_startup(move |state| {
            let source = Arc::clone(&source);
            let runtime = Arc::clone(&on_start);
            let migrations = migrations.clone();
            async move {
                let fail = |err: &dyn std::fmt::Display| {
                    AutumnError::internal_server_error_msg(format!("{FLEET_PLUGIN_NAME}: {err}"))
                };
                let config = Self::resolve(&source).map_err(|err| fail(&err))?;
                let engine = config.engine().migrations(migrations);
                let started = start(&state, &config, engine)
                    .await
                    .map_err(|err| fail(&err))?;
                *runtime.lock().unwrap_or_else(PoisonError::into_inner) = Some(started);
                tracing::info!("the SurrealDB fleet is ready");
                Ok(())
            }
        })
        .on_shutdown(move || {
            let runtime = runtime
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .take();
            async move {
                if let Some(runtime) = runtime {
                    runtime.shutdown().await;
                }
            }
        })
    }
}

#[cfg(test)]
mod tests;
