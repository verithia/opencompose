//! Editorial picture clock settings and frame-label arithmetic.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
/// Editorial picture frame rate.
pub enum PictureRate {
    #[serde(rename = "24000/1001")]
    Ntsc24,
    #[serde(rename = "24")]
    Fps24,
    #[serde(rename = "25")]
    Fps25,
    #[serde(rename = "30000/1001")]
    Ntsc30,
    #[serde(rename = "30")]
    Fps30,
    #[serde(rename = "48000/1001")]
    Ntsc48,
    #[serde(rename = "48")]
    Fps48,
    #[serde(rename = "50")]
    Fps50,
    #[serde(rename = "60000/1001")]
    Ntsc60,
    #[serde(rename = "60")]
    Fps60,
}

impl PictureRate {
    /// Serialized rate label.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ntsc24 => "24000/1001",
            Self::Fps24 => "24",
            Self::Fps25 => "25",
            Self::Ntsc30 => "30000/1001",
            Self::Fps30 => "30",
            Self::Ntsc48 => "48000/1001",
            Self::Fps48 => "48",
            Self::Fps50 => "50",
            Self::Ntsc60 => "60000/1001",
            Self::Fps60 => "60",
        }
    }

    /// Exact `(numerator, denominator, nominal fps)` triple.
    pub fn exact(self) -> (i64, i64, i64) {
        match self {
            Self::Ntsc24 => (24_000, 1_001, 24),
            Self::Fps24 => (24, 1, 24),
            Self::Fps25 => (25, 1, 25),
            Self::Ntsc30 => (30_000, 1_001, 30),
            Self::Fps30 => (30, 1, 30),
            Self::Ntsc48 => (48_000, 1_001, 48),
            Self::Fps48 => (48, 1, 48),
            Self::Fps50 => (50, 1, 50),
            Self::Ntsc60 => (60_000, 1_001, 60),
            Self::Fps60 => (60, 1, 60),
        }
    }
}

/// The label carried by the picture frame at project zero.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct PictureClock {
    pub rate: PictureRate,
    pub origin: String,
}

/// Frame-label arithmetic for one picture clock.
pub struct TimecodeClock {
    nominal: i64,
    drop_frame: bool,
}

impl TimecodeClock {
    /// Build a clock and parse its origin label.
    pub fn from_settings(settings: &PictureClock) -> Result<(Self, i64), String> {
        let (_, _, nominal) = settings.rate.exact();
        let drop_frame = settings.origin.contains(';');
        if drop_frame && !matches!(settings.rate, PictureRate::Ntsc30 | PictureRate::Ntsc60) {
            return Err("Drop-frame labels require 30000/1001 or 60000/1001 fps".into());
        }
        let clock = Self {
            nominal,
            drop_frame,
        };
        let origin = clock.parse(&settings.origin)?;
        Ok((clock, origin))
    }

    /// Whether labels use drop-frame counting.
    pub fn drop_frame(&self) -> bool {
        self.drop_frame
    }

    /// Frames in one nominal 24-hour label day.
    pub fn day_frames(&self) -> i64 {
        let nominal = self.nominal;
        let drop = nominal / 15;
        if self.drop_frame {
            nominal * 86_400 - drop * (1_440 - 144)
        } else {
            nominal * 86_400
        }
    }

