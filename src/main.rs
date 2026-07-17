use std::path::Path;
use std::time::Instant;

use anyhow::Result;
use chrono::{Datelike, Local, NaiveDate, Utc};
use clap::Parser;
use rusqlite::{Connection, OpenFlags};

use tkstat::budget::{BudgetPeriod, BudgetReportRow, BudgetWarning, evaluate_budget_rows};
use tkstat::cli::{ChartMetric, Cli, OutputMode, ProviderArg};
use tkstat::domain::period::{ReportTimeZone, TimePeriod};
use tkstat::domain::usage::AggregatedRow;
use tkstat::render::columns;
use tkstat::{config, db, diagnostics, ingest, render};

fn main() -> Result<()> {
    let mut timings = tkstat::timing::StageTimings::from_env();
    let cli = Cli::parse();

    if cli.no_color {
        // SAFETY: set_var is called before any threads are spawned.
        unsafe { std::env::set_var("NO_COLOR", "1") };
    }

    let db_path = config::resolve_db_path(cli.db_path.as_deref());
    let sources = if cli.doctor {
        diagnostic_sources(&cli)
    } else if cli.pricing_audit {
        ingest::ProviderSources {
            claude_data_dir: None,
            codex_home: None,
        }
    } else {
        ingest::ProviderSources {
            claude_data_dir: config::resolve_data_dir(cli.data_dir.as_deref())?,
            codex_home: config::resolve_codex_home(),
        }
    };
    timings.checkpoint("cli-and-config");

    if cli.doctor {
        let conn = diagnostic_connection(&db_path)?;
        let inventory = diagnostics::gather_inventory(&db_path, &conn, &sources);
        let output = if cli.json {
            serde_json::to_string_pretty(&inventory)?
        } else {
            render::doctor::render_doctor(&inventory)
        };
        println!("{output}");
        if !inventory.blocking_issues().is_empty() {
            std::process::exit(1);
        }
        return Ok(());
    }

    if cli.pricing_audit {
        let (conn, mut findings) = pricing_audit_connection(&db_path)?;
        if findings.is_empty() {
            findings.extend(db::pricing::audit_pricing(&conn)?);
        }
        let has_errors = findings
            .iter()
            .any(|finding| matches!(finding.severity, db::pricing::PricingAuditSeverity::Error));
        let output = if cli.json {
            serde_json::to_string_pretty(&findings)?
        } else {
            render::pricing_audit::render_pricing_audit(&findings)
        };
        println!("{output}");
        if has_errors {
            std::process::exit(1);
        }
        return Ok(());
    }

    let database = db::Database::open(&db_path)?;
    timings.checkpoint("database-open");

    if cli.pricing_seed {
        let inserted = database.seed_pricing()?;
        println!("seeded {inserted} pricing intervals");
        return Ok(());
    }

    if cli.pricing_refresh {
        if std::env::var_os("TKSTAT_PRICING_REFRESH_OFFLINE").is_some() {
            let snapshot = db::pricing::bundled_pricing_snapshot()?;
            let changed = database.refresh_pricing(&snapshot)?;
            println!(
                "refreshed pricing catalog with {changed} interval changes (offline bundled source)"
            );
            return Ok(());
        }
        return refresh_provider_pricing(&database, cli.provider);
    }

    if let Some(path) = &cli.pricing_import {
        let changed = database.import_pricing_catalog(Path::new(path))?;
        println!("imported pricing catalog with {changed} interval changes");
        return Ok(());
    }

    if cli.force_update {
        database.reset()?;
    }

    let start = Instant::now();
    let ingest_report = ingest::sync_with_report(
        &database,
        &sources,
        cli.provider.provider(),
        cli.force_update,
    )?;
    let inserted = ingest_report.inserted_records();
    let ingest_ms = start.elapsed().as_millis();
    timings.checkpoint("source-sync");

    if inserted > 0 {
        eprintln!("ingested {inserted} new records in {ingest_ms}ms");
    }
    for warning in ingestion_warnings(database.conn(), &ingest_report)? {
        eprintln!("{warning}");
    }

    let columns = match &cli.columns {
        Some(spec) => columns::parse_columns(spec).map_err(|e| anyhow::anyhow!(e))?,
        None => columns::default_columns(),
    };
    let table_columns_require_cost = columns_require_cost(&columns);
    let budget_warnings_require_cost =
        cli.daily_budget_usd.is_some() || cli.monthly_budget_usd.is_some();

    let provider_label = cli.provider_label();
    let filter = cli.query_filter();
    let filter_desc = render::filter_description(
        filter.provider.map(|provider| provider.as_str()),
        cli.model.as_deref(),
        cli.model_family.as_deref(),
        cli.project.as_deref(),
        cli.begin.as_ref().map(|d| d.to_string()).as_deref(),
        cli.end.as_ref().map(|d| d.to_string()).as_deref(),
    );

    let output = match cli.output_mode() {
        OutputMode::Heatmap | OutputMode::Chart => {
            let daily = db::query::query_daily_totals_with_cost_requirement(
                database.conn(),
                &filter,
                matches!(cli.chart_metric, ChartMetric::Cost) || budget_warnings_require_cost,
            )?;
            let metric = cli.chart_metric;
            let chart_data: Vec<(String, f64)> = daily
                .into_iter()
                .map(|d| {
                    let val = match metric {
                        ChartMetric::Tokens => d.total_tokens as f64,
                        ChartMetric::Cost => d.cost_usd,
                        ChartMetric::Input => d.input_tokens as f64,
                        ChartMetric::Output => d.output_tokens as f64,
                    };
                    (d.date, val)
                })
                .collect();
            let metric_label = format!("{metric:?}").to_lowercase();
            if cli.heatmap {
                render::heatmap::render_heatmap_with_today(
                    provider_label,
                    &chart_data,
                    &metric_label,
                    report_today(filter.report_timezone),
                )
            } else {
                render::chart::render_chart(provider_label, &chart_data, &metric_label)
            }
        }
        OutputMode::Summary => {
            let summary = db::query::query_summary(database.conn(), &filter)?;
            render::summary::render_summary(provider_label, &summary)
        }
        OutputMode::Oneline => {
            let summary = db::query::query_summary(database.conn(), &filter)?;
            render::oneline::render_oneline(provider_label, &summary)
        }
        OutputMode::Budget => {
            let rows = budget_report_rows(database.conn(), &filter, &cli)?;
            if cli.json {
                serde_json::to_string_pretty(&rows)?
            } else {
                render::budget::render_budget_report(provider_label, &rows)
            }
        }
        OutputMode::CostExplain => {
            let explanation = db::query::explain_cost(database.conn(), &filter)?;
            if cli.json {
                serde_json::to_string_pretty(&explanation)?
            } else {
                render::cost_explain::render_cost_explain(
                    provider_label,
                    &explanation,
                    filter_desc.as_deref(),
                )
            }
        }
        OutputMode::Json(period) => {
            let rows = db::query::query_by_period(
                database.conn(),
                period,
                &filter,
                cli.effective_limit(),
            )?;
            render::json::render_json(&with_provider_label(rows, provider_label))
        }
        mode @ (OutputMode::ByModel | OutputMode::ByProvider | OutputMode::ByProject) => {
            let cost_required =
                cli.json || table_columns_require_cost || budget_warnings_require_cost;
            let (rows, title) = match mode {
                OutputMode::ByModel => (
                    db::query::query_by_model_with_cost_requirement(
                        database.conn(),
                        &filter,
                        cli.effective_limit(),
                        cost_required,
                    )?,
                    "by model",
                ),
                OutputMode::ByProvider => (
                    db::query::query_by_provider_with_cost_requirement(
                        database.conn(),
                        &filter,
                        cli.effective_limit(),
                        cost_required,
                    )?,
                    "by provider",
                ),
                OutputMode::ByProject => (
                    db::query::query_by_project_with_cost_requirement(
                        database.conn(),
                        &filter,
                        cli.effective_limit(),
                        cost_required,
                    )?,
                    "by project",
                ),
                _ => unreachable!(),
            };
            render_tabular_rows(
                &rows,
                TabularOutput {
                    provider_label,
                    title,
                    columns: &columns,
                    filter_description: filter_desc.as_deref(),
                    csv: cli.csv,
                    json: cli.json,
                },
            )
        }
        OutputMode::TopDays => {
            let rows = with_provider_label(
                db::query::query_top_with_cost_requirement(
                    database.conn(),
                    &filter,
                    cli.effective_limit(),
                    cli.json || table_columns_require_cost || budget_warnings_require_cost,
                )?,
                provider_label,
            );
            render_tabular_rows(
                &rows,
                TabularOutput {
                    provider_label,
                    title: "top days",
                    columns: &columns,
                    filter_description: filter_desc.as_deref(),
                    csv: cli.csv,
                    json: cli.json,
                },
            )
        }
        OutputMode::Table(period) => {
            let rows = with_provider_label(
                db::query::query_by_period_with_cost_requirement(
                    database.conn(),
                    period,
                    &filter,
                    cli.effective_limit(),
                    table_columns_require_cost || budget_warnings_require_cost,
                )?,
                provider_label,
            );
            let title = period.to_string();
            render_tabular_rows(
                &rows,
                TabularOutput {
                    provider_label,
                    title: &title,
                    columns: &columns,
                    filter_description: filter_desc.as_deref(),
                    csv: cli.csv,
                    json: false,
                },
            )
        }
    };
    timings.checkpoint("query-and-render");

    for warning in budget_warnings(database.conn(), &filter, &cli)? {
        eprintln!("{}", warning.message(filter_desc.as_deref()));
    }

    print!("{output}");
    timings.checkpoint("warnings-and-output");
    Ok(())
}

