use super::*;
use crate::db::Database;
use crate::db::pricing::{PricingSourceMetadata, calculate_record_cost, upsert_source_metadata};
use crate::domain::pricing::{PricingInterval, TokenCategory};
use crate::domain::usage::ModelFamily;

#[test]
fn cost_reports_join_materialized_costs_not_component_prices() {
    let join = cost_join_sql(true);
    assert!(join.contains("usage_costs"));
    assert!(!join.contains("usage_billing_components"));
    assert!(!join.contains("pricing_intervals"));
}

fn seed_db(db: &Database) {
    db.seed_pricing().unwrap();
    let records = vec![
        make_record("r1", "2026-04-05T10:00:00Z", "opus", "proj-a", 100, 50),
        make_record("r2", "2026-04-05T14:00:00Z", "sonnet", "proj-a", 200, 80),
        make_record("r3", "2026-04-06T09:00:00Z", "opus", "proj-b", 300, 120),
        make_record("r4", "2026-04-07T11:00:00Z", "haiku", "proj-a", 50, 20),
        make_record("r5", "2026-04-07T15:00:00Z", "opus", "proj-a", 500, 200),
    ];
    db.insert_records(&records).unwrap();
}

fn make_record(
    id: &str,
    ts: &str,
    family: &str,
    project: &str,
    input: u64,
    output: u64,
) -> crate::domain::usage::TokenRecord {
    crate::domain::usage::TokenRecord {
        provider: crate::domain::provider::ProviderId::ClaudeCode,
        request_id: id.into(),
        session_id: format!("s-{id}"),
        uuid: format!("u-{id}"),
        timestamp: ts.parse().unwrap(),
        model: family.parse().unwrap_or(ModelFamily::Unknown),
        model_id: format!("claude-{family}-4-6"),
        input_tokens: input,
        output_tokens: output,
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
        cost_usd: (input as f64 * 15.0 + output as f64 * 75.0) / 1_000_000.0,
        project: project.into(),
        source_file: "/test.jsonl".into(),
        is_subagent: false,
    }
}

fn sqlite_local_day(conn: &Connection, timestamp: &str) -> String {
    conn.query_row("SELECT date(?1, 'localtime')", [timestamp], |row| {
        row.get(0)
    })
    .unwrap()
}

fn sqlite_local_hour(conn: &Connection, timestamp: &str) -> String {
    conn.query_row(
        "SELECT strftime('%Y-%m-%d %H:00', ?1, 'localtime')",
        [timestamp],
        |row| row.get(0),
    )
    .unwrap()
}

fn price(
    model_id: &str,
    category: TokenCategory,
    rate: f64,
    from: &str,
    to: Option<&str>,
) -> PricingInterval {
    provider_price("claude-code", model_id, category, rate, from, to)
}

