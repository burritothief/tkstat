use super::*;
use crate::db::Database;
use crate::domain::usage::ModelFamily;

fn snapshot(intervals: Vec<PricingInterval>) -> PricingSnapshot {
    PricingSnapshot::new(intervals, Vec::new())
}

fn interval(category: TokenCategory, rate: f64, from: &str, to: Option<&str>) -> PricingInterval {
    let mut interval = PricingInterval::usd(
        ProviderId::ClaudeCode,
        "claude-opus-4-6",
        category,
        rate,
        from.parse().unwrap(),
        "test",
    );
    interval.effective_to = to.map(|dt| dt.parse().unwrap());
    interval
}

fn with_speed(mut interval: PricingInterval, speed: &str) -> PricingInterval {
    interval.dimensions.speed = Some(speed.into());
    interval
}

fn with_processing_mode(mut interval: PricingInterval, processing_mode: &str) -> PricingInterval {
    interval.dimensions.processing_mode = Some(processing_mode.into());
    interval
}

fn with_source_detail(mut interval: PricingInterval, source_detail: &str) -> PricingInterval {
    interval.dimensions.source_detail = Some(source_detail.into());
    interval
}

fn record(model_id: &str) -> TokenRecord {
    TokenRecord {
        provider: crate::domain::provider::ProviderId::ClaudeCode,
        request_id: "r1".into(),
        session_id: "s1".into(),
        uuid: "u1".into(),
        timestamp: "2026-04-07T10:00:00Z".parse().unwrap(),
        model: ModelFamily::Opus,
        model_id: model_id.into(),
        input_tokens: 1_000_000,
        output_tokens: 1_000_000,
        cache_creation_tokens: 0,
        cache_read_tokens: 0,
        cached_input_tokens: 0,
        reasoning_output_tokens: 0,
        cache_creation_5m_tokens: 0,
        cache_creation_1h_tokens: 0,
        service_tier: None,
        speed: None,
        region: None,
        processing_mode: None,
        cost_usd: 0.0,
        project: "test".into(),
        source_file: "/test.jsonl".into(),
        is_subagent: false,
    }
}

fn insert_raw_pricing_row(
    conn: &Connection,
    provider: &str,
    model_id: &str,
    token_category: &str,
    rate: f64,
    effective_from: &str,
    source: &str,
) {
    conn.execute(
            "INSERT INTO pricing_intervals
             (provider, model_id, token_category, currency, rate_per_1m_tokens, effective_from, effective_to, source)
             VALUES (?1, ?2, ?3, 'USD', ?4, ?5, NULL, ?6)",
            rusqlite::params![
                provider,
                model_id,
                token_category,
                rate,
                effective_from,
                source
            ],
        )
        .unwrap();
}

fn insert_raw_pricing_row_with_to(
    conn: &Connection,
    model_id: &str,
    effective_from: &str,
    effective_to: Option<&str>,
) {
    conn.execute(
            "INSERT INTO pricing_intervals
             (provider, model_id, token_category, currency, rate_per_1m_tokens, effective_from, effective_to, source)
             VALUES ('claude-code', ?1, 'input', 'USD', 1.0, ?2, ?3, 'test')",
            rusqlite::params![model_id, effective_from, effective_to],
        )
        .unwrap();
}

fn single_input_catalog(rate: f64, effective_from: &str) -> String {
    format!(
        r#"{{
  "schema_version": 1,
  "notes": "offline pricing snapshot test catalog",
  "sources": [
    {{
      "id": "test-source",
      "url": "https://example.com/pricing",
      "retrieved_at": "2026-05-23",
      "notes": "test source"
    }}
  ],
  "entries": [
    {{
      "provider": "claude-code",
      "model_ids": ["claude-opus-4-6"],
      "model_aliases": ["opus"],
      "currency": "USD",
      "effective_from": "{effective_from}",
      "effective_to": null,
      "source": "seed:test-source",
      "source_ref": "test-source",
      "dimensions": {{}},
      "rates_per_1m_tokens": {{
        "input": {rate}
      }},
      "notes": "test entry"
    }}
  ]
}}"#
    )
}

fn source_metadata(source: &str, retrieved_at: &str, source_kind: &str) -> PricingSourceMetadata {
    PricingSourceMetadata {
        source: source.into(),
        source_url: "https://example.com/pricing".into(),
        source_retrieved_at: retrieved_at.into(),
        catalog_version: "1".into(),
        source_kind: source_kind.into(),
        notes: "test pricing source metadata".into(),
    }
}

fn stale_audit_finding_present_for_retrieved_at(retrieved_at: &str) -> bool {
    let db = Database::open_in_memory().unwrap();
    let source = format!("reviewed:boundary-{retrieved_at}");
    let mut price = interval(TokenCategory::Input, 10.0, "2026-01-01T00:00:00Z", None);
    price.source = source.clone();
    insert_interval(db.conn(), &price).unwrap();
    upsert_source_metadata(
        db.conn(),
        &source_metadata(&source, retrieved_at, "reviewed"),
    )
    .unwrap();

    let mut usage = record("claude-opus-4-6");
    usage.output_tokens = 0;
    db.insert_records(&[usage]).unwrap();

    let findings =
        audit_pricing_at(db.conn(), NaiveDate::from_ymd_opt(2026, 5, 24).unwrap()).unwrap();
    findings.iter().any(|finding| {
        finding.kind == PricingAuditKind::StaleSource
            && finding.provider == "claude-code"
            && finding.model_id == "claude-opus-4-6"
            && finding.token_category == "input"
            && finding.remediation.contains(&source)
    })
}

#[test]
fn test_insert_and_select_applicable_price() {
    let db = Database::open_in_memory().unwrap();
    let interval = interval(TokenCategory::Input, 15.0, "2026-01-01T00:00:00Z", None);
    validate_interval(&interval).unwrap();
    insert_interval(db.conn(), &interval).unwrap();

    let selected = applicable_interval(
        db.conn(),
        ProviderId::ClaudeCode,
        "claude-opus-4-6",
        TokenCategory::Input,
        "2026-04-07T10:00:00Z".parse().unwrap(),
    )
    .unwrap();
    assert_eq!(selected.rate_per_1m_tokens, 15.0);
}

