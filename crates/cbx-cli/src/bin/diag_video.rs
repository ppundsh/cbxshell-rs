use std::path::Path;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        println!("Usage: diag_video <video_path1> [video_path2 ...]");
        return;
    }

    let settings = cbx_core::settings::Settings::load();
    let config = settings.smart_render_config();

    for path_str in &args[1..] {
        let p = Path::new(path_str);
        println!("\n=== Testing: {} ===", p.display());
        match cbx_core::media::video_mf::extract_video_thumbnail_from_path(p, 256, &config) {
            Ok((img, rep)) => {
                println!(
                    "SUCCESS: {}x{}, lum={:.1}%, ent={:.1}%, score={:.1}, attempts={}, selected_sec={:.1}",
                    img.width(),
                    img.height(),
                    rep.metrics.luminance_percent,
                    rep.metrics.entropy_percent,
                    rep.metrics.score,
                    rep.attempts_made,
                    rep.selected_time_seconds
                );
                let out_name = format!("diag_{}.png", p.file_stem().and_then(|s| s.to_str()).unwrap_or("out"));
                let _ = img.save(&out_name);
                println!("Saved diagnostic thumbnail to: {out_name}");
            }
            Err(e) => println!("FAILED: {e}"),
        }
    }
}