#[cfg(feature = "network-pricing")]
fn refresh_provider_pricing(database: &db::Database, selection: ProviderArg) -> Result<()> {
    let mut total_changed = 0usize;
    let mut failures = Vec::new();
    for &provider in selection.providers() {
        let snapshot = match db::pricing_fetch::LivePricing::fetch(provider)
            .and_then(|snapshot| snapshot.cover_unpriced_observed_usage(database.conn()))
        {
            Ok(snapshot) => snapshot,
            Err(err) => {
                eprintln!(
                    "tkstat pricing refresh warning: {provider} refresh failed; retained last-known-good prices: {err:#}"
                );
                failures.push(provider.to_string());
                continue;
            }
        };
        let changed = match database.refresh_pricing(&snapshot) {
            Ok(changed) => changed,
            Err(err) => {
                eprintln!(
                    "tkstat pricing refresh warning: {provider} refresh failed validation; retained last-known-good prices: {err:#}"
                );
                failures.push(provider.to_string());
                continue;
            }
        };
        total_changed += changed;
        println!("refreshed {provider} pricing with {changed} interval changes");
        let audit_errors = db::pricing::audit_pricing(database.conn())?
            .into_iter()
            .filter(|finding| {
                finding.provider == provider.as_str()
                    && matches!(finding.severity, db::pricing::PricingAuditSeverity::Error)
            })
            .count();
        if audit_errors > 0 {
            eprintln!(
                "tkstat pricing refresh warning: {provider} source was updated, but {audit_errors} pricing audit error(s) remain; run `tkstat --pricing-audit` for details"
            );
            failures.push(provider.to_string());
        }
    }
    if !failures.is_empty() {
        anyhow::bail!("pricing refresh failed for {}", failures.join(", "));
    }
    println!("refreshed pricing catalog with {total_changed} interval changes");
    Ok(())
}

