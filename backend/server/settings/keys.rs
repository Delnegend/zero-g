//! Typed integer settings: one name, default, allowed range, and parse rule each.

use std::ops::RangeInclusive;

use crate::storage::Setting;

/// An integer setting stored as decimal text in `settings`.
pub(crate) struct IntSetting {
    /// Row name in `settings`.
    pub(crate) name: &'static str,
    /// Value used when the row is absent.
    pub(crate) default: u64,
    /// Accepted values.
    pub(crate) range: RangeInclusive<u64>,
}

/// Days to keep request log metadata, charts, and totals.
pub(crate) const LOG_RETENTION_DAYS: IntSetting = IntSetting {
    name: "log_retention_days",
    default: 90,
    range: 1..=36500,
};
/// Days to keep captured request and response bodies.
pub(crate) const BODY_RETENTION_DAYS: IntSetting = IntSetting {
    name: "body_retention_days",
    default: 7,
    range: 1..=36500,
};
/// Datasheet refresh interval in hours; zero disables refresh.
pub(crate) const PRICING_INTERVAL_HOURS: IntSetting = IntSetting {
    name: "pricing_interval_hours",
    default: 24,
    range: 0..=u64::MAX / 3600,
};
/// Model discovery interval in minutes; zero disables discovery.
pub(crate) const DISCOVERY_INTERVAL_MINUTES: IntSetting = IntSetting {
    name: "discovery_interval_minutes",
    default: 60,
    range: 0..=u64::MAX / 60,
};

/// Every integer setting, in dashboard order.
pub(crate) const ALL: [&IntSetting; 4] = [
    &LOG_RETENTION_DAYS,
    &BODY_RETENTION_DAYS,
    &PRICING_INTERVAL_HOURS,
    &DISCOVERY_INTERVAL_MINUTES,
];

impl IntSetting {
    /// Interpret a stored value: absent means the default, anything invalid is an error,
    /// so a damaged retention value can never silently shorten what prune keeps.
    pub(crate) fn parse(&self, stored: Option<&str>) -> toasty::Result<u64> {
        let Some(stored) = stored else {
            return Ok(self.default);
        };
        stored
            .parse::<u64>()
            .ok()
            .filter(|value| self.range.contains(value))
            .ok_or_else(|| {
                toasty::Error::from_args(format_args!(
                    "{} must be an integer between {} and {}",
                    self.name,
                    self.range.start(),
                    self.range.end()
                ))
            })
    }

    /// Read the current value.
    pub(crate) async fn read(&self, exec: &mut dyn toasty::Executor) -> toasty::Result<u64> {
        let row = Setting::filter_by_name(self.name)
            .first()
            .exec(exec)
            .await?;
        self.parse(row.as_ref().map(|row| row.value.as_str()))
    }

    /// Store a value that the caller has checked against `range`.
    pub(crate) async fn write(
        &self,
        exec: &mut dyn toasty::Executor,
        value: u64,
    ) -> toasty::Result<()> {
        Setting::upsert_by_name(self.name)
            .value(value.to_string())
            .exec(exec)
            .await?;
        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    /// Missing values default; malformed or out-of-range values are errors everywhere.
    #[test]
    fn absent_defaults_and_invalid_fails() {
        assert_eq!(LOG_RETENTION_DAYS.parse(None).unwrap(), 90);
        assert_eq!(LOG_RETENTION_DAYS.parse(Some("30")).unwrap(), 30);
        assert!(LOG_RETENTION_DAYS.parse(Some("0")).is_err());
        assert!(LOG_RETENTION_DAYS.parse(Some("ninety")).is_err());
        assert_eq!(PRICING_INTERVAL_HOURS.parse(Some("0")).unwrap(), 0);
        assert!(
            PRICING_INTERVAL_HOURS
                .parse(Some(&u64::MAX.to_string()))
                .is_err()
        );
    }
}
