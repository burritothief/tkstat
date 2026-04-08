use std::time::Instant;

use anyhow::Result;
use clap::Parser;

use tkstat::cli::{ChartMetric, Cli, OutputMode};
use tkstat::render::columns;
use tkstat::{config, db, ingest, render};

fn main() -> Result<()> {
    let cli = Cli::parse();

    let data_dir = config::resolve_data_dir(cli.data_dir.as_deref())?;
    let db_path = config::resolve_db_path(cli.db_path.as_deref());
    let database = db::Database::open(&db_path)?;

    if cli.force_update {
        database.reset()?;
    }

    let start = Instant::now();
    let inserted = ingest::sync(&database, &data_dir, cli.force_update)?;
    let ingest_ms = start.elapsed().as_millis();

    if inserted > 0 {
        eprintln!("ingested {inserted} new records in {ingest_ms}ms");
    }

    let columns = match &cli.columns {
        Some(spec) => columns::parse_columns(spec).map_err(|e| anyhow::anyhow!(e))?,
        None => columns::default_columns(),
    };

    let filter = cli.query_filter();
    let filter_desc = render::filter_description(
        cli.model.as_deref(),
        cli.project.as_deref(),
        cli.begin.as_ref().map(|d| d.to_string()).as_deref(),
        cli.end.as_ref().map(|d| d.to_string()).as_deref(),
    );

    let output = match cli.output_mode() {
        OutputMode::Heatmap | OutputMode::Chart => {
            let daily = db::query::query_daily_totals(database.conn(), &filter)?;
            let chart_data: Vec<(String, f64)> = daily
                .into_iter()
                .map(|(date, tokens, cost)| {
                    let val = match cli.chart_metric {
                        ChartMetric::Cost => cost,
                        _ => tokens as f64,
                    };
                    (date, val)
                })
                .collect();
            let metric_label = format!("{:?}", cli.chart_metric).to_lowercase();
            if cli.heatmap {
                render::heatmap::render_heatmap(&chart_data, &metric_label)
            } else {
                render::braille::render_braille(&chart_data, &metric_label)
            }
        }
        OutputMode::Summary => {
            let summary = db::query::query_summary(database.conn(), &filter)?;
            render::summary::render_summary(&summary)
        }
        OutputMode::Oneline => {
            let summary = db::query::query_summary(database.conn(), &filter)?;
            render::oneline::render_oneline(&summary)
        }
        OutputMode::Json(period) => {
            let rows = db::query::query_by_period(
                database.conn(), period, &filter, cli.effective_limit(),
            )?;
            render::json::render_json(&rows)
        }
        OutputMode::TopDays => {
            let rows = db::query::query_top(database.conn(), &filter, cli.effective_limit())?;
            if cli.json {
                render::json::render_json(&rows)
            } else {
                render::table::render_table("top days", &rows, &columns, filter_desc.as_deref())
            }
        }
        OutputMode::Table(period) => {
            let rows = db::query::query_by_period(
                database.conn(), period, &filter, cli.effective_limit(),
            )?;
            render::table::render_table(&period.to_string(), &rows, &columns, filter_desc.as_deref())
        }
    };

    print!("{output}");
    Ok(())
}
