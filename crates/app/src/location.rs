//! "Use my location": a one-shot position request to Windows Location on a short-lived thread. The result is stored,
//! then `WM_APP_LOCATION` is sent to the controller. The coordinates are only used for prayer times and
//! saved in settings.ini; they are not sent anywhere.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use windows::Devices::Geolocation::{GeolocationAccessStatus, Geolocator};
use windows::Win32::System::WinRT::{RoInitialize, RO_INIT_MULTITHREADED};

use crate::app::WM_APP_LOCATION;
use crate::{media, watch};

/// Position (latitude, longitude), or a short message on why it failed.
pub type Found = std::result::Result<(f64, f64), &'static str>;

static LATEST: Mutex<Option<Found>> = Mutex::new(None);
static BUSY: AtomicBool = AtomicBool::new(false);

/// GPS/Wi-Fi can be slow; anything longer than this counts as unavailable.
const WAIT: Duration = Duration::from_secs(20);

pub fn take() -> Option<Found> {
    LATEST.lock().ok()?.take()
}

/// Requests the position, unless the previous request is still pending.
pub fn request() {
    if BUSY.swap(true, Ordering::AcqRel) {
        return;
    }
    std::thread::spawn(|| {
        unsafe {
            let _ = RoInitialize(RO_INIT_MULTITHREADED);
        }
        let found = locate();
        BUSY.store(false, Ordering::Release);
        if let Ok(mut latest) = LATEST.lock() {
            *latest = Some(found);
        }
        watch::post(WM_APP_LOCATION, 0, 0);
    });
}

fn locate() -> Found {
    let status = Geolocator::RequestAccessAsync().and_then(|op| media::wait_for(op, WAIT)).map_err(|_| "Location isn't available on this PC")?;
    if status != GeolocationAccessStatus::Allowed {
        return Err("Turn on location for desktop apps in Windows Settings");
    }
    let position = Geolocator::new()
        .and_then(|g| g.GetGeopositionAsync())
        .and_then(|op| media::wait_for(op, WAIT))
        .and_then(|p| p.Coordinate())
        .and_then(|c| c.Point())
        .and_then(|p| p.Position())
        .map_err(|_| "Couldn't get your location")?;
    Ok((position.Latitude, position.Longitude))
}
