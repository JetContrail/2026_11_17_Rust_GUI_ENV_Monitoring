//! GUI-independent domain logic for environment monitoring.
//!
//! Nothing in this crate depends on a GUI framework, so it can be unit tested
//! on any host (including headless CI runners).

use std::fmt;

/// Severity of a measured value relative to its limits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum Level {
    #[default]
    Normal,
    Warning,
    Alarm,
}

impl fmt::Display for Level {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Normal => "Normal",
            Self::Warning => "Warning",
            Self::Alarm => "Alarm",
        })
    }
}

/// Error returned when limits are inconsistent.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct InvalidLimits {
    pub warning: f32,
    pub alarm: f32,
}

impl fmt::Display for InvalidLimits {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "warning limit ({}) must be finite and not exceed alarm limit ({})",
            self.warning, self.alarm
        )
    }
}

impl std::error::Error for InvalidLimits {}

/// Upper limits for a single measured quantity.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Limits {
    warning: f32,
    alarm: f32,
}

impl Limits {
    pub fn new(warning: f32, alarm: f32) -> Result<Self, InvalidLimits> {
        if warning.is_finite() && alarm.is_finite() && warning <= alarm {
            Ok(Self { warning, alarm })
        } else {
            Err(InvalidLimits { warning, alarm })
        }
    }

    pub fn warning(&self) -> f32 {
        self.warning
    }

    pub fn alarm(&self) -> f32 {
        self.alarm
    }

    /// Classifies `value`; non-finite values are treated as [`Level::Alarm`].
    pub fn classify(&self, value: f32) -> Level {
        if !value.is_finite() || value >= self.alarm {
            Level::Alarm
        } else if value >= self.warning {
            Level::Warning
        } else {
            Level::Normal
        }
    }
}

/// One sample from an environment sensor.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Reading {
    pub temperature_c: f32,
    pub humidity_pct: f32,
}

/// Limits for every quantity in a [`Reading`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Thresholds {
    pub temperature_c: Limits,
    pub humidity_pct: Limits,
}

impl Default for Thresholds {
    fn default() -> Self {
        Self {
            temperature_c: Limits {
                warning: 30.0,
                alarm: 40.0,
            },
            humidity_pct: Limits {
                warning: 70.0,
                alarm: 85.0,
            },
        }
    }
}

impl Thresholds {
    /// Returns the most severe level across all quantities.
    pub fn evaluate(&self, reading: &Reading) -> Level {
        self.temperature_c
            .classify(reading.temperature_c)
            .max(self.humidity_pct.classify(reading.humidity_pct))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limits_reject_inverted_or_non_finite_values() {
        assert!(Limits::new(10.0, 5.0).is_err());
        assert!(Limits::new(f32::NAN, 5.0).is_err());
        assert!(Limits::new(1.0, f32::INFINITY).is_err());
        assert!(Limits::new(5.0, 5.0).is_ok());
    }

    #[test]
    fn classify_uses_inclusive_boundaries() {
        let limits = Limits::new(30.0, 40.0).unwrap();
        assert_eq!(limits.classify(29.9), Level::Normal);
        assert_eq!(limits.classify(30.0), Level::Warning);
        assert_eq!(limits.classify(40.0), Level::Alarm);
        assert_eq!(limits.classify(f32::NAN), Level::Alarm);
    }

    #[test]
    fn evaluate_returns_most_severe_level() {
        let thresholds = Thresholds::default();
        let reading = Reading {
            temperature_c: 31.0,
            humidity_pct: 90.0,
        };
        assert_eq!(thresholds.evaluate(&reading), Level::Alarm);
        let reading = Reading {
            temperature_c: 20.0,
            humidity_pct: 40.0,
        };
        assert_eq!(thresholds.evaluate(&reading), Level::Normal);
    }
}
