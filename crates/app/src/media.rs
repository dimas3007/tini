//! Currently playing music, read from the Windows media controls (also used by the volume flyout):
//! title, artist, playback state, and prev / play-pause / next buttons.
//!
//! All WinRT calls run on a single background thread, since almost all of them are async and are awaited
//! there without blocking the UI thread. Windows events (sessions added/removed, track changed, play/
//! pause) wake that thread; the result is stored, then `WM_APP_MEDIA` is sent to the controller.
//! No polling.

use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use windows::{
    core::*,
    Foundation::TypedEventHandler,
    Media::Control::{
        GlobalSystemMediaTransportControlsSession as Session, GlobalSystemMediaTransportControlsSessionManager as Manager,
        GlobalSystemMediaTransportControlsSessionPlaybackStatus as Status,
    },
    Win32::Foundation::E_ABORT,
    Win32::System::WinRT::{RoInitialize, RO_INIT_MULTITHREADED},
};
use windows_future::{AsyncOperationCompletedHandler, IAsyncOperation};

use crate::app::WM_APP_MEDIA;
use crate::watch;

/// What is playing (or paused) in the selected media session.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct NowPlaying {
    pub title: String,
    pub artist: String,
    pub playing: bool,
    pub can_prev: bool,
    pub can_next: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    Prev,
    PlayPause,
    Next,
}

enum Job {
    /// The session list changed: re-attach per-session events, then re-read.
    Sessions,
    Refresh,
    Command(Command),
}

/// Logs only in debug builds, like in `app`.
macro_rules! log {
    ($($arg:tt)*) => {
        if cfg!(debug_assertions) {
            eprintln!("[media] {}", format!($($arg)*));
        }
    };
}

/// Limit for waiting on a single player call; past this it is treated as failed and cancelled.
const WAIT: Duration = Duration::from_secs(3);

static LATEST: Mutex<Option<NowPlaying>> = Mutex::new(None);
static JOBS: OnceLock<Sender<Job>> = OnceLock::new();

/// Runs the media thread. If Windows provides no media controls, the widget is never shown.
pub fn start() {
    let (tx, rx) = mpsc::channel();
    if JOBS.set(tx.clone()).is_err() {
        return;
    }
    let _ = std::thread::Builder::new().name("media".into()).spawn(move || {
        let _ = run(tx, rx);
    });
}

/// Latest result; read by the controller after `WM_APP_MEDIA`.
pub fn latest() -> Option<NowPlaying> {
    LATEST.lock().ok().and_then(|g| g.clone())
}

pub fn send(cmd: Command) {
    if let Some(tx) = JOBS.get() {
        let _ = tx.send(Job::Command(cmd));
    }
}

/// The session shown: the one that is playing first (which is also the "current session" if there are
/// several), then Windows' current session, then the first session.
fn pick(sessions: &[(bool, bool)]) -> Option<usize> {
    let find = |f: &dyn Fn(&(bool, bool)) -> bool| sessions.iter().position(f);
    find(&|&(playing, current)| playing && current).or_else(|| find(&|&(playing, _)| playing)).or_else(|| find(&|&(_, current)| current)).or((!sessions.is_empty()).then_some(0))
}

/// A player with no title (e.g. a freshly opened browser tab) is not shown.
fn now_playing(title: String, artist: String, playing: bool, can_prev: bool, can_next: bool) -> Option<NowPlaying> {
    let title = title.trim().to_string();
    (!title.is_empty()).then(|| NowPlaying { title, artist: artist.trim().to_string(), playing, can_prev, can_next })
}

/// Like `join`, but gives up after `WAIT`: a player that never answers (e.g. a hung browser
/// tab) must not freeze this thread, because the next event would never be read.
fn wait<T: RuntimeType + 'static>(op: IAsyncOperation<T>) -> Result<T> {
    wait_for(op, WAIT)
}

/// `wait` with its own timeout; also used by `location`.
pub fn wait_for<T: RuntimeType + 'static>(op: IAsyncOperation<T>, limit: Duration) -> Result<T> {
    let (done, finished) = mpsc::sync_channel(1);
    // An already-completed operation calls this handler immediately when attached.
    op.SetCompleted(&AsyncOperationCompletedHandler::new(move |_, _| {
        let _ = done.try_send(());
        Ok(())
    }))?;
    if finished.recv_timeout(limit).is_err() {
        let _ = op.Cancel();
        return Err(E_ABORT.into());
    }
    op.GetResults()
}

