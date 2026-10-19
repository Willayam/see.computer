pub mod controller;
pub mod folder;
pub mod holders;
pub mod notes;
pub mod recorder;
pub mod tap;
pub mod transcript;

use std::time::{Duration, Instant};

use holders::{AudioProcess, MicHolder};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Native,
    Browser,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MeetingApp {
    pub name: &'static str,
    pub kind: Kind,
}

const fn native(name: &'static str) -> MeetingApp {
    MeetingApp {
        name,
        kind: Kind::Native,
    }
}

const fn browser(name: &'static str) -> MeetingApp {
    MeetingApp {
        name,
        kind: Kind::Browser,
    }
}

/// A holder matches an entry when its bundle id equals the prefix or extends it
/// after a dot, because Chrome, Zoom, and Teams run audio in helper processes.
const APPS: &[(&str, MeetingApp)] = &[
    ("us.zoom.xos", native("Zoom")),
    ("com.microsoft.teams2", native("Microsoft Teams")),
    ("com.microsoft.teams", native("Microsoft Teams")),
    ("com.cisco.webexmeetingsapp", native("Webex")),
    ("Cisco-Systems.Spark", native("Webex")),
    ("com.tinyspeck.slackmacgap", native("Slack")),
    ("com.apple.FaceTime", native("FaceTime")),
    ("com.hnc.Discord", native("Discord")),
    ("com.google.Chrome", browser("Chrome")),
    ("company.thebrowser.Browser", browser("Arc")),
    ("company.thebrowser.dia", browser("Dia")),
    ("com.microsoft.edgemac", browser("Edge")),
    ("com.brave.Browser", browser("Brave")),
    ("org.mozilla.firefox", browser("Firefox")),
    ("com.apple.Safari", browser("Safari")),
];

pub fn app_for(bundle: &str) -> Option<MeetingApp> {
    APPS.iter().find_map(|&(prefix, app)| {
        let head = bundle.get(..prefix.len())?;
        let rest = &bundle[prefix.len()..];
        (head.eq_ignore_ascii_case(prefix) && (rest.is_empty() || rest.starts_with('.')))
            .then_some(app)
    })
}

pub fn classify(holders: &[MicHolder]) -> Option<(i32, MeetingApp)> {
    let mut found = holders
        .iter()
        .filter_map(|holder| app_for(holder.bundle_id.as_deref()?).map(|app| (holder.pid, app)));
    let first = found.next()?;
    if first.1.kind == Kind::Native {
        return Some(first);
    }
    Some(
        found
            .find(|(_, app)| app.kind == Kind::Native)
            .unwrap_or(first),
    )
}

/// The process objects a tap on `app` should hear: every process of the app,
/// helpers included, since browsers and Teams play call audio from a helper.
pub fn processes_of(app: MeetingApp, processes: &[AudioProcess]) -> Vec<u32> {
    processes
        .iter()
        .filter(|process| {
            process
                .bundle_id
                .as_deref()
                .and_then(app_for)
                .is_some_and(|found| found == app)
        })
        .map(|process| process.object)
        .collect()
}

pub const START_AFTER: Duration = Duration::from_secs(3);
pub const END_AFTER: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    Started(MeetingApp),
    Ended(MeetingApp),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum State {
    #[default]
    Idle,
    Starting {
        app: MeetingApp,
        since: Instant,
    },
    Live {
        app: MeetingApp,
    },
    Ending {
        app: MeetingApp,
        since: Instant,
    },
}

#[derive(Debug, Default)]
pub struct Detector {
    state: State,
}

