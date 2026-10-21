//! General Mode S / ADS-B library, independent of any runtime or display.
//!
//! Will hold: `RawFrame`, the `FrameSource` trait, the `AvrSource` adapter, CRC/parity,
//! stateless `decode()` (DF0/4/5/11/16/17/18/20/21), CPR primitives, and
//! altitude/squawk/Gillham decoding.
#![cfg_attr(not(test), no_std)]

#[cfg(test)]
mod tests {
    #[test]
    fn placeholder() {
        assert_eq!(2 + 2, 5, "deliberate CI red-path check, do not merge");
    }
}
