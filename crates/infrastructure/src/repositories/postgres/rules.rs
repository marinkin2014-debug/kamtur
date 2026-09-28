use sqlx::PgPool;

use domain::entities::ProviderId;
use domain::errors::RepositoryError;
use domain::rules::{EnrichmentRule, RuleConfig, RuleType};

pub(super) async fn load_rules(
    pool: &PgPool,
    provider_id: &ProviderId,
) -> Result<Vec<EnrichmentRule>, RepositoryError> {
    #[derive(sqlx::FromRow)]
    struct Row {
        field_name: String,
        rule_type: String,
        rule_config: serde_json::Value,
        priority: i32,
    }

    let rows: Vec<Row> = sqlx::query_as(
        "SELECT field_name, rule_type, rule_config, priority
         FROM enrichment_rules
         WHERE is_active = true
           AND (cruise_provider_id = $1 OR cruise_provider_id IS NULL)
         ORDER BY priority ASC, id ASC",
    )
    .bind(&provider_id.0)
    .fetch_all(pool)
    .await
    .map_err(|e| RepositoryError::Connection(e.to_string()))?;

    let mut rules = Vec::with_capacity(rows.len());
    for r in rows {
        let rule_type = match r.rule_type.as_str() {
            "constant" => RuleType::Constant,
            "template" => RuleType::Template,
            "days_between" => RuleType::DaysBetween,
            "config_lookup" => RuleType::ConfigLookup,
            other => {
                tracing::warn!(rule_type = other, "unknown rule_type, skipping");
                continue;
            }
        };
        let config: RuleConfig = serde_json::from_value(r.rule_config)
            .map_err(|e| RepositoryError::Connection(format!("rule_config: {e}")))?;
        rules.push(EnrichmentRule {
            field_name: r.field_name,
            rule_type,
            config,
            priority: r.priority,
        });
    }

    Ok(rules)
}
