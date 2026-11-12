//! Convert editorial timecode and project positions on the same musical clock.

use oc_dsl::ast::{BarBeat, ProjectProperties};
use oc_dsl::timecode::{PictureClock, PictureRate, TimecodeClock};
use oc_dsl::timing::ProjectClock;
use serde::{Deserialize, Serialize};
use tauri::State;

use crate::state::AppState;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
/// Request for picture timecode.
pub struct PictureTimecodeRequest {
    rate: Option<PictureRate>,
    origin: Option<String>,
    points: Vec<PicturePoint>,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum PicturePoint {
    Timecode(String),
    Position(BarBeat),
    Seconds(f64),
}

#[derive(Serialize)]
/// Picture timecode report.
pub struct PictureTimecodeReport {
    fps_numerator: i64,
    fps_denominator: i64,
    drop_frame: bool,
    points: Vec<ResolvedPoint>,
}

#[derive(Serialize)]
struct ResolvedPoint {
    timecode: String,
    frame_offset: i64,
    project_seconds: f64,
    frame_seconds: f64,
    offset_to_frame_ms: f64,
    quarter: Option<f64>,
    nearest_position: Option<BarBeat>,
    offset_to_grid_ms: Option<f64>,
}

fn convert(
    properties: &ProjectProperties,
    request: PictureTimecodeRequest,
) -> Result<PictureTimecodeReport, String> {
    if !(1..=64).contains(&request.points.len()) {
        return Err("Provide between 1 and 64 picture positions".into());
    }
    let settings = match (request.rate, request.origin) {
        (Some(rate), Some(origin)) => PictureClock { rate, origin },
        (None, None) => properties
            .picture_clock
            .clone()
            .ok_or("Set the project's picture clock or supply rate and origin")?,
        _ => return Err("Supply both picture rate and origin, or use the saved clock".into()),
    };
    let (numerator, denominator, _) = settings.rate.exact();
    let (timecode, origin) = TimecodeClock::from_settings(&settings)?;
    let drop_frame = timecode.drop_frame();
    let music = ProjectClock::new(properties)?;
    let frame_duration = denominator as f64 / numerator as f64;
    let mut points = Vec::with_capacity(request.points.len());
    for point in request.points {
        let (project_seconds, frame_offset) = match point {
            PicturePoint::Timecode(label) => {
                let frame = timecode.parse(&label)? - origin;
                (frame as f64 * frame_duration, frame)
            }
            point => {
                let seconds = match point {
                    PicturePoint::Position(at) => {
                        music.seconds_at_quarter(music.quarter_notes_at(&at)?)?
                    }
                    PicturePoint::Seconds(seconds) => seconds,
                    PicturePoint::Timecode(_) => unreachable!(),
                };
                if !seconds.is_finite() || seconds.abs() > 90_000.0 {
                    return Err(
                        "Picture seconds must be finite and within one day of the origin".into(),
                    );
                }
                (seconds, (seconds / frame_duration).round() as i64)
            }
        };
        let frame_seconds = frame_offset as f64 * frame_duration;
        let quarter = (project_seconds >= 0.0)
            .then(|| music.quarter_at_seconds(project_seconds))
            .transpose()?;
        let nearest = quarter
            .map(|quarter| {
                let earlier = music.bar_beat_floor_at_quarter(quarter)?;
                let later = super::bounce::covering_position(&music, quarter)?;
                let distance = |at| -> Result<f64, String> {
                    music
                        .seconds_at_quarter(music.quarter_notes_at(at)?)
                        .map(|grid| (project_seconds - grid) * 1_000.0)
                };
                let early_offset = distance(&earlier)?;
                let late_offset = distance(&later)?;
                Ok::<_, String>(if late_offset.abs() < early_offset.abs() {
                    (later, late_offset)
                } else {
                    (earlier, early_offset)
                })
            })
            .transpose()?;
        let (nearest_position, offset_to_grid_ms) = nearest.unzip();
        points.push(ResolvedPoint {
            timecode: timecode.format(origin + frame_offset)?,
            frame_offset,
            project_seconds,
            frame_seconds,
            offset_to_frame_ms: (project_seconds - frame_seconds) * 1_000.0,
            quarter,
            nearest_position,
            offset_to_grid_ms,
        });
    }
    Ok(PictureTimecodeReport {
        fps_numerator: numerator,
        fps_denominator: denominator,
        drop_frame,
        points,
    })
}

pub(crate) fn resolve_picture_timecode(
    state: &AppState,
    request: PictureTimecodeRequest,
) -> Result<PictureTimecodeReport, String> {
    let properties = state.lock_inner().project.properties.clone();
    convert(&properties, request)
}

/// Compute picture timecode.
#[tauri::command]
pub fn picture_timecode(
    request: PictureTimecodeRequest,
    state: State<AppState>,
) -> Result<PictureTimecodeReport, String> {
    resolve_picture_timecode(state.inner(), request)
}

/// Set the picture clock.
#[tauri::command]
pub fn set_picture_clock(
    clock: Option<PictureClock>,
    expected_revision: u64,
    state: State<AppState>,
) -> Result<super::shared::ProjectSnapshot, String> {
    let mut inner = state.lock_inner();
    if inner.transaction.is_some() {
        return Err("Finish the active agent transaction before changing the picture clock".into());
    }
    set_picture_clock_inner(&mut inner, clock, expected_revision)
}

pub(crate) fn set_picture_clock_inner(
    inner: &mut crate::state::AppStateInner,
    clock: Option<PictureClock>,
    expected_revision: u64,
) -> Result<super::shared::ProjectSnapshot, String> {
    use crate::state::{UndoEdit, UndoScope};
    inner.refuse_in_preview()?;
    inner.check_revision(Some(expected_revision))?;
    if let Some(settings) = &clock {
        TimecodeClock::from_settings(settings)?;
    }
    if inner.project.properties.picture_clock != clock {
        let source = oc_dsl::parser::rewrite_picture_clock(&inner.dsl_source, clock.as_ref())
            .map_err(|error| error.to_string())?;
        let old = inner.project.clone();
        inner.project.properties.picture_clock = clock;
        if let Err(error) = inner.undo.push(
            old.clone(),
            &inner.project,
            UndoEdit::new("picture clock", UndoScope::Transport),
        ) {
            inner.project = old;
            return Err(error);
        }
        inner.dsl_source = source;
        inner.dirty = true;
        inner.bump_revision();
    }
    Ok(super::shared::snapshot(inner))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn query(args: serde_json::Value) -> Result<PictureTimecodeReport, String> {
        let project =
            oc_dsl::parse("@project\n  tempo: 120\n  time_signature: 4/4\n  tempo_map: 5:1=144\n")
                .unwrap();
        convert(&project.properties, serde_json::from_value(args).unwrap())
    }

    #[test]
    fn picture_clock_edit_supplies_the_default_for_conversion() {
        let state = AppState::new();
        let mut inner = state.lock_inner();
        let revision = inner.revision;
        let clock = PictureClock {
            rate: PictureRate::Ntsc30,
            origin: "01:00:00;00".into(),
        };
        let saved = set_picture_clock_inner(&mut inner, Some(clock.clone()), revision).unwrap();
        assert_eq!(
            saved.project.properties.picture_clock.as_ref(),
            Some(&clock)
        );
        assert!(saved.dirty);
        assert_eq!(
            oc_dsl::parse(&inner.dsl_source)
                .unwrap()
                .properties
                .picture_clock,
            Some(clock)
        );
        let request = serde_json::from_value(json!({"points": [{"seconds": 0}]})).unwrap();
        let report = convert(&inner.project.properties, request).unwrap();
        assert_eq!(report.points[0].timecode, "01:00:00;00");
        assert!(set_picture_clock_inner(&mut inner, None, revision).is_err());
        let cleared = set_picture_clock_inner(&mut inner, None, saved.revision).unwrap();
        assert!(cleared.project.properties.picture_clock.is_none());
        assert_eq!(inner.undo_step().unwrap(), "picture clock");
        assert_eq!(
            oc_dsl::parse(&inner.dsl_source)
                .unwrap()
                .properties
                .picture_clock
                .unwrap()
                .origin,
            "01:00:00;00"
        );
    }

    #[test]
    fn picture_timecode_uses_project_tempo_map_and_keeps_off_grid_seconds() {
        let report = query(json!({
            "rate": "24", "origin": "01:00:00:00",
            "points": [
                {"position": {"bar": 5, "beat": 1, "subdivision": 0}},
                {"position": {"bar": 8, "beat": 1, "subdivision": 0}},
                {"timecode": "01:00:13:01"},
                {"timecode": "00:59:59:23"},
                {"seconds": 8.013}
            ]
        }))
        .unwrap();
        assert_eq!(report.points[0].timecode, "01:00:08:00");
        assert_eq!(report.points[1].timecode, "01:00:13:00");
        assert_eq!(report.points[2].frame_offset, 313);
        assert_eq!(
            report.points[2].nearest_position,
            Some(BarBeat::new(8, 1, 0))
        );
        assert!((report.points[2].offset_to_grid_ms.unwrap() - 1_000.0 / 24.0).abs() < 1e-8);
        assert_eq!(report.points[3].frame_offset, -1);
        assert_eq!(report.points[3].quarter, None);
        assert!((report.points[4].offset_to_frame_ms - 13.0).abs() < 1e-8);
    }

    #[test]
    fn picture_timecode_carries_an_explicit_day_prefix() {
        let report = query(json!({
            "rate":"24", "origin":"01:00:00:00",
            "points":[{"timecode":"D+1 01:00:00:00"}]
        }))
        .unwrap();
        assert_eq!(report.points[0].frame_offset, 24 * 86_400);
        assert_eq!(report.points[0].timecode, "D+1 01:00:00:00");
        assert_eq!(report.points[0].project_seconds, 86_400.0);
    }

    #[test]
    fn picture_timecode_round_trips_drop_frame_boundaries() {
        let report = query(json!({
            "rate":"30000/1001", "origin":"00:00:00;00",
            "points":[{"timecode":"00:00:59;29"},{"timecode":"00:01:00;02"},{"timecode":"00:10:00;00"}]
        })).unwrap();
        assert!(report.drop_frame);
        assert_eq!(
            (report.fps_numerator, report.fps_denominator),
            (30_000, 1_001)
        );
        assert_eq!(report.points[0].frame_offset, 1_799);
        assert_eq!(report.points[1].frame_offset, 1_800);
        assert!((report.points[1].frame_seconds - 60.06).abs() < 1e-10);
        assert_eq!(report.points[2].frame_offset, 17_982);
        let (clock, _) = TimecodeClock::from_settings(&PictureClock {
            rate: PictureRate::Ntsc30,
            origin: "00:00:00;00".into(),
        })
        .unwrap();
        for index in (0..2_589_408)
            .step_by(13)
            .chain([1_799, 1_800, 17_982, 2_589_407])
        {
            assert_eq!(clock.parse(&clock.format(index).unwrap()).unwrap(), index);
        }
        assert!(clock.parse("00:01:00;00").is_err());
        assert!(clock.parse("00:01:00:02").is_err());
        assert_eq!(clock.format(2_589_408).unwrap(), "D+1 00:00:00;00");
        assert_eq!(clock.parse("D+1 00:00:00;00").unwrap(), 2_589_408);
    }

    #[test]
    fn picture_timecode_preserves_fractional_non_drop_rate() {
        let report = query(json!({
            "rate":"24000/1001","origin":"00:00:00:00",
            "points":[{"timecode":"01:00:00:00"}]
        }))
        .unwrap();
        assert!(!report.drop_frame);
        assert_eq!(report.points[0].frame_offset, 86_400);
        assert!((report.points[0].frame_seconds - 3_603.6).abs() < 1e-9);
        assert!(
            query(json!({
                "rate":"24","origin":"00:00:00;00","points":[{"seconds":0}]
            }))
            .is_err()
        );
    }
}