#[test]
fn test_insert_interval_canonicalizes_utc_timestamp_storage() {
    let db = Database::open_in_memory().unwrap();
    let mut interval = PricingInterval::usd(
        ProviderId::ClaudeCode,
        "claude-offset",
        TokenCategory::Input,
        15.0,
        "2026-04-07T03:00:00-07:00"
            .parse::<DateTime<chrono::FixedOffset>>()
            .unwrap()
            .with_timezone(&Utc),
        "test",
    );
    interval.effective_to = Some(
        "2026-04-08T03:30:00-07:00"
            .parse::<DateTime<chrono::FixedOffset>>()
            .unwrap()
            .with_timezone(&Utc),
    );

    insert_interval(db.conn(), &interval).unwrap();

    let (stored_from, stored_to): (String, String) = db
            .conn()
            .query_row(
                "SELECT effective_from, effective_to FROM pricing_intervals WHERE model_id = 'claude-offset'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
    assert_eq!(stored_from, "2026-04-07T10:00:00+00:00");
    assert_eq!(stored_to, "2026-04-08T10:30:00+00:00");
}

#[test]
fn test_selects_effective_interval_by_timestamp() {
    let db = Database::open_in_memory().unwrap();
    insert_interval(
        db.conn(),
        &interval(
            TokenCategory::Input,
            10.0,
            "2026-01-01T00:00:00Z",
            Some("2026-04-01T00:00:00Z"),
        ),
    )
    .unwrap();
    insert_interval(
        db.conn(),
        &interval(TokenCategory::Input, 15.0, "2026-04-01T00:00:00Z", None),
    )
    .unwrap();

    let selected = applicable_interval(
        db.conn(),
        ProviderId::ClaudeCode,
        "claude-opus-4-6",
        TokenCategory::Input,
        "2026-04-07T10:00:00Z".parse().unwrap(),
    )
    .unwrap();
    assert_eq!(selected.rate_per_1m_tokens, 15.0);

    let offset_timestamp = "2026-03-31T17:30:00-07:00"
        .parse::<DateTime<chrono::FixedOffset>>()
        .unwrap()
        .with_timezone(&Utc);
    let selected = applicable_interval(
        db.conn(),
        ProviderId::ClaudeCode,
        "claude-opus-4-6",
        TokenCategory::Input,
        offset_timestamp,
    )
    .unwrap();
    assert_eq!(selected.rate_per_1m_tokens, 15.0);
}

#[test]
fn test_lookup_selects_matching_pricing_dimensions_and_default_only_matches_default() {
    let db = Database::open_in_memory().unwrap();
    insert_interval(
        db.conn(),
        &interval(TokenCategory::Input, 10.0, "2026-01-01T00:00:00Z", None),
    )
    .unwrap();
    insert_interval(
        db.conn(),
        &with_speed(
            interval(TokenCategory::Input, 20.0, "2026-01-01T00:00:00Z", None),
            "turbo",
        ),
    )
    .unwrap();

    let default = applicable_interval(
        db.conn(),
        ProviderId::ClaudeCode,
        "claude-opus-4-6",
        TokenCategory::Input,
        "2026-04-07T10:00:00Z".parse().unwrap(),
    )
    .unwrap();
    assert_eq!(default.rate_per_1m_tokens, 10.0);

    let standard_dimensions = PricingDimensions {
        service_tier: Some("standard".into()),
        speed: Some("standard".into()),
        ..Default::default()
    };
    let standard = applicable_interval_for_dimensions(
        db.conn(),
        ProviderId::ClaudeCode,
        "claude-opus-4-6",
        TokenCategory::Input,
        "2026-04-07T10:00:00Z".parse().unwrap(),
        &standard_dimensions,
    )
    .unwrap();
    assert_eq!(standard.rate_per_1m_tokens, 10.0);

    for region in ["not_available", "global"] {
        let region_placeholder = PricingDimensions {
            region: Some(region.into()),
            ..Default::default()
        };
        let selected = applicable_interval_for_dimensions(
            db.conn(),
            ProviderId::ClaudeCode,
            "claude-opus-4-6",
            TokenCategory::Input,
            "2026-04-07T10:00:00Z".parse().unwrap(),
            &region_placeholder,
        )
        .unwrap();
        assert_eq!(selected.rate_per_1m_tokens, 10.0);
    }

    let fast_dimensions = PricingDimensions {
        speed: Some("fast".into()),
        ..Default::default()
    };
    let fast = applicable_interval_for_dimensions(
        db.conn(),
        ProviderId::ClaudeCode,
        "claude-opus-4-6",
        TokenCategory::Input,
        "2026-04-07T10:00:00Z".parse().unwrap(),
        &fast_dimensions,
    )
    .unwrap();
    assert_eq!(fast.rate_per_1m_tokens, 10.0);

    let turbo_dimensions = PricingDimensions {
        speed: Some("turbo".into()),
        ..Default::default()
    };
    let turbo = applicable_interval_for_dimensions(
        db.conn(),
        ProviderId::ClaudeCode,
        "claude-opus-4-6",
        TokenCategory::Input,
        "2026-04-07T10:00:00Z".parse().unwrap(),
        &turbo_dimensions,
    )
    .unwrap();
    assert_eq!(turbo.rate_per_1m_tokens, 20.0);

    let priority_dimensions = PricingDimensions {
        speed: Some("priority".into()),
        ..Default::default()
    };
    let err = applicable_interval_for_dimensions(
        db.conn(),
        ProviderId::ClaudeCode,
        "claude-opus-4-6",
        TokenCategory::Input,
        "2026-04-07T10:00:00Z".parse().unwrap(),
        &priority_dimensions,
    )
    .unwrap_err()
    .to_string();
    assert!(err.contains("missing price"));
    assert!(err.contains("speed=priority"));
}

#[test]
fn test_claude_standard_modifiers_use_default_pricing_dimensions() {
    let db = Database::open_in_memory().unwrap();
    insert_interval(
        db.conn(),
        &PricingInterval::usd(
            ProviderId::ClaudeCode,
            "claude-haiku-4-5-20251001",
            TokenCategory::CacheRead,
            0.1,
            "2026-01-01T00:00:00Z".parse().unwrap(),
            "test",
        ),
    )
    .unwrap();

    let mut usage = record("claude-haiku-4-5-20251001");
    usage.model = ModelFamily::Haiku;
    usage.input_tokens = 0;
    usage.output_tokens = 0;
    usage.cache_creation_tokens = 0;
    usage.cache_read_tokens = 1_000_000;
    usage.service_tier = Some("standard".into());
    usage.speed = Some("standard".into());

    let cost = calculate_record_cost(db.conn(), &usage).unwrap();
    assert!((cost - 0.1).abs() < 0.000001);
}

#[test]
fn test_claude_unsupported_speed_still_requires_specialized_pricing() {
    let db = Database::open_in_memory().unwrap();
    insert_interval(
        db.conn(),
        &PricingInterval::usd(
            ProviderId::ClaudeCode,
            "claude-haiku-4-5-20251001",
            TokenCategory::CacheRead,
            0.1,
            "2026-01-01T00:00:00Z".parse().unwrap(),
            "test",
        ),
    )
    .unwrap();

    let mut usage = record("claude-haiku-4-5-20251001");
    usage.model = ModelFamily::Haiku;
    usage.input_tokens = 0;
    usage.output_tokens = 0;
    usage.cache_creation_tokens = 0;
    usage.cache_read_tokens = 1_000_000;
    usage.service_tier = Some("standard".into());
    usage.speed = Some("turbo".into());

    let err = calculate_record_cost(db.conn(), &usage)
        .unwrap_err()
        .to_string();
    assert!(err.contains("missing price"));
    assert!(err.contains("speed=turbo"));

    insert_interval(
        db.conn(),
        &with_speed(
            PricingInterval::usd(
                ProviderId::ClaudeCode,
                "claude-haiku-4-5-20251001",
                TokenCategory::CacheRead,
                0.2,
                "2026-01-01T00:00:00Z".parse().unwrap(),
                "test",
            ),
            "turbo",
        ),
    )
    .unwrap();

    let cost = calculate_record_cost(db.conn(), &usage).unwrap();
    assert!((cost - 0.2).abs() < 0.000001);
}

#[test]
fn test_unknown_model_returns_explicit_error() {
    let db = Database::open_in_memory().unwrap();
    insert_interval(
        db.conn(),
        &interval(TokenCategory::Input, 15.0, "2026-01-01T00:00:00Z", None),
    )
    .unwrap();

    let err = calculate_record_cost(db.conn(), &record("claude-unknown-1"))
        .unwrap_err()
        .to_string();
    assert!(err.contains("missing price"));
    assert!(err.contains("claude-unknown-1"));
}

#[test]
fn test_uncovered_interval_returns_explicit_error() {
    let db = Database::open_in_memory().unwrap();
    insert_interval(
        db.conn(),
        &interval(TokenCategory::Input, 15.0, "2026-05-01T00:00:00Z", None),
    )
    .unwrap();

    let err = calculate_record_cost(db.conn(), &record("claude-opus-4-6"))
        .unwrap_err()
        .to_string();
    assert!(err.contains("missing price"));
    assert!(err.contains("2026-04-07T10:00:00+00:00"));
}

#[test]
fn test_overlapping_intervals_return_explicit_error() {
    let db = Database::open_in_memory().unwrap();
    insert_interval(
        db.conn(),
        &interval(TokenCategory::Input, 10.0, "2026-01-01T00:00:00Z", None),
    )
    .unwrap();
    insert_interval(
        db.conn(),
        &interval(TokenCategory::Input, 15.0, "2026-02-01T00:00:00Z", None),
    )
    .unwrap();

    let err = applicable_interval(
        db.conn(),
        ProviderId::ClaudeCode,
        "claude-opus-4-6",
        TokenCategory::Input,
        "2026-04-07T10:00:00Z".parse().unwrap(),
    )
    .unwrap_err()
    .to_string();
    assert!(err.contains("overlapping prices"));
}

#[test]
fn test_calculate_record_cost_uses_all_nonzero_categories() {
    let db = Database::open_in_memory().unwrap();
    insert_interval(
        db.conn(),
        &interval(TokenCategory::Input, 15.0, "2026-01-01T00:00:00Z", None),
    )
    .unwrap();
    insert_interval(
        db.conn(),
        &interval(TokenCategory::Output, 75.0, "2026-01-01T00:00:00Z", None),
    )
    .unwrap();

    let cost = calculate_record_cost(db.conn(), &record("claude-opus-4-6")).unwrap();
    assert!((cost - 90.0).abs() < 0.001);
}

#[test]
fn test_calculate_codex_cost_uses_non_overlapping_billable_categories() {
    let db = Database::open_in_memory().unwrap();
    for (category, rate) in [
        (TokenCategory::Input, 10.0),
        (TokenCategory::CachedInput, 1.0),
        (TokenCategory::Output, 100.0),
        (TokenCategory::ReasoningOutput, 999.0),
    ] {
        insert_interval(
            db.conn(),
            &PricingInterval::usd(
                ProviderId::Codex,
                "gpt-audit",
                category,
                rate,
                "2026-01-01T00:00:00Z".parse().unwrap(),
                "test",
            ),
        )
        .unwrap();
    }

    let mut usage = record("gpt-audit");
    usage.provider = crate::domain::provider::ProviderId::Codex;
    usage.model = ModelFamily::Unknown;
    usage.input_tokens = 100;
    usage.cached_input_tokens = 40;
    usage.output_tokens = 20;
    usage.reasoning_output_tokens = 7;

    let cost = calculate_record_cost(db.conn(), &usage).unwrap();
    let expected = (60.0 * 10.0 + 40.0 * 1.0 + 20.0 * 100.0) / 1_000_000.0;
    assert!((cost - expected).abs() < 0.000001);
}

#[test]
fn test_calculate_codex_cost_uses_processing_mode_specific_price() {
    let db = Database::open_in_memory().unwrap();
    for (mode, rate) in [("standard", 10.0), ("batch", 5.0)] {
        insert_interval(
            db.conn(),
            &with_processing_mode(
                PricingInterval::usd(
                    ProviderId::Codex,
                    "gpt-audit",
                    TokenCategory::Input,
                    rate,
                    "2026-01-01T00:00:00Z".parse().unwrap(),
                    "test",
                ),
                mode,
            ),
        )
        .unwrap();
    }

    let mut usage = record("gpt-audit");
    usage.provider = crate::domain::provider::ProviderId::Codex;
    usage.model = ModelFamily::Unknown;
    usage.output_tokens = 0;
    usage.processing_mode = Some("batch".into());

    let cost = calculate_record_cost(db.conn(), &usage).unwrap();
    assert!((cost - 5.0).abs() < 0.000001);

    usage.processing_mode = Some("priority".into());
    let err = calculate_record_cost(db.conn(), &usage)
        .unwrap_err()
        .to_string();
    assert!(err.contains("missing price"));
    assert!(err.contains("processing_mode=priority"));
}

#[test]
fn test_calculate_claude_cost_requires_cache_creation_source_detail_prices() {
    let db = Database::open_in_memory().unwrap();
    for (category, rate) in [
        (TokenCategory::Input, 10.0),
        (TokenCategory::Output, 20.0),
        (TokenCategory::CacheRead, 1.0),
        (TokenCategory::CacheCreation, 12.0),
    ] {
        insert_interval(
            db.conn(),
            &interval(category, rate, "2026-01-01T00:00:00Z", None),
        )
        .unwrap();
    }

    let mut usage = record("claude-opus-4-6");
    usage.input_tokens = 0;
    usage.output_tokens = 0;
    usage.cache_creation_tokens = 150;
    usage.cache_creation_5m_tokens = 100;
    usage.cache_creation_1h_tokens = 50;

    let err = calculate_record_cost(db.conn(), &usage)
        .unwrap_err()
        .to_string();
    assert!(err.contains("source_detail=ephemeral_5m"));

    insert_interval(
        db.conn(),
        &with_source_detail(
            interval(
                TokenCategory::CacheCreation,
                12.0,
                "2026-01-01T00:00:00Z",
                None,
            ),
            "ephemeral_5m",
        ),
    )
    .unwrap();
    insert_interval(
        db.conn(),
        &with_source_detail(
            interval(
                TokenCategory::CacheCreation,
                60.0,
                "2026-01-01T00:00:00Z",
                None,
            ),
            "ephemeral_1h",
        ),
    )
    .unwrap();

    let cost = calculate_record_cost(db.conn(), &usage).unwrap();
    let expected = (100.0 * 12.0 + 50.0 * 60.0) / 1_000_000.0;
    assert!((cost - expected).abs() < 0.000001);
}

#[test]
fn test_calculate_claude_cost_requires_unsupported_speed_specific_price() {
    let db = Database::open_in_memory().unwrap();
    insert_interval(
        db.conn(),
        &interval(TokenCategory::Input, 10.0, "2026-01-01T00:00:00Z", None),
    )
    .unwrap();

    let mut usage = record("claude-opus-4-6");
    usage.output_tokens = 0;
    usage.speed = Some("turbo".into());

    let err = calculate_record_cost(db.conn(), &usage)
        .unwrap_err()
        .to_string();
    assert!(err.contains("speed=turbo"));
}

#[test]
fn test_seed_pricing_inserts_bundled_fallback_once() {
    let db = Database::open_in_memory().unwrap();
    let inserted = seed_pricing(db.conn()).unwrap();
    assert!(inserted > 0);
    assert_eq!(seed_pricing(db.conn()).unwrap(), 0);
    let legacy_count: i64 = db
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM pricing_intervals WHERE provider = 'claude'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let canonical_count: i64 = db
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM pricing_intervals WHERE provider = 'claude-code'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(legacy_count, 0);
    assert!(canonical_count > 0);

    let selected = applicable_interval_for_dimensions(
        db.conn(),
        ProviderId::Codex,
        "gpt-5.4",
        TokenCategory::CachedInput,
        "2026-05-24T00:00:00Z".parse().unwrap(),
        &PricingDimensions {
            processing_mode: Some("standard".into()),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(selected.rate_per_1m_tokens, 0.25);
    assert!(selected.source.starts_with("seed:"));
    let source_kind: String = db
        .conn()
        .query_row(
            "SELECT source_kind FROM pricing_sources WHERE source = ?1",
            [selected.source],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(source_kind, "bundled");
}

#[test]
fn test_bundled_pricing_catalog_schema_is_valid() {
    let catalog: PricingCatalog = serde_json::from_str(BUNDLED_PRICING_CATALOG_JSON).unwrap();
    validate_catalog(&catalog).unwrap();

    assert_eq!(catalog.schema_version, 1);
    assert!(catalog.notes.contains("offline pricing snapshot"));
    assert!(catalog.sources.iter().all(|source| {
        source.url.starts_with("https://")
            && !source.retrieved_at.is_empty()
            && !source.notes.is_empty()
    }));
    assert!(catalog.entries.iter().any(|entry| {
        entry.provider == "codex"
            && entry.dimensions.processing_mode.as_deref() == Some("standard")
            && entry.rates_per_1m_tokens.contains_key("cached_input")
    }));
}

#[test]
fn test_bundled_pricing_catalog_intervals_insert_and_audit_cleanly() {
    let intervals = bundled_catalog_intervals().unwrap();
    assert_eq!(intervals.len(), 86);

    let db = Database::open_in_memory().unwrap();
    assert_eq!(seed_pricing_intervals(db.conn(), &intervals).unwrap(), 86);
    applicable_interval_for_dimensions(
        db.conn(),
        ProviderId::Codex,
        "codex-auto-review",
        TokenCategory::Input,
        "2026-06-17T23:57:18.227Z".parse().unwrap(),
        &PricingDimensions {
            processing_mode: Some("standard".into()),
            ..Default::default()
        },
    )
    .expect("seeded Codex pricing should cover codex-auto-review standard input");
    let sol_output = applicable_interval_for_dimensions(
        db.conn(),
        ProviderId::Codex,
        "gpt-5.6-sol",
        TokenCategory::Output,
        "2026-07-01T20:09:01.866Z".parse().unwrap(),
        &PricingDimensions {
            processing_mode: Some("standard".into()),
            ..Default::default()
        },
    )
    .expect("seeded Codex pricing should cover GPT-5.6 Sol standard output");
    assert_eq!(sol_output.rate_per_1m_tokens, 30.0);
    let findings = audit_pricing(db.conn()).unwrap();
    assert!(findings.is_empty());
}

#[test]
fn test_bundled_pricing_covers_claude_cache_creation_ttl_dimensions() {
    let db = Database::open_in_memory().unwrap();
    db.seed_pricing().unwrap();

    let five_minute = applicable_interval_for_dimensions(
        db.conn(),
        ProviderId::ClaudeCode,
        "claude-sonnet-4-5-20250929",
        TokenCategory::CacheCreation,
        "2026-01-31T21:37:42.435Z".parse().unwrap(),
        &PricingDimensions {
            source_detail: Some("ephemeral_5m".into()),
            ..Default::default()
        },
    )
    .expect("seeded Claude pricing should cover 5-minute cache creation writes");
    assert_eq!(five_minute.rate_per_1m_tokens, 3.75);

    let one_hour = applicable_interval_for_dimensions(
        db.conn(),
        ProviderId::ClaudeCode,
        "claude-sonnet-4-5-20250929",
        TokenCategory::CacheCreation,
        "2026-01-31T21:37:42.435Z".parse().unwrap(),
        &PricingDimensions {
            source_detail: Some("ephemeral_1h".into()),
            ..Default::default()
        },
    )
    .expect("seeded Claude pricing should cover 1-hour cache creation writes");
    assert_eq!(one_hour.rate_per_1m_tokens, 6.0);
}

#[test]
fn test_pricing_catalog_docs_explain_snapshot_sources() {
    let docs = include_str!("../../../docs/pricing-catalog.md");
    assert!(docs.contains("pricing/catalog.json"));
    assert!(docs.contains("snapshot of official provider"));
    assert!(docs.contains("effective-dated SQLite"));
}

#[test]
fn test_seed_pricing_intervals_prevalidates_and_leaves_catalog_unchanged_on_error() {
    let db = Database::open_in_memory().unwrap();
    let valid = interval(TokenCategory::Input, 15.0, "2026-01-01T00:00:00Z", None);
    let invalid = interval(TokenCategory::Output, -1.0, "2026-01-01T00:00:00Z", None);

    let err = seed_pricing_intervals(db.conn(), &[valid, invalid])
        .unwrap_err()
        .to_string();

    assert!(err.contains("negative price"));
    let count: i64 = db
        .conn()
        .query_row("SELECT COUNT(*) FROM pricing_intervals", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(count, 0);
}

#[test]
fn test_import_pricing_catalog_closes_changed_open_interval() {
    let db = Database::open_in_memory().unwrap();
    assert_eq!(
        import_pricing_catalog_json(
            db.conn(),
            &single_input_catalog(10.0, "2026-01-01T00:00:00+00:00"),
        )
        .unwrap(),
        1
    );
    assert_eq!(
        import_pricing_catalog_json(
            db.conn(),
            &single_input_catalog(12.0, "2026-03-01T00:00:00+00:00"),
        )
        .unwrap(),
        2
    );

    let old = applicable_interval(
        db.conn(),
        ProviderId::ClaudeCode,
        "claude-opus-4-6",
        TokenCategory::Input,
        "2026-02-01T00:00:00Z".parse().unwrap(),
    )
    .unwrap();
    let new = applicable_interval(
        db.conn(),
        ProviderId::ClaudeCode,
        "claude-opus-4-6",
        TokenCategory::Input,
        "2026-04-01T00:00:00Z".parse().unwrap(),
    )
    .unwrap();
    assert_eq!(old.rate_per_1m_tokens, 10.0);
    assert_eq!(
        old.effective_to,
        Some("2026-03-01T00:00:00Z".parse().unwrap())
    );
    assert_eq!(new.rate_per_1m_tokens, 12.0);
    assert_eq!(new.effective_to, None);
    let source_kind: String = db
        .conn()
        .query_row(
            "SELECT source_kind FROM pricing_sources WHERE source = 'seed:test-source'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(source_kind, "reviewed");
}

#[test]
fn test_seed_pricing_covers_observed_claude_opus_20251101_categories() {
    let db = Database::open_in_memory().unwrap();
    seed_pricing(db.conn()).unwrap();

    for category in [
        TokenCategory::Input,
        TokenCategory::Output,
        TokenCategory::CacheRead,
        TokenCategory::CacheCreation,
    ] {
        let selected = applicable_interval(
            db.conn(),
            ProviderId::ClaudeCode,
            "claude-opus-4-5-20251101",
            category,
            "2026-01-31T21:20:19.858Z".parse().unwrap(),
        )
        .unwrap();
        assert_eq!(selected.currency, "USD");
        assert!(selected.rate_per_1m_tokens > 0.0);
    }
}

#[test]
fn test_pricing_audit_clean_seed_has_no_findings() {
    let db = Database::open_in_memory().unwrap();
    seed_pricing(db.conn()).unwrap();
    let findings = audit_pricing(db.conn()).unwrap();
    assert!(findings.is_empty());
}

#[test]
fn test_pricing_audit_detects_gap_between_intervals() {
    let db = Database::open_in_memory().unwrap();
    insert_interval(
        db.conn(),
        &interval(
            TokenCategory::Input,
            10.0,
            "2026-01-01T00:00:00Z",
            Some("2026-02-01T00:00:00Z"),
        ),
    )
    .unwrap();
    insert_interval(
        db.conn(),
        &interval(TokenCategory::Input, 20.0, "2026-03-01T00:00:00Z", None),
    )
    .unwrap();
    let findings = audit_pricing(db.conn()).unwrap();
    assert!(
        findings
            .iter()
            .any(|finding| finding.kind == PricingAuditKind::Gap)
    );
}

#[test]
fn test_pricing_audit_detects_overlapping_intervals() {
    let db = Database::open_in_memory().unwrap();
    insert_interval(
        db.conn(),
        &interval(
            TokenCategory::Input,
            10.0,
            "2026-01-01T00:00:00Z",
            Some("2026-03-01T00:00:00Z"),
        ),
    )
    .unwrap();
    insert_interval(
        db.conn(),
        &interval(TokenCategory::Input, 20.0, "2026-02-01T00:00:00Z", None),
    )
    .unwrap();
    let findings = audit_pricing(db.conn()).unwrap();
    assert!(
        findings
            .iter()
            .any(|finding| finding.kind == PricingAuditKind::Overlap)
    );
}

#[test]
fn test_pricing_audit_detects_duplicate_open_intervals() {
    let db = Database::open_in_memory().unwrap();
    insert_interval(
        db.conn(),
        &interval(TokenCategory::Input, 10.0, "2026-01-01T00:00:00Z", None),
    )
    .unwrap();
    insert_interval(
        db.conn(),
        &interval(TokenCategory::Input, 20.0, "2026-02-01T00:00:00Z", None),
    )
    .unwrap();
    let findings = audit_pricing(db.conn()).unwrap();
    assert!(
        findings
            .iter()
            .any(|finding| finding.kind == PricingAuditKind::DuplicateCurrent)
    );
}

#[test]
fn test_pricing_audit_treats_modifier_timelines_independently() {
    let db = Database::open_in_memory().unwrap();
    insert_interval(
        db.conn(),
        &interval(TokenCategory::Input, 10.0, "2026-01-01T00:00:00Z", None),
    )
    .unwrap();
    insert_interval(
        db.conn(),
        &with_speed(
            interval(
                TokenCategory::Input,
                20.0,
                "2026-01-01T00:00:00Z",
                Some("2026-02-01T00:00:00Z"),
            ),
            "turbo",
        ),
    )
    .unwrap();
    insert_interval(
        db.conn(),
        &with_speed(
            interval(TokenCategory::Input, 30.0, "2026-03-01T00:00:00Z", None),
            "turbo",
        ),
    )
    .unwrap();

    let findings = audit_pricing(db.conn()).unwrap();
    assert!(findings.iter().any(|finding| {
        finding.kind == PricingAuditKind::Gap && finding.remediation.contains("speed=turbo")
    }));
    assert!(!findings.iter().any(|finding| {
        finding.kind == PricingAuditKind::DuplicateCurrent
            || finding.kind == PricingAuditKind::Overlap
    }));
}

#[test]
fn test_pricing_audit_detects_unsupported_currency() {
    let db = Database::open_in_memory().unwrap();
    db.conn()
            .execute(
                "INSERT INTO pricing_intervals
                 (provider, model_id, token_category, currency, rate_per_1m_tokens, effective_from, effective_to, source)
                 VALUES ('claude-code', 'claude-opus-4-6', 'input', 'EUR', 1.0, '2026-01-01T00:00:00+00:00', NULL, 'test')",
                [],
            )
            .unwrap();
    let findings = audit_pricing(db.conn()).unwrap();
    assert!(
        findings
            .iter()
            .any(|finding| finding.kind == PricingAuditKind::UnsupportedCurrency)
    );
}

#[test]
fn test_pricing_audit_reports_billing_component_integrity_findings() {
    let db = Database::open_in_memory().unwrap();
    insert_interval(
        db.conn(),
        &interval(TokenCategory::Input, 10.0, "2026-01-01T00:00:00Z", None),
    )
    .unwrap();
    insert_interval(
        db.conn(),
        &interval(TokenCategory::Output, 20.0, "2026-01-01T00:00:00Z", None),
    )
    .unwrap();
    db.insert_records(&[record("claude-opus-4-6")]).unwrap();
    db.conn()
        .execute(
            "DELETE FROM usage_billing_components
                 WHERE provider = 'claude-code'
                   AND request_id = 'r1'
                   AND token_category = 'output'",
            [],
        )
        .unwrap();
    db.conn()
        .execute(
            "INSERT INTO usage_billing_components
                 (usage_id, provider, request_id, model_id, timestamp, token_category, tokens,
                  component_ordinal)
                 VALUES (9999, 'claude-code', 'missing-request', 'claude-opus-4-6',
                         '2026-04-07T10:00:00+00:00', 'input', 10, 999)",
            [],
        )
        .unwrap();

    let findings = audit_pricing(db.conn()).unwrap();
    assert!(findings.iter().any(|finding| {
        finding.kind == PricingAuditKind::BillingComponentIntegrity
            && finding
                .remediation
                .contains("expected 1000000 billable tokens")
            && finding.remediation.contains("found 0")
    }));
    assert!(findings.iter().any(|finding| {
        finding.kind == PricingAuditKind::BillingComponentIntegrity
            && finding.remediation.contains("missing-request")
            && finding.remediation.contains("no matching token_usage row")
    }));
}

#[test]
fn test_pricing_audit_reports_malformed_catalog_rows() {
    let db = Database::open_in_memory().unwrap();
    db.conn()
        .execute("PRAGMA ignore_check_constraints = ON", [])
        .unwrap();
    insert_raw_pricing_row(
        db.conn(),
        ProviderId::ClaudeCode.as_str(),
        "claude-opus-4-6",
        "mystery",
        1.0,
        "2026-01-01T00:00:00Z",
        "test",
    );
    insert_raw_pricing_row(
        db.conn(),
        ProviderId::ClaudeCode.as_str(),
        "claude-opus-4-7",
        "input",
        1.0,
        "not-a-date",
        "test",
    );
    insert_raw_pricing_row(
        db.conn(),
        ProviderId::ClaudeCode.as_str(),
        "claude-opus-4-8",
        "input",
        -1.0,
        "2026-01-01T00:00:00Z",
        "test",
    );
    insert_raw_pricing_row(
        db.conn(),
        "",
        "claude-opus-4-9",
        "input",
        1.0,
        "2026-01-01T00:00:00Z",
        "test",
    );
    insert_raw_pricing_row(
        db.conn(),
        "claude",
        "claude-opus-4-alias",
        "input",
        1.0,
        "2026-01-01T00:00:00Z",
        "test",
    );
    insert_raw_pricing_row(
        db.conn(),
        ProviderId::ClaudeCode.as_str(),
        "",
        "input",
        1.0,
        "2026-01-01T00:00:00Z",
        "test",
    );
    insert_raw_pricing_row(
        db.conn(),
        ProviderId::ClaudeCode.as_str(),
        "claude-opus-4-10",
        "input",
        1.0,
        "2026-01-01T00:00:00Z",
        "",
    );

    let findings = audit_pricing(db.conn()).unwrap();
    for (provider, model_id, category, remediation) in [
        (
            "claude-code",
            "claude-opus-4-6",
            "mystery",
            "supported token category",
        ),
        ("claude-code", "claude-opus-4-7", "input", "RFC3339"),
        (
            "claude-code",
            "claude-opus-4-8",
            "input",
            "non-negative rate",
        ),
        ("", "claude-opus-4-9", "input", "non-empty provider"),
        (
            "claude",
            "claude-opus-4-alias",
            "input",
            "canonical provider id",
        ),
        ("claude-code", "", "input", "non-empty model id"),
        (
            "claude-code",
            "claude-opus-4-10",
            "input",
            "non-empty source",
        ),
    ] {
        assert!(
            findings.iter().any(|finding| {
                finding.kind == PricingAuditKind::MalformedCatalogRow
                    && finding.provider == provider
                    && finding.model_id == model_id
                    && finding.token_category == category
                    && finding.remediation.contains(remediation)
            }),
            "missing malformed finding for {provider}/{model_id}/{category}: {remediation}; findings: {findings:#?}"
        );
    }
}

#[test]
fn test_pricing_audit_reports_noncanonical_catalog_timestamps() {
    let db = Database::open_in_memory().unwrap();
    db.conn()
        .execute("PRAGMA ignore_check_constraints = ON", [])
        .unwrap();
    for (model_id, from, to) in [
        ("zulu-utc", "2026-04-07T10:00:00Z", None),
        ("naive", "2026-04-07 10:00:00", None),
        ("local-offset", "2026-04-07T03:00:00-07:00", None),
        (
            "noncanonical-to",
            "2026-04-07T10:00:00+00:00",
            Some("2026-04-08T03:30:00-07:00"),
        ),
    ] {
        insert_raw_pricing_row_with_to(db.conn(), model_id, from, to);
    }

    let findings = audit_pricing(db.conn()).unwrap();
    for model_id in ["zulu-utc", "naive", "local-offset", "noncanonical-to"] {
        assert!(
            findings.iter().any(|finding| {
                finding.kind == PricingAuditKind::MalformedCatalogRow
                    && finding.model_id == model_id
                    && finding.remediation.contains("canonical UTC RFC3339")
            }),
            "missing noncanonical catalog timestamp finding for {model_id}: {findings:#?}"
        );
    }
}

#[test]
fn test_pricing_audit_detects_missing_cached_token_pricing_for_usage() {
    let db = Database::open_in_memory().unwrap();
    let mut usage = record("gpt-audit");
    usage.provider = crate::domain::provider::ProviderId::Codex;
    usage.model = ModelFamily::Unknown;
    usage.input_tokens = 0;
    usage.output_tokens = 0;
    usage.cached_input_tokens = 10;
    db.insert_records(&[usage]).unwrap();

    let findings = audit_pricing(db.conn()).unwrap();
    assert!(findings.iter().any(|finding| {
        finding.kind == PricingAuditKind::MissingCoverage
            && finding.provider == "codex"
            && finding.model_id == "gpt-audit"
            && finding.token_category == "cached_input"
    }));
}

#[test]
fn test_pricing_audit_reports_stale_source_metadata_for_used_prices() {
    let db = Database::open_in_memory().unwrap();
    let mut price = interval(TokenCategory::Input, 10.0, "2026-01-01T00:00:00Z", None);
    price.source = "reviewed:old-source".into();
    insert_interval(db.conn(), &price).unwrap();
    upsert_source_metadata(
        db.conn(),
        &source_metadata("reviewed:old-source", "2025-01-01", "reviewed"),
    )
    .unwrap();
    let mut usage = record("claude-opus-4-6");
    usage.output_tokens = 0;
    db.insert_records(&[usage]).unwrap();

    let findings = audit_pricing(db.conn()).unwrap();
    assert!(findings.iter().any(|finding| {
        finding.kind == PricingAuditKind::StaleSource
            && finding.provider == "claude-code"
            && finding.model_id == "claude-opus-4-6"
            && finding.token_category == "input"
            && finding.remediation.contains("reviewed:old-source")
    }));
}

#[test]
fn test_pricing_audit_stale_source_boundary_uses_reference_date() {
    assert!(
        !stale_audit_finding_present_for_retrieved_at("2026-02-23"),
        "retrieval date exactly at the 90-day cutoff should not be stale"
    );
    assert!(
        stale_audit_finding_present_for_retrieved_at("2026-02-22"),
        "retrieval date just before the 90-day cutoff should be stale"
    );
    assert!(
        !stale_audit_finding_present_for_retrieved_at("2026-02-24"),
        "retrieval date just after the 90-day cutoff should not be stale"
    );
}

#[test]
fn test_pricing_audit_reports_missing_source_metadata_for_used_prices() {
    let db = Database::open_in_memory().unwrap();
    let mut price = interval(TokenCategory::Input, 10.0, "2026-01-01T00:00:00Z", None);
    price.source = "manual:without-metadata".into();
    insert_interval(db.conn(), &price).unwrap();
    let mut usage = record("claude-opus-4-6");
    usage.output_tokens = 0;
    db.insert_records(&[usage]).unwrap();

    let findings = audit_pricing(db.conn()).unwrap();
    assert!(findings.iter().any(|finding| {
        finding.kind == PricingAuditKind::MissingSourceMetadata
            && finding.provider == "claude-code"
            && finding.model_id == "claude-opus-4-6"
            && finding.token_category == "input"
            && finding.remediation.contains("manual:without-metadata")
    }));
}

#[test]
fn test_pricing_audit_reports_unknown_observed_model_ids() {
    let db = Database::open_in_memory().unwrap();
    let mut usage = record("claude-unknown-4-9");
    usage.output_tokens = 0;
    db.insert_records(&[usage]).unwrap();

    let findings = audit_pricing(db.conn()).unwrap();
    assert!(findings.iter().any(|finding| {
        finding.kind == PricingAuditKind::UnknownObservedModel
            && finding.provider == "claude-code"
            && finding.model_id == "claude-unknown-4-9"
            && finding.token_category.is_empty()
    }));
}

#[test]
fn test_pricing_audit_reports_observed_modifiers_without_specialized_prices() {
    let db = Database::open_in_memory().unwrap();
    insert_interval(
        db.conn(),
        &interval(TokenCategory::Input, 10.0, "2026-01-01T00:00:00Z", None),
    )
    .unwrap();
    let mut usage = record("claude-opus-4-6");
    usage.output_tokens = 0;
    usage.speed = Some("turbo".into());
    db.insert_records(&[usage]).unwrap();

    let findings = audit_pricing(db.conn()).unwrap();
    assert!(findings.iter().any(|finding| {
        finding.kind == PricingAuditKind::UnsupportedModifier
            && finding.provider == "claude-code"
            && finding.model_id == "claude-opus-4-6"
            && finding.token_category == "input"
            && finding.remediation.contains("speed=turbo")
    }));
}

#[test]
fn test_pricing_audit_reports_bundled_fallback_usage() {
    let db = Database::open_in_memory().unwrap();
    seed_pricing(db.conn()).unwrap();
    let mut usage = record("claude-opus-4-6");
    usage.output_tokens = 0;
    db.insert_records(&[usage]).unwrap();

    let findings = audit_pricing(db.conn()).unwrap();
    assert!(findings.iter().any(|finding| {
        finding.kind == PricingAuditKind::BundledFallbackSource
            && finding.severity == PricingAuditSeverity::Info
            && finding.provider == "claude-code"
            && finding.model_id == "claude-opus-4-6"
            && finding.token_category == "input"
    }));
}

#[test]
fn test_pricing_audit_reports_unsupported_usage_provider_without_aborting() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
            "CREATE TABLE pricing_intervals (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                provider TEXT NOT NULL,
                model_id TEXT NOT NULL,
                token_category TEXT NOT NULL,
                currency TEXT NOT NULL DEFAULT 'USD',
                rate_per_1m_tokens REAL NOT NULL,
                effective_from TEXT NOT NULL,
                effective_to TEXT,
                source TEXT NOT NULL
             );
             CREATE TABLE token_usage (
                provider TEXT NOT NULL,
                request_id TEXT NOT NULL,
                session_id TEXT NOT NULL,
                uuid TEXT NOT NULL,
                timestamp TEXT NOT NULL,
                model_family TEXT NOT NULL,
                model_id TEXT NOT NULL,
                input_tokens INTEGER NOT NULL,
                output_tokens INTEGER NOT NULL,
                cache_creation_tokens INTEGER NOT NULL,
                cache_read_tokens INTEGER NOT NULL,
                cached_input_tokens INTEGER NOT NULL DEFAULT 0,
                reasoning_output_tokens INTEGER NOT NULL DEFAULT 0,
                total_tokens INTEGER NOT NULL,
                cost_usd REAL NOT NULL,
                project TEXT NOT NULL,
                source_file TEXT NOT NULL,
                is_subagent INTEGER NOT NULL DEFAULT 0
             );
             INSERT INTO token_usage
                (provider, request_id, session_id, uuid, timestamp, model_family, model_id,
                 input_tokens, output_tokens, cache_creation_tokens, cache_read_tokens,
                 cached_input_tokens, reasoning_output_tokens, total_tokens, cost_usd, project, source_file, is_subagent)
             VALUES
                ('unsupported-provider', 'r1', 's1', 'u1', '2026-04-07T10:00:00+00:00', 'unknown', 'legacy-model',
                 10, 0, 0, 0, 0, 0, 10, 0.0, 'legacy', '/legacy.jsonl', 0);",
        )
        .unwrap();

    let findings = audit_pricing(&conn).unwrap();
    assert!(findings.iter().any(|finding| {
        finding.kind == PricingAuditKind::UnsupportedProviderId
            && finding.provider == "unsupported-provider"
            && finding.model_id == "legacy-model"
            && finding.token_category.is_empty()
            && finding
                .remediation
                .contains("supported canonical provider id")
    }));
}

#[test]
fn test_pricing_audit_reports_malformed_usage_timestamps_without_aborting() {
    let db = Database::open_in_memory().unwrap();
    for (request_id, timestamp, model_id) in [
        ("bad", "not-a-date", "bad-model"),
        ("naive", "2026-04-07 10:00:00", "naive-model"),
        ("offset", "2026-04-07T03:00:00-07:00", "offset-model"),
    ] {
        db.conn()
                .execute(
                    "INSERT INTO token_usage
                     (provider, request_id, session_id, uuid, timestamp, model_family, model_id,
                      input_tokens, output_tokens, cache_creation_tokens, cache_read_tokens,
                      cached_input_tokens, reasoning_output_tokens, cost_usd, project, source_file, is_subagent)
                     VALUES ('claude-code', ?1, 's1', 'u1', ?2, 'unknown', ?3,
                      10, 0, 0, 0, 0, 0, 0.0, 'manual', '/manual.jsonl', 0)",
                    rusqlite::params![request_id, timestamp, model_id],
                )
                .unwrap();
    }

    let findings = audit_pricing(db.conn()).unwrap();
    for model_id in ["bad-model", "naive-model", "offset-model"] {
        assert!(
            findings.iter().any(|finding| {
                finding.kind == PricingAuditKind::MalformedUsageRow
                    && finding.provider == "claude-code"
                    && finding.model_id == model_id
                    && finding.remediation.contains("canonical UTC RFC3339")
            }),
            "missing malformed usage timestamp finding for {model_id}: {findings:#?}"
        );
    }
}

#[test]
fn test_pricing_audit_does_not_require_separate_codex_reasoning_price() {
    let db = Database::open_in_memory().unwrap();
    insert_interval(
        db.conn(),
        &PricingInterval::usd(
            ProviderId::Codex,
            "gpt-audit",
            TokenCategory::Output,
            100.0,
            "2026-01-01T00:00:00Z".parse().unwrap(),
            "test",
        ),
    )
    .unwrap();
    let mut usage = record("gpt-audit");
    usage.provider = crate::domain::provider::ProviderId::Codex;
    usage.model = ModelFamily::Unknown;
    usage.input_tokens = 0;
    usage.output_tokens = 20;
    usage.reasoning_output_tokens = 7;
    db.insert_records(&[usage]).unwrap();

    let findings = audit_pricing(db.conn()).unwrap();
    assert!(!findings.iter().any(|finding| {
        finding.kind == PricingAuditKind::MissingCoverage
            && finding.token_category == "reasoning_output"
    }));
}

#[test]
fn test_pricing_audit_detects_usage_before_first_price_interval() {
    let db = Database::open_in_memory().unwrap();
    insert_interval(
        db.conn(),
        &interval(TokenCategory::Input, 10.0, "2026-01-01T00:00:00Z", None),
    )
    .unwrap();
    let mut usage = record("claude-opus-4-6");
    usage.timestamp = "2025-12-31T23:00:00Z".parse().unwrap();
    usage.output_tokens = 0;
    db.insert_records(&[usage]).unwrap();

    let findings = audit_pricing(db.conn()).unwrap();
    assert!(
        findings
            .iter()
            .any(|finding| finding.kind == PricingAuditKind::UsageBeforeFirstInterval)
    );
}

#[test]
fn test_refresh_pricing_noops_when_prices_unchanged() {
    let db = Database::open_in_memory().unwrap();
    let interval = interval(TokenCategory::Input, 15.0, "2026-01-01T00:00:00Z", None);
    insert_interval(db.conn(), &interval).unwrap();

    let snapshot = snapshot(vec![interval]);
    assert_eq!(refresh_pricing(db.conn(), &snapshot).unwrap(), 0);
}

#[test]
fn test_refresh_pricing_closes_previous_open_interval_when_price_changes() {
    let db = Database::open_in_memory().unwrap();
    insert_interval(
        db.conn(),
        &interval(TokenCategory::Input, 10.0, "2026-01-01T00:00:00Z", None),
    )
    .unwrap();

    let changed = interval(TokenCategory::Input, 15.0, "2026-04-01T00:00:00Z", None);
    let snapshot = snapshot(vec![changed]);
    assert_eq!(refresh_pricing(db.conn(), &snapshot).unwrap(), 2);

    let old_to: String = db
        .conn()
        .query_row(
            "SELECT effective_to FROM pricing_intervals WHERE rate_per_1m_tokens = 10.0",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(old_to, "2026-04-01T00:00:00+00:00");

    let selected = applicable_interval(
        db.conn(),
        ProviderId::ClaudeCode,
        "claude-opus-4-6",
        TokenCategory::Input,
        "2026-04-07T10:00:00Z".parse().unwrap(),
    )
    .unwrap();
    assert_eq!(selected.rate_per_1m_tokens, 15.0);

    let (count, open_count): (i64, i64) = db
        .conn()
        .query_row(
            "SELECT COUNT(*), SUM(CASE WHEN effective_to IS NULL THEN 1 ELSE 0 END)
                 FROM pricing_intervals
                 WHERE provider = 'claude-code'
                   AND model_id = 'claude-opus-4-6'
                   AND token_category = 'input'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(count, 2);
    assert_eq!(open_count, 1);
}

#[test]
fn test_refresh_pricing_replaces_same_effective_date_interval_in_place() {
    let db = Database::open_in_memory().unwrap();
    let mut original = interval(TokenCategory::Input, 10.0, "2026-01-01T00:00:00Z", None);
    original.source = "seed:old-source".into();
    insert_interval(db.conn(), &original).unwrap();

    let mut corrected = interval(TokenCategory::Input, 15.0, "2026-01-01T00:00:00Z", None);
    corrected.source = "reviewed:corrected-source".into();
    let snapshot = snapshot(vec![corrected]);
    assert_eq!(refresh_pricing(db.conn(), &snapshot).unwrap(), 1);

    let (count, open_count, rate, source): (i64, i64, f64, String) = db
        .conn()
        .query_row(
            "SELECT COUNT(*),
                        SUM(CASE WHEN effective_to IS NULL THEN 1 ELSE 0 END),
                        MAX(rate_per_1m_tokens),
                        MAX(source)
                 FROM pricing_intervals
                 WHERE provider = 'claude-code'
                   AND model_id = 'claude-opus-4-6'
                   AND token_category = 'input'
                   AND effective_from = '2026-01-01T00:00:00+00:00'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!(count, 1);
    assert_eq!(open_count, 1);
    assert_eq!(rate, 15.0);
    assert_eq!(source, "reviewed:corrected-source");

    let selected = applicable_interval(
        db.conn(),
        ProviderId::ClaudeCode,
        "claude-opus-4-6",
        TokenCategory::Input,
        "2026-04-07T10:00:00Z".parse().unwrap(),
    )
    .unwrap();
    assert_eq!(selected.rate_per_1m_tokens, 15.0);
}

#[test]
fn test_pricing_audit_accepts_same_effective_date_replacement() {
    let db = Database::open_in_memory().unwrap();
    insert_interval(
        db.conn(),
        &interval(TokenCategory::Input, 10.0, "2026-01-01T00:00:00Z", None),
    )
    .unwrap();

    let corrected = interval(TokenCategory::Input, 15.0, "2026-01-01T00:00:00Z", None);
    let snapshot = snapshot(vec![corrected]);
    refresh_pricing(db.conn(), &snapshot).unwrap();

    let findings = audit_pricing(db.conn()).unwrap();
    assert!(
        findings.iter().all(|finding| {
            !matches!(
                finding.kind,
                PricingAuditKind::MissingCurrent
                    | PricingAuditKind::Gap
                    | PricingAuditKind::Overlap
            )
        }),
        "unexpected lifecycle finding after same-date replacement: {findings:?}"
    );
}

#[test]
fn test_refresh_pricing_updates_only_matching_pricing_dimensions() {
    let db = Database::open_in_memory().unwrap();
    insert_interval(
        db.conn(),
        &interval(TokenCategory::Input, 10.0, "2026-01-01T00:00:00Z", None),
    )
    .unwrap();
    insert_interval(
        db.conn(),
        &with_speed(
            interval(TokenCategory::Input, 20.0, "2026-01-01T00:00:00Z", None),
            "turbo",
        ),
    )
    .unwrap();

    let changed = with_speed(
        interval(TokenCategory::Input, 30.0, "2026-04-01T00:00:00Z", None),
        "turbo",
    );
    let snapshot = snapshot(vec![changed]);
    assert_eq!(refresh_pricing(db.conn(), &snapshot).unwrap(), 2);

    let default_open_count: i64 = db
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM pricing_intervals
                 WHERE speed IS NULL AND effective_to IS NULL",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(default_open_count, 1);

    let old_fast_to: String = db
        .conn()
        .query_row(
            "SELECT effective_to FROM pricing_intervals
                 WHERE speed = 'turbo' AND rate_per_1m_tokens = 20.0",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(old_fast_to, "2026-04-01T00:00:00+00:00");
}

#[test]
fn test_refresh_pricing_rolls_back_when_later_interval_is_invalid() {
    let db = Database::open_in_memory().unwrap();
    let valid = interval(TokenCategory::Input, 15.0, "2026-01-01T00:00:00Z", None);
    let mut invalid = interval(TokenCategory::Output, 75.0, "2026-01-01T00:00:00Z", None);
    invalid.currency = "EUR".into();
    let snapshot = snapshot(vec![valid, invalid]);

    let err = refresh_pricing(db.conn(), &snapshot)
        .unwrap_err()
        .to_string();
    assert!(err.contains("unsupported pricing currency"));
    let count: i64 = db
        .conn()
        .query_row("SELECT COUNT(*) FROM pricing_intervals", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(count, 0);
}

#[test]
fn test_refresh_pricing_rejects_stale_effective_interval_and_preserves_existing() {
    let db = Database::open_in_memory().unwrap();
    insert_interval(
        db.conn(),
        &interval(TokenCategory::Input, 10.0, "2026-05-01T00:00:00Z", None),
    )
    .unwrap();
    let stale = interval(TokenCategory::Input, 15.0, "2026-04-01T00:00:00Z", None);
    let snapshot = snapshot(vec![stale]);

    let err = refresh_pricing(db.conn(), &snapshot)
        .unwrap_err()
        .to_string();
    assert!(err.contains("stale pricing interval"));
    assert!(err.contains("2026-04-01T00:00:00+00:00"));
    assert!(err.contains("2026-05-01T00:00:00+00:00"));

    let (count, open_count, rate): (i64, i64, f64) = db
            .conn()
            .query_row(
                "SELECT COUNT(*), SUM(CASE WHEN effective_to IS NULL THEN 1 ELSE 0 END), MAX(rate_per_1m_tokens)
                 FROM pricing_intervals",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
    assert_eq!(count, 1);
    assert_eq!(open_count, 1);
    assert_eq!(rate, 10.0);
}

#[test]
fn test_offline_fallback_uses_cached_prices_when_refresh_fails() {
    let db = Database::open_in_memory().unwrap();
    let interval = interval(TokenCategory::Input, 15.0, "2026-01-01T00:00:00Z", None);
    insert_interval(db.conn(), &interval).unwrap();

    let fetched: Result<PricingSnapshot> = Err(anyhow!("offline"));
    assert!(fetched.is_err());
    let selected = applicable_interval(
        db.conn(),
        ProviderId::ClaudeCode,
        "claude-opus-4-6",
        TokenCategory::Input,
        "2026-04-07T10:00:00Z".parse().unwrap(),
    )
    .unwrap();
    assert_eq!(selected.rate_per_1m_tokens, 15.0);
}
