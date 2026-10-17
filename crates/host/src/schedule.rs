//! Sleeps, restarts or shuts down this PC at times set from the owner's other
//! devices.
//!
//! The agent checks the schedule twice a minute. When a time comes while
//! someone is using the PC (connected through Conductor, using its keyboard or
//! mouse in the last ten minutes, or playing a video), it waits for them to
//! stop, for up to an hour, and otherwise skips that day. Times that pass while
//! the PC is asleep or off are skipped too, so a PC woken a little after its
//! bedtime stays awake.

use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Result, bail};
use conductor_proto::PowerSchedule;
use serde::{Deserialize, Serialize};
use windows::Win32::System::Power::{CallNtPowerInformation, ES_DISPLAY_REQUIRED, SystemExecutionState};
use windows::Win32::System::SystemInformation::{GetLocalTime, GetTickCount};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetLastInputInfo, LASTINPUTINFO};

use crate::state::AppState;

const CHECK_EVERY: Duration = Duration::from_secs(30);
/// How long after its time an action may still run, in minutes.
const WINDOW: i64 = 60;
/// Keyboard or mouse use more recent than this means someone is at the PC.
const IDLE_FOR: Duration = Duration::from_secs(10 * 60);
pub const MAX_ENTRIES: usize = 8;

const MINUTES_PER_DAY: i64 = 24 * 60;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct Entry {
    #[serde(flatten)]
    schedule: PowerSchedule,
    /// Times up to this one have been dealt with, in minutes since 1970.
    handled: i64,
}

/// Reading and writing the file, so the agent's checks and changes from
/// other devices don't overwrite each other.
static FILE: Mutex<()> = Mutex::new(());

fn path() -> std::path::PathBuf {
    crate::settings::data_dir().join("schedule.json")
}

fn read() -> Vec<Entry> {
    match std::fs::read(path()) {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|e| {
            tracing::warn!("schedule.json is invalid, ignoring it: {e}");
            Vec::new()
        }),
        Err(_) => Vec::new(),
    }
}

fn write(entries: &[Entry]) -> Result<()> {
    let path = path();
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(entries)?)?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

pub fn load() -> Vec<PowerSchedule> {
    let _file = FILE.lock().unwrap();
    read().into_iter().map(|e| e.schedule).collect()
}

/// Checks a schedule, and puts its days in order and drops repeats.
fn normalize(list: Vec<PowerSchedule>) -> Result<Vec<PowerSchedule>> {
    let mut out: Vec<PowerSchedule> = Vec::new();
    for mut s in list {
        if s.at as i64 >= MINUTES_PER_DAY || s.days.is_empty() || s.days.iter().any(|&d| d > 6) {
            bail!("That isn't a valid time and day.");
        }
        s.days.sort_unstable();
        s.days.dedup();
        if !out.contains(&s) {
            out.push(s);
        }
    }
    if out.len() > MAX_ENTRIES {
        bail!("A PC can have at most {MAX_ENTRIES} scheduled actions.");
    }
    Ok(out)
}

/// Replaces the schedule. Times that already passed today wait until they come round again.
pub fn save(list: Vec<PowerSchedule>) -> Result<Vec<PowerSchedule>> {
    let list = normalize(list)?;
    let now = now_minutes();
    let _file = FILE.lock().unwrap();
    let old = read();
    // Entries that didn't change keep their place, such as one waiting for the PC to be free.
    let entries: Vec<Entry> = list
        .iter()
        .map(|s| {
            let handled = old.iter().find(|e| &e.schedule == s).map_or(now, |e| e.handled);
            Entry { schedule: s.clone(), handled }
        })
        .collect();
    write(&entries)?;
    Ok(list)
}

fn now_minutes() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64 / 60).unwrap_or(0)
}

/// The first whole minute at or after `t`, in minutes since 1970.
fn minute_from(t: SystemTime) -> i64 {
    t.duration_since(UNIX_EPOCH).map(|d| (d.as_secs() as i64 + 59) / 60).unwrap_or(0)
}

/// The time of day in minutes after midnight, and the day of the week (0 is Sunday).
#[derive(Debug, Clone, Copy)]
struct Local {
    minute: i64,
    weekday: u8,
}

