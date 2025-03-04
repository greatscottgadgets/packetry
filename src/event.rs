//! Types for representing non-packet events.
#![allow(dead_code)]

use crate::usb::Speed;

/// Types of events that may occur.
#[derive(Clone, Debug)]
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
#[derive(Clone, Debug)]
pub enum StopReason {
    /// Requested by user.
    Requested,
    /// Capture buffer full.
    BufferFull,
    /// An error occured.
    Error,
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