fn provider_price(
    provider: &str,
    model_id: &str,
    category: TokenCategory,
    rate: f64,
    from: &str,
    to: Option<&str>,
) -> PricingInterval {
    let mut interval = PricingInterval::usd(
        crate::domain::provider::ProviderId::from_canonical(provider).unwrap(),
        model_id,
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

fn with_region(mut interval: PricingInterval, region: &str) -> PricingInterval {
    interval.dimensions.region = Some(region.into());
    interval
}

fn reviewed_source(source: &str) -> PricingSourceMetadata {
    PricingSourceMetadata {
        source: source.into(),
        source_url: "https://example.com/pricing".into(),
        source_retrieved_at: "2026-05-23".into(),
        catalog_version: "1".into(),
        source_kind: "reviewed".into(),
        notes: "reviewed test pricing source".into(),
    }
}

fn stale_cost_assumption_present_for_retrieved_at(retrieved_at: &str) -> bool {
    let db = Database::open_in_memory().unwrap();
    let source = format!("reviewed:boundary-{retrieved_at}");
    let mut interval = provider_price(
        "claude-code",
        "claude-opus-4-6",
        TokenCategory::Input,
        10.0,
        "2026-01-01T00:00:00Z",
        None,
    );
    interval.source = source.clone();
    db.insert_pricing_interval(&interval).unwrap();
    let mut metadata = reviewed_source(&source);
    metadata.source_retrieved_at = retrieved_at.into();
    upsert_source_metadata(db.conn(), &metadata).unwrap();

    let mut record = make_record(
        "stale-boundary",
        "2026-04-05T10:00:00Z",
        "opus",
        "proj-a",
        1_000_000,
        0,
    );
    record.output_tokens = 0;
    db.insert_records(&[record]).unwrap();

    let explanation = explain_cost_at(
        db.conn(),
        &QueryFilter {
            include_subagents: true,
            ..Default::default()
        },
        NaiveDate::from_ymd_opt(2026, 5, 24).unwrap(),
    )
    .unwrap();
    explanation.assumptions.iter().any(|assumption| {
        assumption.kind == CostAssumptionKind::StalePricingSource
            && assumption.source.as_deref() == Some(source.as_str())
    })
}

#[test]
fn test_query_daily() {
    let db = Database::open_in_memory().unwrap();
    seed_db(&db);
    let filter = QueryFilter {
        include_subagents: true,
        ..Default::default()
    };
    let rows = query_by_period(db.conn(), TimePeriod::Daily, &filter, 30).unwrap();
    assert!(rows.len() >= 2);
}

#[test]
fn test_query_with_model_filter() {
    let db = Database::open_in_memory().unwrap();
    seed_db(&db);
    let filter = QueryFilter {
        model: Some("opus".into()),
        include_subagents: true,
        ..Default::default()
    };
    let rows = query_by_period(db.conn(), TimePeriod::Daily, &filter, 30).unwrap();
    let total_input: u64 = rows.iter().map(|r| r.input_tokens).sum();
    assert_eq!(total_input, 100 + 300 + 500);
}

#[test]
fn test_query_with_exact_model_id_filter() {
    let db = Database::open_in_memory().unwrap();
    db.seed_pricing().unwrap();
    let mut opus_45 = make_record("r1", "2026-04-05T10:00:00Z", "opus", "proj-a", 100, 50);
    opus_45.model_id = "claude-opus-4-5-20250929".into();
    let mut opus_46 = make_record("r2", "2026-04-05T11:00:00Z", "opus", "proj-a", 200, 50);
    opus_46.model_id = "claude-opus-4-6".into();
    db.insert_records(&[opus_45, opus_46]).unwrap();

    let filter = QueryFilter {
        model: Some("claude-opus-4-5-20250929".into()),
        include_subagents: true,
        ..Default::default()
    };
    let summary = query_summary(db.conn(), &filter).unwrap();
    assert_eq!(summary.request_count, 1);
    assert_eq!(summary.input_tokens, 100);
}

#[test]
fn test_query_with_model_family_filter_keeps_alias_behavior() {
    let db = Database::open_in_memory().unwrap();
    seed_db(&db);
    let filter = QueryFilter {
        model_family: Some("opus".into()),
        include_subagents: true,
        ..Default::default()
    };
    let summary = query_summary(db.conn(), &filter).unwrap();
    assert_eq!(summary.input_tokens, 100 + 300 + 500);
}

#[test]
fn test_query_by_model_groups_provider_and_exact_model_id() {
    let db = Database::open_in_memory().unwrap();
    seed_db(&db);
    let mut codex = make_record(
        "r-codex",
        "2026-04-05T10:00:00Z",
        "unknown",
        "proj-a",
        700,
        80,
    );
    codex.provider = crate::domain::provider::ProviderId::Codex;
    codex.model_id = "gpt-5.1-codex".into();
    codex.processing_mode = Some("standard".into());
    db.insert_records(&[codex]).unwrap();

    let filter = QueryFilter {
        include_subagents: true,
        ..Default::default()
    };
    let rows = query_by_model(db.conn(), &filter, 10).unwrap();

    assert!(rows.iter().any(|r| {
        r.provider.as_deref() == Some("claude-code")
            && r.model_id.as_deref() == Some("claude-opus-4-6")
    }));
    assert!(rows.iter().any(|r| {
        r.provider.as_deref() == Some("codex") && r.model_id.as_deref() == Some("gpt-5.1-codex")
    }));
}

#[test]
fn test_query_provider_filter_and_combined_provider_aggregation() {
    let db = Database::open_in_memory().unwrap();
    db.seed_pricing().unwrap();
    let mut claude = make_record(
        "r-claude",
        "2026-04-05T10:00:00Z",
        "sonnet",
        "proj-a",
        100,
        50,
    );
    claude.session_id = "shared-session".into();
    let mut codex = make_record(
        "r-codex",
        "2026-04-05T10:00:00Z",
        "unknown",
        "proj-a",
        200,
        60,
    );
    codex.provider = crate::domain::provider::ProviderId::Codex;
    codex.model_id = "gpt-5.5".into();
    codex.session_id = "shared-session".into();
    codex.processing_mode = Some("standard".into());
    db.insert_records(&[claude, codex]).unwrap();

    let codex_summary = query_summary(
        db.conn(),
        &QueryFilter {
            provider: Some(crate::domain::provider::ProviderId::Codex),
            include_subagents: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(codex_summary.request_count, 1);
    assert_eq!(codex_summary.input_tokens, 200);

    let combined = query_summary(
        db.conn(),
        &QueryFilter {
            include_subagents: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(combined.request_count, 2);
    assert_eq!(combined.input_tokens, 300);
    assert_eq!(combined.session_count, 2);
}

#[test]
fn test_query_by_provider_groups_totals_and_respects_provider_filter() {
    let db = Database::open_in_memory().unwrap();
    db.seed_pricing().unwrap();
    let mut claude = make_record(
        "r-claude",
        "2026-04-05T10:00:00Z",
        "sonnet",
        "proj-a",
        100,
        50,
    );
    claude.session_id = "shared-session".into();
    let mut codex = make_record(
        "r-codex",
        "2026-04-05T10:00:00Z",
        "unknown",
        "proj-a",
        200,
        60,
    );
    codex.provider = crate::domain::provider::ProviderId::Codex;
    codex.model_id = "gpt-5.5".into();
    codex.session_id = "shared-session".into();
    codex.processing_mode = Some("standard".into());
    db.insert_records(&[claude, codex]).unwrap();

    let rows = query_by_provider(
        db.conn(),
        &QueryFilter {
            include_subagents: true,
            ..Default::default()
        },
        10,
    )
    .unwrap();
    assert_eq!(rows.len(), 2);
    let codex = rows
        .iter()
        .find(|row| row.provider.as_deref() == Some("codex"))
        .unwrap();
    assert_eq!(codex.period, "codex");
    assert_eq!(codex.request_count, 1);
    assert_eq!(codex.input_tokens, 200);
    assert_eq!(codex.session_count, 1);

    let filtered = query_by_provider(
        db.conn(),
        &QueryFilter {
            provider: Some(crate::domain::provider::ProviderId::Codex),
            include_subagents: true,
            ..Default::default()
        },
        10,
    )
    .unwrap();
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0].provider.as_deref(), Some("codex"));
}

#[test]
fn test_query_by_provider_respects_project_and_model_filters() {
    let db = Database::open_in_memory().unwrap();
    seed_db(&db);
    let rows = query_by_provider(
        db.conn(),
        &QueryFilter {
            project: Some("proj-b".into()),
            model: Some("opus".into()),
            include_subagents: true,
            ..Default::default()
        },
        10,
    )
    .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].provider.as_deref(), Some("claude-code"));
    assert_eq!(rows[0].input_tokens, 300);
}

#[test]
fn test_query_by_project_groups_totals_and_unknown_project() {
    let db = Database::open_in_memory().unwrap();
    seed_db(&db);
    let mut unknown = make_record(
        "r-unknown-project",
        "2026-04-05T10:00:00Z",
        "unknown",
        "",
        25,
        5,
    );
    unknown.provider = crate::domain::provider::ProviderId::Codex;
    unknown.model_id = "gpt-5.5".into();
    unknown.processing_mode = Some("standard".into());
    db.insert_records(&[unknown]).unwrap();

    let rows = query_by_project(
        db.conn(),
        &QueryFilter {
            include_subagents: true,
            ..Default::default()
        },
        10,
    )
    .unwrap();
    let proj_a = rows
        .iter()
        .find(|row| row.project.as_deref() == Some("proj-a"))
        .unwrap();
    assert_eq!(proj_a.input_tokens, 100 + 200 + 50 + 500);
    let unknown = rows
        .iter()
        .find(|row| row.project.as_deref() == Some("unknown"))
        .unwrap();
    assert_eq!(unknown.period, "unknown");
    assert_eq!(unknown.input_tokens, 25);
}

#[test]
fn test_query_by_project_respects_project_provider_and_model_filters() {
    let db = Database::open_in_memory().unwrap();
    seed_db(&db);
    let rows = query_by_project(
        db.conn(),
        &QueryFilter {
            provider: Some(crate::domain::provider::ProviderId::ClaudeCode),
            project: Some("proj-b".into()),
            model: Some("opus".into()),
            include_subagents: true,
            ..Default::default()
        },
        10,
    )
    .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].project.as_deref(), Some("proj-b"));
    assert_eq!(rows[0].input_tokens, 300);
}

#[test]
fn test_query_with_project_filter() {
    let db = Database::open_in_memory().unwrap();
    seed_db(&db);
    let filter = QueryFilter {
        project: Some("proj-b".into()),
        include_subagents: true,
        ..Default::default()
    };
    let rows = query_by_period(db.conn(), TimePeriod::Daily, &filter, 30).unwrap();
    let total_input: u64 = rows.iter().map(|r| r.input_tokens).sum();
    assert_eq!(total_input, 300);
}

