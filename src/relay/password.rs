// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

use argon2::Algorithm;
use argon2::Argon2;
use argon2::Params;
use argon2::Version;
use blake2::Blake2bMac;
use blake2::digest::KeyInit;
use blake2::digest::Mac;
use blake2::digest::consts::U16;

// The whole chain is frozen for compatibility with the stock client. None of
// these numbers may be tuned, however weak they look: changing any of them
// makes every existing client fail the password check.
const SALT_KEY: [u8; 16] = [
    0xEB, 0x52, 0x1D, 0x14, 0x87, 0xA8, 0xB8, 0x61, 0x07, 0xF0, 0x30, 0x6D, 0x08, 0x22, 0x9E, 0x20,
];
// 32768 bytes, expressed in the 1 KiB blocks Argon2 counts in.
const M_COST_KIB: u32 = 32;
const T_COST: u32 = 2;
const P_COST: u32 = 1;
const OUTPUT_LEN: usize = 32;

// Returns uppercase hex, or the empty string for an empty input. That
// short-circuit is what makes a server with no password accept the empty
// password every direct-IP client sends.
pub fn hash(input: &str, salt: &[u8]) -> String {
    if input.is_empty() {
        return String::new();
    }

    let mut mac = Blake2bMac::<U16>::new_from_slice(&SALT_KEY)
        .expect("16 bytes is a valid BLAKE2b key length");
    mac.update(salt);
    let derived = mac.finalize().into_bytes();

    let params = Params::new(M_COST_KIB, T_COST, P_COST, Some(OUTPUT_LEN))
        .expect("the frozen Argon2 parameters are in range");
    let argon = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);

    let mut out = [0u8; OUTPUT_LEN];
    argon
        .hash_password_into(input.as_bytes(), &derived, &mut out)
        .expect("output length matches the configured parameters");

    hex::encode_upper(out)
}
