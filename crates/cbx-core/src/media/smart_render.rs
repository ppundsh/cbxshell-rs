/// Smart Rendering Algorithm (聰明渲染演算法)
/// 
/// Re-implements and enhances Media Preview's visual evaluation metrics:
/// 1. Neutral position seeking (避開片頭片尾中立定位, default 33%)
/// 2. Mean Luminance calculation (平均亮度檢測, default 40%..80%)
/// 3. Shannon Entropy estimation (香農圖像熵度檢測, default >= 20%)
/// 4. Forward stepping probe & GOP snap-back guard (跳步探測、跨 GOP 防回彈推進與加權評分選取)
/// 5. ACES Filmic HDR Tone-Mapping for modern HDR10/PQ/HLG content

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SmartRenderConfig {
    pub position_seek_percent: u32,
    pub seek_mode: u32,             // 0: Percent (default 25%), 1: Fixed seconds, 2: Conditional threshold
    pub seek_fixed_sec: u32,        // e.g. 120s
    pub seek_threshold_min: u32,    // e.g. 60min
    pub step_probe_sec: u32,        // 跳步探測秒數 (default 6s)
    pub max_gop_hops: u32,          // 跨 GOP 防回彈推進上限 (default 8, 0 = disabled)
    pub consecutive_unacceptable_limit: u32, // 關鍵影格探測嘗試上限 (default 6)
    pub luminance_enabled: bool,
    pub luminance_lower_limit: u32,
    pub luminance_upper_limit: u32,
    pub entropy_enabled: bool,
    pub entropy_lower_limit: u32,
    pub hdr_tonemap_enabled: bool,
    pub watchdog_timeout_sec: u32,
}