#[test]
fn test_query_top() {
    let db = Database::open_in_memory().unwrap();
    seed_db(&db);
    let filter = QueryFilter {
        include_subagents: true,
        ..Default::default()
    };
    let rows = query_top(db.conn(), &filter, 10).unwrap();
    assert!(!rows.is_empty());
    // Verify descending order by total_tokens
    for w in rows.windows(2) {
        assert!(
            w[0].total_tokens >= w[1].total_tokens,
            "expected descending order: {} >= {}",
            w[0].total_tokens,
            w[1].total_tokens
        );
    }
}

#[test]
fn test_query_summary() {
    let db = Database::open_in_memory().unwrap();
    seed_db(&db);
    let filter = QueryFilter {
        include_subagents: true,
        ..Default::default()
    };
    let summary = query_summary(db.conn(), &filter).unwrap();
    assert_eq!(summary.request_count, 5);
    assert_eq!(summary.input_tokens, 100 + 200 + 300 + 50 + 500);
}

#[test]
fn test_query_date_range() {
    let db = Database::open_in_memory().unwrap();
    seed_db(&db);
    let filter = QueryFilter {
        begin: NaiveDate::from_ymd_opt(2026, 4, 6),
        end: NaiveDate::from_ymd_opt(2026, 4, 6),
        include_subagents: true,
        ..Default::default()
    };
    let summary = query_summary(db.conn(), &filter).unwrap();
    assert_eq!(summary.request_count, 1);
    assert_eq!(summary.input_tokens, 300);
}

#[test]
fn test_query_daily_and_date_filters_default_to_local_boundaries() {
    let db = Database::open_in_memory().unwrap();
    db.seed_pricing().unwrap();
    let before_local_midnight = "2026-04-08T06:30:00+00:00";
    let after_local_midnight = "2026-04-08T07:30:00+00:00";
    db.insert_records(&[
        make_record("local-late", before_local_midnight, "opus", "proj-a", 10, 1),
        make_record("local-early", after_local_midnight, "opus", "proj-a", 20, 1),
    ])
    .unwrap();

    let filter = QueryFilter {
        include_subagents: true,
        ..Default::default()
    };
    let rows = query_by_period(db.conn(), TimePeriod::Daily, &filter, 30).unwrap();
    let expected_days = [
        sqlite_local_day(db.conn(), before_local_midnight),
        sqlite_local_day(db.conn(), after_local_midnight),
    ];
    let mut expected_periods = vec![expected_days[0].as_str()];
    if expected_days[1] != expected_days[0] {
        expected_periods.push(expected_days[1].as_str());
    }
    assert_eq!(
        rows.iter()
            .map(|row| row.period.as_str())
            .collect::<Vec<_>>(),
        expected_periods
    );

    let filter = QueryFilter {
        begin: NaiveDate::parse_from_str(&expected_days[0], "%Y-%m-%d").ok(),
        end: NaiveDate::parse_from_str(&expected_days[0], "%Y-%m-%d").ok(),
        include_subagents: true,
        ..Default::default()
    };
    let summary = query_summary(db.conn(), &filter).unwrap();
    if expected_days[0] == expected_days[1] {
        assert_eq!(summary.request_count, 2);
        assert_eq!(summary.input_tokens, 30);
    } else {
        assert_eq!(summary.request_count, 1);
        assert_eq!(summary.input_tokens, 10);
    }
}

#[test]
fn test_query_daily_and_date_filters_can_use_utc_boundaries() {
    let db = Database::open_in_memory().unwrap();
    db.seed_pricing().unwrap();
    db.insert_records(&[
        make_record("late", "2026-04-07T23:30:00Z", "opus", "proj-a", 10, 1),
        make_record("early", "2026-04-08T00:30:00Z", "opus", "proj-a", 20, 1),
    ])
    .unwrap();

    let filter = QueryFilter {
        report_timezone: ReportTimeZone::Utc,
        include_subagents: true,
        ..Default::default()
    };
    let rows = query_by_period(db.conn(), TimePeriod::Daily, &filter, 30).unwrap();
    assert_eq!(
        rows.iter()
            .map(|row| row.period.as_str())
            .collect::<Vec<_>>(),
        vec!["2026-04-07", "2026-04-08"]
    );

    let filter = QueryFilter {
        begin: NaiveDate::from_ymd_opt(2026, 4, 8),
        end: NaiveDate::from_ymd_opt(2026, 4, 8),
        report_timezone: ReportTimeZone::Utc,
        include_subagents: true,
        ..Default::default()
    };
    let summary = query_summary(db.conn(), &filter).unwrap();
    assert_eq!(summary.request_count, 1);
    assert_eq!(summary.input_tokens, 20);
}

#[test]
fn test_hourly_grouping_defaults_to_local_time() {
    let db = Database::open_in_memory().unwrap();
    db.seed_pricing().unwrap();
    let first = "2026-04-08T07:30:00+00:00";
    let second = "2026-04-08T08:30:00+00:00";
    db.insert_records(&[
        make_record("local-hour-1", first, "opus", "proj-a", 10, 1),
        make_record("local-hour-2", second, "opus", "proj-a", 20, 1),
    ])
    .unwrap();

    let local_day = sqlite_local_day(db.conn(), first);
    let filter = QueryFilter {
        begin: NaiveDate::parse_from_str(&local_day, "%Y-%m-%d").ok(),
        end: NaiveDate::parse_from_str(&local_day, "%Y-%m-%d").ok(),
        include_subagents: true,
        ..Default::default()
    };
    let rows = query_by_period(db.conn(), TimePeriod::Hourly, &filter, 30).unwrap();
    assert_eq!(
        rows.iter()
            .map(|row| row.period.as_str())
            .collect::<Vec<_>>(),
        vec![
            sqlite_local_hour(db.conn(), first),
            sqlite_local_hour(db.conn(), second)
        ]
    );
}

#[test]
fn test_hourly_grouping_can_use_utc_across_dst_spring_boundary() {
    let db = Database::open_in_memory().unwrap();
    db.seed_pricing().unwrap();
    db.insert_records(&[
        make_record("spring-1", "2026-03-08T09:30:00Z", "opus", "proj-a", 10, 1),
        make_record("spring-2", "2026-03-08T10:30:00Z", "opus", "proj-a", 20, 1),
    ])
    .unwrap();

    let filter = QueryFilter {
        begin: NaiveDate::from_ymd_opt(2026, 3, 8),
        end: NaiveDate::from_ymd_opt(2026, 3, 8),
        report_timezone: ReportTimeZone::Utc,
        include_subagents: true,
        ..Default::default()
    };
    let rows = query_by_period(db.conn(), TimePeriod::Hourly, &filter, 30).unwrap();
    assert_eq!(
        rows.iter()
            .map(|row| row.period.as_str())
            .collect::<Vec<_>>(),
        vec!["2026-03-08 09:00", "2026-03-08 10:00"]
    );
}

