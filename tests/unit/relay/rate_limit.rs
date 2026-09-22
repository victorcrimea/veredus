// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

use chrono::TimeDelta;
use chrono::TimeZone;

use super::*;

fn t0() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap()
}

fn takes(bucket: &mut TokenBucket, now: DateTime<Utc>, k: u32, n: usize) -> Vec<Verdict> {
    (0..n).map(|_| bucket.take(Some(now), k)).collect()
}

#[test]
fn bucket_passes_its_burst_then_drops() {
    let mut bucket = TokenBucket::new(1, 3);
    let got = takes(&mut bucket, t0(), 0, 4);
    assert_eq!(
        got,
        [Verdict::Pass, Verdict::Pass, Verdict::Pass, Verdict::Drop]
    );
}

#[test]
fn bucket_refills_at_its_rate() {
    let mut bucket = TokenBucket::new(2, 2);
    takes(&mut bucket, t0(), 0, 2);
    assert_eq!(bucket.take(Some(t0()), 0), Verdict::Drop);
    // Half a second at two a second is exactly one message.
    let later = t0() + TimeDelta::milliseconds(500);
    assert_eq!(bucket.take(Some(later), 0), Verdict::Pass);
    assert_eq!(bucket.take(Some(later), 0), Verdict::Drop);
}

#[test]
fn bucket_refill_is_capped_at_the_burst() {
    let mut bucket = TokenBucket::new(10, 2);
    takes(&mut bucket, t0(), 0, 2);
    let later = t0() + TimeDelta::hours(1);
    let got = takes(&mut bucket, later, 0, 3);
    assert_eq!(got, [Verdict::Pass, Verdict::Pass, Verdict::Drop]);
}

#[test]
fn bucket_starts_full_before_the_first_tick() {
    let mut bucket = TokenBucket::new(1, 2);
    assert_eq!(bucket.take(None, 0), Verdict::Pass);
    assert_eq!(bucket.take(None, 0), Verdict::Pass);
    assert_eq!(bucket.take(None, 0), Verdict::Drop);
}

#[test]
fn bucket_clock_stepping_back_refills_nothing_and_reanchors() {
    let mut bucket = TokenBucket::new(1, 1);
    assert_eq!(bucket.take(Some(t0()), 0), Verdict::Pass);
    let earlier = t0() - TimeDelta::seconds(30);
    assert_eq!(bucket.take(Some(earlier), 0), Verdict::Drop);
    // Measured from the re-anchored time, not the original one.
    let after = earlier + TimeDelta::seconds(1);
    assert_eq!(bucket.take(Some(after), 0), Verdict::Pass);
}

#[test]
fn bucket_with_rate_zero_never_limits() {
    let mut bucket = TokenBucket::new(0, 0);
    let got = takes(&mut bucket, t0(), 1, 1000);
    assert!(got.iter().all(|v| *v == Verdict::Pass));
}

#[test]
fn bucket_kicks_at_the_multiple() {
    // Burst 2 at multiple 3 leaves room for 4 drops before the kick.
    let mut bucket = TokenBucket::new(1, 2);
    let got = takes(&mut bucket, t0(), 3, 7);
    assert_eq!(
        got,
        [
            Verdict::Pass,
            Verdict::Pass,
            Verdict::Drop,
            Verdict::Drop,
            Verdict::Drop,
            Verdict::Kick,
            Verdict::Kick,
        ]
    );
}

#[test]
fn bucket_kick_multiple_one_kicks_on_first_excess() {
    let mut bucket = TokenBucket::new(1, 1);
    let got = takes(&mut bucket, t0(), 1, 2);
    assert_eq!(got, [Verdict::Pass, Verdict::Kick]);
}

#[test]
fn bucket_without_kick_builds_no_debt() {
    let mut bucket = TokenBucket::new(1, 1);
    takes(&mut bucket, t0(), 0, 100);
    let later = t0() + TimeDelta::seconds(1);
    assert_eq!(bucket.take(Some(later), 0), Verdict::Pass);
}

