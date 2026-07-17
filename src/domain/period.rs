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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ReportTimeZone {
    #[default]
    Local,
    Utc,
}

impl ReportTimeZone {
    fn local_bucket(self, timestamp: &str, period: &str) -> Option<String> {
        matches!(self, Self::Local).then(|| format!("tkstat_local_bucket({timestamp}, '{period}')"))
    }
}

impl TimePeriod {
    /// SQL expression to bucket stored UTC timestamps into the selected report timezone.
    pub fn sql_group_expr(&self, timezone: ReportTimeZone) -> String {
        if let Some(local) = timezone.local_bucket(
            "timestamp",
            match self {
                Self::FiveMinutes => "five_minutes",
                Self::Hourly => "hour",
                Self::Daily => "day",
                Self::Monthly => "month",
                Self::Yearly => "year",
            },
        ) {
            return local;
        }
        match self {
            Self::FiveMinutes => "strftime('%Y-%m-%d %H:', timestamp) || printf('%02d', (cast(strftime('%M', timestamp) as integer) / 5) * 5)".into(),
            Self::Hourly => "strftime('%Y-%m-%d %H:00', timestamp)".into(),
            Self::Daily => day_sql_expr(timezone),
            Self::Monthly => "strftime('%Y-%m', timestamp)".into(),
            Self::Yearly => "strftime('%Y', timestamp)".into(),
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

pub fn day_sql_expr(timezone: ReportTimeZone) -> String {
    timezone
        .local_bucket("timestamp", "day")
        .unwrap_or_else(|| "date(timestamp)".into())
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
    fn test_sql_group_expr_requires_explicit_timezone_policy() {
        for period in [
            TimePeriod::FiveMinutes,
            TimePeriod::Hourly,
            TimePeriod::Daily,
            TimePeriod::Monthly,
            TimePeriod::Yearly,
        ] {
            assert!(!period.sql_group_expr(ReportTimeZone::Utc).is_empty());
            assert!(
                period
                    .sql_group_expr(ReportTimeZone::Local)
                    .contains("tkstat_local_bucket")
            );
        }
        assert_eq!(day_sql_expr(ReportTimeZone::Utc), "date(timestamp)");
        assert_eq!(
            day_sql_expr(ReportTimeZone::Local),
            "tkstat_local_bucket(timestamp, 'day')"
        );
    }
}
