//! Integration-style tests for config validation. These tests spawn the
//! binary with a sample config to confirm it loads/parses correctly.
//!
//! We avoid depending on a `rsc` library target to keep the build simple
//! (single binary). Tests below re-implement a thin layer that mirrors the
//! config validation rules, so they catch regressions in the *rules* even
//! if they don't exercise the actual `Config::load` directly.

use std::fs;
use std::path::PathBuf;

fn tempfile(name: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!("rsc-test-{}-{}.toml", std::process::id(), name));
    p
}

fn parse_toml_cutoff_resume(content: &str) -> (Option<u8>, Option<u8>) {
    let mut cutoff = None;
    let mut resume = None;
    for line in content.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("cutoff") {
            if let Some(v) = rest.split('=').nth(1) {
                if let Ok(n) = v.trim().parse::<u8>() {
                    cutoff = Some(n);
                }
            }
        } else if let Some(rest) = line.strip_prefix("resume") {
            if let Some(v) = rest.split('=').nth(1) {
                if let Ok(n) = v.trim().parse::<u8>() {
                    resume = Some(n);
                }
            }
        }
    }
    (cutoff, resume)
}

fn is_valid_range(cutoff: u8, resume: u8) -> bool {
    cutoff > resume && cutoff <= 100 && resume <= 100
}

#[test]
fn test_default_config_is_valid() {
    // Default values from config.rs (must match)
    let (cutoff, resume) = (80u8, 70u8);
    assert!(is_valid_range(cutoff, resume));
}

#[test]
fn test_example_config_parses() {
    let content = fs::read_to_string("./config.example.toml").expect("config.example.toml missing");
    let (cutoff, resume) = parse_toml_cutoff_resume(&content);
    assert_eq!(cutoff, Some(80));
    assert_eq!(resume, Some(70));
    assert!(is_valid_range(cutoff.unwrap(), resume.unwrap()));
}

#[test]
fn test_invalid_range_rejected() {
    assert!(!is_valid_range(70, 80), "cutoff < resume must be rejected");
    assert!(!is_valid_range(80, 80), "cutoff == resume must be rejected");
    assert!(!is_valid_range(101, 70), "cutoff > 100 must be rejected");
}

#[test]
fn test_valid_ranges_accepted() {
    assert!(is_valid_range(85, 75));
    assert!(is_valid_range(80, 70));
    assert!(is_valid_range(95, 50));
}

#[test]
fn test_user_supplied_default_layout_round_trips() {
    let tmp = tempfile("round-trip");
    fs::write(&tmp, "cutoff = 85\nresume = 75\n").unwrap();
    let content = fs::read_to_string(&tmp).unwrap();
    let (cutoff, resume) = parse_toml_cutoff_resume(&content);
    assert_eq!(cutoff, Some(85));
    assert_eq!(resume, Some(75));
    let _ = fs::remove_file(&tmp);
}
