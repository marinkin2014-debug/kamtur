use domain::entities::{CanonicalData, EnrichedTour};
use domain::rules::{EnrichmentRule, RuleContext};

/// РџСЂРёРјРµРЅРёС‚СЊ РїСЂР°РІРёР»Р° РѕР±РѕРіР°С‰РµРЅРёСЏ РєРѕ РІСЃРµРј С‚СѓСЂР°Рј.
///
/// Семантика: правила перебираются в порядке, заданном репозиторием
/// (`priority ASC, id ASC`). Для каждого `field_name` **первое сработавшее**
/// правило выигрывает; последующие правила для этого поля игнорируются.
pub fn enrich_tours(
    canonical: &CanonicalData,
    rules: &[EnrichmentRule],
    provider_config: Option<&str>,
) -> Vec<EnrichedTour> {
    canonical
        .tours
        .iter()
        .map(|tour| {
            let mut enriched = EnrichedTour {
                external_cruise_id: tour.external_cruise_id.clone(),
                fields: std::collections::HashMap::with_capacity(rules.len()),
            };

            let ctx = RuleContext {
                cruise_id: &tour.external_cruise_id,
                name: &tour.name,
                begin_date: tour.begin_date,
                end_date: tour.end_date,
                provider_config,
            };

            for rule in rules {
                if enriched.fields.contains_key(rule.field_name.as_str()) {
                    continue;
                }
                if let Some(value) = rule.evaluate(&ctx) {
                    enriched.set(&rule.field_name, value);
                }
            }

            enriched
        })
        .collect()
}
