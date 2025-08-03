const SILENCE_THRESHOLD: f32 = 0.001; // silence threshold

pub fn normalize(buf: &mut [f32]) {
    let max = buf.iter().fold(0.0, |max, &s| f32::max(max, s.abs()));

    if max > 1.0 {
        let factor = 1.0 / max;
        for sample in buf {
            *sample *= factor;
        }
    }
}

pub fn soft_clip(buf: &mut [f32]) {
    for sample in buf {
        *sample = sample.tanh(); // thanks deepseek. the range of tanh is -1 to +1. this will do the soft clipping for us
    }
}

pub fn remove_dc_bias(buf: &mut [f32], prev: &mut (f32, f32)) {
    const ALPHA: f32 = 0.995; // DC removal coefficient

    for i in (0..buf.len()).step_by(2) {
        let left = buf[i];
        let right = buf[i + 1];

        // Apply DC removal filter (high-pass)
        let new_left = left - prev.0 + ALPHA * prev.0;
        let new_right = right - prev.1 + ALPHA * prev.1;

        prev.0 = new_left;
        prev.1 = new_right;

        buf[i] = new_left;
        buf[i + 1] = new_right;
    }
}

// pub fn remove_dc_bias(buf: &mut [f32], prev: &mut (f32, f32)) {
//     const ALPHA: f32 = 0.999; // Strong DC removal

//     for i in (0..buf.len()).step_by(2) {
//         // Left channel
//         let left = buf[i] - prev.0;
//         prev.0 = buf[i];
//         buf[i] = left * ALPHA;

//         // Right channel
//         let right = buf[i+1] - prev.1;
//         prev.1 = buf[i+1];
//         buf[i+1] = right * ALPHA;
//     }
// }

pub fn compress(buf: &mut [f32], threshold: f32, ratio: f32) {
    for sample in buf {
        let abs = sample.abs();
        if abs > threshold {
            let sign = sample.signum();
            let excess = abs - threshold;
            let compressed = threshold + (excess * ratio);
            *sample = sign * compressed;
        }
    }
}

// util:
pub fn is_silent(buf: &[f32]) -> bool {
    // new impl: calculate RMS for better silence detection
    let sum_sq: f32 = buf.iter().map(|s| s * s).sum();
    let rms = (sum_sq / buf.len() as f32).sqrt();

    rms < SILENCE_THRESHOLD
}
