// SPDX-License-Identifier: MIT

use pgrx::pg_sys;
use std::{ffi, fs, path};

mod cursors;
mod dml;
mod errors;
mod explain;
mod prepared;
mod transactions;
mod triggers;
mod utility;

fn clear_trace(filename: &str) {
    let data = unsafe { ffi::CStr::from_ptr(pg_sys::DataDir) };

    // ignore missing
    let _ = fs::remove_file(
        path::Path::new(&data.to_string_lossy().into_owned()).join(format!("{filename}.tsv")),
    );
}

fn read_trace(filename: &str) -> Vec<(&'static str, &'static str)> {
    let data = unsafe { ffi::CStr::from_ptr(pg_sys::DataDir) };
    let filepath =
        path::Path::new(&data.to_string_lossy().into_owned()).join(format!("{filename}.tsv"));

    let Ok(content) = fs::read_to_string(filepath) else {
        return vec![];
    };
    let content: &'static str = Box::leak(content.into_boxed_str());

    content
        .lines()
        .filter_map(|line| {
            let parts: Vec<&str> = line.split('\t').collect();
            if parts.len() >= 4 {
                Some((parts[2], parts[3]))
            } else {
                None
            }
        })
        .collect()
}

fn setup_test(filename: &str) -> postgres::Client {
    const FNV: u64 = 0x811C9DC5;
    const KEY: i64 = FNV as i64;
    pgrx::Spi::get_one::<()>(format!("SELECT pg_advisory_xact_lock({KEY})").as_str())
        .expect("failed to acquire pg_advisory_xact_lock");

    // clear any leftovers
    clear_trace(filename);

    let conn_str: String = pgrx::Spi::get_one_with_args(
        "SELECT format('host=%L port=%L user=%L dbname=%L options=%L', \
                split_part(current_setting('unix_socket_directories'), ',', 1), \
                current_setting('port'), current_user, current_database(), \
                format('-c sequencing.trace_file=%s.tsv', $1))",
        &[filename.into()],
    )
    .expect("failed to generate connection string")
    .expect("some connection string");

    postgres::Client::connect(&conn_str, postgres::NoTls).unwrap()
}