#[cfg(not(feature = "network-pricing"))]
fn refresh_provider_pricing(_database: &db::Database, _selection: ProviderArg) -> Result<()> {
    anyhow::bail!(
        "live pricing refresh is unavailable in this build; reinstall tkstat with the `network-pricing` feature or use `--pricing-import`"
    )
}

fn with_provider_label(mut rows: Vec<AggregatedRow>, provider_label: &str) -> Vec<AggregatedRow> {
    for row in &mut rows {
        if row.provider.is_none() {
            row.provider = Some(provider_label.to_string());
        }
    }
    rows
}

struct TabularOutput<'a> {
    provider_label: &'a str,
    title: &'a str,
    columns: &'a [columns::Column],
    filter_description: Option<&'a str>,
    csv: bool,
    json: bool,
}

fn render_tabular_rows(rows: &[AggregatedRow], output: TabularOutput<'_>) -> String {
    if output.csv {
        render::csv::render_csv(rows, output.columns)
    } else if output.json {
        render::json::render_json(rows)
    } else {
        render::table::render_table(
            output.provider_label,
            output.title,
            rows,
            output.columns,
            output.filter_description,
        )
    }
}

fn columns_require_cost(columns: &[columns::Column]) -> bool {
    columns
        .iter()
        .any(|column| matches!(column, columns::Column::Cost))
}

fn report_today(timezone: ReportTimeZone) -> NaiveDate {
    match timezone {
        ReportTimeZone::Local => Local::now().date_naive(),
        ReportTimeZone::Utc => Utc::now().date_naive(),
    }
}

