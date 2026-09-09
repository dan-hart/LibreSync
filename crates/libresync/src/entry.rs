use std::cmp::Ordering;

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Serialize, Deserialize, Eq, PartialEq)]
pub struct LamportClock {
    pub counter: u64,
    pub device_id: String,
}

impl Ord for LamportClock {
    fn cmp(&self, other: &Self) -> Ordering {
        match self.counter.cmp(&other.counter) {
            Ordering::Equal => self.device_id.cmp(&other.device_id),
            ordering => ordering,
        }
    }
}

impl PartialOrd for LamportClock {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, Eq, PartialEq)]
pub struct Entry {
    pub key: String,
    pub value: Vec<u8>,
    pub clock: LamportClock,
}

#[cfg(test)]
mod tests {
    use super::LamportClock;

    #[test]
    fn lamport_clock_orders_by_counter_then_device() {
        let first = LamportClock {
            counter: 1,
            device_id: "a".to_string(),
        };
        let second = LamportClock {
            counter: 2,
            device_id: "a".to_string(),
        };
        let third = LamportClock {
            counter: 1,
            device_id: "b".to_string(),
        };

        assert!(second > first);
        assert!(third > first);
    }
}