impl Detector {
    pub fn observe(&mut self, seen: Option<MeetingApp>, now: Instant) -> Option<Event> {
        let (next, event) = match (self.state, seen) {
            (State::Idle, Some(app)) => (State::Starting { app, since: now }, None),
            (State::Idle, None) => (State::Idle, None),
            (State::Starting { app, since }, Some(found))
                if app == found && now.saturating_duration_since(since) >= START_AFTER =>
            {
                (State::Live { app }, Some(Event::Started(app)))
            }
            (State::Starting { app, since }, Some(found)) if app == found => {
                (State::Starting { app, since }, None)
            }
            (State::Starting { .. }, Some(app)) => (State::Starting { app, since: now }, None),
            (State::Starting { .. }, None) => (State::Idle, None),
            (State::Live { app }, Some(_)) => (State::Live { app }, None),
            (State::Live { app }, None) => (State::Ending { app, since: now }, None),
            (State::Ending { app, .. }, Some(_)) => (State::Live { app }, None),
            (State::Ending { app, since }, None)
                if now.saturating_duration_since(since) >= END_AFTER =>
            {
                (State::Idle, Some(Event::Ended(app)))
            }
            (State::Ending { app, since }, None) => (State::Ending { app, since }, None),
        };
        self.state = next;
        event
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn holder(pid: i32, bundle: Option<&str>) -> MicHolder {
        MicHolder {
            pid,
            bundle_id: bundle.map(str::to_owned),
        }
    }

    #[test]
    fn classifies_bundles() {
        assert_eq!(
            classify(&[holder(1, Some("us.zoom.xos"))]),
            Some((1, native("Zoom")))
        );
        assert_eq!(
            classify(&[holder(2, Some("com.google.Chrome.helper"))]),
            Some((2, browser("Chrome")))
        );
        assert_eq!(
            classify(&[holder(3, Some("com.microsoft.teams2"))]),
            Some((3, native("Microsoft Teams")))
        );
        assert_eq!(classify(&[holder(4, Some("com.microsoft.teamsfoo"))]), None);
        assert_eq!(classify(&[holder(5, None)]), None);
        assert_eq!(classify(&[holder(6, Some("computer.see.app"))]), None);
        assert_eq!(
            classify(&[holder(7, Some("US.ZOOM.XOS.Helper"))]),
            Some((7, native("Zoom")))
        );
    }

    #[test]
    fn native_beats_browser() {
        assert_eq!(
            classify(&[
                holder(1, Some("com.google.Chrome")),
                holder(2, Some("us.zoom.xos"))
            ]),
            Some((2, native("Zoom")))
        );
    }

    #[test]
    fn debounce_start_and_end() {
        let app = native("Zoom");
        let t = Instant::now();
        let mut detector = Detector::default();
        assert_eq!(detector.observe(Some(app), t), None);
        assert_eq!(detector.observe(None, t + Duration::from_secs(2)), None);
        assert_eq!(
            detector.observe(Some(app), t + Duration::from_secs(3)),
            None
        );
        assert_eq!(
            detector.observe(Some(app), t + Duration::from_secs(6)),
            Some(Event::Started(app))
        );
        assert_eq!(
            detector.observe(Some(app), t + Duration::from_secs(7)),
            None
        );
        assert_eq!(detector.observe(None, t + Duration::from_secs(8)), None);
        assert_eq!(
            detector.observe(Some(app), t + Duration::from_secs(10)),
            None
        );
        assert_eq!(detector.observe(None, t + Duration::from_secs(11)), None);
        assert_eq!(
            detector.observe(None, t + Duration::from_secs(16)),
            Some(Event::Ended(app))
        );
    }

    #[test]
    fn switching_while_starting_restarts_clock() {
        let zoom = native("Zoom");
        let slack = native("Slack");
        let t = Instant::now();
        let mut detector = Detector::default();
        assert_eq!(detector.observe(Some(zoom), t), None);
        assert_eq!(
            detector.observe(Some(slack), t + Duration::from_secs(2)),
            None
        );
        assert_eq!(
            detector.observe(Some(slack), t + Duration::from_secs(3)),
            None
        );
        assert_eq!(
            detector.observe(Some(slack), t + Duration::from_secs(5)),
            Some(Event::Started(slack))
        );
        assert_eq!(
            detector.observe(Some(zoom), t + Duration::from_secs(6)),
            None
        );
        assert_eq!(detector.observe(None, t + Duration::from_secs(7)), None);
        assert_eq!(
            detector.observe(None, t + Duration::from_secs(12)),
            Some(Event::Ended(slack))
        );
    }
}