#[test]
fn test_hourly_grouping_can_use_utc_across_dst_fall_boundary() {
    let db = Database::open_in_memory().unwrap();
    db.seed_pricing().unwrap();
    db.insert_records(&[
        make_record("fall-1", "2026-11-01T08:30:00Z", "opus", "proj-a", 10, 1),
        make_record("fall-2", "2026-11-01T09:30:00Z", "opus", "proj-a", 20, 1),
    ])
    .unwrap();

    let filter = QueryFilter {
        begin: NaiveDate::from_ymd_opt(2026, 11, 1),
        end: NaiveDate::from_ymd_opt(2026, 11, 1),
        report_timezone: ReportTimeZone::Utc,
        include_subagents: true,
        ..Default::default()
    };
    let rows = query_by_period(db.conn(), TimePeriod::Hourly, &filter, 30).unwrap();
    assert_eq!(
        rows.iter()
            .map(|row| row.period.as_str())
            .collect::<Vec<_>>(),
        vec!["2026-11-01 08:00", "2026-11-01 09:00"]
    );
}

#[test]
fn test_local_hourly_gap_fill_skips_spring_forward_nonexistent_hour() {
    let db = Database::open_in_memory().unwrap();
    db.seed_pricing().unwrap();
    let first = "2026-03-08T09:30:00Z";
    let second = "2026-03-08T10:30:00Z";
    db.insert_records(&[
        make_record("spring-local-1", first, "opus", "proj-a", 10, 1),
        make_record("spring-local-2", second, "opus", "proj-a", 20, 1),
    ])
    .unwrap();

    let local_day = sqlite_local_day(db.conn(), first);
    let filter = QueryFilter {
        begin: NaiveDate::parse_from_str(&local_day, "%Y-%m-%d").ok(),
        end: NaiveDate::parse_from_str(&local_day, "%Y-%m-%d").ok(),
        include_subagents: true,
        ..Default::default()
    };
    let rows = query_by_period(db.conn(), TimePeriod::Hourly, &filter, 30).unwrap();
    let expected = vec![
        sqlite_local_hour(db.conn(), first),
        sqlite_local_hour(db.conn(), second),
    ];
    assert_eq!(
        rows.iter()
            .map(|row| row.period.as_str())
            .collect::<Vec<_>>(),
        expected
    );
    assert!(
        rows.iter().all(|row| !row.period.ends_with("02:00")),
        "local gap filling should not synthesize the nonexistent DST spring-forward hour"
    );
}

#[test]
fn test_local_hourly_gap_fill_combines_fall_back_repeated_hour() {
    let db = Database::open_in_memory().unwrap();
    db.seed_pricing().unwrap();
    let first = "2026-11-01T08:30:00Z";
    let second = "2026-11-01T09:30:00Z";
    db.insert_records(&[
        make_record("fall-local-1", first, "opus", "proj-a", 10, 1),
        make_record("fall-local-2", second, "opus", "proj-a", 20, 1),
    ])
    .unwrap();

    let local_day = sqlite_local_day(db.conn(), first);
    let filter = QueryFilter {
        begin: NaiveDate::parse_from_str(&local_day, "%Y-%m-%d").ok(),
        end: NaiveDate::parse_from_str(&local_day, "%Y-%m-%d").ok(),
        include_subagents: true,
        ..Default::default()
    };
    let rows = query_by_period(db.conn(), TimePeriod::Hourly, &filter, 30).unwrap();
    let first_label = sqlite_local_hour(db.conn(), first);
    let second_label = sqlite_local_hour(db.conn(), second);
    assert_eq!(first_label, second_label);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].period, first_label);
    assert_eq!(rows[0].request_count, 2);
    assert_eq!(rows[0].input_tokens, 30);
}

#[test]
fn test_report_timezone_does_not_change_cost_totals() {
    let db = Database::open_in_memory().unwrap();
    db.seed_pricing().unwrap();
    db.insert_records(&[
        make_record(
            "cost-local-1",
            "2026-04-08T06:30:00Z",
            "opus",
            "proj-a",
            10,
            1,
        ),
        make_record(
            "cost-local-2",
            "2026-04-08T07:30:00Z",
            "opus",
            "proj-a",
            20,
            2,
        ),
    ])
    .unwrap();

    let local = QueryFilter {
        report_timezone: ReportTimeZone::Local,
        include_subagents: true,
        ..Default::default()
    };
    let utc = QueryFilter {
        report_timezone: ReportTimeZone::Utc,
        include_subagents: true,
        ..Default::default()
    };
    let local_total = query_summary(db.conn(), &local).unwrap().cost_usd;
    let utc_total = query_summary(db.conn(), &utc).unwrap().cost_usd;
    assert!((local_total - utc_total).abs() < f64::EPSILON);
}

#[test]
fn test_query_empty_db() {
    let db = Database::open_in_memory().unwrap();
    let filter = QueryFilter {
        include_subagents: true,
        ..Default::default()
    };
    let rows = query_by_period(db.conn(), TimePeriod::Daily, &filter, 30).unwrap();
    assert!(rows.is_empty());
    let summary = query_summary(db.conn(), &filter).unwrap();
    assert_eq!(summary.request_count, 0);
}

#[test]
fn test_gap_fill_daily_labels() {
    let labels = generate_time_labels(
        "2026-04-01",
        "2026-04-05",
        "%Y-%m-%d",
        TimeDelta::days(1),
        "%Y-%m-%d",
    )
    .unwrap();
    assert_eq!(labels.len(), 5);
    assert_eq!(labels[0], "2026-04-01");
    assert_eq!(labels[4], "2026-04-05");
}

#[test]
fn test_gap_fill_hourly_labels() {
    let labels = generate_time_labels(
        "2026-04-01 10:00",
        "2026-04-01 14:00",
        "%Y-%m-%d %H:%M",
        TimeDelta::hours(1),
        "%Y-%m-%d %H:00",
    )
    .unwrap();
    assert_eq!(labels.len(), 5);
}

#[test]
fn test_gap_fill_5min_labels() {
    let labels = generate_time_labels(
        "2026-04-01 10:00",
        "2026-04-01 10:20",
        "%Y-%m-%d %H:%M",
        TimeDelta::minutes(5),
        "%Y-%m-%d %H:%M",
    )
    .unwrap();
    assert_eq!(labels.len(), 5);
    assert_eq!(labels[1], "2026-04-01 10:05");
}

#[test]
fn test_gap_fill_monthly_labels() {
    let labels = generate_monthly_labels("2026-01", "2026-04").unwrap();
    assert_eq!(labels.len(), 4);
}

#[test]
fn test_gap_fill_inserts_zero_rows() {
    let rows = vec![
        AggregatedRow {
            period: "2026-04-01".into(),
            request_count: 5,
            total_tokens: 100,
            ..Default::default()
        },
        AggregatedRow {
            period: "2026-04-03".into(),
            request_count: 3,
            total_tokens: 200,
            ..Default::default()
        },
    ];
    let filled = fill_naive_gaps(TimePeriod::Daily, rows);
    assert_eq!(filled.len(), 3);
    assert_eq!(filled[1].period, "2026-04-02");
    assert_eq!(filled[1].request_count, 0);
}

