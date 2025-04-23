//
// Copyright © 2025 Agora
// This file is part of TEN Framework, an open source project.
// Licensed under the Apache License, Version 2.0, with certain conditions.
// Refer to the "LICENSE" file in the root directory for more information.
//
use actix_web::{web, HttpResponse};
use prometheus::{Encoder, Registry, TextEncoder};

use crate::constants::METRICS;

pub fn configure_telemetry_route(
    cfg: &mut web::ServiceConfig,
    registry: Registry,
) {
    cfg.route(
        METRICS,
        web::get().to(move || {
            let registry_for_request = registry.clone();

            async move {
                let metric_families = registry_for_request.gather();
                let encoder = TextEncoder::new();
                let mut buffer = Vec::new();

                if encoder.encode(&metric_families, &mut buffer).is_err() {
                    return HttpResponse::InternalServerError().finish();
                }

                let response = match String::from_utf8(buffer) {
                    Ok(v) => v,
                    Err(_) => {
                        return HttpResponse::InternalServerError().finish()
                    }
                };

                HttpResponse::Ok().body(response)
            }
        }),
    );
}