fn local_now() -> Local {
    let t = unsafe { GetLocalTime() };
    Local { minute: t.wHour as i64 * 60 + t.wMinute as i64, weekday: t.wDayOfWeek as u8 }
}

/// When `s` last came round, in minutes since 1970, if that was within the last hour.
fn occurrence(s: &PowerSchedule, local: Local, now: i64) -> Option<i64> {
    // Today, or late yesterday evening.
    for back in 0..2u8 {
        let day = (local.weekday + 7 - back) % 7;
        let ago = local.minute - s.at as i64 + MINUTES_PER_DAY * back as i64;
        if (0..WINDOW).contains(&ago) && s.days.contains(&day) {
            return Some(now - ago);
        }
    }
    None
}

/// How long since this session's keyboard and mouse were last used. When that
/// can't be told, it counts as just now.
fn idle() -> Duration {
    let mut info = LASTINPUTINFO { cbSize: size_of::<LASTINPUTINFO>() as u32, dwTime: 0 };
    if !unsafe { GetLastInputInfo(&mut info) }.as_bool() {
        return Duration::ZERO;
    }
    let ticks = unsafe { GetTickCount() };
    Duration::from_millis(ticks.wrapping_sub(info.dwTime) as u64)
}

/// Something wants the screen kept on, such as a video playing.
fn screen_wanted() -> bool {
    let mut state = 0u32;
    let status = unsafe {
        CallNtPowerInformation(
            SystemExecutionState,
            None,
            0,
            Some(&mut state as *mut u32 as *mut _),
            size_of::<u32>() as u32,
        )
    };
    status.is_ok() && state & ES_DISPLAY_REQUIRED.0 != 0
}

/// Why the PC counts as in use, if it does.
fn in_use(state: &AppState) -> Option<&'static str> {
    if !state.controllers().is_empty() {
        Some("someone is connected")
    } else if idle() < IDLE_FOR {
        Some("its keyboard or mouse was used recently")
    } else if screen_wanted() {
        Some("something is keeping the screen on")
    } else {
        None
    }
}

/// Runs scheduled actions for as long as the agent runs.
pub async fn run(state: Arc<AppState>) {
    let mut last = SystemTime::now();
    let mut watching_since = minute_from(last);
    loop {
        tokio::time::sleep(CHECK_EVERY).await;
        let now = SystemTime::now();
        // Checks much further apart than usual mean the PC was asleep.
        if now.duration_since(last).map_or(true, |gap| gap > CHECK_EVERY * 3) {
            watching_since = minute_from(now);
        }
        last = now;
        if let Err(e) = check(&state, watching_since) {
            tracing::warn!("checking the schedule: {e:#}");
        }
    }
}

/// What a check found: whether any entry changed, and the entry that's due
/// with the time it came round.
fn review(entries: &mut [Entry], local: Local, now: i64, watching_since: i64) -> (bool, Option<(usize, i64)>) {
    let mut changed = false;
    let mut due = None;
    for (i, e) in entries.iter_mut().enumerate() {
        let Some(t) = occurrence(&e.schedule, local, now).filter(|&t| t > e.handled) else { continue };
        if t < watching_since {
            tracing::info!("skipped a scheduled {:?}: this PC was asleep or off at the time", e.schedule.action);
            e.handled = handled_through(t);
            changed = true;
        } else if due.is_none() {
            due = Some((i, t));
        }
    }
    (changed, due)
}

/// Covers the whole window after `t`, so when the clocks go back and a time
/// comes round twice, it only counts once.
fn handled_through(t: i64) -> i64 {
    t + WINDOW
}

