pub mod entities;
pub mod queries;

use anyhow::{Context, Result};
use sea_orm::{ConnectOptions, Database, DatabaseConnection};
use sea_orm_migration::MigratorTrait;

use crate::migration::Migrator;

/// Open (creating if absent) the SQLite database at `path` and return a connection.
/// Does not run migrations — call [`run_migrations`] for that.
pub async fn connect(path: &str) -> Result<DatabaseConnection> {
    // `mode=rwc` => open read-write, create the file if it does not exist.
    let url = format!("sqlite://{path}?mode=rwc");
    let mut opts = ConnectOptions::new(url);
    opts.sqlx_logging(false);
    Database::connect(opts)
        .await
        .with_context(|| format!("opening sqlite database at {path}"))
}

/// Apply any pending migrations. Idempotent.
pub async fn run_migrations(db: &DatabaseConnection) -> Result<()> {
    Migrator::up(db, None)
        .await
        .context("running database migrations")
}
