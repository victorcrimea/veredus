// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

use chrono::TimeZone;

use super::*;

const MIN: TimeDelta = TimeDelta::seconds(5);

fn t0() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap()
}

fn at(secs: i64) -> DateTime<Utc> {
    t0() + TimeDelta::seconds(secs)
}

fn budget() -> (PauseBudget, Guid) {
    let mut budget = PauseBudget::new(DEFAULT_BUDGET, MIN);
    budget.check(t0(), 2, &[]);
    (budget, Guid::new())
}

#[test]
fn set_pausing_reports_only_changes() {
    let (mut budget, uuid) = budget();
    assert!(budget.set_pausing(&uuid, true));
    assert!(!budget.set_pausing(&uuid, true));
    assert!(budget.set_pausing(&uuid, false));
    assert!(!budget.set_pausing(&uuid, false));
}

#[test]
fn short_pause_costs_the_minimum() {
    let (mut budget, uuid) = budget();
    budget.set_pausing(&uuid, true);
    budget.check(at(1), 2, &[]);
    budget.set_pausing(&uuid, false);
    assert_eq!(budget.remaining(&uuid), DEFAULT_BUDGET - MIN);
}

#[test]
fn instant_toggle_costs_the_minimum_each_time() {
    let (mut budget, uuid) = budget();
    for _ in 0..3 {
        budget.set_pausing(&uuid, true);
        budget.set_pausing(&uuid, false);
    }
    assert_eq!(budget.remaining(&uuid), DEFAULT_BUDGET - MIN * 3);
}

#[test]
fn long_pause_costs_only_its_length() {
    let (mut budget, uuid) = budget();
    budget.set_pausing(&uuid, true);
    budget.check(at(20), 2, &[]);
    budget.set_pausing(&uuid, false);
    assert_eq!(
        budget.remaining(&uuid),
        DEFAULT_BUDGET - TimeDelta::seconds(20)
    );
}

#[test]
fn leaving_while_paused_costs_the_minimum() {
    let (mut budget, uuid) = budget();
    budget.set_pausing(&uuid, true);
    budget.clear_pausing(&uuid);
    assert_eq!(budget.remaining(&uuid), DEFAULT_BUDGET - MIN);
}

#[test]
fn leaving_while_not_paused_costs_nothing() {
    let (mut budget, uuid) = budget();
    budget.clear_pausing(&uuid);
    assert_eq!(budget.remaining(&uuid), DEFAULT_BUDGET);
}

#[test]
fn minimum_charge_never_goes_below_zero() {
    let mut budget = PauseBudget::new(TimeDelta::seconds(3), MIN);
    budget.check(t0(), 2, &[]);
    let uuid = Guid::new();
    budget.set_pausing(&uuid, true);
    budget.set_pausing(&uuid, false);
    assert_eq!(budget.remaining(&uuid), TimeDelta::zero());
}

#[test]
fn zero_minimum_charges_nothing_extra() {
    let mut budget = PauseBudget::new(DEFAULT_BUDGET, TimeDelta::zero());
    budget.check(t0(), 2, &[]);
    let uuid = Guid::new();
    budget.set_pausing(&uuid, true);
    budget.set_pausing(&uuid, false);
    assert_eq!(budget.remaining(&uuid), DEFAULT_BUDGET);
}

#[test]
fn pause_before_any_tick_is_not_charged_the_minimum() {
    let mut budget = PauseBudget::new(DEFAULT_BUDGET, MIN);
    let uuid = Guid::new();
    budget.set_pausing(&uuid, true);
    budget.set_pausing(&uuid, false);
    assert_eq!(budget.remaining(&uuid), DEFAULT_BUDGET);
}

#[test]
fn expiry_forgets_when_the_pause_began() {
    let mut budget = PauseBudget::new(TimeDelta::seconds(2), MIN);
    budget.check(t0(), 2, &[]);
    let uuid = Guid::new();
    budget.set_pausing(&uuid, true);
    budget.check(at(3), 2, &[]);
    assert!(budget.pausing().next().is_none());
    assert!(budget.paused_at.is_empty());
}
