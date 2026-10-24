//! Runtime identity of the host operating system and application target.

/// The host operating system and the kind of application delivering the UI.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PlatformInfo {
    /// The operating system whose input conventions the application follows.
    pub os: OperatingSystem,
    /// How the application is delivered.
    pub target: PlatformTarget,
}

/// The host operating system.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum OperatingSystem {
    /// macOS.
    Mac,
    /// Windows.
    Windows,
    /// Linux.
    Linux,
    /// FreeBSD.
    FreeBsd,
    /// iOS or iPadOS.
    Ios,
    /// Android.
    Android,
    /// The backend could not identify the host operating system.
    Unknown,
}

/// The kind of application delivering the UI.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PlatformTarget {
    /// A native application, including headless execution.
    Native,
    /// An application running in a browser.
    Web,
}

impl PlatformInfo {
    /// Returns whether the host follows macOS input conventions.
    pub fn is_mac(self) -> bool {
        self.os == OperatingSystem::Mac
    }
}
