//! Logging and opt-in profiling, separate from window state and native event handling.
use std::cell::RefCell;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::OnceLock;
use std::time::Instant;

static LOG_PATH: OnceLock<PathBuf> = OnceLock::new();

pub fn log_file() -> &'static PathBuf {
    LOG_PATH.get_or_init(|| {
        let base = std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir)
            .join("tile-rs");
        let _ = std::fs::create_dir_all(&base);
        base.join("tile-rs.log")
    })
}

/// Cap on the log file size: once it is exceeded it is truncated and restarted, so a long session
/// cannot grow it without bound.
const LOG_MAX_BYTES: u64 = 1 << 20;

/// Keeps the log file open instead of reopening it for every line (a syscall per `log` call adds up
/// on the re-tile path). Each logging thread has its own sink.
struct LogSink {
    file: Option<std::fs::File>,
    written: u64,
}

thread_local! {
    static LOG: RefCell<LogSink> = const { RefCell::new(LogSink { file: None, written: 0 }) };
}

pub fn log(msg: impl AsRef<str>) {
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let msg = format!("[{timestamp}] {}", msg.as_ref());
    LOG.with(|l| {
        let mut sink = l.borrow_mut();
        if sink.file.is_none() {
            if let Ok(f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(log_file())
            {
                sink.written = f.metadata().map(|m| m.len()).unwrap_or(0);
                sink.file = Some(f);
            }
        }
        if sink.file.is_none() {
            return; // could not open the log: stay silent, like before
        }
        if sink.written > LOG_MAX_BYTES {
            if let Some(f) = sink.file.as_mut() {
                if f.set_len(0).is_ok() {
                    sink.written = 0;
                }
            }
        }
        if let Some(f) = sink.file.as_mut() {
            if writeln!(f, "{msg}").is_ok() {
                sink.written += msg.len() as u64 + 1;
            }
        }
    });
}

pub(crate) fn log_reset() {
    let _ = std::fs::write(log_file(), "");
    LOG.with(|l| {
        let mut sink = l.borrow_mut();
        sink.written = 0;
        sink.file = None; // reopen lazily so the freshly truncated file is picked up
    });
}

// ---------------------------------------------------------------------------
// profiling (enabled with the TILE_RS_PROFILE environment variable)
// ---------------------------------------------------------------------------

/// Logs how long a block of code took, when profiling is on (on every exit path).
pub(crate) struct Timer(&'static str, Option<Instant>);

impl Timer {
    pub(crate) fn new(name: &'static str) -> Self {
        Timer(name, prof::on().then(Instant::now))
    }
}

impl Drop for Timer {
    fn drop(&mut self) {
        if let Some(t) = self.1 {
            log(format!(
                "profile: {} {}us collect={} placements={} events_queued={} coalesced={} queue_peak={}",
                self.0,
                t.elapsed().as_micros(),
                prof::COLLECT_CALLS.load(Ordering::Relaxed),
                prof::PLACEMENTS.load(Ordering::Relaxed),
                prof::EVENTS_QUEUED.load(Ordering::Relaxed),
                prof::EVENTS_COALESCED.load(Ordering::Relaxed),
                prof::EVENT_QUEUE_PEAK.load(Ordering::Relaxed)
            ));
        }
    }
}

/// Per-frame stats, aggregated until the animation queue drains.
#[derive(Default)]
struct AnimProf {
    start: Option<Instant>,
    last_frame: Option<Instant>,
    frames: u64,
    intervals_ms: Vec<f32>,
    setpos_us: Vec<f32>,
}

const MAX_FRAME_SAMPLES: usize = 4096;

fn record_sample(samples: &mut Vec<f32>, sequence: u64, sample: f32) {
    if samples.len() < MAX_FRAME_SAMPLES {
        samples.push(sample);
    } else {
        samples[sequence as usize % MAX_FRAME_SAMPLES] = sample;
    }
}

thread_local! {
    static ANIM_PROF: RefCell<AnimProf> = RefCell::new(AnimProf::default());
}

pub(crate) fn anim_prof_reset() {
    ANIM_PROF.with(|p| *p.borrow_mut() = AnimProf::default());
}

/// Records one animation frame (only called when profiling is on).
pub(crate) fn anim_prof_frame(now: Instant, setpos_us: f32) {
    ANIM_PROF.with(|p| {
        let mut p = p.borrow_mut();
        if p.start.is_none() {
            p.start = Some(now);
        }
        if let Some(prev) = p.last_frame {
            let sequence = p.frames.saturating_sub(1);
            record_sample(
                &mut p.intervals_ms,
                sequence,
                (now - prev).as_secs_f32() * 1000.0,
            );
        }
        p.last_frame = Some(now);
        let sequence = p.frames;
        record_sample(&mut p.setpos_us, sequence, setpos_us);
        p.frames += 1;
    });
}

/// Logs the summary of a finished animation and resets the stats.
pub(crate) fn anim_prof_finish(now: Instant) {
    ANIM_PROF.with(|p| {
        let mut p = p.borrow_mut();
        let dur = p
            .start
            .map(|s| (now - s).as_secs_f32() * 1000.0)
            .unwrap_or(0.0);
        let setpos_avg = if p.setpos_us.is_empty() {
            0.0
        } else {
            p.setpos_us.iter().sum::<f32>() / p.setpos_us.len() as f32
        };
        let setpos_max = p.setpos_us.iter().cloned().fold(0.0_f32, f32::max);
        log(format!(
            "anim: frames={} dur={:.0}ms interval avg/p95/max={}ms setpos avg/max={:.2}/{:.2}ms",
            p.frames,
            dur,
            stat_line(&p.intervals_ms),
            setpos_avg / 1000.0,
            setpos_max / 1000.0
        ));
        *p = AnimProf::default();
    });
}

fn stat_line(v: &[f32]) -> String {
    if v.is_empty() {
        return "0/0/0".into();
    }
    let mut s = v.to_vec();
    s.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let avg = s.iter().sum::<f32>() / s.len() as f32;
    let p95 = s[((s.len() as f32 * 0.95) as usize).min(s.len() - 1)];
    let max = *s.last().unwrap();
    format!("{avg:.1}/{p95:.1}/{max:.1}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_animation_diagnostics_keep_a_bounded_recent_sample() {
        let mut samples = Vec::new();
        for sequence in 0..(MAX_FRAME_SAMPLES * 3) {
            record_sample(&mut samples, sequence as u64, sequence as f32);
        }
        assert_eq!(samples.len(), MAX_FRAME_SAMPLES);
        assert!(samples
            .iter()
            .all(|sample| *sample >= (MAX_FRAME_SAMPLES * 2) as f32));
    }
}

pub(crate) mod prof;
