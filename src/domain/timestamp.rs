use chrono::{DateTime, Utc};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimestampStorageError {
    InvalidRfc3339,
    NonUtcOffset,
    NonCanonicalUtc,
}

impl std::fmt::Display for TimestampStorageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidRfc3339 => {
                f.write_str("timestamp must be RFC3339 with an explicit offset")
            }
            Self::NonUtcOffset => f.write_str("timestamp must use the UTC +00:00 offset"),
            Self::NonCanonicalUtc => {
                f.write_str("timestamp must use canonical UTC RFC3339 storage")
            }
        }
    }
}

impl std::error::Error for TimestampStorageError {}

pub fn format_utc_rfc3339(timestamp: DateTime<Utc>) -> String {
    timestamp.to_rfc3339()
}

pub fn parse_canonical_utc_rfc3339(value: &str) -> Result<DateTime<Utc>, TimestampStorageError> {
    let parsed =
        DateTime::parse_from_rfc3339(value).map_err(|_| TimestampStorageError::InvalidRfc3339)?;
    if parsed.offset().local_minus_utc() != 0 {
        return Err(TimestampStorageError::NonUtcOffset);
    }

    let utc = parsed.with_timezone(&Utc);
    if format_utc_rfc3339(utc) != value {
        return Err(TimestampStorageError::NonCanonicalUtc);
    }
    Ok(utc)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_utc_rfc3339_uses_single_storage_form() {
        let timestamp = "2026-04-07T03:00:00-07:00"
            .parse::<DateTime<chrono::FixedOffset>>()
            .unwrap()
            .with_timezone(&Utc);

        assert_eq!(format_utc_rfc3339(timestamp), "2026-04-07T10:00:00+00:00");
    }

    #[test]
    fn test_parse_canonical_utc_rfc3339_rejects_noncanonical_storage() {
        assert!(parse_canonical_utc_rfc3339("2026-04-07T10:00:00+00:00").is_ok());
        assert_eq!(
            parse_canonical_utc_rfc3339("2026-04-07T10:00:00Z").unwrap_err(),
            TimestampStorageError::NonCanonicalUtc
        );
        assert_eq!(
            parse_canonical_utc_rfc3339("2026-04-07T03:00:00-07:00").unwrap_err(),
            TimestampStorageError::NonUtcOffset
        );
        assert_eq!(
            parse_canonical_utc_rfc3339("2026-04-07 10:00:00").unwrap_err(),
            TimestampStorageError::InvalidRfc3339
        );
    }
}
