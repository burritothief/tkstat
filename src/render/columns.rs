use crate::domain::usage::{AggregatedRow, format_cost, format_tokens};

/// Available table columns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Column {
    Input,
    Output,
    CacheRead,
    CacheCreation,
    Total,
    Cost,
    Requests,
    Sessions,
}

impl std::str::FromStr for Column {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "input" | "in" => Ok(Self::Input),
            "output" | "out" => Ok(Self::Output),
            "cache_rd" | "crd" | "cacherd" | "cache_read" => Ok(Self::CacheRead),
            "cache_cr" | "ccr" | "cachecr" | "cache_creation" => Ok(Self::CacheCreation),
            "total" | "tot" => Ok(Self::Total),
            "cost" => Ok(Self::Cost),
            "reqs" | "requests" | "req" => Ok(Self::Requests),
            "sessions" | "sess" => Ok(Self::Sessions),
            _ => Err(format!(
                "unknown column '{s}'. Available: {}",
                Self::available_names()
            )),
        }
    }
}

impl Column {
    /// Column header label.
    pub fn header(self) -> &'static str {
        match self {
            Self::Input => "input",
            Self::Output => "output",
            Self::CacheRead => "cache rd",
            Self::CacheCreation => "cache cr",
            Self::Total => "total",
            Self::Cost => "cost",
            Self::Requests => "reqs",
            Self::Sessions => "sessions",
        }
    }

    /// Format the value from an aggregated row for this column.
    pub fn format_value(self, row: &AggregatedRow) -> String {
        match self {
            Self::Input => format_tokens(row.input_tokens),
            Self::Output => format_tokens(row.output_tokens),
            Self::CacheRead => format_tokens(row.cache_read_tokens),
            Self::CacheCreation => format_tokens(row.cache_creation_tokens),
            Self::Total => format_tokens(row.total_tokens),
            Self::Cost => format_cost(row.cost_usd),
            Self::Requests => row.request_count.to_string(),
            Self::Sessions => row.session_count.to_string(),
        }
    }

    /// All available column names for help text.
    pub fn available_names() -> &'static str {
        "input, output, cache_rd, cache_cr, total, cost, reqs, sessions"
    }
}

/// Default column set when --columns is not specified.
pub fn default_columns() -> Vec<Column> {
    vec![
        Column::Input,
        Column::Output,
        Column::CacheRead,
        Column::CacheCreation,
        Column::Total,
        Column::Cost,
    ]
}

/// Parse a comma-separated column list.
pub fn parse_columns(s: &str) -> Result<Vec<Column>, String> {
    let mut cols = Vec::new();
    for part in s.split(',') {
        let trimmed = part.trim();
        if trimmed.is_empty() {
            continue;
        }
        cols.push(trimmed.parse::<Column>()?);
    }
    if cols.is_empty() {
        return Err("no columns specified".into());
    }
    Ok(cols)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_columns_basic() {
        let cols = parse_columns("input,output,cost").unwrap();
        assert_eq!(cols, vec![Column::Input, Column::Output, Column::Cost]);
    }

    #[test]
    fn test_parse_columns_aliases() {
        let cols = parse_columns("in,out,crd,ccr,tot,reqs,sess").unwrap();
        assert_eq!(cols.len(), 7);
    }

    #[test]
    fn test_parse_via_fromstr_trait() {
        let col: Column = "cost".parse().unwrap();
        assert_eq!(col, Column::Cost);
        assert!("bogus".parse::<Column>().is_err());
    }

    #[test]
    fn test_parse_columns_case_insensitive() {
        let cols = parse_columns("Input,OUTPUT,Cost").unwrap();
        assert_eq!(cols, vec![Column::Input, Column::Output, Column::Cost]);
    }

    #[test]
    fn test_parse_columns_with_spaces() {
        let cols = parse_columns("input, output, cost").unwrap();
        assert_eq!(cols, vec![Column::Input, Column::Output, Column::Cost]);
    }

    #[test]
    fn test_parse_columns_unknown() {
        assert!(parse_columns("input,bogus,cost").is_err());
    }

    #[test]
    fn test_parse_columns_empty() {
        assert!(parse_columns("").is_err());
    }

    #[test]
    fn test_default_columns() {
        let cols = default_columns();
        assert_eq!(cols.len(), 6);
        assert!(!cols.contains(&Column::Requests));
    }

    #[test]
    fn test_column_format_value() {
        let row = AggregatedRow {
            input_tokens: 1500,
            request_count: 42,
            cost_usd: 1.23,
            ..Default::default()
        };
        assert_eq!(Column::Input.format_value(&row), "1.5 K");
        assert_eq!(Column::Requests.format_value(&row), "42");
        assert_eq!(Column::Cost.format_value(&row), "$1.23");
    }
}
