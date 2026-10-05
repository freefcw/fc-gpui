#![doc = include_str!("../README.md")]
#![cfg(any(target_os = "linux", target_os = "freebsd"))]
#![allow(unused_mut)] // False positives in platform-specific state handling

mod linux;

pub use linux::{current_platform, linux_platform};