#[test]
fn bucket_debt_is_repaid_before_passing_again() {
    let mut bucket = TokenBucket::new(1, 1);
    // One pass, then two drops leave it two messages in debt.
    let got = takes(&mut bucket, t0(), 10, 3);
    assert_eq!(got, [Verdict::Pass, Verdict::Drop, Verdict::Drop]);
    let two_later = t0() + TimeDelta::seconds(2);
    assert_eq!(bucket.take(Some(two_later), 10), Verdict::Drop);
    let four_later = t0() + TimeDelta::seconds(5);
    assert_eq!(bucket.take(Some(four_later), 10), Verdict::Pass);
}

#[test]
fn quota_caps_commands_per_turn() {
    let mut quota = TurnQuota::default();
    let got: Vec<Verdict> = (0..3).map(|_| quota.charge(5, 10, 3, 2, 0, 0)).collect();
    assert_eq!(got, [Verdict::Pass, Verdict::Pass, Verdict::Drop]);
    // Another turn has a quota of its own.
    assert_eq!(quota.charge(6, 10, 3, 2, 0, 0), Verdict::Pass);
}

#[test]
fn quota_caps_bytes_per_turn() {
    let mut quota = TurnQuota::default();
    assert_eq!(quota.charge(5, 60, 3, 0, 100, 0), Verdict::Pass);
    assert_eq!(quota.charge(5, 40, 3, 0, 100, 0), Verdict::Pass);
    assert_eq!(quota.charge(5, 1, 3, 0, 100, 0), Verdict::Drop);
}

#[test]
fn quota_forgets_released_turns() {
    let mut quota = TurnQuota::default();
    quota.charge(5, 0, 3, 1, 0, 0);
    assert_eq!(quota.charge(5, 0, 3, 1, 0, 0), Verdict::Drop);
    // Once turn 5 is released its entry goes, and the table stays small.
    quota.charge(9, 0, 5, 1, 0, 0);
    assert!(quota.open.iter().all(|u| u.turn > 5));
    assert_eq!(quota.open.len(), 1);
}

#[test]
fn quota_with_zero_caps_never_limits() {
    let mut quota = TurnQuota::default();
    let got: Vec<Verdict> = (0..500)
        .map(|_| quota.charge(5, 65535, 3, 0, 0, 1))
        .collect();
    assert!(got.iter().all(|v| *v == Verdict::Pass));
}

#[test]
fn quota_kicks_past_the_multiple_of_count() {
    let mut quota = TurnQuota::default();
    let got: Vec<Verdict> = (0..7).map(|_| quota.charge(5, 0, 3, 2, 0, 3)).collect();
    assert_eq!(
        got,
        [
            Verdict::Pass,
            Verdict::Pass,
            Verdict::Drop,
            Verdict::Drop,
            Verdict::Drop,
            Verdict::Drop,
            Verdict::Kick,
        ]
    );
}

#[test]
fn quota_kicks_past_the_multiple_of_bytes() {
    let mut quota = TurnQuota::default();
    assert_eq!(quota.charge(5, 100, 3, 0, 100, 2), Verdict::Pass);
    assert_eq!(quota.charge(5, 100, 3, 0, 100, 2), Verdict::Drop);
    assert_eq!(quota.charge(5, 1, 3, 0, 100, 2), Verdict::Kick);
}

#[test]
fn limits_follow_the_config() {
    let config = Config {
        chat_per_sec: 1,
        chat_burst: 1,
        flare_per_sec: 0,
        ..Config::default()
    };
    let mut limits = Limits::new(&config);
    assert_eq!(limits.chat.take(Some(t0()), 0), Verdict::Pass);
    assert_eq!(limits.chat.take(Some(t0()), 0), Verdict::Drop);
    assert_eq!(limits.flare.take(Some(t0()), 0), Verdict::Pass);
    assert_eq!(limits.flare.take(Some(t0()), 0), Verdict::Pass);
}
