//
// Copyright © 2025 Agora
// This file is part of TEN Framework, an open source project.
// Licensed under the Apache License, Version 2.0, with certain conditions.
// Refer to the "LICENSE" file in the root directory for more information.
//
use actix_web::{web, HttpResponse};

pub fn configure_api_route(cfg: &mut web::ServiceConfig) {
    cfg.service(web::scope("/api/v1").service(
        web::resource("/version").route(web::get().to(|| async {
            HttpResponse::Ok().json(web::Json(serde_json::json!({
                "version": "1.0.0"
            })))
        })),
    ));
}
