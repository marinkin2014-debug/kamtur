use serde::Deserialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleType {
    Constant,
    Template,
    DaysBetween,
    ConfigLookup,
}

/// Конфиг правила. Поля опциональны — правило использует только те,
/// которые нужны для его `rule_type`.
///
/// ## Про удалённое поле `key`
///
/// Раньше здесь было `key: Option<String>`. Поле не участвовало ни в
/// одном правиле: `ConfigLookup` возвращает `provider_config` целиком
/// (одну строку), а не значение по ключу из map. В проде `provider_config`
/// у Volga-провайдера — `None`, так что `ConfigLookup` деградирует в
/// `Constant` с fallback на `config.value`.
///
/// Если в будущем провайдеры начнут возвращать JSON-конфиг — добавим
/// `key` обратно и реализуем разбор. YAGNI.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct RuleConfig {
    pub value: Option<String>,
    pub template: Option<String>,
    pub plus: Option<i32>,
}

#[derive(Debug, Clone)]
pub struct EnrichmentRule {
    pub field_name: String,
    pub rule_type: RuleType,
    pub config: RuleConfig,
    pub priority: i32,
}

pub struct RuleContext<'a> {
    pub cruise_id: &'a str,
    pub name: &'a str,
    pub begin_date: chrono::NaiveDate,
    pub end_date: chrono::NaiveDate,
    pub provider_config: Option<&'a str>,
}

impl EnrichmentRule {
    pub fn evaluate(&self, ctx: &RuleContext<'_>) -> Option<String> {
        match self.rule_type {
            RuleType::Constant => self.config.value.clone(),
            RuleType::ConfigLookup => ctx
                .provider_config
                .map(|s| s.to_string())
                .or_else(|| self.config.value.clone()),
            RuleType::Template => {
                let tpl = self.config.template.as_deref()?;
                Some(render_template(tpl, ctx.cruise_id, ctx.name))
            }
            RuleType::DaysBetween => {
                let plus = self.config.plus.unwrap_or(0) as i64;
                let days = (ctx.end_date - ctx.begin_date).num_days() + plus;
                Some(days.to_string())
            }
        }
    }
}

fn render_template(tpl: &str, cruise_id: &str, name: &str) -> String {
    let mut out = String::with_capacity(tpl.len() + 16);
    let mut chars = tpl.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '{' {
            let mut key = String::new();
            while let Some(&nc) = chars.peek() {
                if nc == '}' {
                    chars.next();
                    break;
                }
                key.push(nc);
                chars.next();
            }
            match key.as_str() {
                "cruise_id" => out.push_str(cruise_id),
                "name" => out.push_str(name),
                other => {
                    out.push('{');
                    out.push_str(other);
                    out.push('}');
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn ctx<'a>(
        cruise_id: &'a str,
        name: &'a str,
        begin: NaiveDate,
        end: NaiveDate,
        provider_config: Option<&'a str>,
    ) -> RuleContext<'a> {
        RuleContext {
            cruise_id,
            name,
            begin_date: begin,
            end_date: end,
            provider_config,
        }
    }

    fn date(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    #[test]
    fn constant_returns_value() {
        let rule = EnrichmentRule {
            field_name: "site_name".into(),
            rule_type: RuleType::Constant,
            config: RuleConfig {
                value: Some("Volga".into()),
                ..Default::default()
            },
            priority: 1,
        };
        let c = ctx("1", "n", date(2026, 9, 1), date(2026, 9, 5), None);
        assert_eq!(rule.evaluate(&c), Some("Volga".into()));
    }

    #[test]
    fn constant_without_value_returns_none() {
        let rule = EnrichmentRule {
            field_name: "x".into(),
            rule_type: RuleType::Constant,
            config: RuleConfig::default(),
            priority: 1,
        };
        let c = ctx("1", "n", date(2026, 9, 1), date(2026, 9, 5), None);
        assert_eq!(rule.evaluate(&c), None);
    }

    #[test]
    fn template_replaces_cruise_id_and_name() {
        let rule = EnrichmentRule {
            field_name: "site_name".into(),
            rule_type: RuleType::Template,
            config: RuleConfig {
                template: Some("{name} (id={cruise_id})".into()),
                ..Default::default()
            },
            priority: 1,
        };
        let c = ctx(
            "448",
            "Пермь - Самара",
            date(2026, 9, 1),
            date(2026, 9, 5),
            None,
        );
        assert_eq!(rule.evaluate(&c), Some("Пермь - Самара (id=448)".into()));
    }

    #[test]
    fn template_keeps_unknown_placeholders() {
        let rule = EnrichmentRule {
            field_name: "x".into(),
            rule_type: RuleType::Template,
            config: RuleConfig {
                template: Some("{foo} and {name}".into()),
                ..Default::default()
            },
            priority: 1,
        };
        let c = ctx("1", "NAME", date(2026, 9, 1), date(2026, 9, 5), None);
        assert_eq!(rule.evaluate(&c), Some("{foo} and NAME".into()));
    }

    #[test]
    fn days_between_counts_inclusive_plus_offset() {
        let rule = EnrichmentRule {
            field_name: "days".into(),
            rule_type: RuleType::DaysBetween,
            config: RuleConfig {
                plus: Some(1),
                ..Default::default()
            },
            priority: 1,
        };
        let c = ctx("1", "n", date(2026, 9, 1), date(2026, 9, 5), None);
        assert_eq!(rule.evaluate(&c), Some("5".into()));
    }

    #[test]
    fn days_between_without_plus_is_exact_diff() {
        let rule = EnrichmentRule {
            field_name: "days".into(),
            rule_type: RuleType::DaysBetween,
            config: RuleConfig::default(),
            priority: 1,
        };
        let c = ctx("1", "n", date(2026, 9, 1), date(2026, 9, 5), None);
        assert_eq!(rule.evaluate(&c), Some("4".into()));
    }

    #[test]
    fn config_lookup_prefers_provider_config() {
        let rule = EnrichmentRule {
            field_name: "site_cruise_object_id".into(),
            rule_type: RuleType::ConfigLookup,
            config: RuleConfig {
                value: Some("fallback".into()),
                ..Default::default()
            },
            priority: 1,
        };
        let c = ctx(
            "1",
            "n",
            date(2026, 9, 1),
            date(2026, 9, 5),
            Some("from-config"),
        );
        assert_eq!(rule.evaluate(&c), Some("from-config".into()));
    }

    #[test]
    fn config_lookup_falls_back_to_value() {
        let rule = EnrichmentRule {
            field_name: "x".into(),
            rule_type: RuleType::ConfigLookup,
            config: RuleConfig {
                value: Some("fallback".into()),
                ..Default::default()
            },
            priority: 1,
        };
        let c = ctx("1", "n", date(2026, 9, 1), date(2026, 9, 5), None);
        assert_eq!(rule.evaluate(&c), Some("fallback".into()));
    }
}
