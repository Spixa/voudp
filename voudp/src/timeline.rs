use std::collections::HashMap;

#[derive(Clone, Copy, Default)]
pub struct ActivityBits {
    bits: u128,
}

impl ActivityBits {
    pub fn tick(&mut self) {
        self.bits <<= 1;
    }

    pub fn set_active(&mut self) {
        self.bits |= 1;
    }

    pub fn get(&self) -> u128 {
        self.bits
    }
}

#[derive(Default)]
pub struct ChannelTimeline {
    activity: HashMap<String, ActivityBits>,
}

impl ChannelTimeline {
    pub fn ensure_user(&mut self, user: &str) {
        self.activity.entry(user.to_string()).or_default();
    }

    pub fn tick(&mut self) {
        for bits in self.activity.values_mut() {
            bits.tick();
        }
    }

    pub fn mark_active(&mut self, user: &str) {
        self.activity
            .entry(user.to_string())
            .or_default()
            .set_active();
    }

    pub fn get(&self, user: &str) -> Option<u128> {
        self.activity.get(user).map(|a| a.get())
    }
}
