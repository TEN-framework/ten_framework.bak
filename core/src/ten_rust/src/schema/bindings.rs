//
// Copyright © 2025 Agora
// This file is part of TEN Framework, an open source project.
// Licensed under the Apache License, Version 2.0, with certain conditions.
// Refer to the "LICENSE" file in the root directory for more information.
//
#![allow(non_camel_case_types)]

#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct ten_schema_t {
    _unused: [u8; 0],
}

extern "C" {
    pub fn ten_schema_create_from_json_str_proxy(
        json_str: *const ::std::os::raw::c_char,
        err_msg: *mut *const ::std::os::raw::c_char,
    ) -> *mut ten_schema_t;

    pub fn ten_schema_destroy_proxy(self_: *const ten_schema_t);

    pub fn ten_schema_adjust_and_validate_json_str_proxy(
        self_: *const ten_schema_t,
        json_str: *const ::std::os::raw::c_char,
        err_msg: *mut *const ::std::os::raw::c_char,
    ) -> bool;

    pub fn ten_schema_is_compatible_proxy(
        self_: *const ten_schema_t,
        target: *const ten_schema_t,
        err_msg: *mut *const ::std::os::raw::c_char,
    ) -> bool;
}
