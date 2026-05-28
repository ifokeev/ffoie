//! Nickname normalization, collision resolution, and team assignment.
//!
//! The server owns canonical nicknames — the client's Connect.nickname is
//! advisory only.  This module enforces uniqueness and sanitizes input so
//! downstream code can treat every nickname as already valid.

use std::collections::HashSet;

use ffoie_protocol::Team;

// ── Normalization ─────────────────────────────────────────────────────────────

#[allow(dead_code)] // consumed by plan 02-03 (ws.rs)
/// Normalize a raw client-supplied nickname.
///
/// Rules (in order):
/// 1. Trim leading / trailing whitespace.
/// 2. If empty after trimming → `"guest"`.
/// 3. Truncate to 20 Unicode scalar values (code points, not bytes).
pub fn normalize_nick(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return "guest".to_string();
    }
    // Take the first 20 Unicode scalar values (chars).
    trimmed.chars().take(20).collect()
}

// ── Collision resolution ──────────────────────────────────────────────────────

#[allow(dead_code)] // consumed by plan 02-03 (ws.rs)
/// Assign a unique nickname given a set of already-taken names.
///
/// Algorithm:
/// 1. Normalize the requested name.
/// 2. If the normalized name is not taken, return it directly.
/// 3. Otherwise, append `#NNNN` (4-digit zero-padded random) and retry up
///    to 10 times.
/// 4. If all 10 attempts collide, return `"guest#NNNN"` with a fresh random
///    suffix as a last-resort fallback.
pub fn assign_nick(requested: &str, taken: &HashSet<String>) -> String {
    let base = normalize_nick(requested);

    if !taken.contains(&base) {
        return base;
    }

    for _ in 0..10 {
        let candidate = format!("{}#{:04}", base, fastrand::u32(0..10000));
        if !taken.contains(&candidate) {
            return candidate;
        }
    }

    // Last-resort fallback.
    format!("guest#{:04}", fastrand::u32(0..10000))
}

// ── Team assignment ───────────────────────────────────────────────────────────

#[allow(dead_code)] // consumed by plan 02-03 (ws.rs)
/// Randomly assign a team — Red or Blue, 50/50.
///
/// Team::None is never returned; that value is reserved for the protocol
/// handshake before a Connect message arrives.
pub fn assign_team() -> Team {
    if fastrand::bool() {
        Team::Red
    } else {
        Team::Blue
    }
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── normalize_nick ────────────────────────────────────────────────────────

    #[test]
    fn empty_string_becomes_guest() {
        assert_eq!(normalize_nick(""), "guest");
    }

    #[test]
    fn whitespace_only_becomes_guest() {
        assert_eq!(normalize_nick("   "), "guest");
        assert_eq!(normalize_nick("\t\n"), "guest");
    }

    #[test]
    fn short_name_unchanged() {
        assert_eq!(normalize_nick("AB"), "AB");
    }

    #[test]
    fn trimmed_name() {
        assert_eq!(normalize_nick("  hello  "), "hello");
    }

    #[test]
    fn overlong_truncated_to_20_code_points() {
        // 25 ASCII chars.
        let long = "abcdefghijklmnopqrstuvwxy";
        let result = normalize_nick(long);
        assert_eq!(result.chars().count(), 20);
        assert_eq!(result, "abcdefghijklmnopqrst");
    }

    #[test]
    fn truncation_counts_unicode_scalars_not_bytes() {
        // Each '€' is 3 bytes but 1 code point.
        let long: String = std::iter::repeat('€').take(25).collect();
        let result = normalize_nick(&long);
        assert_eq!(result.chars().count(), 20);
    }

    #[test]
    fn exactly_20_code_points_unchanged() {
        let twenty: String = "a".repeat(20);
        assert_eq!(normalize_nick(&twenty), twenty);
    }

    // ── assign_nick ───────────────────────────────────────────────────────────

    #[test]
    fn no_collision_returns_normalized() {
        let taken = HashSet::new();
        assert_eq!(assign_nick("Ranger", &taken), "Ranger");
    }

    #[test]
    fn collision_appends_suffix() {
        let mut taken = HashSet::new();
        taken.insert("Ranger".to_string());

        let result = assign_nick("Ranger", &taken);
        // Should be "Ranger#NNNN" where NNNN is 4 digits.
        assert!(
            result.starts_with("Ranger#"),
            "expected suffix, got: {result}"
        );
        let suffix = &result["Ranger#".len()..];
        assert_eq!(suffix.len(), 4, "suffix should be 4 digits");
        assert!(suffix.chars().all(|c| c.is_ascii_digit()));
    }

    #[test]
    fn all_collisions_falls_back_to_guest_suffix() {
        // Pre-fill all 10000 Ranger#NNNN combinations.
        let mut taken = HashSet::new();
        taken.insert("Ranger".to_string());
        for i in 0..10000u32 {
            taken.insert(format!("Ranger#{i:04}"));
        }

        let result = assign_nick("Ranger", &taken);
        assert!(
            result.starts_with("guest#"),
            "should fall back to guest#NNNN, got: {result}"
        );
        let suffix = &result["guest#".len()..];
        assert_eq!(suffix.len(), 4);
        assert!(suffix.chars().all(|c| c.is_ascii_digit()));
    }

    #[test]
    fn empty_request_normalizes_then_checks_collision() {
        let taken = HashSet::new();
        // Empty → normalized to "guest", not taken → returned as "guest".
        assert_eq!(assign_nick("", &taken), "guest");
    }

    // ── assign_team ───────────────────────────────────────────────────────────

    #[test]
    fn team_is_red_or_blue_never_none() {
        // Run many times to exercise both branches.
        for _ in 0..200 {
            let t = assign_team();
            assert!(
                matches!(t, Team::Red | Team::Blue),
                "assign_team returned Team::None"
            );
        }
    }

    #[test]
    fn team_returns_both_values_over_many_calls() {
        // With 200 calls, the probability of never seeing one branch is
        // (0.5)^200 ≈ 10^-60 — effectively impossible.
        let mut saw_red = false;
        let mut saw_blue = false;
        for _ in 0..200 {
            match assign_team() {
                Team::Red => saw_red = true,
                Team::Blue => saw_blue = true,
                Team::None => panic!("assign_team must not return Team::None"),
            }
        }
        assert!(saw_red, "never saw Team::Red in 200 calls");
        assert!(saw_blue, "never saw Team::Blue in 200 calls");
    }
}