    /// Split an optional explicit `D+1 ` / `D-1 ` day prefix.
    fn split_day(text: &str) -> Result<(i64, &str), String> {
        let Some(rest) = text.strip_prefix("D+").or_else(|| text.strip_prefix("D-")) else {
            return Ok((0, text));
        };
        let negative = text.starts_with("D-");
        let Some((digits, label)) = rest.split_once(' ') else {
            return Err(format!("Timecode day prefix needs a space: {text}"));
        };
        if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(format!("Timecode day prefix is not a number: {text}"));
        }
        let day = digits
            .parse::<i64>()
            .map_err(|_| format!("Timecode day is out of range: {text}"))?;
        let day = if negative { -day } else { day };
        Ok((day, label))
    }

    /// Parse `HH:MM:SS:FF` (or `;FF`) into a frame index.
    ///
    /// An optional `D+1 ` or `D-1 ` prefix selects an explicit adjacent day;
    /// labels never wrap implicitly.
    pub fn parse(&self, text: &str) -> Result<i64, String> {
        let (day, text) = Self::split_day(text)?;
        let bytes = text.as_bytes();
        let separator = if self.drop_frame { b';' } else { b':' };
        if bytes.len() != 11
            || bytes[2] != b':'
            || bytes[5] != b':'
            || bytes[8] != separator
            || ![0, 1, 3, 4, 6, 7, 9, 10]
                .into_iter()
                .all(|i| bytes[i].is_ascii_digit())
        {
            return Err(format!(
                "Timecode must use HH:MM:SS{}FF: {text}",
                char::from(separator)
            ));
        }
        let read = |i: usize| i64::from((bytes[i] - b'0') * 10 + bytes[i + 1] - b'0');
        let (h, m, s, f) = (read(0), read(3), read(6), read(9));
        if h >= 24 || m >= 60 || s >= 60 || f >= self.nominal {
            return Err(format!("Timecode field is out of range: {text}"));
        }
        let drop = self.nominal / 15;
        if self.drop_frame && m % 10 != 0 && s == 0 && f < drop {
            return Err(format!(
                "This label is skipped in drop-frame timecode: {text}"
            ));
        }
        let minutes = h * 60 + m;
        let omitted = if self.drop_frame {
            drop * (minutes - minutes / 10)
        } else {
            0
        };
        let local = (minutes * 60 + s) * self.nominal + f - omitted;
        day.checked_mul(self.day_frames())
            .and_then(|base| base.checked_add(local))
            .ok_or_else(|| format!("Timecode is out of range: {text}"))
    }

    /// Format a frame index, adding a `D+1 ` / `D-1 ` prefix when the index
    /// falls outside the label's nominal 24-hour day.
    pub fn format(&self, index: i64) -> Result<String, String> {
        let nominal = self.nominal;
        let drop = nominal / 15;
        let limit = self.day_frames();
        let day = index.div_euclid(limit);
        let index = index.rem_euclid(limit);
        let label = if self.drop_frame {
            let per_ten = nominal * 600 - drop * 9;
            let within_ten = index % per_ten;
            index
                + drop * 9 * (index / per_ten)
                + drop * ((within_ten - drop).max(0) / (nominal * 60 - drop))
        } else {
            index
        };
        let seconds = label / nominal;
        let separator = if self.drop_frame { ';' } else { ':' };
        let label = format!(
            "{:02}:{:02}:{:02}{separator}{:02}",
            seconds / 3600,
            seconds / 60 % 60,
            seconds % 60,
            label % nominal
        );
        Ok(if day == 0 {
            label
        } else {
            format!("D{day:+} {label}")
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{parse, serialize};

    #[test]
    fn a_day_prefixed_origin_survives_serialization() {
        let clock = PictureClock {
            rate: PictureRate::Fps24,
            origin: "D-1 23:59:59:23".into(),
        };
        let json = serde_json::to_string(&clock).unwrap();
        assert_eq!(serde_json::from_str::<PictureClock>(&json).unwrap(), clock);
        let (timecode, origin) = TimecodeClock::from_settings(&clock).unwrap();
        assert_eq!(origin, -1);
        assert_eq!(timecode.format(origin).unwrap(), clock.origin);
    }

    #[test]
    fn explicit_day_prefixes_round_trip_and_never_wrap() {
        let clock = TimecodeClock {
            nominal: 24,
            drop_frame: false,
        };
        let day = 24 * 86_400;
        assert_eq!(clock.format(day).unwrap(), "D+1 00:00:00:00");
        assert_eq!(clock.parse("D+1 00:00:00:00").unwrap(), day);
        assert_eq!(clock.format(-1).unwrap(), "D-1 23:59:59:23");
        assert_eq!(clock.parse("D-1 23:59:59:23").unwrap(), -1);
        assert!(clock.parse("D+ 00:00:00:00").is_err());
        assert!(clock.parse("D+1 24:00:00:00").is_err());
    }

    #[test]
    fn a_sixty_fps_drop_frame_day_boundary_round_trips() {
        let (clock, _) = TimecodeClock::from_settings(&PictureClock {
            rate: PictureRate::Ntsc60,
            origin: "00:00:00;00".into(),
        })
        .unwrap();
        let limit = clock.day_frames();
        assert_eq!(clock.format(limit).unwrap(), "D+1 00:00:00;00");
        assert_eq!(clock.parse("D+1 00:00:00;00").unwrap(), limit);
        assert_eq!(clock.format(limit - 1).unwrap(), "23:59:59;59");
        assert_eq!(clock.parse("23:59:59;59").unwrap(), limit - 1);
    }

    #[test]
    fn high_frame_rates_and_sixty_drop_frame_round_trip() {
        for (rate, nominal) in [
            (PictureRate::Ntsc48, 48),
            (PictureRate::Fps48, 48),
            (PictureRate::Fps50, 50),
            (PictureRate::Ntsc60, 60),
            (PictureRate::Fps60, 60),
        ] {
            assert_eq!(rate.exact().2, nominal);
            let clock = TimecodeClock {
                nominal,
                drop_frame: false,
            };
            let last = format!("00:00:00:{:02}", nominal - 1);
            assert_eq!(clock.parse(&last).unwrap(), nominal - 1);
            assert_eq!(clock.format(nominal - 1).unwrap(), last);
        }
        let (clock, _) = TimecodeClock::from_settings(&PictureClock {
            rate: PictureRate::Ntsc60,
            origin: "00:00:00;00".into(),
        })
        .unwrap();
        assert_eq!(clock.parse("00:00:00;59").unwrap(), 59);
        assert!(
            clock.parse("00:01:00;00").is_err(),
            "the first four labels drop"
        );
        let dropped = clock.parse("00:01:00;04").unwrap();
        assert_eq!(clock.format(dropped).unwrap(), "00:01:00;04");
    }

    #[test]
    fn picture_clock_round_trips_and_preserves_authored_source() {
        let old = "@project\n  tempo: 120 // keep this comment\n";
        let clock = PictureClock {
            rate: PictureRate::Ntsc30,
            origin: "01:00:00;00".into(),
        };
        let project = parse(old).unwrap();
        assert!(
            serde_json::to_value(&project.properties)
                .unwrap()
                .get("picture_clock")
                .is_none()
        );
        let source = crate::parser::rewrite_picture_clock(old, Some(&clock)).unwrap();
        assert!(source.contains("tempo: 120 // keep this comment"));
        let project = parse(&source).unwrap();
        assert_eq!(project.properties.picture_clock.as_ref(), Some(&clock));
        assert_eq!(
            parse(&serialize(&project))
                .unwrap()
                .properties
                .picture_clock
                .as_ref(),
            Some(&clock)
        );
        let json = serde_json::to_string(&project.properties).unwrap();
        let decoded: crate::ast::ProjectProperties = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.picture_clock.as_ref(), Some(&clock));
        use crate::diff::compressed::{decode_project, encode_project};
        let saved = encode_project(&project).unwrap();
        let reopened = decode_project(&saved).unwrap();
        assert_eq!(reopened.properties.picture_clock, Some(clock));
        let cleared = crate::parser::rewrite_picture_clock(&source, None).unwrap();
        assert!(cleared.contains("tempo: 120 // keep this comment"));
        assert_eq!(parse(&cleared).unwrap().properties.picture_clock, None);
        assert!(parse(&source.replace("01:00:00;00", "01:01:00;00")).is_err());
    }
}
