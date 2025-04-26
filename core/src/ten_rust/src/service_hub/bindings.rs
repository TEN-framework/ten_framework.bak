//
// Copyright © 2025 Agora
// This file is part of TEN Framework, an open source project.
// Licensed under the Apache License, Version 2.0, with certain conditions.
// Refer to the "LICENSE" file in the root directory for more information.
//
#![allow(non_camel_case_types)]

#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct ten_app_t {
    _unused: [u8; 0],
}

extern "C" {
    #[allow(dead_code)]
    pub fn ten_get_runtime_version() -> *const ::std::os::raw::c_char;

    #[allow(dead_code)]
    pub fn ten_get_global_log_path(
        app: *mut ten_app_t,
    ) -> *const ::std::os::raw::c_char;
}
