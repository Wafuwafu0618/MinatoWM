//! Animation spike: checks capture speed and the overlay on this machine
//! without touching any window.
//!
//!     cargo run -p minato-wm --example anim_spike
//!
//! It times GDI capture for each monitor, then on every monitor times a
//! freeze (GPU capture through Desktop Duplication, falling back to GDI)
//! and plays a fade, a slide and an interrupted fade. Watch that:
//! * the screen doesn't flash when the picture appears,
//! * clicks and typing still reach the window under the overlay,
//! * the taskbar doesn't flicker, and Chrome / Terminal look right.

use std::time::{Duration, Instant};

use minato_wm::anim::curve::Easing;
use minato_wm::anim::{capture, Animator, Play};
use minato_wm::win32;

fn main() {
    win32::set_dpi_awareness();
    let monitors = win32::monitors();
    for m in &monitors {
        let t = Instant::now();
        capture::capture(m.work_area).expect("capture failed");
        println!(
            "{} {}x{}: GDI capture {:?}",
            m.device,
            m.work_area.w,
            m.work_area.h,
            t.elapsed()
        );
    }

    let anim = Animator::start();
    if !anim.available() {
        eprintln!("overlay could not be created; see the log");
        return;
    }
    let fade = |ms| Play::Fade {
        hold: Duration::ZERO,
        duration: Duration::from_millis(ms),
        curve: Easing::Spring { damping: 1.0 },
    };

    for m in &monitors {
        let key = m.handle;
        // The first freeze sets up duplication; time a warm one too.
        for label in ["cold", "warm"] {
            let t = Instant::now();
            assert!(anim.freeze(key, m.work_area, Some((key, 99))));
            println!("{} freeze ({label}): picture on screen after {:?}", m.device, t.elapsed());
            anim.play(key, fade(250));
            std::thread::sleep(Duration::from_millis(450));
        }
    }

    let m = &monitors[0];
    let key = m.handle;

    // Check 1: the overlay must never be the window under the mouse.
    anim.freeze(key, m.work_area, None);
    let (cx, cy) = m.work_area.center();
    let hit = unsafe {
        windows_sys::Win32::UI::WindowsAndMessaging::WindowFromPoint(windows_sys::Win32::Foundation::POINT {
            x: cx,
            y: cy,
        })
    };
    let class = win32::Hwnd::from_raw(hit).map(|h| h.class()).unwrap_or_default();
    println!(
        "click-through: window under the overlay is {class:?} -> {}",
        if class == "MinatoWM.Overlay" { "FAIL (overlay takes the mouse)" } else { "ok" }
    );
    anim.play(key, fade(150));
    std::thread::sleep(Duration::from_millis(300));

    // Check 2: the overlay actually draws (mid-slide the screen differs).
    let before = capture::capture(m.work_area).unwrap();
    anim.freeze(key, m.work_area, None);
    anim.play(
        key,
        Play::Slide {
            incoming: None,
            dir: (1, 0),
            duration: Duration::from_millis(800),
            curve: Easing::LINEAR,
        },
    );
    std::thread::sleep(Duration::from_millis(400));
    let mid = capture::capture(m.work_area).unwrap();
    let changed = before
        .pixels
        .chunks(4)
        .zip(mid.pixels.chunks(4))
        .filter(|(a, b)| a[..3] != b[..3])
        .count();
    let pct = changed as f64 * 100.0 / (before.w * before.h) as f64;
    println!(
        "overlay renders: {pct:.1}% of pixels differ mid-slide -> {}",
        if pct > 1.0 { "ok" } else { "FAIL (nothing drawn, or a uniform screen)" }
    );
    std::thread::sleep(Duration::from_millis(600));

    println!("slide on {}: push left, cached picture comes in from the right", m.device);
    anim.freeze(key, m.work_area, None);
    anim.play(
        key,
        Play::Slide {
            incoming: Some((key, 99)),
            dir: (1, 0),
            duration: Duration::from_millis(450),
            curve: Easing::Spring { damping: 1.0 },
        },
    );
    std::thread::sleep(Duration::from_millis(900));

    println!("interrupt: re-freeze halfway through a slow fade");
    anim.freeze(key, m.work_area, None);
    anim.play(key, fade(1500));
    std::thread::sleep(Duration::from_millis(700));
    let t = Instant::now();
    anim.freeze(key, m.work_area, None);
    println!("  re-freeze took {:?} (should show the half-faded picture)", t.elapsed());
    anim.play(key, fade(300));
    std::thread::sleep(Duration::from_millis(600));
    println!("done");
}
