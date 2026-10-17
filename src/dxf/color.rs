//! AutoCAD Color Index (ACI) → RGB.

/// RGB (0..=255) of ACI colour `index` (1..=255) using AutoCAD's standard
/// palette. Index 7 (white on a black screen, black on paper) is returned as
/// black because the output is for print. 0 (BYBLOCK) and 256 (BYLAYER) are
/// not colours; they map to black too.
pub fn aci_rgb(index: i64) -> [u8; 3] {
    let i = index.unsigned_abs();
    match i {
        1 => [255, 0, 0],
        2 => [255, 255, 0],
        3 => [0, 255, 0],
        4 => [0, 255, 255],
        5 => [0, 0, 255],
        6 => [255, 0, 255],
        7 => [0, 0, 0],
        8 => [65, 65, 65],
        9 => [128, 128, 128],
        10..=249 => {
            let hue = (i - 10) / 10; // 0..24, 15° steps
            let v = (i % 10) as usize;
            // Full-saturation colour at this hue (HSV with S = V = 1).
            let h = hue as f64 * 15.0;
            let f = |n: f64| {
                let k = (n + h / 60.0) % 6.0;
                let x = 1.0 - (k.min(4.0 - k).clamp(0.0, 1.0));
                (255.0 * x).floor()
            };
            let base = [f(5.0), f(3.0), f(1.0)];
            // Odd indices are pastel (one third saturation towards white).
            let tint: [f64; 3] = if v % 2 == 1 { base.map(|c| 255.0 - ((255.0 - c) / 3.0).floor()) } else { base };
            const LEVELS: [f64; 5] = [255.0, 189.0, 129.0, 104.0, 79.0];
            let l = LEVELS[v / 2] / 255.0;
            tint.map(|c| (c * l).round().clamp(0.0, 255.0) as u8)
        }
        250 => [51, 51, 51],
        251 => [80, 80, 80],
        252 => [105, 105, 105],
        253 => [130, 130, 130],
        254 => [190, 190, 190],
        255 => [255, 255, 255],
        _ => [0, 0, 0],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standard_entries() {
        assert_eq!(aci_rgb(1), [255, 0, 0]);
        assert_eq!(aci_rgb(7), [0, 0, 0]);
        assert_eq!(aci_rgb(10), [255, 0, 0]);
        assert_eq!(aci_rgb(11), [255, 170, 170]);
        assert_eq!(aci_rgb(12), [189, 0, 0]);
        assert_eq!(aci_rgb(13), [189, 126, 126]);
        assert_eq!(aci_rgb(20), [255, 63, 0]);
        assert_eq!(aci_rgb(30), [255, 127, 0]);
        assert_eq!(aci_rgb(40), [255, 191, 0]);
        assert_eq!(aci_rgb(50), [255, 255, 0]);
        assert_eq!(aci_rgb(90), [0, 255, 0]);
        assert_eq!(aci_rgb(130), [0, 255, 255]);
        assert_eq!(aci_rgb(170), [0, 0, 255]);
        assert_eq!(aci_rgb(210), [255, 0, 255]);
        assert_eq!(aci_rgb(140), [0, 191, 255]);
        assert_eq!(aci_rgb(254), [190, 190, 190]);
        for i in 10..250 {
            let c = aci_rgb(i);
            assert!(c.iter().any(|&v| v > 0), "{i}");
        }
    }
}