fn run(tx: Sender<Job>, rx: Receiver<Job>) -> Result<()> {
    unsafe {
        let _ = RoInitialize(RO_INIT_MULTITHREADED);
    }
    let manager = wait(Manager::RequestAsync()?)?;
    let wake = |job: fn() -> Job| {
        let tx = tx.clone();
        move || {
            let _ = tx.send(job());
        }
    };
    let on_sessions = wake(|| Job::Sessions);
    manager.SessionsChanged(&TypedEventHandler::new(move |_, _| {
        on_sessions();
        Ok(())
    }))?;
    let on_current = wake(|| Job::Refresh);
    manager.CurrentSessionChanged(&TypedEventHandler::new(move |_, _| {
        on_current();
        Ok(())
    }))?;

    // Sessions along with their event tokens, so they can be released when the session list changes.
    let mut watched: Vec<(Session, i64, i64)> = Vec::new();
    let mut chosen: Option<Session> = None;
    let _ = tx.send(Job::Sessions);
    while let Ok(first) = rx.recv() {
        // Events often arrive in a burst (track change = properties + status); reading once is enough.
        let (mut resubscribe, mut refresh) = (false, false);
        for job in std::iter::once(first).chain(rx.try_iter()) {
            match job {
                Job::Sessions => resubscribe = true,
                Job::Refresh => refresh = true,
                Job::Command(cmd) => {
                    if let Some(s) = &chosen {
                        let op = match cmd {
                            Command::Prev => s.TrySkipPreviousAsync(),
                            Command::PlayPause => s.TryTogglePlayPauseAsync(),
                            Command::Next => s.TrySkipNextAsync(),
                        };
                        let _ = op.and_then(wait);
                    }
                }
            }
        }
        if resubscribe {
            for (s, a, b) in watched.drain(..) {
                let _ = s.RemovePlaybackInfoChanged(a);
                let _ = s.RemoveMediaPropertiesChanged(b);
            }
            if let Ok(list) = manager.GetSessions() {
                for s in list {
                    let on_playback = wake(|| Job::Refresh);
                    let on_props = wake(|| Job::Refresh);
                    let a = s.PlaybackInfoChanged(&TypedEventHandler::new(move |_, _| {
                        on_playback();
                        Ok(())
                    }));
                    let b = s.MediaPropertiesChanged(&TypedEventHandler::new(move |_, _| {
                        on_props();
                        Ok(())
                    }));
                    if let (Ok(a), Ok(b)) = (a, b) {
                        watched.push((s, a, b));
                    }
                }
            }
        }
        if resubscribe || refresh {
            log!("wake: resubscribe={resubscribe} sessions={}", watched.len());
            let (session, next) = read(&manager, &watched);
            log!("read: {next:?}");
            chosen = session;
            publish(next);
        }
    }
    Ok(())
}

fn read(manager: &Manager, watched: &[(Session, i64, i64)]) -> (Option<Session>, Option<NowPlaying>) {
    let current = manager.GetCurrentSession().ok().and_then(|s| s.SourceAppUserModelId().ok());
    let flags: Vec<(bool, bool)> = watched
        .iter()
        .map(|(s, ..)| {
            let playing = s.GetPlaybackInfo().and_then(|i| i.PlaybackStatus()).is_ok_and(|st| st == Status::Playing);
            (playing, current.is_some() && s.SourceAppUserModelId().ok() == current)
        })
        .collect();
    log!("sessions (playing, current): {flags:?}");
    let Some(i) = pick(&flags) else { return (None, None) };
    let session = watched[i].0.clone();
    let info = (|| -> Result<Option<NowPlaying>> {
        let props = wait(session.TryGetMediaPropertiesAsync()?)?;
        let controls = session.GetPlaybackInfo()?.Controls()?;
        Ok(now_playing(
            props.Title()?.to_string(),
            props.Artist()?.to_string(),
            flags[i].0,
            controls.IsPreviousEnabled()?,
            controls.IsNextEnabled()?,
        ))
    })();
    if let Err(e) = &info {
        log!("properties: {e}");
    }
    (Some(session), info.ok().flatten())
}

/// The controller is woken only if what is shown actually changed.
fn publish(next: Option<NowPlaying>) {
    let Ok(mut latest) = LATEST.lock() else { return };
    if *latest != next {
        *latest = next;
        drop(latest);
        watch::post(WM_APP_MEDIA, 0, 0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefers_the_session_that_is_playing() {
        assert_eq!(pick(&[]), None);
        assert_eq!(pick(&[(false, false), (true, false)]), Some(1));
        assert_eq!(pick(&[(true, false), (true, true)]), Some(1));
        assert_eq!(pick(&[(false, false), (false, true)]), Some(1));
        assert_eq!(pick(&[(false, false), (false, false)]), Some(0));
    }

    #[test]
    fn untitled_players_are_hidden() {
        assert_eq!(now_playing("  ".into(), "x".into(), true, true, true), None);
        let n = now_playing(" Song ".into(), " Artist ".into(), false, false, true).unwrap();
        assert_eq!((n.title.as_str(), n.artist.as_str(), n.playing), ("Song", "Artist", false));
    }
}
