//! Exact v2 SQLite snapshots and copy-back under a legacy migration fence.

use crate::error::{InfrastructureError, Result};
use crate::state::sql::{FailureKind, SqlStateStore, SqlTransport, SqlValue, Statement};
use crate::state::{
    LastWriter, LegacyMigrationAccess, LegacyMigrationCopy, LegacyMigrationDeleteRequest,
    LegacyMigrationRecordRequest, LegacyMigrationSnapshot, LegacyMigrationSnapshotRead,
    LegacyMigrationWriterRequest, MigrationMutation,
};
use crate::tenant::ProjectInstance;

const FENCE: &str = "EXISTS (SELECT 1 FROM cuenv_infrastructure_locks
    WHERE module_path = ?1 AND project = ?2 AND environment = ?3 AND lock_identifier = ?4)
    AND (SELECT COALESCE(MAX(version), 0) FROM cuenv_infrastructure_migrations) = 2";

#[derive(Clone, Copy)]
enum Observation {
    Changed,
    Unchanged,
    Uncertain,
}

fn args(access: &LegacyMigrationAccess<'_>) -> Vec<SqlValue> {
    vec![
        SqlValue::text(access.tenant.module_path()),
        SqlValue::text(access.tenant.project()),
        SqlValue::text(access.tenant.environment().unwrap_or("")),
        SqlValue::text(&access.fence.lock_identifier),
    ]
}

fn encode<T: serde::Serialize>(value: &T) -> Result<String> {
    serde_json::to_string(value)
        .map_err(|_| InfrastructureError::state("cannot encode a legacy migration record"))
}

fn validate_writer(writer: Option<&LastWriter>) -> Result<()> {
    if writer.is_some_and(|writer| writer.recorded_at.chars().any(char::is_control)) {
        return Err(InfrastructureError::configuration(
            "invalid legacy migration writer metadata",
        ));
    }
    Ok(())
}

async fn guarded<T: SqlTransport>(
    store: &SqlStateStore<T>,
    access: &LegacyMigrationAccess<'_>,
) -> Result<bool> {
    Ok(!store
        .execute(Statement::carrying_state(
            format!("SELECT 1 WHERE {FENCE}"),
            args(access),
        ))
        .await?
        .rows
        .is_empty())
}

async fn send<T: SqlTransport>(
    store: &SqlStateStore<T>,
    statement: &Statement,
) -> Result<Observation> {
    match store
        .transport
        .execute_once(std::slice::from_ref(statement))
        .await
    {
        Ok(results) => Ok(match results.first() {
            Some(result) if result.affected_row_count == 1 => Observation::Changed,
            Some(result) if result.affected_row_count == 0 => Observation::Unchanged,
            Some(_) | None => Observation::Uncertain,
        }),
        Err(failure) if failure.kind == FailureKind::Statement => Err(store.error(&failure)),
        Err(_) => Ok(Observation::Uncertain),
    }
}

async fn unresolved<T: SqlTransport>(
    store: &SqlStateStore<T>,
    access: &LegacyMigrationAccess<'_>,
    observation: Observation,
) -> MigrationMutation {
    if matches!(observation, Observation::Unchanged)
        && matches!(guarded(store, access).await, Ok(false))
    {
        MigrationMutation::Stale
    } else {
        MigrationMutation::Indeterminate
    }
}

pub(in crate::state) async fn check<T: SqlTransport>(
    store: &SqlStateStore<T>,
    copy: &LegacyMigrationCopy<'_>,
) -> Result<MigrationMutation> {
    copy.validate()?;
    validate_writer(copy.writer)?;
    for resource in copy.resources {
        encode(&resource.state)?;
        encode(&resource.dependencies)?;
        encode(&resource.sensitive_attributes)?;
        if let Some(identity) = &resource.identity {
            encode(identity)?;
        }
    }
    Ok(if guarded(store, &copy.access).await? {
        MigrationMutation::Applied
    } else {
        MigrationMutation::Stale
    })
}

