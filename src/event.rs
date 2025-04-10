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
    /// Suspend state changed.
    SuspendChange(bool),
    /// Capture stopped for the specified reason.
    CaptureStop(StopReason),
    /// Bus reset was detected.
    BusReset,
    /// Device HS chirp was validated.
    DeviceChirpValid,
    /// Host HS chirp was validated.
    HostChirpValid,
    /// Line state change.
    LineStateChange(LineState),
    /// Device attached at Full Speed.
    FsAttach,
    /// Device attached at Low Speed.
    LsAttach,
}

/// USB line states
#[derive(Clone, Debug, PartialEq)]
pub enum LineState {
    /// Single-ended 0: both D+ and D- at 0V.
    SE0,
    /// Chirp differential 1, i.e. J-state at ~0.6-0.8V.
    ChirpJ,
    /// Chirp differential 0, i.e. K-state at ~0.6-0.8V.
    ChirpK,
    /// Chirp single-ended 1, i.e. both D+ and D- at ~0.6-0.8V.
    ChirpSE1,
    /// Differential 1 at 3.3V, i.e. FS J or LS K state.
    DR1,
    /// Differential 0 at 3.3V, i.e. FS K or LS J state.
    DR0,
    /// Single-ended 1 at 3.3V.
    SE1,
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

use EventType::*;
use StopReason::*;
use Speed::*;
use LineState::*;

impl std::fmt::Display for EventType {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
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
            VbusChange(false) => "VBUS voltage became invalid",
            VbusChange(true) => "VBUS voltage became valid",
            SuspendChange(false) => "USB suspend ended",
            SuspendChange(true) => "USB suspend started",
            BusReset => "Bus reset detected",
            DeviceChirpValid => "Device HS chirp validated",
            HostChirpValid => "Host HS chirp validated",
            LineStateChange(SE0)  => "SE0 line state detected",
            LineStateChange(ChirpJ) => "Chirp J state detected",
            LineStateChange(ChirpK) => "Chirp K state detected",
            LineStateChange(ChirpSE1) => "Chirp SE1 state detected",
            LineStateChange(DR1) => "FS J or LS K state detected",
            LineStateChange(DR0) => "FS K or LS J state detected",
            LineStateChange(SE1) => "SE1 line state detected",
            FsAttach => "Device attached at Full Speed",
            LsAttach => "Device attached at Low Speed",
        })
    }
}

impl EventType {
    pub fn code(&self) -> u8 {
        match self {
            CaptureStop(Requested)    => 1,
            CaptureStop(BufferFull)   => 2,
            CaptureStop(Error)        => 3,
            CaptureStart(High)        => 4,
            CaptureStart(Full)        => 5,
            CaptureStart(Low)         => 6,
            CaptureStart(Auto)        => 7,
            SpeedChange(High)         => 8,
            SpeedChange(Full)         => 9,
            SpeedChange(Low)          => 10,
            SpeedChange(Auto)         => 11,
            VbusChange(false)         => 12,
            VbusChange(true)          => 13,
            SuspendChange(false)      => 14,
            SuspendChange(true)       => 15,
            BusReset                  => 16,
            DeviceChirpValid          => 17,
            HostChirpValid            => 18,
            LineStateChange(SE0)      => 19,
            LineStateChange(ChirpJ)   => 20,
            LineStateChange(ChirpK)   => 21,
            LineStateChange(ChirpSE1) => 22,
            LineStateChange(DR1)      => 23,
            LineStateChange(DR0)      => 24,
            LineStateChange(SE1)      => 25,
            FsAttach                  => 26,
            LsAttach                  => 27,
        }
    }

    pub fn from_code(code: u8) -> Option<Self> {
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
            17 => DeviceChirpValid,
            18 => HostChirpValid,
            19 => LineStateChange(SE0),
            20 => LineStateChange(ChirpJ),
            21 => LineStateChange(ChirpK),
            22 => LineStateChange(ChirpSE1),
            23 => LineStateChange(DR1),
            24 => LineStateChange(DR0),
            25 => LineStateChange(SE1),
            26 => FsAttach,
            27 => LsAttach,
            _ => return None
        })
    }
}