impl Default for SmartRenderConfig {
    fn default() -> Self {
        Self {
            position_seek_percent: 25,
            seek_mode: 2,
            seek_fixed_sec: 120,
            seek_threshold_min: 60,
            step_probe_sec: 6,
            max_gop_hops: 8,
            consecutive_unacceptable_limit: 6,
            luminance_enabled: true,
            luminance_lower_limit: 40,
            luminance_upper_limit: 80,
            entropy_enabled: true,
            entropy_lower_limit: 20,
            hdr_tonemap_enabled: false,
            watchdog_timeout_sec: 12,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FrameMetrics {
    pub luminance_percent: f32,
    pub entropy_percent: f32,
    pub score: f32,
    pub passed_luminance: bool,
    pub passed_entropy: bool,
    pub is_accepted: bool,
}

/// Computes the mean perceived luminance of a BGRA or RGBA pixel buffer as a percentage (0.0% .. 100.0%).
/// Uses the Rec.601 integer approximation: Y = (77*R + 150*G + 29*B) >> 8.
/// Samples up to 65,536 pixels with a stride for ultra-fast evaluation.
pub fn compute_luminance(pixels: &[u8], is_bgra: bool) -> f32 {
    let total_pixels = pixels.len() / 4;
    if total_pixels == 0 {
        return 0.0;
    }

    let step = (total_pixels / 65536).max(1);
    let mut sum_y: u64 = 0;
    let mut sampled_count: u64 = 0;

    let mut i = 0;
    while i < total_pixels {
        let base = i * 4;
        if base + 3 < pixels.len() {
            let (r, g, b) = if is_bgra {
                (pixels[base + 2] as u32, pixels[base + 1] as u32, pixels[base] as u32)
            } else {
                (pixels[base] as u32, pixels[base + 1] as u32, pixels[base + 2] as u32)
            };
            let y = (77 * r + 150 * g + 29 * b) >> 8;
            sum_y += y as u64;
            sampled_count += 1;
        }
        i += step;
    }

    if sampled_count == 0 {
        return 0.0;
    }

    let mean_y = sum_y as f32 / sampled_count as f32;
    (mean_y / 255.0) * 100.0
}

/// Computes the Shannon Entropy of pixel intensities normalized to 0.0% .. 100.0%.
/// Max 8-bit Shannon entropy is 8.0 bits.
/// Flat, solid color, or single-tone images have near 0% entropy.
/// Rich, high-detail frames have higher entropy (typically 30%..85%).
pub fn compute_shannon_entropy(pixels: &[u8], is_bgra: bool) -> f32 {
    let total_pixels = pixels.len() / 4;
    if total_pixels == 0 {
        return 0.0;
    }

    let step = (total_pixels / 65536).max(1);
    let mut hist = [0u32; 256];
    let mut sampled_count: u32 = 0;

    let mut i = 0;
    while i < total_pixels {
        let base = i * 4;
        if base + 3 < pixels.len() {
            let (r, g, b) = if is_bgra {
                (pixels[base + 2] as u32, pixels[base + 1] as u32, pixels[base] as u32)
            } else {
                (pixels[base] as u32, pixels[base + 1] as u32, pixels[base + 2] as u32)
            };
            let y = ((77 * r + 150 * g + 29 * b) >> 8) as usize;
            hist[y.min(255)] += 1;
            sampled_count += 1;
        }
        i += step;
    }

    if sampled_count == 0 {
        return 0.0;
    }

    let n = sampled_count as f32;
    let mut entropy = 0.0f32;
    for &count in hist.iter() {
        if count > 0 {
            let p = count as f32 / n;
            entropy -= p * p.log2();
        }
    }

    // Normalize: 8.0 bits max -> 0.0 .. 100.0%
    ((entropy / 8.0) * 100.0).clamp(0.0, 100.0)
}

/// Ultra-fast single-pass zero-allocation luminance & entropy calculation for NV12 Y-plane.
/// Avoids all color conversion and memory allocation during candidate frame exploration.
pub fn compute_luminance_and_entropy_y_plane(y_plane: &[u8]) -> (f32, f32) {
    let total = y_plane.len();
    if total == 0 {
        return (0.0, 0.0);
    }
    let step = (total / 16384).max(1);
    let mut sum_y: u64 = 0;
    let mut hist = [0u32; 256];
    let mut sampled = 0u32;
    let mut i = 0;
    while i < total {
        let y = y_plane[i] as usize;
        sum_y += y as u64;
        hist[y] += 1;
        sampled += 1;
        i += step;
    }
    if sampled == 0 {
        return (0.0, 0.0);
    }
    let mean_y = (sum_y as f32 / sampled as f32) / 255.0 * 100.0;
    let n = sampled as f32;
    let mut entropy = 0.0f32;
    for &count in hist.iter() {
        if count > 0 {
            let p = count as f32 / n;
            entropy -= p * p.log2();
        }
    }
    let entropy_percent = ((entropy / 8.0) * 100.0).clamp(0.0, 100.0);
    (mean_y, entropy_percent)
}

/// Ultra-fast single-pass zero-allocation luminance & entropy calculation for 32bpp BGRA slices.
pub fn compute_luminance_and_entropy_bgra(pixels: &[u8]) -> (f32, f32) {
    let total_pixels = pixels.len() / 4;
    if total_pixels == 0 {
        return (0.0, 0.0);
    }
    let step = (total_pixels / 16384).max(1);
    let mut sum_y: u64 = 0;
    let mut hist = [0u32; 256];
    let mut sampled = 0u32;
    let mut i = 0;
    while i < total_pixels {
        let base = i * 4;
        if base + 2 < pixels.len() {
            let b = pixels[base] as u32;
            let g = pixels[base + 1] as u32;
            let r = pixels[base + 2] as u32;
            let y = (((77 * r + 150 * g + 29 * b) >> 8) as usize).min(255);
            sum_y += y as u64;
            hist[y] += 1;
            sampled += 1;
        }
        i += step;
    }
    if sampled == 0 {
        return (0.0, 0.0);
    }
    let mean_y = (sum_y as f32 / sampled as f32) / 255.0 * 100.0;
    let n = sampled as f32;
    let mut entropy = 0.0f32;
    for &count in hist.iter() {
        if count > 0 {
            let p = count as f32 / n;
            entropy -= p * p.log2();
        }
    }
    let entropy_percent = ((entropy / 8.0) * 100.0).clamp(0.0, 100.0);
    (mean_y, entropy_percent)
}

/// Ultra-fast single-pass zero-allocation luminance & entropy calculation for 16bpp YUY2 slices.
pub fn compute_luminance_and_entropy_yuy2(pixels: &[u8]) -> (f32, f32) {
    let total_pixels = pixels.len() / 2;
    if total_pixels == 0 {
        return (0.0, 0.0);
    }
    let step = (total_pixels / 16384).max(1);
    let mut sum_y: u64 = 0;
    let mut hist = [0u32; 256];
    let mut sampled = 0u32;
    let mut i = 0;
    while i < total_pixels {
        let base = i * 2;
        if base < pixels.len() {
            let y = pixels[base] as usize;
            sum_y += y as u64;
            hist[y] += 1;
            sampled += 1;
        }
        i += step;
    }
    if sampled == 0 {
        return (0.0, 0.0);
    }
    let mean_y = (sum_y as f32 / sampled as f32) / 255.0 * 100.0;
    let n = sampled as f32;
    let mut entropy = 0.0f32;
    for &count in hist.iter() {
        if count > 0 {
            let p = count as f32 / n;
            entropy -= p * p.log2();
        }
    }
    let entropy_percent = ((entropy / 8.0) * 100.0).clamp(0.0, 100.0);
    (mean_y, entropy_percent)
}

/// Evaluates metrics directly from precalculated luminance and entropy percentages.
pub fn evaluate_metrics(lum: f32, ent: f32, config: &SmartRenderConfig) -> FrameMetrics {
    let pass_lum = if config.luminance_enabled {
        lum >= config.luminance_lower_limit as f32 && lum <= config.luminance_upper_limit as f32
    } else {
        true
    };

    let pass_ent = if config.entropy_enabled {
        ent >= config.entropy_lower_limit as f32
    } else {
        true
    };

    let _is_accepted = pass_lum && pass_ent;

    let mut penalty = 0.0f32;
    if config.luminance_enabled {
        let low = config.luminance_lower_limit as f32;
        let high = config.luminance_upper_limit as f32;
        if lum < low {
            penalty += (low - lum) * 1.5;
            if lum < 15.0 && ent < 35.0 {
                // Severe black screen penalty
                penalty += 35.0;
            }
        } else if lum > high {
            penalty += (lum - high) * 2.0;
            if lum > 88.0 && ent < 35.0 {
                // Severe monochromatic white screen / flash transition penalty
                penalty += 40.0;
            }
        }
    }
    if config.entropy_enabled && ent < config.entropy_lower_limit as f32 {
        penalty += (config.entropy_lower_limit as f32 - ent) * 2.0;
    }

    // Low-texture landscape & monochrome defense:
    // 1. Sky / overexposed flat sky: high luminance + low entropy
    if lum > 62.0 && ent < 45.0 {
        penalty += 35.0;
    }
    // 2. Cave / muddy dark scene: low luminance + low entropy
    if lum < 24.0 && ent < 45.0 {
        penalty += 35.0;
    }
    // 3. Monochromatic / flat single-color tone
    if ent < 25.0 {
        penalty += (25.0 - ent) * 2.0;
    }

    let is_accepted = pass_lum && pass_ent;

    let score = ent - penalty;

    FrameMetrics {
        luminance_percent: lum,
        entropy_percent: ent,
        score,
        passed_luminance: pass_lum,
        passed_entropy: pass_ent,
        is_accepted,
    }
}

/// Evaluates a candidate frame against the Smart Render configuration.
/// Returns FrameMetrics with whether the frame is acceptable and its fallback score.
pub fn evaluate_frame(pixels: &[u8], is_bgra: bool, config: &SmartRenderConfig) -> FrameMetrics {
    let lum = compute_luminance(pixels, is_bgra);
    let ent = compute_shannon_entropy(pixels, is_bgra);
    let mut m = evaluate_metrics(lum, ent, config);
    if is_image_too_simple(pixels, is_bgra) {
        m.is_accepted = false;
        m.score -= 50.0;
    }
    m
}

/// ACES Filmic Tone-Mapping Curve
/// Maps HDR high-luminance colors (e.g. PQ / HLG / BT.2020) smoothly into sRGB [0..255]
/// preventing washed-out, milky, or gray appearances on modern HDR videos.
#[inline]
pub fn aces_filmic_tone_mapping(r: f32, g: f32, b: f32) -> (u8, u8, u8) {
    #[inline]
    fn curve(x: f32) -> f32 {
        let a = 2.51f32;
        let b = 0.03f32;
        let c = 2.43f32;
        let d = 0.59f32;
        let e = 0.14f32;
        ((x * (a * x + b)) / (x * (c * x + d) + e)).clamp(0.0, 1.0)
    }

    let cr = (curve(r) * 255.0).round() as u8;
    let cg = (curve(g) * 255.0).round() as u8;
    let cb = (curve(b) * 255.0).round() as u8;
    (cr, cg, cb)
}

/// Evaluates whether an image is considered "too simple" or "monochromatic/plain"
/// (such as pure blank/solid background, single-color page, or plain background with only text disclaimers/credits).
///
/// Returns `true` if the image should be skipped as a cover candidate, or treated as having no cover.
pub fn is_image_too_simple(pixels: &[u8], is_bgra: bool) -> bool {
    let total_pixels = pixels.len() / 4;
    if total_pixels == 0 {
        return true;
    }

    // Sample up to 16,384 pixels evenly across the image for instant evaluation
    let step = (total_pixels / 16384).max(1);
    let mut color_hist = [0u16; 4096]; // 12-bit quantized RGB (4-bit R, 4-bit G, 4-bit B)
    let mut lum_hist = [0u32; 256];
    let mut sampled_count: u32 = 0;
    let mut max_color_count: u16 = 0;
    let mut unique_colors: u32 = 0;

    let mut i = 0;
    while i < total_pixels {
        let base = i * 4;
        if base + 3 < pixels.len() {
            let (r, g, b) = if is_bgra {
                (pixels[base + 2] as u32, pixels[base + 1] as u32, pixels[base] as u32)
            } else {
                (pixels[base] as u32, pixels[base + 1] as u32, pixels[base + 2] as u32)
            };

            // Quantize to 12-bit (0..4095)
            let bin = (((r >> 4) << 8) | ((g >> 4) << 4) | (b >> 4)) as usize;
            if color_hist[bin] == 0 {
                unique_colors += 1;
            }
            color_hist[bin] = color_hist[bin].saturating_add(1);
            if color_hist[bin] > max_color_count {
                max_color_count = color_hist[bin];
            }

            let y = ((77 * r + 150 * g + 29 * b) >> 8) as usize;
            lum_hist[y.min(255)] += 1;
            sampled_count += 1;
        }
        i += step;
    }

    if sampled_count == 0 {
        return true;
    }

    let dominant_ratio = max_color_count as f32 / sampled_count as f32;

    // Calculate normalized Shannon Entropy (0.0% .. 100.0%)
    let n = sampled_count as f32;
    let mut entropy = 0.0f32;
    for &count in lum_hist.iter() {
        if count > 0 {
            let p = count as f32 / n;
            entropy -= p * p.log2();
        }
    }
    let entropy_percent = ((entropy / 8.0) * 100.0).clamp(0.0, 100.0);

    // 1. Pure solid color / blank page: dominant color >= 98% OR entropy < 1.0%
    if dominant_ratio >= 0.98 || entropy_percent < 1.0 {
        return true;
    }

    // 2. Plain background + text disclaimer / credit page:
    //    Dominant background >= 88%, very low color diversity (<= 40 bins in 12-bit space),
    //    and low entropy (<= 22.0%).
    if dominant_ratio >= 0.88 && unique_colors <= 40 && entropy_percent <= 22.0 {
        return true;
    }

    false
}