fn check(state: &AppState, watching_since: i64) -> Result<()> {
    let _file = FILE.lock().unwrap();
    let mut entries = read();
    let (local, now) = (local_now(), now_minutes());
    let (mut changed, due) = review(&mut entries, local, now, watching_since);
    let mut action = None;
    if let Some((i, t)) = due {
        match in_use(state) {
            Some(why) => tracing::debug!("waiting to {:?}: {why}", entries[i].schedule.action),
            None => {
                action = Some(entries[i].schedule.action);
                entries[i].handled = handled_through(t);
                changed = true;
            }
        }
    }
    // Before acting, so it doesn't happen again when the PC comes back.
    if changed {
        write(&entries)?;
    }
    if let Some(action) = action {
        tracing::info!("scheduled {action:?}");
        crate::power::act(action)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use conductor_proto::PowerAction;

    fn sleep_at(at: u16, days: &[u8]) -> PowerSchedule {
        PowerSchedule { action: PowerAction::Sleep, at, days: days.to_vec() }
    }

    fn entry(s: PowerSchedule, handled: i64) -> Entry {
        Entry { schedule: s, handled }
    }

    #[test]
    fn occurrences() {
        let every_day = [0, 1, 2, 3, 4, 5, 6];
        let monday_1am = Local { minute: 60, weekday: 1 };
        // Right on time, and 59 minutes late.
        assert_eq!(occurrence(&sleep_at(60, &every_day), monday_1am, 1000), Some(1000));
        assert_eq!(occurrence(&sleep_at(1, &every_day), monday_1am, 1000), Some(941));
        // An hour late is too late, and a time still to come hasn't happened.
        assert_eq!(occurrence(&sleep_at(0, &every_day), monday_1am, 1000), None);
        assert_eq!(occurrence(&sleep_at(61, &every_day), monday_1am, 1000), None);
        // Not on Mondays.
        assert_eq!(occurrence(&sleep_at(60, &[0, 2]), monday_1am, 1000), None);
        // 23:30 on Sunday, seen at 00:10 on Monday.
        let monday_0010 = Local { minute: 10, weekday: 1 };
        assert_eq!(occurrence(&sleep_at(23 * 60 + 30, &[0]), monday_0010, 1000), Some(960));
        assert_eq!(occurrence(&sleep_at(23 * 60 + 30, &[1]), monday_0010, 1000), None);
        let sunday_0010 = Local { minute: 10, weekday: 0 };
        assert_eq!(occurrence(&sleep_at(23 * 60 + 30, &[6]), sunday_0010, 1000), Some(960));
    }

    #[test]
    fn due_and_skipped() {
        let every_day = [0, 1, 2, 3, 4, 5, 6];
        // Monday 1:20, with a sleep set for 1:00, and "now" 1000 minutes after 1970.
        let local = Local { minute: 80, weekday: 1 };
        let one_am = 980;

        // Watching since before 1:00: it's due.
        let mut entries = vec![entry(sleep_at(60, &every_day), 0)];
        assert_eq!(review(&mut entries, local, 1000, 900), (false, Some((0, one_am))));

        // Woken at 1:15: 1:00 is skipped, not run late.
        let (changed, due) = review(&mut entries, local, 1000, 995);
        assert!(changed && due.is_none());
        assert_eq!(entries[0].handled, one_am + WINDOW);
        assert_eq!(review(&mut entries, local, 1000, 900), (false, None));

        // Already run.
        let mut entries = vec![entry(sleep_at(60, &every_day), handled_through(one_am))];
        assert_eq!(review(&mut entries, local, 1000, 900), (false, None));

        // When the clocks go back, 1:00 comes round again an hour later, and doesn't run twice.
        let local_again = Local { minute: 60, weekday: 1 };
        assert_eq!(review(&mut entries, local_again, one_am + 60, 900), (false, None));
        // The next day it runs.
        assert_eq!(review(&mut entries, local_again, one_am + 1440, 900), (false, Some((0, one_am + 1440))));
    }

    #[test]
    fn normalizes() {
        let list = vec![sleep_at(60, &[3, 1, 1]), sleep_at(60, &[1, 3]), sleep_at(90, &[0])];
        assert_eq!(normalize(list).unwrap(), vec![sleep_at(60, &[1, 3]), sleep_at(90, &[0])]);
        assert!(normalize(vec![sleep_at(60, &[])]).is_err());
        assert!(normalize(vec![sleep_at(60, &[7])]).is_err());
        assert!(normalize(vec![sleep_at(24 * 60, &[0])]).is_err());
        let many: Vec<_> = (0..=MAX_ENTRIES as u16).map(|i| sleep_at(i, &[0])).collect();
        assert!(normalize(many).is_err());
    }

    #[test]
    fn idle_time() {
        assert!(idle() < Duration::from_secs(60 * 60 * 24 * 365));
    }

    #[test]
    fn screen_requests() {
        // Whatever else is running, a request of our own shows up.
        let _awake = crate::power::KeepAwake::new("a test").unwrap();
        assert!(screen_wanted());
    }
}