#[test]
fn test_limit_applied_after_gap_fill() {
    let db = Database::open_in_memory().unwrap();
    seed_db(&db);
    let filter = QueryFilter {
        include_subagents: true,
        ..Default::default()
    };
    let rows = query_by_period(db.conn(), TimePeriod::Daily, &filter, 2).unwrap();
    assert_eq!(rows.len(), 2);
}

#[test]
fn test_query_daily_totals() {
    let db = Database::open_in_memory().unwrap();
    seed_db(&db);
    let filter = QueryFilter {
        include_subagents: true,
        ..Default::default()
    };
    let totals = query_daily_totals(db.conn(), &filter).unwrap();
    assert!(totals.len() >= 2);
    assert!(totals.first().unwrap().date <= totals.last().unwrap().date);
}

#[test]
fn test_query_cost_uses_usage_timestamp_across_price_change() {
    let db = Database::open_in_memory().unwrap();
    db.insert_pricing_interval(&price(
        "claude-opus-4-6",
        TokenCategory::Input,
        10.0,
        "2026-01-01T00:00:00Z",
        Some("2026-04-06T00:00:00Z"),
    ))
    .unwrap();
    db.insert_pricing_interval(&price(
        "claude-opus-4-6",
        TokenCategory::Input,
        20.0,
        "2026-04-06T00:00:00Z",
        None,
    ))
    .unwrap();

    let mut before = make_record(
        "before",
        "2026-04-05T10:00:00Z",
        "opus",
        "proj-a",
        1_000_000,
        0,
    );
    before.output_tokens = 0;
    let mut after = make_record(
        "after",
        "2026-04-07T10:00:00Z",
        "opus",
        "proj-a",
        1_000_000,
        0,
    );
    after.output_tokens = 0;
    db.insert_records(&[before, after]).unwrap();

    let summary = query_summary(
        db.conn(),
        &QueryFilter {
            include_subagents: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert!((summary.cost_usd - 30.0).abs() < 0.001);
}

#[test]
fn test_query_cost_joins_component_pricing_dimensions() {
    let db = Database::open_in_memory().unwrap();
    db.insert_pricing_interval(&price(
        "claude-opus-4-6",
        TokenCategory::Input,
        10.0,
        "2026-01-01T00:00:00Z",
        None,
    ))
    .unwrap();
    db.insert_pricing_interval(&with_speed(
        price(
            "claude-opus-4-6",
            TokenCategory::Input,
            20.0,
            "2026-01-01T00:00:00Z",
            None,
        ),
        "turbo",
    ))
    .unwrap();
    let mut record = make_record("r1", "2026-04-05T10:00:00Z", "opus", "proj-a", 1_000_000, 0);
    record.output_tokens = 0;
    record.speed = Some("turbo".into());
    db.insert_records(&[record]).unwrap();

    let summary = query_summary(
        db.conn(),
        &QueryFilter {
            include_subagents: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert!((summary.cost_usd - 20.0).abs() < 0.001);
}

#[test]
fn test_explain_cost_high_confidence_when_modifiers_and_reviewed_source_are_explicit() {
    let db = Database::open_in_memory().unwrap();
    let mut interval = price(
        "claude-opus-4-6",
        TokenCategory::Input,
        10.0,
        "2026-01-01T00:00:00Z",
        None,
    );
    interval.dimensions.service_tier = Some("priority".into());
    interval.dimensions.speed = Some("turbo".into());
    interval.dimensions.region = Some("us".into());
    interval.source = "reviewed:explicit-source".into();
    db.insert_pricing_interval(&interval).unwrap();
    upsert_source_metadata(db.conn(), &reviewed_source("reviewed:explicit-source")).unwrap();
    let mut record = make_record(
        "explicit",
        "2026-04-05T10:00:00Z",
        "opus",
        "proj-a",
        1_000_000,
        0,
    );
    record.output_tokens = 0;
    record.service_tier = Some("priority".into());
    record.speed = Some("turbo".into());
    record.region = Some("us".into());
    db.insert_records(&[record]).unwrap();

    let explanation = explain_cost(
        db.conn(),
        &QueryFilter {
            include_subagents: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(explanation.confidence, CostConfidence::High);
    assert_eq!(explanation.component_count, 1);
    assert_eq!(explanation.cost_usd, 10.0);
    assert!(explanation.assumptions.is_empty());
}

#[test]
fn test_explain_cost_reports_assumed_standard_processing_mode() {
    let db = Database::open_in_memory().unwrap();
    let mut interval = provider_price(
        "codex",
        "gpt-explain",
        TokenCategory::Input,
        10.0,
        "2026-01-01T00:00:00Z",
        None,
    );
    interval.dimensions.processing_mode = Some("standard".into());
    interval.source = "reviewed:codex-source".into();
    db.insert_pricing_interval(&interval).unwrap();
    upsert_source_metadata(db.conn(), &reviewed_source("reviewed:codex-source")).unwrap();
    let mut record = make_record(
        "codex-standard",
        "2026-04-05T10:00:00Z",
        "unknown",
        "proj-a",
        1_000_000,
        0,
    );
    record.provider = crate::domain::provider::ProviderId::Codex;
    record.model = ModelFamily::Unknown;
    record.model_id = "gpt-explain".into();
    record.output_tokens = 0;
    record.processing_mode = Some("standard".into());
    db.insert_records(&[record]).unwrap();

    let explanation = explain_cost(
        db.conn(),
        &QueryFilter {
            provider: Some(crate::domain::provider::ProviderId::Codex),
            include_subagents: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(explanation.confidence, CostConfidence::Estimated);
    assert!(explanation.assumptions.iter().any(|assumption| {
        assumption.kind == CostAssumptionKind::AssumedDefaultModifier
            && assumption.dimension.as_deref() == Some("processing_mode")
            && assumption.value.as_deref() == Some("standard")
    }));
}

#[test]
fn test_explain_cost_reports_bundled_pricing_source() {
    let db = Database::open_in_memory().unwrap();
    db.seed_pricing().unwrap();
    let mut record = make_record(
        "bundled",
        "2026-04-05T10:00:00Z",
        "opus",
        "proj-a",
        1_000_000,
        0,
    );
    record.output_tokens = 0;
    db.insert_records(&[record]).unwrap();

    let explanation = explain_cost(
        db.conn(),
        &QueryFilter {
            include_subagents: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(explanation.confidence, CostConfidence::Estimated);
    assert!(explanation.assumptions.iter().any(|assumption| {
        assumption.kind == CostAssumptionKind::BundledPricingSource
            && assumption
                .source
                .as_deref()
                .is_some_and(|source| source.starts_with("seed:"))
    }));
}

#[test]
fn test_explain_cost_stale_source_boundary_uses_reference_date() {
    assert!(
        !stale_cost_assumption_present_for_retrieved_at("2026-02-23"),
        "retrieval date exactly at the 90-day cutoff should not be stale"
    );
    assert!(
        stale_cost_assumption_present_for_retrieved_at("2026-02-22"),
        "retrieval date just before the 90-day cutoff should be stale"
    );
    assert!(
        !stale_cost_assumption_present_for_retrieved_at("2026-02-24"),
        "retrieval date just after the 90-day cutoff should not be stale"
    );
}

#[test]
fn test_query_fails_when_pricing_interval_missing() {
    let db = Database::open_in_memory().unwrap();
    let mut record = make_record("r1", "2026-04-05T10:00:00Z", "opus", "proj-a", 1_000_000, 0);
    record.output_tokens = 0;
    db.insert_records(&[record]).unwrap();

    let err = query_summary(
        db.conn(),
        &QueryFilter {
            include_subagents: true,
            ..Default::default()
        },
    )
    .unwrap_err()
    .to_string();
    assert!(err.contains("missing pricing coverage"));
    assert!(err.contains("claude-opus-4-6"));
    assert!(err.contains("tkstat --pricing-refresh"));
}

#[test]
fn test_cost_query_fails_when_billable_usage_has_no_components() {
    let db = Database::open_in_memory().unwrap();
    seed_db(&db);
    db.conn()
        .execute(
            "DELETE FROM usage_billing_components
                 WHERE provider = 'claude-code' AND request_id = 'r1'",
            [],
        )
        .unwrap();
    let filter = QueryFilter {
        include_subagents: true,
        ..Default::default()
    };

    let err = query_summary(db.conn(), &filter).unwrap_err().to_string();
    assert!(err.contains("billing component integrity error"));
    assert!(err.contains("request_id=r1"));
    assert!(err.contains("expected 100 billable tokens"));
    assert!(err.contains("found 0"));

    let token_only =
        query_by_period_with_cost_requirement(db.conn(), TimePeriod::Daily, &filter, 30, false)
            .unwrap();
    assert!(!token_only.is_empty());
}

#[test]
fn test_cost_query_fails_when_billable_usage_has_partial_components() {
    let db = Database::open_in_memory().unwrap();
    seed_db(&db);
    db.conn()
        .execute(
            "DELETE FROM usage_billing_components
                 WHERE provider = 'claude-code'
                   AND request_id = 'r1'
                   AND token_category = 'output'",
            [],
        )
        .unwrap();

    let err = query_summary(
        db.conn(),
        &QueryFilter {
            include_subagents: true,
            ..Default::default()
        },
    )
    .unwrap_err()
    .to_string();
    assert!(err.contains("billing component integrity error"));
    assert!(err.contains("request_id=r1"));
    assert!(err.contains("category=output"));
    assert!(err.contains("expected 50 billable tokens"));
    assert!(err.contains("found 0"));
}

#[test]
fn test_cost_query_fails_when_billing_component_is_orphaned() {
    let db = Database::open_in_memory().unwrap();
    seed_db(&db);
    db.conn()
        .execute(
            "INSERT INTO usage_billing_components
                 (usage_id, provider, request_id, model_id, timestamp, token_category, tokens,
                  component_ordinal)
                 VALUES (9999, 'claude-code', 'missing-request', 'claude-opus-4-6',
                         '2026-04-05T10:00:00+00:00', 'input', 10, 999)",
            [],
        )
        .unwrap();

    let err = query_summary(
        db.conn(),
        &QueryFilter {
            include_subagents: true,
            ..Default::default()
        },
    )
    .unwrap_err()
    .to_string();
    assert!(err.contains("billing component integrity error"));
    assert!(err.contains("request_id=missing-request"));
    assert!(err.contains("no matching token_usage row"));
}

#[test]
fn test_cost_query_rejects_component_identity_mismatch() {
    let db = Database::open_in_memory().unwrap();
    seed_db(&db);
    db.conn()
        .execute(
            "UPDATE usage_billing_components
             SET request_id = 'wrong-request'
             WHERE request_id = 'r1' AND token_category = 'input'",
            [],
        )
        .unwrap();

    let err = query_summary(
        db.conn(),
        &QueryFilter {
            include_subagents: true,
            ..Default::default()
        },
    )
    .unwrap_err()
    .to_string();
    assert!(err.contains("billing component integrity error"));
    assert!(err.contains("request_id=wrong-request"));
    assert!(err.contains("no matching token_usage row"));
}

#[test]
fn test_query_fails_when_component_pricing_modifier_is_missing() {
    let db = Database::open_in_memory().unwrap();
    db.insert_pricing_interval(&price(
        "claude-opus-4-6",
        TokenCategory::Input,
        10.0,
        "2026-01-01T00:00:00Z",
        None,
    ))
    .unwrap();
    let mut record = make_record("r1", "2026-04-05T10:00:00Z", "opus", "proj-a", 1_000_000, 0);
    record.output_tokens = 0;
    record.speed = Some("turbo".into());
    db.insert_records(&[record]).unwrap();

    let err = query_summary(
        db.conn(),
        &QueryFilter {
            include_subagents: true,
            ..Default::default()
        },
    )
    .unwrap_err()
    .to_string();
    assert!(err.contains("missing pricing coverage"));
    assert!(err.contains("speed=turbo"));
}

#[test]
fn test_query_uses_default_pricing_for_claude_standard_modifiers() {
    let db = Database::open_in_memory().unwrap();
    db.seed_pricing().unwrap();
    let mut record = make_record(
        "haiku-standard",
        "2026-04-05T10:00:00Z",
        "haiku",
        "proj-a",
        0,
        0,
    );
    record.model_id = "claude-haiku-4-5-20251001".into();
    record.cache_read_tokens = 1_000_000;
    record.service_tier = Some("standard".into());
    record.speed = Some("fast".into());
    db.insert_records(&[record]).unwrap();

    let summary = query_summary(
        db.conn(),
        &QueryFilter {
            include_subagents: true,
            ..Default::default()
        },
    )
    .unwrap();

    assert!((summary.cost_usd - 0.1).abs() < 0.000001);
}

#[test]
fn test_query_uses_default_pricing_for_claude_placeholder_regions() {
    let db = Database::open_in_memory().unwrap();
    db.seed_pricing().unwrap();
    let mut unavailable = make_record(
        "haiku-region-not-available",
        "2026-04-05T10:00:00Z",
        "haiku",
        "proj-a",
        0,
        0,
    );
    unavailable.model_id = "claude-haiku-4-5-20251001".into();
    unavailable.cache_creation_tokens = 1_000_000;
    unavailable.cache_creation_5m_tokens = 1_000_000;
    unavailable.region = Some("not_available".into());

    let mut global = make_record(
        "opus-region-global",
        "2026-04-05T10:00:00Z",
        "opus",
        "proj-a",
        0,
        0,
    );
    global.cache_read_tokens = 1_000_000;
    global.region = Some("global".into());

    db.insert_records(&[unavailable, global]).unwrap();

    let summary = query_summary(
        db.conn(),
        &QueryFilter {
            include_subagents: true,
            ..Default::default()
        },
    )
    .unwrap();

    assert!((summary.cost_usd - 1.75).abs() < 0.000001);
}

#[test]
fn test_query_preserves_fail_closed_for_real_claude_region_modifier() {
    let db = Database::open_in_memory().unwrap();
    db.seed_pricing().unwrap();
    let mut record = make_record(
        "haiku-region-us",
        "2026-04-05T10:00:00Z",
        "haiku",
        "proj-a",
        0,
        0,
    );
    record.model_id = "claude-haiku-4-5-20251001".into();
    record.cache_read_tokens = 1_000_000;
    record.region = Some("us".into());
    db.insert_records(&[record]).unwrap();

    let err = query_summary(
        db.conn(),
        &QueryFilter {
            include_subagents: true,
            ..Default::default()
        },
    )
    .unwrap_err()
    .to_string();
    assert!(err.contains("missing pricing coverage"));
    assert!(err.contains("region=us"));

    db.insert_pricing_interval(&with_region(
        price(
            "claude-haiku-4-5-20251001",
            TokenCategory::CacheRead,
            0.11,
            "2026-01-01T00:00:00Z",
            None,
        ),
        "us",
    ))
    .unwrap();

    let summary = query_summary(
        db.conn(),
        &QueryFilter {
            include_subagents: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert!((summary.cost_usd - 0.11).abs() < 0.000001);
}

#[test]
fn test_query_preserves_fail_closed_for_unsupported_claude_speed_modifier() {
    let db = Database::open_in_memory().unwrap();
    db.seed_pricing().unwrap();
    let mut record = make_record(
        "haiku-turbo",
        "2026-04-05T10:00:00Z",
        "haiku",
        "proj-a",
        0,
        0,
    );
    record.model_id = "claude-haiku-4-5-20251001".into();
    record.cache_read_tokens = 1_000_000;
    record.service_tier = Some("standard".into());
    record.speed = Some("turbo".into());
    db.insert_records(&[record]).unwrap();

    let err = query_summary(
        db.conn(),
        &QueryFilter {
            include_subagents: true,
            ..Default::default()
        },
    )
    .unwrap_err()
    .to_string();
    assert!(err.contains("missing pricing coverage"));
    assert!(err.contains("speed=turbo"));

    db.insert_pricing_interval(&with_speed(
        price(
            "claude-haiku-4-5-20251001",
            TokenCategory::CacheRead,
            0.2,
            "2026-01-01T00:00:00Z",
            None,
        ),
        "turbo",
    ))
    .unwrap();

    let summary = query_summary(
        db.conn(),
        &QueryFilter {
            include_subagents: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert!((summary.cost_usd - 0.2).abs() < 0.000001);
}

#[test]
fn test_query_fails_when_pricing_intervals_overlap() {
    let db = Database::open_in_memory().unwrap();
    db.insert_pricing_interval(&price(
        "claude-opus-4-6",
        TokenCategory::Input,
        10.0,
        "2026-01-01T00:00:00Z",
        None,
    ))
    .unwrap();
    db.insert_pricing_interval(&price(
        "claude-opus-4-6",
        TokenCategory::Input,
        20.0,
        "2026-02-01T00:00:00Z",
        None,
    ))
    .unwrap();
    let mut record = make_record("r1", "2026-04-05T10:00:00Z", "opus", "proj-a", 1_000_000, 0);
    record.output_tokens = 0;
    db.insert_records(&[record]).unwrap();

    let err = query_summary(
        db.conn(),
        &QueryFilter {
            include_subagents: true,
            ..Default::default()
        },
    )
    .unwrap_err()
    .to_string();
    assert!(err.contains("overlapping pricing intervals"));
}

#[test]
fn test_query_coverage_ignores_unobserved_price_gaps() {
    let db = Database::open_in_memory().unwrap();
    db.insert_pricing_interval(&price(
        "claude-opus-4-6",
        TokenCategory::Input,
        10.0,
        "2026-01-01T00:00:00Z",
        Some("2026-02-01T00:00:00Z"),
    ))
    .unwrap();
    db.insert_pricing_interval(&price(
        "claude-opus-4-6",
        TokenCategory::Input,
        10.0,
        "2026-03-01T00:00:00Z",
        None,
    ))
    .unwrap();
    let mut before = make_record(
        "before-gap",
        "2026-01-15T10:00:00Z",
        "opus",
        "proj-a",
        1_000_000,
        0,
    );
    before.output_tokens = 0;
    let mut after = make_record(
        "after-gap",
        "2026-03-15T10:00:00Z",
        "opus",
        "proj-a",
        1_000_000,
        0,
    );
    after.output_tokens = 0;
    db.insert_records(&[before, after]).unwrap();

    let summary = query_summary(
        db.conn(),
        &QueryFilter {
            include_subagents: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(summary.cost_usd, 20.0);
}

#[test]
fn test_query_coverage_fails_for_observed_price_gap() {
    let db = Database::open_in_memory().unwrap();
    db.insert_pricing_interval(&price(
        "claude-opus-4-6",
        TokenCategory::Input,
        10.0,
        "2026-01-01T00:00:00Z",
        Some("2026-02-01T00:00:00Z"),
    ))
    .unwrap();
    db.insert_pricing_interval(&price(
        "claude-opus-4-6",
        TokenCategory::Input,
        10.0,
        "2026-03-01T00:00:00Z",
        None,
    ))
    .unwrap();
    let mut record = make_record(
        "in-gap",
        "2026-02-15T10:00:00Z",
        "opus",
        "proj-a",
        1_000_000,
        0,
    );
    record.output_tokens = 0;
    db.insert_records(&[record]).unwrap();

    let err = query_summary(
        db.conn(),
        &QueryFilter {
            include_subagents: true,
            ..Default::default()
        },
    )
    .unwrap_err()
    .to_string();
    assert!(err.contains("missing pricing coverage"));
    assert!(err.contains("usage timestamp 2026-02-15T10:00:00+00:00"));
}

#[test]
fn test_query_coverage_detects_dimension_specific_overlap() {
    let db = Database::open_in_memory().unwrap();
    db.insert_pricing_interval(&price(
        "claude-opus-4-6",
        TokenCategory::Input,
        10.0,
        "2026-01-01T00:00:00Z",
        None,
    ))
    .unwrap();
    db.insert_pricing_interval(&with_speed(
        price(
            "claude-opus-4-6",
            TokenCategory::Input,
            20.0,
            "2026-01-01T00:00:00Z",
            None,
        ),
        "turbo",
    ))
    .unwrap();
    db.insert_pricing_interval(&with_speed(
        price(
            "claude-opus-4-6",
            TokenCategory::Input,
            30.0,
            "2026-02-01T00:00:00Z",
            None,
        ),
        "turbo",
    ))
    .unwrap();
    let mut record = make_record(
        "turbo",
        "2026-04-05T10:00:00Z",
        "opus",
        "proj-a",
        1_000_000,
        0,
    );
    record.output_tokens = 0;
    record.speed = Some("turbo".into());
    db.insert_records(&[record]).unwrap();

    let err = query_summary(
        db.conn(),
        &QueryFilter {
            include_subagents: true,
            ..Default::default()
        },
    )
    .unwrap_err()
    .to_string();
    assert!(err.contains("overlapping pricing intervals"));
    assert!(err.contains("speed=turbo"));
}

#[test]
fn test_query_allows_zero_cost_only_when_price_is_truly_zero() {
    let db = Database::open_in_memory().unwrap();
    db.insert_pricing_interval(&price(
        "claude-opus-4-6",
        TokenCategory::Input,
        0.0,
        "2026-01-01T00:00:00Z",
        None,
    ))
    .unwrap();
    let mut record = make_record("r1", "2026-04-05T10:00:00Z", "opus", "proj-a", 1_000_000, 0);
    record.output_tokens = 0;
    db.insert_records(&[record]).unwrap();

    let summary = query_summary(
        db.conn(),
        &QueryFilter {
            include_subagents: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(summary.input_tokens, 1_000_000);
    assert_eq!(summary.cost_usd, 0.0);
}

#[test]
fn test_query_codex_cost_uses_non_overlapping_billing_and_display_total() {
    let db = Database::open_in_memory().unwrap();
    for (category, rate) in [
        (TokenCategory::Input, 10.0),
        (TokenCategory::CachedInput, 1.0),
        (TokenCategory::Output, 100.0),
    ] {
        db.insert_pricing_interval(&provider_price(
            "codex",
            "gpt-audit",
            category,
            rate,
            "2026-01-01T00:00:00Z",
            None,
        ))
        .unwrap();
    }
    let mut record = make_record(
        "codex",
        "2026-04-07T10:00:00Z",
        "unknown",
        "proj-a",
        100,
        20,
    );
    record.provider = crate::domain::provider::ProviderId::Codex;
    record.model = ModelFamily::Unknown;
    record.model_id = "gpt-audit".into();
    record.cached_input_tokens = 40;
    record.reasoning_output_tokens = 7;
    record.cost_usd = 0.0;
    db.insert_records(&[record]).unwrap();

    let summary = query_summary(
        db.conn(),
        &QueryFilter {
            provider: Some(crate::domain::provider::ProviderId::Codex),
            include_subagents: true,
            ..Default::default()
        },
    )
    .unwrap();

    let expected = (60.0 * 10.0 + 40.0 * 1.0 + 20.0 * 100.0) / 1_000_000.0;
    assert!((summary.cost_usd - expected).abs() < 0.000001);
    assert_eq!(summary.input_tokens, 100);
    assert_eq!(summary.cached_input_tokens, 40);
    assert_eq!(summary.output_tokens, 20);
    assert_eq!(summary.reasoning_output_tokens, 7);
    assert_eq!(summary.total_tokens, 120);
}

#[test]
fn test_sql_cost_matches_record_cost_billing_policy_for_mixed_providers() {
    let db = Database::open_in_memory().unwrap();
    for (category, rate) in [
        (TokenCategory::Input, 10.0),
        (TokenCategory::Output, 100.0),
        (TokenCategory::CacheCreation, 12.5),
        (TokenCategory::CacheRead, 1.0),
        (TokenCategory::CachedInput, 0.5),
        (TokenCategory::ReasoningOutput, 50.0),
    ] {
        db.insert_pricing_interval(&provider_price(
            "claude-code",
            "claude-policy",
            category,
            rate,
            "2026-01-01T00:00:00Z",
            None,
        ))
        .unwrap();
    }
    for (category, rate) in [
        (TokenCategory::Input, 10.0),
        (TokenCategory::CachedInput, 1.0),
        (TokenCategory::Output, 100.0),
    ] {
        db.insert_pricing_interval(&provider_price(
            "codex",
            "gpt-policy",
            category,
            rate,
            "2026-01-01T00:00:00Z",
            None,
        ))
        .unwrap();
    }

    let mut claude = make_record(
        "policy-claude",
        "2026-04-07T10:00:00Z",
        "unknown",
        "proj-a",
        100,
        20,
    );
    claude.model_id = "claude-policy".into();
    claude.cache_creation_tokens = 10;
    claude.cache_read_tokens = 5;
    claude.reasoning_output_tokens = 3;

    let mut codex_cached = make_record(
        "policy-codex-cached",
        "2026-04-07T11:00:00Z",
        "unknown",
        "proj-a",
        100,
        20,
    );
    codex_cached.provider = crate::domain::provider::ProviderId::Codex;
    codex_cached.model = ModelFamily::Unknown;
    codex_cached.model_id = "gpt-policy".into();
    codex_cached.cached_input_tokens = 40;
    codex_cached.reasoning_output_tokens = 7;

    let mut codex_overcached = make_record(
        "policy-codex-overcached",
        "2026-04-07T12:00:00Z",
        "unknown",
        "proj-a",
        30,
        10,
    );
    codex_overcached.provider = crate::domain::provider::ProviderId::Codex;
    codex_overcached.model = ModelFamily::Unknown;
    codex_overcached.model_id = "gpt-policy".into();
    codex_overcached.cached_input_tokens = 40;
    codex_overcached.reasoning_output_tokens = 9;

    let mut codex_uncached = make_record(
        "policy-codex-uncached",
        "2026-04-07T13:00:00Z",
        "unknown",
        "proj-a",
        25,
        5,
    );
    codex_uncached.provider = crate::domain::provider::ProviderId::Codex;
    codex_uncached.model = ModelFamily::Unknown;
    codex_uncached.model_id = "gpt-policy".into();

    let records = vec![claude, codex_cached, codex_overcached, codex_uncached];
    db.insert_records(&records).unwrap();

    let expected = records
        .iter()
        .map(|record| calculate_record_cost(db.conn(), record).unwrap())
        .sum::<f64>();
    let filter = QueryFilter {
        include_subagents: true,
        ..Default::default()
    };
    let summary = query_summary(db.conn(), &filter).unwrap();
    let daily = query_by_period(db.conn(), TimePeriod::Daily, &filter, 10).unwrap();

    assert!((summary.cost_usd - expected).abs() < 0.000001);
    assert_eq!(daily.len(), 1);
    assert!((daily[0].cost_usd - expected).abs() < 0.000001);
    assert_eq!(summary.input_tokens, 255);
    assert_eq!(summary.cached_input_tokens, 80);
    assert_eq!(summary.reasoning_output_tokens, 19);
}

#[test]
fn test_seed_pricing_covers_observed_claude_opus_cache_creation_usage() {
    let db = Database::open_in_memory().unwrap();
    db.seed_pricing().unwrap();
    let mut record = make_record(
        "observed-opus",
        "2026-01-31T21:20:19.858Z",
        "opus",
        "proj-a",
        0,
        0,
    );
    record.model_id = "claude-opus-4-5-20251101".into();
    record.cache_creation_tokens = 100;
    db.insert_records(&[record]).unwrap();

    let summary = query_summary(
        db.conn(),
        &QueryFilter {
            model: Some("claude-opus-4-5-20251101".into()),
            include_subagents: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(summary.request_count, 1);
    assert_eq!(summary.cache_creation_tokens, 100);
    assert!(summary.cost_usd > 0.0);
}