pub(in crate::state) async fn snapshot<T: SqlTransport>(
    store: &SqlStateStore<T>,
    access: &LegacyMigrationAccess<'_>,
) -> Result<LegacyMigrationSnapshotRead> {
    access.validate()?;
    let statement = Statement::carrying_state(
        "SELECT r.resource_type, r.resource_name, r.provider, r.provider_source, r.schema_version,
         r.resource_state_json, r.private, r.dependencies_json, r.tainted, r.identity_json,
         r.serial, r.generation, r.sensitive_attributes, o.instance, o.claimed_at
         FROM cuenv_infrastructure_locks AS held
         LEFT JOIN cuenv_infrastructure_resources AS r ON r.module_path = held.module_path
           AND r.project = held.project AND r.environment = held.environment
         LEFT JOIN cuenv_infrastructure_owners AS o ON o.module_path = held.module_path
           AND o.project = held.project AND o.environment = held.environment
         WHERE held.module_path = ?1 AND held.project = ?2 AND held.environment = ?3
           AND held.lock_identifier = ?4 AND (SELECT COALESCE(MAX(version), 0)
             FROM cuenv_infrastructure_migrations) = 2 ORDER BY r.resource_type, r.resource_name",
        args(access),
    );
    let result = store.execute(statement).await?;
    let Some(first) = result.rows.first() else {
        return Ok(LegacyMigrationSnapshotRead::Stale);
    };
    let writer = match (first.get(13), first.get(14)) {
        (Some(SqlValue::Null), Some(SqlValue::Null)) => None,
        (Some(SqlValue::Text(instance)), Some(SqlValue::Text(at)))
            if crate::error::strip_control_characters(instance) == *instance
                && crate::error::strip_control_characters(at) == *at =>
        {
            Some(LastWriter {
                instance: ProjectInstance::from_stored(instance),
                recorded_at: at.clone(),
            })
        }
        _ => {
            return Err(InfrastructureError::state(
                "invalid legacy source writer metadata",
            ));
        }
    };
    let mut resources = Vec::new();
    for row in &result.rows {
        if absent_resource(row)? {
            continue;
        }
        resources.push(crate::state::sql::row_to_resource(row)?);
    }
    LegacyMigrationCopy {
        access: *access,
        resources: &resources,
        writer: writer.as_ref(),
    }
    .validate()?;
    validate_writer(writer.as_ref())?;
    Ok(LegacyMigrationSnapshotRead::Available(Box::new(
        LegacyMigrationSnapshot { resources, writer },
    )))
}

fn absent_resource(row: &[SqlValue]) -> Result<bool> {
    let columns = row
        .get(..13)
        .ok_or_else(|| InfrastructureError::state("invalid legacy source snapshot"))?;
    if matches!(columns.first(), Some(SqlValue::Null)) {
        if columns.iter().all(|value| matches!(value, SqlValue::Null)) {
            return Ok(true);
        }
        return Err(InfrastructureError::state("invalid legacy source snapshot"));
    }
    Ok(false)
}

