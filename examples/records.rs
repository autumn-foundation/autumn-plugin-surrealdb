//! Creates a record and reads it back with a SurrealQL query.
//!
//! Needs a reachable SurrealDB. Start one first:
//!
//! ```text
//! surreal start --user root --pass root
//! ```
//!
//! Then run with credentials in the environment:
//!
//! ```text
//! export SURREALDB_USER=root SURREALDB_PASS=root
//! cargo run --example records
//! ```

use std::collections::HashMap;

use autumn_plugin_surrealdb::{SurrealDb, SurrealDbConfig};

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct Person {
    name: String,
    age: u32,
}

#[tokio::main]
async fn main() -> Result<(), autumn_plugin_surrealdb::SurrealDbError> {
    let mut config = SurrealDbConfig::default();
    config.namespace = "demo".into();
    config.database = "demo".into();
    if let Ok(user) = std::env::var("SURREALDB_USER") {
        config.username = Some(user);
    }
    if let Ok(pass) = std::env::var("SURREALDB_PASS") {
        config.password = Some(pass);
    }
    config.validate()?;

    let db = SurrealDb::connect(&config).await?;
    println!("server version: {}", db.version().await?);

    let created = db
        .create_record(
            "person",
            Some("ada"),
            &Person {
                name: "Ada Lovelace".into(),
                age: 36,
            },
        )
        .await?;
    println!("created: {created}");

    // Raw SurrealQL with bound variables.
    let mut vars = serde_json::Map::new();
    vars.insert("min_age".into(), serde_json::Value::from(30_u64));
    let rows = db
        .query_json("SELECT * FROM person WHERE age > $min_age", &vars)
        .await?;
    println!("query returned {} statement(s)", rows.len());
    for (index, statement) in rows.iter().enumerate() {
        println!("statement {index}: {statement:?}");
    }

    // A graph pattern: relate records, then walk the edges.
    let mut vars = serde_json::Map::new();
    vars.insert("name".into(), serde_json::Value::from("Ada Lovelace"));
    let walks = db
        .query_json(
            "SELECT name, ->knows->person.name AS knows FROM person WHERE name = $name",
            &vars,
        )
        .await?;
    println!("graph walk: {walks:?}");

    // A key-value pattern: one document per key in a `kv` table.
    let mut bucket = HashMap::new();
    bucket.insert("key", "feature:dark-mode");
    bucket.insert("value", "on");
    let stored = db
        .create_record("kv", Some("feature_dark_mode"), &bucket)
        .await?;
    println!("kv stored: {stored}");
    let back = db.select_record("kv", "feature_dark_mode").await?;
    println!("kv read: {back:?}");

    db.delete_record("person", "ada").await?;
    println!("cleaned up");
    Ok(())
}
