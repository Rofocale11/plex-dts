//! FORK: in-process PMS shim boot (NuvioNative import).
//!
//! Starts the Stremio-backed PMS shim once at app boot, keeps the handle alive for the
//! process, and installs the prober adapter that plugs the app's own FFmpeg prober and
//! device-codec table into the shim's "can this TV play this stream?" decision.

use std::ffi::CString;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

struct ShimState {
    origin: CString,
    port: u16,
}

static SHIM: OnceLock<ShimState> = OnceLock::new();

/// Start the shim once. Idempotent.
pub fn start() -> Result<(), String> {
    if SHIM.get().is_some() {
        return Ok(());
    }

    let set = plx_stremio::addons::AddonSet::defaults();
    let backend = Arc::new(plx_pmsshim::AddonBackend::new(set));

    let cfg = plx_pmsshim::Config {
        data_dir: None,          // in-memory for first bring-up; move to a DB8-backed path later
        torrent_server: None,
        stream_deadline: Duration::from_secs(60),
        stream_grace: Duration::from_millis(2500),
        token: "plxnative-shim".into(),
    };

    let handle = plx_pmsshim::start(backend.clone(), cfg, 0)
        .map_err(|e| format!("bind 127.0.0.1:0: {e}"))?;

    handle.shim.probes.set_prober(Arc::new(probe_adapter));

    let origin = CString::new(handle.origin()).map_err(|_| "origin has NUL".to_string())?;
    let _ = SHIM.set(ShimState { origin, port: handle.port });
    Ok(())
}

/// The shim's loopback origin as a NUL-terminated string, valid for the process lifetime.
pub fn origin_cstr() -> *const std::os::raw::c_char {
    SHIM.get().map_or(std::ptr::null(), |s| s.origin.as_ptr())
}

pub fn port() -> u16 {
    SHIM.get().map_or(0, |s| s.port)
}



/// The app's prober. Opens the URL with the bundled libavformat, reports the real shape,
/// and marks each track with the TV's own decodability verdict. That verdict includes the
/// in-app AC3 encode lane: a DTS 5.1 48 kHz track is `decodable` (the `EncLane` takes it),
/// while a DTS 96 kHz track is not (no resampler).
fn probe_adapter(
    url: &str,
    deadline: Duration,
) -> Result<plx_pmsshim::shim_probe::ProbeInfo, String> {
    let p = plx_media::ff::probe::probe_url(url, deadline)?;
    let caps = plx_platform::devcaps::caps();
    let container = p.container().to_string();
    
    let video_decodable = p
        .video
        .as_ref()
        .is_some_and(|v| match v.codec.to_ascii_lowercase().as_str() {
            "h264" | "avc" => true,
            "hevc" | "h265" => caps.hevc,
            _ => false,
        });

    let video = p.video.map(|v| plx_pmsshim::shim_probe::Video {
        codec: v.codec,
        width: v.width,
        height: v.height,
        fps: v.fps,
        bit_depth: v.bit_depth,
        profile: v.profile,
        color_trc: v.color_trc,
        color_primaries: v.color_primaries,
        color_space: v.color_space,
        dovi: v.dovi.map(|d| plx_pmsshim::shim_probe::Dovi {
            profile: d.profile,
            level: d.level,
            bl_compat: d.bl_compat,
            rpu: d.rpu,
            el: d.el,
            bl: d.bl,
        }),
    });

    let audio = p
        .audio
        .into_iter()
        .map(|a| {
            let key = plx_platform::devcaps::dp_audio_key(&a.codec);
            let decodable =
                caps.audio_supports_at(key, a.channels as i64, a.sample_rate as i64);
            plx_pmsshim::shim_probe::Audio {
                codec: a.codec,
                profile: a.profile,
                channels: a.channels,
                language: a.language,
                title: a.title,
                default: a.default,
                decodable,
            }
        })
        .collect();

    let subtitles = p
        .subtitles
        .into_iter()
        .map(|s| plx_pmsshim::shim_probe::Sub {
            codec: s.codec,
            language: s.language,
            title: s.title,
            default: s.default,
            forced: s.forced,
        })
        .collect();

    Ok(plx_pmsshim::shim_probe::ProbeInfo {
        container,
        duration_ms: p.duration_ms,
        size: p.size,
        video,
        video_decodable,
        audio,
        subtitles,
    })
}