fn diagnostic_sources(cli: &Cli) -> ingest::ProviderSources {
    let claude_data_dir = cli
        .data_dir
        .as_deref()
        .map(std::path::PathBuf::from)
        .or_else(|| config::resolve_data_dir(None).ok().flatten());
    ingest::ProviderSources {
        claude_data_dir,
        codex_home: config::resolve_codex_home(),
    }
}

fn diagnostic_connection(db_path: &std::path::Path) -> Result<Connection> {
    if db_path.exists() {
        Ok(Connection::open_with_flags(
            db_path,
            OpenFlags::SQLITE_OPEN_READ_ONLY,
        )?)
    } else {
        Ok(Connection::open_in_memory()?)
    }
}

fn pricing_audit_connection(
    db_path: &std::path::Path,
) -> Result<(Connection, Vec<db::pricing::PricingAuditFinding>)> {
    if db_path.exists() {
        Ok((
            Connection::open_with_flags(db_path, OpenFlags::SQLITE_OPEN_READ_ONLY)?,
            Vec::new(),
        ))
    } else {
        Ok((
            Connection::open_in_memory()?,
            vec![db::pricing::missing_database_finding(db_path)],
        ))
    }
}

fn budget_warnings(
    conn: &Connection,
    filter: &db::query::QueryFilter,
    cli: &Cli,
) -> Result<Vec<BudgetWarning>> {
    let mut warnings = Vec::new();
    if let Some(threshold) = cli.daily_budget_usd {
        let rows = db::query::query_by_period(conn, TimePeriod::Daily, filter, u32::MAX)?;
        warnings.extend(evaluate_budget_rows(
            BudgetPeriod::Daily,
            Some(threshold),
            &rows,
        ));
    }
    if let Some(threshold) = cli.monthly_budget_usd {
        let rows = db::query::query_by_period(conn, TimePeriod::Monthly, filter, u32::MAX)?;
        warnings.extend(evaluate_budget_rows(
            BudgetPeriod::Monthly,
            Some(threshold),
            &rows,
        ));
    }
    Ok(warnings)
}

fn ingestion_warnings(conn: &Connection, report: &ingest::IngestReport) -> Result<Vec<String>> {
    let mut warnings = Vec::new();
    for provider in &report.providers {
        match provider.status {
            ingest::ProviderIngestStatus::NotConfigured => warnings.push(format!(
                "tkstat warning: {} source path is not configured{}; run `tkstat doctor` for details",
                provider.provider,
                cached_data_suffix(provider)
            )),
            ingest::ProviderIngestStatus::Missing => warnings.push(format!(
                "tkstat warning: {} source path is missing{}{}; run `tkstat doctor` for details",
                provider.provider,
                provider
                    .path
                    .as_ref()
                    .map(|path| format!(" ({})", path.display()))
                    .unwrap_or_default(),
                cached_data_suffix(provider)
            )),
            ingest::ProviderIngestStatus::Available => {
                if provider.discovered_files == 0 {
                    warnings.push(format!(
                        "tkstat warning: {} source has no session files{}; run `tkstat doctor` for details",
                        provider.provider,
                        cached_data_suffix(provider)
                    ));
                }
            }
        }

        if provider.parse_errors > 0 {
            let first_path = provider
                .findings
                .iter()
                .find(|finding| matches!(finding.kind, ingest::IngestFindingKind::MalformedLine))
                .map(|finding| format!("; first malformed file: {}", finding.path.display()))
                .unwrap_or_default();
            warnings.push(format!(
                "tkstat warning: {} skipped {} malformed JSONL line(s) while reading {} file(s){}; run `tkstat doctor` for details",
                provider.provider, provider.parse_errors, provider.processed_files, first_path
            ));
        }
        for finding in provider
            .findings
            .iter()
            .filter(|finding| matches!(finding.kind, ingest::IngestFindingKind::FileError))
        {
            warnings.push(format!(
                "tkstat warning: {} could not ingest {}: {}; run `tkstat doctor` for details",
                finding.provider,
                finding.path.display(),
                finding.message
            ));
        }
    }

    if usage_row_count(conn)? == 0 {
        warnings
            .push("tkstat warning: no usage records found; run `tkstat doctor` for details".into());
    }

    Ok(warnings)
}