async fn record_matches<T: SqlTransport>(
    store: &SqlStateStore<T>,
    request: &LegacyMigrationRecordRequest<'_>,
) -> Result<bool> {
    let mut values = args(&request.access);
    values.push(SqlValue::text(&request.resource.address.resource_type));
    values.push(SqlValue::text(&request.resource.address.name));
    let result = store.execute(Statement::carrying_state(format!(
        "SELECT resource_type, resource_name, provider, provider_source, schema_version, resource_state_json,
         private, dependencies_json, tainted, identity_json, serial, generation, sensitive_attributes
         FROM cuenv_infrastructure_resources WHERE module_path = ?1 AND project = ?2 AND environment = ?3
           AND resource_type = ?5 AND resource_name = ?6 AND {FENCE}"), values)).await?;
    match result.rows.first() {
        Some(row) => Ok(crate::state::sql::row_to_resource(row)? == *request.resource),
        None => Ok(false),
    }
}

pub(in crate::state) async fn replace<T: SqlTransport>(
    store: &SqlStateStore<T>,
    request: &LegacyMigrationRecordRequest<'_>,
) -> Result<MigrationMutation> {
    request.validate()?;
    let resource = request.resource;
    let identity = resource.identity.as_ref().map(encode).transpose()?;
    let mut values = args(&request.access);
    values.extend([
        SqlValue::text(&resource.address.resource_type),
        SqlValue::text(&resource.address.name),
        SqlValue::text(&resource.provider),
        SqlValue::text(&resource.provider_source),
        SqlValue::integer(resource.schema_version),
        SqlValue::text(&encode(&resource.state)?),
        SqlValue::blob(&resource.private),
        SqlValue::text(&encode(&resource.dependencies)?),
        SqlValue::integer(i64::from(resource.tainted)),
        SqlValue::optional_text(identity.as_deref()),
        SqlValue::text(&encode(&resource.sensitive_attributes)?),
        SqlValue::integer(resource.serial),
        SqlValue::text(&resource.generation.to_string()),
        SqlValue::text(&chrono::Utc::now().to_rfc3339()),
    ]);
    let blob = T::DIALECT.blob_parameter.replacen('?', "?11", 1);
    let optional = T::DIALECT.optional_text_parameter.replacen('?', "?14", 1);
    let statement = Statement::carrying_state(format!(
        "INSERT INTO cuenv_infrastructure_resources (module_path, project, environment, resource_type, resource_name,
         provider, provider_source, schema_version, resource_state_json, private, dependencies_json, tainted,
         identity_json, sensitive_attributes, serial, generation, created_at, updated_at)
         SELECT ?1, ?2, ?3, ?5, ?6, ?7, ?8, ?9, ?10, {blob}, ?12, ?13, {optional}, ?15, ?16, ?17, ?18, ?18
         WHERE {FENCE} ON CONFLICT(module_path, project, environment, resource_type, resource_name) DO UPDATE SET
         provider = excluded.provider, provider_source = excluded.provider_source, schema_version = excluded.schema_version,
         resource_state_json = excluded.resource_state_json, private = excluded.private, dependencies_json = excluded.dependencies_json,
         tainted = excluded.tainted, identity_json = excluded.identity_json, sensitive_attributes = excluded.sensitive_attributes,
         serial = excluded.serial, generation = excluded.generation, updated_at = excluded.updated_at"), values);
    let observation = send(store, &statement).await?;
    if matches!(observation, Observation::Changed)
        || matches!(record_matches(store, request).await, Ok(true))
    {
        return Ok(MigrationMutation::Applied);
    }
    Ok(unresolved(store, &request.access, observation).await)
}

async fn absent<T: SqlTransport>(
    store: &SqlStateStore<T>,
    request: &LegacyMigrationDeleteRequest<'_>,
) -> Result<bool> {
    let mut values = args(&request.access);
    values.push(SqlValue::text(&request.address.resource_type));
    values.push(SqlValue::text(&request.address.name));
    Ok(!store
        .execute(Statement::carrying_state(
            format!(
                "SELECT 1 WHERE {FENCE} AND NOT EXISTS (
        SELECT 1 FROM cuenv_infrastructure_resources WHERE module_path = ?1 AND project = ?2
        AND environment = ?3 AND resource_type = ?5 AND resource_name = ?6)"
            ),
            values,
        ))
        .await?
        .rows
        .is_empty())
}

pub(in crate::state) async fn delete<T: SqlTransport>(
    store: &SqlStateStore<T>,
    request: &LegacyMigrationDeleteRequest<'_>,
) -> Result<MigrationMutation> {
    request.access.validate()?;
    let mut values = args(&request.access);
    values.push(SqlValue::text(&request.address.resource_type));
    values.push(SqlValue::text(&request.address.name));
    let statement = Statement::carrying_state(format!("DELETE FROM cuenv_infrastructure_resources WHERE module_path = ?1
        AND project = ?2 AND environment = ?3 AND resource_type = ?5 AND resource_name = ?6 AND {FENCE}"), values);
    let observation = send(store, &statement).await?;
    if matches!(observation, Observation::Changed)
        || matches!(absent(store, request).await, Ok(true))
    {
        return Ok(MigrationMutation::Applied);
    }
    Ok(unresolved(store, &request.access, observation).await)
}

async fn writer_matches<T: SqlTransport>(
    store: &SqlStateStore<T>,
    request: &LegacyMigrationWriterRequest<'_>,
) -> Result<bool> {
    let mut values = args(&request.access);
    let predicate = if let Some(writer) = request.writer {
        values.push(SqlValue::text(writer.instance.as_str()));
        values.push(SqlValue::text(&writer.recorded_at));
        "EXISTS (SELECT 1 FROM cuenv_infrastructure_owners WHERE module_path = ?1 AND project = ?2
         AND environment = ?3 AND instance = ?5 AND claimed_at = ?6)"
    } else {
        "NOT EXISTS (SELECT 1 FROM cuenv_infrastructure_owners WHERE module_path = ?1 AND project = ?2 AND environment = ?3)"
    };
    Ok(!store
        .execute(Statement::carrying_state(
            format!("SELECT 1 WHERE {FENCE} AND {predicate}"),
            values,
        ))
        .await?
        .rows
        .is_empty())
}

pub(in crate::state) async fn set_writer<T: SqlTransport>(
    store: &SqlStateStore<T>,
    request: &LegacyMigrationWriterRequest<'_>,
) -> Result<MigrationMutation> {
    request.access.validate()?;
    validate_writer(request.writer)?;
    let mut values = args(&request.access);
    let sql = if let Some(writer) = request.writer {
        values.push(SqlValue::text(writer.instance.as_str()));
        values.push(SqlValue::text(&writer.recorded_at));
        format!("INSERT INTO cuenv_infrastructure_owners (module_path, project, environment, instance, claimed_at)
         SELECT ?1, ?2, ?3, ?5, ?6 WHERE {FENCE} ON CONFLICT(module_path, project, environment)
         DO UPDATE SET instance = excluded.instance, claimed_at = excluded.claimed_at")
    } else {
        format!(
            "DELETE FROM cuenv_infrastructure_owners WHERE module_path = ?1 AND project = ?2 AND environment = ?3 AND {FENCE}"
        )
    };
    let observation = send(store, &Statement::carrying_state(sql, values)).await?;
    if matches!(observation, Observation::Changed)
        || matches!(writer_matches(store, request).await, Ok(true))
    {
        return Ok(MigrationMutation::Applied);
    }
    Ok(unresolved(store, &request.access, observation).await)
}

#[cfg(test)]
mod tests {
    use super::SqlValue;

    #[test]
    fn an_empty_joined_resource_requires_every_resource_column_to_be_null() {
        let mut row = vec![SqlValue::Null; 13];
        assert!(super::absent_resource(&row).unwrap());
        row[1] = SqlValue::text("unreadable-resource");
        assert!(super::absent_resource(&row).is_err());
        assert!(super::absent_resource(&row[..12]).is_err());
        row[0] = SqlValue::text("type");
        assert!(!super::absent_resource(&row).unwrap());
    }
}
