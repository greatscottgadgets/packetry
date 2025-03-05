//! Types for representing non-packet events.
#![allow(dead_code)]

use crate::usb::Speed;

/// Types of events that may occur.
#[derive(Clone, Debug, PartialEq)]
pub enum EventType {
    /// Capture started with the specified speed.
    CaptureStart(Speed),
    /// Capture speed was changed.
    SpeedChange(Speed),
    /// VBUS presence changed.
    VbusChange(bool),
    /// Bus reset was detected.
    BusReset,
    /// Device HS chirp was detected.
    DeviceChirp,
    /// Host HS chirp was detected.
    HostChirp,
    /// Suspend state changed.
    SuspendChange(bool),
    /// Capture stopped for the specified reason.
    CaptureStop(StopReason),
}

/// A reason for stopping the capture.
#[derive(Clone, Debug, PartialEq)]
pub enum StopReason {
    /// Requested by user.
    Requested,
    /// Capture buffer full.
    BufferFull,
    /// An error occured.
    Error,
}

impl std::fmt::Display for EventType {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        use EventType::*;
        use StopReason::*;
        use Speed::*;
        f.write_str(match self {
            CaptureStop(Requested) => "Capture stopped by request",
            CaptureStop(BufferFull) => "Capture stopped due to full buffer",
            CaptureStop(Error) => "Capture stopped due to an error",
            CaptureStart(High) => "Capture started at High Speed (480 Mbps)",
            CaptureStart(Full) => "Capture started at Full Speed (12 Mbps)",
            CaptureStart(Low) => "Capture started at Low Speed (1.5 Mbps)",
            CaptureStart(Auto) => "Capture started with automatic speed selection",
            SpeedChange(High) => "Speed changed to High Speed (480 Mbps)",
            SpeedChange(Full) => "Speed changed to Full Speed (12 Mbps)",
            SpeedChange(Low) => "Speed changed to Low Speed (1.5 Mbps)",
            SpeedChange(Auto) => "Speed changed to automatic selection",
            VbusChange(false) => "VBUS voltage became valid",
            VbusChange(true) => "VBUS voltage became invalid",
            SuspendChange(false) => "USB suspend ended",
            SuspendChange(true) => "USB suspend started",
            BusReset => "Bus reset detected",
            DeviceChirp => "Device HS chirp detected",
            HostChirp => "Host HS chirp detected",
        })
    }
}

impl EventType {
    pub fn code(&self) -> u8 {
        use EventType::*;
        use StopReason::*;
        use Speed::*;
        match self {
            CaptureStop(Requested)  => 1,
            CaptureStop(BufferFull) => 2,
            CaptureStop(Error)      => 3,
            CaptureStart(High)      => 4,
            CaptureStart(Full)      => 5,
            CaptureStart(Low)       => 6,
            CaptureStart(Auto)      => 7,
            SpeedChange(High)       => 8,
            SpeedChange(Full)       => 9,
            SpeedChange(Low)        => 10,
            SpeedChange(Auto)       => 11,
            VbusChange(false)       => 12,
            VbusChange(true)        => 13,
            SuspendChange(false)    => 14,
            SuspendChange(true)     => 15,
            BusReset                => 16,
            DeviceChirp             => 17,
            HostChirp               => 18,
        }
    }

    pub fn from_code(code: u8) -> Option<Self> {
        use EventType::*;
        use StopReason::*;
        use Speed::*;
        Some(match code {
            1  => CaptureStop(Requested),
            2  => CaptureStop(BufferFull),
            3  => CaptureStop(Error),
            4  => CaptureStart(High),
            5  => CaptureStart(Full),
            6  => CaptureStart(Low),
            7  => CaptureStart(Auto),
            8  => SpeedChange(High),
            9  => SpeedChange(Full),
            10 => SpeedChange(Low),
            11 => SpeedChange(Auto),
            12 => VbusChange(false),
            13 => VbusChange(true),
            14 => SuspendChange(false),
            15 => SuspendChange(true),
            16 => BusReset,
            17 => DeviceChirp,
            18 => HostChirp,
            _ => return None
        })
    }
}
