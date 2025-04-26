//
// Copyright © 2025 Agora
// This file is part of TEN Framework, an open source project.
// Licensed under the Apache License, Version 2.0, with certain conditions.
// Refer to the "LICENSE" file in the root directory for more information.
//
use std::ffi::CStr;
#[cfg(test)]
use std::os::raw::c_char;

use actix_web::{web, HttpResponse};

// When running unit tests, use the mock implementation.
#[cfg(test)]
#[no_mangle]
pub extern "C" fn ten_get_runtime_version() -> *const c_char {
    "1.0.0".as_ptr() as *const c_char
}

// In normal builds, use the implementation from bindings.
#[cfg(not(test))]
use super::bindings::ten_get_runtime_version;

pub fn configure_api_route(cfg: &mut web::ServiceConfig) {
    cfg.service(web::scope("/api/v1").service(
        web::resource("/version").route(web::get().to(|| async {
            let version = unsafe {
                let c_str = CStr::from_ptr(ten_get_runtime_version());
                c_str.to_string_lossy().into_owned()
            };

            HttpResponse::Ok().json(web::Json(serde_json::json!({
                "version": version
            })))
        })),
    ));
}
