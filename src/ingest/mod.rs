pub mod parser;
pub mod walker;

use anyhow::Result;

use crate::db::Database;
use crate::domain::pricing;

/// Run the full ingestion pipeline: walk → parse → dedup → insert.
/// Returns the number of new records inserted.
pub fn sync(db: &Database, data_dir: &std::path::Path, force: bool) -> Result<usize> {
    let files = walker::discover_jsonl_files(data_dir)?;
    let mut total_inserted = 0;

    for file in &files {
        let state = if force {
            None
        } else {
            db.get_file_state(&file.path)?
        };

        if let Some(ref st) = state
            && st.size_bytes == file.size_bytes as i64
            && st.mtime_secs == file.mtime_secs
        {
            continue;
        }

        let offset = match &state {
            Some(st) if file.size_bytes as i64 > st.size_bytes => st.last_byte_offset as u64,
            _ => 0,
        };

        let records = parser::parse_jsonl_file(&file.path, offset, file)?;

        let records: Vec<_> = records
            .into_iter()
            .map(|mut r| {
                let p = pricing::pricing_for(r.model);
                r.cost_usd = p.calculate(
                    r.input_tokens,
                    r.output_tokens,
                    r.cache_read_tokens,
                    r.cache_creation_tokens,
                );
                r
            })
            .collect();

        let count = db.insert_records(&records)?;
        total_inserted += count;

        db.update_file_state(
            &file.path,
            file.size_bytes,
            file.mtime_secs,
            file.size_bytes as i64,
        )?;
    }

    Ok(total_inserted)
}
