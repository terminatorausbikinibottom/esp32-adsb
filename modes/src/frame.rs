/// Frame payload: 56-bit (short) or 112-bit (long) Mode S message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameData {
    Short([u8; 7]),
    Long([u8; 14]),
}

/// What a timestamp's ticks count; lets MLAT-grade stamps pass through unchanged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClockKind {
    /// Beast/dump1090 12 MHz counter (48-bit).
    Counter12MHz,
    /// Sample index of an SDR stream at the given rate.
    SampleIndex { rate_hz: u32 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Timestamp {
    pub ticks: u64,
    pub clock: ClockKind,
}

/// RSSI-like value; its scale depends on the source.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SignalLevel(pub u8);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RawFrame {
    pub data: FrameData,
    pub timestamp: Option<Timestamp>,
    pub signal: Option<SignalLevel>,
}
