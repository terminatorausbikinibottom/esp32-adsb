//! Application logic on top of `modes`.
//!
//! Will hold: tracker / aircraft table with ageing, CPR pairing state, known-ICAO filter, geo
//! maths, dead reckoning, closest point of approach, vicinity and overhead selection,
//! `Snapshot`, and the metadata cache. Time is passed in as `now_ms: u64`.
#![cfg_attr(not(test), no_std)]

pub mod geo;
pub mod kinematics;

#[cfg(test)]
mod tests {
    #[test]
    fn placeholder() {
        assert_eq!(2 + 2, 4);
    }
}
