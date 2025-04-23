//
// Copyright © 2025 Agora
// This file is part of TEN Framework, an open source project.
// Licensed under the Apache License, Version 2.0, with certain conditions.
// Refer to the "LICENSE" file in the root directory for more information.
//
mod api;
mod telemetry;

use std::os::raw::c_char;
use std::ptr;
use std::{ffi::CStr, thread};

use actix_web::{web, App, HttpServer};
use anyhow::Result;
use futures::channel::oneshot;
use futures::future::select;
use futures::FutureExt;
use prometheus::Registry;

use crate::constants::{
    ENDPOINT_SERVER_BIND_MAX_RETRIES, ENDPOINT_SERVER_BIND_RETRY_INTERVAL_SECS,
};

pub struct TelemetrySystem {
    registry: Registry,

    actix_thread: Option<thread::JoinHandle<()>>,

    /// Used to send a shutdown signal to the actix system where the server is
    /// located.
    actix_shutdown_tx: Option<oneshot::Sender<()>>,
}

/// Configure API endpoints.
fn configure_routes(cfg: &mut web::ServiceConfig, registry: Registry) {
    let registry_clone = registry;

    // Configure telemetry endpoint.
    telemetry::configure_telemetry_route(cfg, registry_clone.clone());

    // Configure API endpoints.
    api::configure_api_route(cfg);
}

/// Creates an HTTP server with retry mechanism if binding fails.
///
/// This function attempts to bind an HTTP server to the specified endpoint.
/// If binding fails, it will retry up to a configured maximum number of
/// attempts with a delay between each attempt.
fn create_endpoint_server_with_retry(
    endpoint_str: &str,
    registry: Registry,
) -> Option<actix_web::dev::Server> {
    let mut attempts = 0;
    let max_attempts = ENDPOINT_SERVER_BIND_MAX_RETRIES;
    let wait_duration = std::time::Duration::from_secs(
        ENDPOINT_SERVER_BIND_RETRY_INTERVAL_SECS,
    );

    // Try to create and bind the HTTP server, with retries if it fails.
    let server_builder = loop {
        let registry_clone = registry.clone();

        // Create a new HTTP server with the configured routes.
        let result = HttpServer::new(move || {
            App::new()
                .configure(|cfg| configure_routes(cfg, registry_clone.clone()))
        })
        // Make actix not linger on the socket.
        .shutdown_timeout(0)
        .bind(&endpoint_str);

        match result {
            Ok(server) => break server,
            Err(e) => {
                attempts += 1;

                // If we've reached the maximum number of attempts, log the
                // error and return None.
                if attempts >= max_attempts {
                    eprintln!(
                        "Error binding to address: {} after {} attempts: {:?}",
                        endpoint_str, attempts, e
                    );
                    return None;
                }

                // Otherwise, log the error and retry after a delay.
                eprintln!(
                    "Failed to bind to address: {}. Attempt {} of {}. \
                     Retrying in {} second{}...",
                    endpoint_str,
                    attempts,
                    max_attempts,
                    ENDPOINT_SERVER_BIND_RETRY_INTERVAL_SECS,
                    if ENDPOINT_SERVER_BIND_RETRY_INTERVAL_SECS == 1 {
                        ""
                    } else {
                        "s"
                    }
                );
                std::thread::sleep(wait_duration);
            }
        }
    };

    // Start the server and return it.
    Some(server_builder.run())
}

/// Creates and starts a server thread that runs the actix system with the
/// provided server.
///
/// This function encapsulates the logic for running an actix server in a
/// separate thread, handling both normal operation and shutdown requests.
fn create_server_thread(
    server: actix_web::dev::Server,
) -> (thread::JoinHandle<()>, oneshot::Sender<()>) {
    // Get a handle to the server to control it later.
    let server_handle = server.handle();

    // Create a channel to send shutdown signals to the server thread.
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();

    // Spawn a new thread to run the actix system.
    let server_thread_handle = thread::spawn(move || {
        // Create a new actix system.
        let system = actix_rt::System::new();

        // Block on the async executor to run our server and shutdown logic.
        let result: Result<()> = system.block_on(async move {
            // Set up the concurrent execution of server and shutdown tasks.

            // The server task handles normal operation and error reporting.
            let server_future = async {
                match server.await {
                    Ok(_) => {
                        // Server completed normally (unlikely).
                        eprintln!("Endpoint server completed normally");
                    }
                    Err(e) => {
                        // Server encountered an error.
                        eprintln!("Endpoint server error: {e}");
                        // Force the entire process to exit immediately.
                        std::process::exit(-1);
                    }
                }
            }
            .fuse();

            // The shutdown task waits for a signal to gracefully stop the
            // server.
            let shutdown_future = async move {
                // Wait for shutdown signal.
                let _ = shutdown_rx.await;

                eprintln!("Shutting down endpoint server (graceful stop)...");

                // Gracefully stop the server.
                server_handle.stop(true).await;

                // Terminate the actix system after the server is fully down.
                actix_rt::System::current().stop();
            }
            .fuse();

            // Use `futures::select!` to concurrently execute both futures
            // and respond to whichever completes first.
            futures::pin_mut!(server_future, shutdown_future);
            select(server_future, shutdown_future).await;

            eprintln!("Endpoint server shut down.");
            Ok(())
        });

        // Handle any errors from the actix system.
        if let Err(e) = result {
            eprintln!("Fatal error in endpoint server thread: {:?}", e);
            std::process::exit(-1);
        }
    });

    (server_thread_handle, shutdown_tx)
}

