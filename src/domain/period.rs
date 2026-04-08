use std::fmt;

/// Time granularity for aggregation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimePeriod {
    FiveMinutes,
    Hourly,
    Daily,
    Monthly,
    Yearly,
}

impl TimePeriod {
    /// SQL expression to bucket timestamps into this period.
    pub fn sql_group_expr(&self) -> &'static str {
        match self {
            Self::FiveMinutes => {
                "strftime('%Y-%m-%d %H:', timestamp, 'localtime') || printf('%02d', (cast(strftime('%M', timestamp, 'localtime') as integer) / 5) * 5)"
            }
            Self::Hourly => "strftime('%Y-%m-%d %H:00', timestamp, 'localtime')",
            Self::Daily => "date(timestamp, 'localtime')",
            Self::Monthly => "strftime('%Y-%m', timestamp, 'localtime')",
            Self::Yearly => "strftime('%Y', timestamp, 'localtime')",
        }
    }

    /// Default row limit for this period.
    pub fn default_limit(&self) -> u32 {
        match self {
            Self::FiveMinutes => 40,
            Self::Hourly => 30,
            Self::Daily => 30,
            Self::Monthly => 12,
            Self::Yearly => 10,
        }
    }
}

impl fmt::Display for TimePeriod {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::FiveMinutes => write!(f, "5 minutes"),
            Self::Hourly => write!(f, "hourly"),
            Self::Daily => write!(f, "daily"),
            Self::Monthly => write!(f, "monthly"),
            Self::Yearly => write!(f, "yearly"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_display() {
        assert_eq!(TimePeriod::FiveMinutes.to_string(), "5 minutes");
        assert_eq!(TimePeriod::Hourly.to_string(), "hourly");
        assert_eq!(TimePeriod::Daily.to_string(), "daily");
        assert_eq!(TimePeriod::Monthly.to_string(), "monthly");
        assert_eq!(TimePeriod::Yearly.to_string(), "yearly");
    }

    #[test]
    fn test_default_limits() {
        assert_eq!(TimePeriod::FiveMinutes.default_limit(), 40);
        assert_eq!(TimePeriod::Hourly.default_limit(), 30);
        assert_eq!(TimePeriod::Daily.default_limit(), 30);
        assert_eq!(TimePeriod::Monthly.default_limit(), 12);
        assert_eq!(TimePeriod::Yearly.default_limit(), 10);
    }

    #[test]
    fn test_sql_group_expr_not_empty() {
        for period in [
            TimePeriod::FiveMinutes,
            TimePeriod::Hourly,
            TimePeriod::Daily,
            TimePeriod::Monthly,
            TimePeriod::Yearly,
        ] {
            assert!(!period.sql_group_expr().is_empty());
        }
    }
}
