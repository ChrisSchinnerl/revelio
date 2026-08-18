mod app;
mod apps;
mod backend;
mod store;
mod worldmap;

use std::io::Write;
use std::path::PathBuf;

/// The platform-conventional data directory, or `None` if it can't be resolved.
fn project_data_dir() -> Option<PathBuf> {
    directories::ProjectDirs::from("dev", "ChrisSchinnerl", "revelio")
        .map(|d| d.data_dir().to_path_buf())
}

/// Returns the DB path, creating the data dir if needed.
pub fn db_path() -> PathBuf {
    let dir = project_data_dir().unwrap_or_else(|| PathBuf::from("."));
    if let Err(e) = std::fs::create_dir_all(&dir) {
        log::warn!("failed to create data dir {}: {e}", dir.display());
    }
    dir.join("revelio.db")
}

/// Deletes revelio's data directory. Prompts for confirmation unless `force`.
fn run_reset(force: bool) {
    let Some(dir) = project_data_dir() else {
        eprintln!("Could not resolve the data directory.");
        std::process::exit(1);
    };
    if !dir.exists() {
        println!("Nothing to reset — {} does not exist.", dir.display());
        return;
    }

    if !force {
        print!(
            "This deletes all revelio data (connections, keys, synced objects) at\n  {}\nContinue? [y/N] ",
            dir.display()
        );
        let _ = std::io::stdout().flush();
        let mut input = String::new();
        if std::io::stdin().read_line(&mut input).is_err()
            || !matches!(input.trim().to_ascii_lowercase().as_str(), "y" | "yes")
        {
            println!("Aborted.");
            return;
        }
    }

    match std::fs::remove_dir_all(&dir) {
        Ok(()) => println!("Deleted {}", dir.display()),
        Err(e) => {
            eprintln!("Failed to delete {}: {e}", dir.display());
            std::process::exit(1);
        }
    }
}

fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn lerp3(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    [
        a[0] + (b[0] - a[0]) * t,
        a[1] + (b[1] - a[1]) * t,
        a[2] + (b[2] - a[2]) * t,
    ]
}

/// Signed distance from `p` to the segment `a`–`b`.
fn sd_segment(p: [f32; 2], a: [f32; 2], b: [f32; 2]) -> f32 {
    let pa = [p[0] - a[0], p[1] - a[1]];
    let ba = [b[0] - a[0], b[1] - a[1]];
    let h = ((pa[0] * ba[0] + pa[1] * ba[1]) / (ba[0] * ba[0] + ba[1] * ba[1])).clamp(0.0, 1.0);
    let d = [pa[0] - ba[0] * h, pa[1] - ba[1] * h];
    (d[0] * d[0] + d[1] * d[1]).sqrt()
}

/// Signed distance from `p` (relative to centre) to a rounded rectangle.
fn sd_rounded_rect(p: [f32; 2], half: f32, r: f32) -> f32 {
    let qx = p[0].abs() - (half - r);
    let qy = p[1].abs() - (half - r);
    (qx.max(0.0).hypot(qy.max(0.0))) + qx.max(qy).min(0.0) - r
}

/// Builds the app icon: a rounded, anti-aliased dark tile with a green "r".
fn app_icon() -> eframe::egui::IconData {
    const S: usize = 256;
    let sf = S as f32;
    let aa = 1.6 / sf; // ~1.6px edge softness in normalized units
    let green = [45.0, 200.0, 110.0];
    let bg_top = [24.0, 26.0, 30.0];
    let bg_bottom = [9.0, 10.0, 12.0];

    let mut rgba = vec![0u8; S * S * 4];
    for y in 0..S {
        for x in 0..S {
            let p = [(x as f32 + 0.5) / sf, (y as f32 + 0.5) / sf];

            // Rounded-rect tile, near full-bleed with softly rounded corners.
            let d_tile = sd_rounded_rect([p[0] - 0.5, p[1] - 0.5], 0.47, 0.22);
            let tile = smoothstep(aa, -aa, d_tile);

            // Lowercase "r": vertical stem with a shoulder off the top.
            let stem = sd_segment(p, [0.414, 0.385], [0.414, 0.639]) - 0.048;
            let arm = sd_segment(p, [0.414, 0.385], [0.586, 0.361]) - 0.048;
            let letter = smoothstep(aa, -aa, stem.min(arm));

            let bg = lerp3(bg_top, bg_bottom, p[1]);
            let color = lerp3(bg, green, letter);
            let i = (y * S + x) * 4;
            rgba[i] = color[0] as u8;
            rgba[i + 1] = color[1] as u8;
            rgba[i + 2] = color[2] as u8;
            rgba[i + 3] = (tile * 255.0) as u8;
        }
    }
    eframe::egui::IconData {
        rgba,
        width: S as u32,
        height: S as u32,
    }
}

fn main() -> eframe::Result {
    // CLI subcommands, handled before the GUI starts.
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("reset") {
        let force = args.iter().any(|a| a == "-y" || a == "--yes");
        run_reset(force);
        return Ok(());
    }

    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .target(env_logger::Target::Stdout)
        .init();

    let native_options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_icon(std::sync::Arc::new(app_icon()))
            .with_inner_size([1400.0, 900.0])
            .with_min_inner_size([900.0, 500.0]),
        ..Default::default()
    };
    eframe::run_native(
        "revelio",
        native_options,
        Box::new(|cc| Ok(Box::new(app::App::new(cc)))),
    )
}