/// Initialize the endpoint system.
///
/// # Safety
///
/// This function takes a raw C string pointer. The pointer must be valid and
/// point to a properly null-terminated string. The returned pointer must be
/// freed with `ten_endpoint_system_shutdown` to avoid memory leaks.
#[no_mangle]
pub unsafe extern "C" fn ten_endpoint_system_create(
    endpoint: *const c_char,
) -> *mut TelemetrySystem {
    // Safely convert C string to Rust string.
    let endpoint_str = match CStr::from_ptr(endpoint).to_str() {
        Ok(s) if !s.trim().is_empty() => s.to_string(),
        _ => return ptr::null_mut(),
    };

    // Create a new Prometheus registry.
    //
    // Note: `prometheus::Registry` internally uses `Arc` and `RwLock` to
    // achieve thread safety, so there is no need to add additional locking
    // mechanisms. It can be used directly here.
    let registry = Registry::new();

    // Start the actix-web server to provide metrics data at the specified path.
    let server = match create_endpoint_server_with_retry(
        &endpoint_str,
        registry.clone(),
    ) {
        Some(server) => server,
        None => return ptr::null_mut(),
    };

    // Create the server thread and get the shutdown channel.
    let (server_thread_handle, shutdown_tx) = create_server_thread(server);

    // Create and return the TelemetrySystem.
    let system = TelemetrySystem {
        registry,
        actix_thread: Some(server_thread_handle),
        actix_shutdown_tx: Some(shutdown_tx),
    };

    // Convert to raw pointer for C API.
    Box::into_raw(Box::new(system))
}

/// Shut down the endpoint system, stop the server, and clean up all resources.
///
/// # Safety
///
/// This function assumes that `system_ptr` is either null or a valid pointer to
/// a `TelemetrySystem` that was previously created with
/// `ten_endpoint_system_create`. Calling this function with an invalid pointer
/// will lead to undefined behavior.
#[no_mangle]
pub unsafe extern "C" fn ten_endpoint_system_shutdown(
    system_ptr: *mut TelemetrySystem,
) {
    debug_assert!(!system_ptr.is_null(), "System pointer is null");
    // Early return for null pointers.
    if system_ptr.is_null() {
        eprintln!("Warning: Attempt to shut down null TelemetrySystem pointer");
        return;
    }

    // Retrieve ownership using `Box::from_raw`. This transfers ownership to
    // Rust, and the Box will be automatically dropped when it goes out of
    // scope.
    let system = Box::from_raw(system_ptr);

    // Notify the actix system to shut down through the `oneshot` channel.
    if let Some(shutdown_tx) = system.actix_shutdown_tx {
        eprintln!("Shutting down endpoint server...");
        if let Err(e) = shutdown_tx.send(()) {
            eprintln!("Failed to send shutdown signal: {:?}", e);

            panic!("Failed to send shutdown signal");
        }
    } else {
        eprintln!("No shutdown channel available for the endpoint server");
    }

    // Wait for the server thread to complete.
    if let Some(server_thread_handle) = system.actix_thread {
        eprintln!("Waiting for endpoint server to shut down...");
        match server_thread_handle.join() {
            Ok(_) => eprintln!("Endpoint server thread joined successfully"),
            Err(e) => {
                eprintln!("Error joining endpoint server thread: {:?}", e)
            }
        }
    } else {
        eprintln!("No thread handle available for the endpoint server");
    }

    // The system will be automatically dropped here, cleaning up all resources.
}
