use rusqlite::{params, Connection};

fn has_column(connection: &Connection, table: &str, column: &str) -> Result<bool, String> {
    connection
        .prepare(&format!("PRAGMA table_info({table})"))
        .and_then(|mut statement| {
            statement
                .query_map([], |row| row.get::<_, String>(1))?
                .collect::<Result<Vec<_>, _>>()
        })
        .map_err(|error| error.to_string())
        .map(|columns| columns.iter().any(|name| name == column))
}

pub fn upgrade(connection: &mut Connection, applied_at: i64) -> Result<(), String> {
    let transaction = connection
        .transaction()
        .map_err(|error| error.to_string())?;
    if !has_column(&transaction, "plan_runs", "snapshot_hash")? {
        transaction
            .execute_batch("ALTER TABLE plan_runs ADD COLUMN snapshot_hash TEXT;")
            .map_err(|error| error.to_string())?;
    }
    if !has_column(&transaction, "operation_logs", "expected_size")? {
        transaction
            .execute_batch("ALTER TABLE operation_logs ADD COLUMN expected_size INTEGER;")
            .map_err(|error| error.to_string())?;
    }
    if !has_column(&transaction, "library_files", "kind")? {
        transaction
            .execute_batch(
                "ALTER TABLE library_files ADD COLUMN kind TEXT NOT NULL DEFAULT 'music';",
            )
            .map_err(|error| error.to_string())?;
    }
    for (table, column, definition) in [
        ("plan_runs", "parent_plan_id", "TEXT"),
        ("plan_runs", "rules_json", "TEXT"),
        (
            "plan_items",
            "target_origin",
            "TEXT NOT NULL DEFAULT 'rule'",
        ),
        ("plan_items", "conflict_group_id", "TEXT"),
    ] {
        if !has_column(&transaction, table, column)? {
            transaction
                .execute_batch(&format!(
                    "ALTER TABLE {table} ADD COLUMN {column} {definition};"
                ))
                .map_err(|error| error.to_string())?;
        }
    }
    for version in [1, 2, 3] {
        transaction
            .execute(
                "INSERT OR IGNORE INTO schema_migrations(version,applied_at) VALUES(?1,?2)",
                params![version, applied_at],
            )
            .map_err(|error| error.to_string())?;
    }
    transaction.commit().map_err(|error| error.to_string())
}
