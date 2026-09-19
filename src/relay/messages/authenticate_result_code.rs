// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

#[repr(C)]
#[derive(Debug, Copy, Clone, PartialEq)]
pub enum AuthenticateResultCode {
    Ok,
    OkSavedGame,
    OkRejoining,
    PasswordInvalid,
}
