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

// util:
const THRESHOLD: f32 = 0.0001;
pub fn is_silent(buf: &[f32]) -> bool {
    let energy: f32 = buf.iter().map(|s| s * s).sum();

    energy < THRESHOLD
}