fn cached_data_suffix(provider: &ingest::ProviderIngestReport) -> String {
    provider
        .last_ingested_at
        .as_deref()
        .map(|last| format!("; using cached data last ingested at {last}"))
        .unwrap_or_default()
}

fn usage_row_count(conn: &Connection) -> Result<u64> {
    let count: i64 = conn.query_row("SELECT COUNT(*) FROM token_usage", [], |row| row.get(0))?;
    Ok(count.max(0) as u64)
}

fn budget_report_rows(
    conn: &Connection,
    base_filter: &db::query::QueryFilter,
    cli: &Cli,
) -> Result<Vec<BudgetReportRow>> {
    let today = report_today(base_filter.report_timezone);
    let month_start = today.with_day(1).expect("day 1 is always valid");
    let mut rows = Vec::new();
    rows.push(budget_report_row(
        conn,
        base_filter,
        "today",
        today,
        today,
        cli.daily_budget_usd,
    )?);
    rows.push(budget_report_row(
        conn,
        base_filter,
        "month-to-date",
        month_start,
        today,
        cli.monthly_budget_usd,
    )?);
    if let (Some(begin), Some(end)) = (cli.begin, cli.end) {
        rows.push(budget_report_row(
            conn,
            base_filter,
            "selected",
            begin,
            end,
            None,
        )?);
    }
    Ok(rows)
}

fn budget_report_row(
    conn: &Connection,
    base_filter: &db::query::QueryFilter,
    label: &str,
    begin: chrono::NaiveDate,
    end: chrono::NaiveDate,
    threshold_usd: Option<f64>,
) -> Result<BudgetReportRow> {
    let mut filter = base_filter.clone();
    filter.begin = Some(begin);
    filter.end = Some(end);
    let summary = db::query::query_summary(conn, &filter)?;
    let mut row = BudgetReportRow::new(
        label,
        begin.to_string(),
        end.to_string(),
        summary.cost_usd,
        threshold_usd,
    );
    if summary.request_count > 0 {
        row.top_provider = db::query::query_by_provider(conn, &filter, 1)?
            .into_iter()
            .next()
            .and_then(|row| row.provider);
        row.top_model_id = db::query::query_by_model(conn, &filter, 1)?
            .into_iter()
            .next()
            .and_then(|row| row.model_id);
        row.top_project = db::query::query_by_project(conn, &filter, 1)?
            .into_iter()
            .next()
            .and_then(|row| row.project);
    }
    Ok(row)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tkstat::domain::provider::ProviderId;

    #[test]
    fn test_with_provider_label_adds_provider_to_period_rows() {
        let rows = vec![AggregatedRow {
            period: "2026-04-01".into(),
            request_count: 1,
            ..Default::default()
        }];
        let rows = with_provider_label(rows, "codex");
        assert_eq!(rows[0].provider.as_deref(), Some("codex"));
    }

    #[test]
    fn test_with_provider_label_preserves_model_group_provider() {
        let rows = vec![AggregatedRow {
            period: "codex/gpt-5.5".into(),
            provider: Some("codex".into()),
            model_id: Some("gpt-5.5".into()),
            request_count: 1,
            ..Default::default()
        }];
        let rows = with_provider_label(rows, "all providers");
        assert_eq!(rows[0].provider.as_deref(), Some("codex"));
    }

    #[test]
    fn test_report_today_uses_selected_timezone_policy() {
        assert_eq!(
            report_today(ReportTimeZone::Local),
            chrono::Local::now().date_naive()
        );
        assert_eq!(
            report_today(ReportTimeZone::Utc),
            chrono::Utc::now().date_naive()
        );
    }

    #[test]
    fn test_ingestion_warnings_include_cached_freshness() {
        let db = db::Database::open_in_memory().unwrap();
        let report = ingest::IngestReport {
            providers: vec![ingest::ProviderIngestReport {
                provider: ProviderId::ClaudeCode,
                path: Some(PathBuf::from("/missing")),
                status: ingest::ProviderIngestStatus::Missing,
                discovered_files: 0,
                processed_files: 0,
                inserted_records: 0,
                parse_errors: 0,
                findings: Vec::new(),
                last_ingested_at: Some("2026-05-23 20:05:00".into()),
            }],
        };
        let warnings = ingestion_warnings(db.conn(), &report).unwrap();
        assert!(
            warnings
                .iter()
                .any(|warning| warning.contains("last ingested at 2026-05-23 20:05:00"))
        );
    }
}
