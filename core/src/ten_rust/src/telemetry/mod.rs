//
// Copyright © 2025 Agora
// This file is part of TEN Framework, an open source project.
// Licensed under the Apache License, Version 2.0, with certain conditions.
// Refer to the "LICENSE" file in the root directory for more information.
//
use std::os::raw::c_char;
use std::ptr;
use std::{ffi::CStr, thread};

use actix_web::{web, App, HttpResponse, HttpServer};
use anyhow::Result;
use futures::channel::oneshot;
use futures::future::select;
use futures::FutureExt;
use prometheus::{
    Counter, CounterVec, Encoder, Gauge, GaugeVec, Histogram, HistogramOpts,
    HistogramVec, Opts, Registry, TextEncoder,
};

use crate::constants::{
    METRICS, TELEMETRY_SERVER_START_RETRY_INTERVAL,
    TELEMETRY_SERVER_START_RETRY_MAX_ATTEMPTS,
};

pub struct TelemetrySystem {
    registry: Registry,

    actix_thread: Option<thread::JoinHandle<()>>,

    /// Used to send a shutdown signal to the actix system where the server is
    /// located.
    actix_shutdown_tx: Option<oneshot::Sender<()>>,
}

pub enum MetricHandle {
    Counter(Counter),
    CounterVec(CounterVec),
    Gauge(Gauge),
    GaugeVec(GaugeVec),
    Histogram(Histogram),
    HistogramVec(HistogramVec),
}

/// Initialize the telemetry system.
#[no_mangle]
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn ten_telemetry_system_create(
    endpoint: *const c_char,
) -> *mut TelemetrySystem {
    let endpoint_str = match unsafe { CStr::from_ptr(endpoint) }.to_str() {
        Ok(s) if !s.trim().is_empty() => s.to_string(),
        _ => return ptr::null_mut(),
    };

    // Note: `prometheus::Registry` internally uses `Arc` and `RwLock` to
    // achieve thread safety, so there is no need to add additional locking
    // mechanisms. It can be used directly here.
    let registry = Registry::new();

    // Start the actix-web server to provide metrics data at the specified path.

    let mut attempts = 0;
    let max_attempts = TELEMETRY_SERVER_START_RETRY_MAX_ATTEMPTS;
    let wait_duration =
        std::time::Duration::from_secs(TELEMETRY_SERVER_START_RETRY_INTERVAL);

    let server_builder = loop {
        let registry_clone = registry.clone();

        let result = HttpServer::new(move || {
            App::new().route(
                METRICS,
                web::get().to({
                    let registry_handler = registry_clone.clone();

                    move || {
                        let registry_for_request = registry_handler.clone();

                        async move {
                            let metric_families = registry_for_request.gather();
                            let encoder = TextEncoder::new();
                            let mut buffer = Vec::new();

                            if encoder
                                .encode(&metric_families, &mut buffer)
                                .is_err()
                            {
                                return HttpResponse::InternalServerError()
                                    .finish();
                            }

                            let response = match String::from_utf8(buffer) {
                                Ok(v) => v,
                                Err(_) => {
                                    return HttpResponse::InternalServerError()
                                        .finish()
                                }
                            };

                            HttpResponse::Ok().body(response)
                        }
                    }
                }),
            )
        })
        // Make actix not linger on the socket.
        .shutdown_timeout(0)
        .bind(&endpoint_str);

        match result {
            Ok(server) => break server,
            Err(e) => {
                attempts += 1;
                if attempts >= max_attempts {
                    eprintln!(
                        "Error binding to address: {} after {} attempts: {:?}",
                        endpoint_str, attempts, e
                    );
                    return ptr::null_mut();
                }

                eprintln!(
              "Failed to bind to address: {}. Attempt {} of {}. Retrying in {} second...",
              endpoint_str, attempts, max_attempts, TELEMETRY_SERVER_START_RETRY_INTERVAL
          );
                std::thread::sleep(wait_duration);
            }
        }
    };

    let server = server_builder.run();
    let server_handle = server.handle();

    // Create an `oneshot` channel to notify the actix system to shut down.
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();

    let server_thread_handle = thread::spawn(move || {
        let sys = actix_rt::System::new();

        // Use `sys.block_on(...)` to execute an async block.
        let result: Result<()> = sys.block_on(async move {
            // Wait for either the server to finish (unlikely) or the shutdown
            // signal.
            let server_future = async {
                if let Err(e) = server.await {
                    eprintln!("Telemetry server error: {e}");
                    // Force the entire process to exit immediately.
                    std::process::exit(-1);
                }
            }
            .fuse();

            // Instead of just calling `System::current().stop()`, we call
            // `server_handle.stop(true).await` to shut down the server
            // gracefully, then stop the system.
            let shutdown_future = async move {
                let _ = shutdown_rx.await;

                eprintln!("Shutting down telemetry server (graceful stop)...");
                server_handle.stop(true).await;

                // The server is actually stopped (socket closed).

                // Terminates the actix system after the server is fully down.
                actix_rt::System::current().stop();
            }
            .fuse();

            // Use `futures::select!` to concurrently execute two futures.
            futures::pin_mut!(server_future, shutdown_future);
            select(server_future, shutdown_future).await;

            eprintln!("Telemetry server shut down.");
            Ok(())
        });

        if let Err(e) = result {
            eprintln!("Fatal error in telemetry server thread: {:?}", e);

            std::process::exit(-1);
        }
    });

    let actix_thread = Some(server_thread_handle);

    let system = TelemetrySystem {
        registry,
        actix_thread,
        actix_shutdown_tx: Some(shutdown_tx),
    };

    Box::into_raw(Box::new(system))
}

/// Shut down the telemetry system, stop the server, and clean up all resources.
#[no_mangle]
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn ten_telemetry_system_shutdown(
    system_ptr: *mut TelemetrySystem,
) {
    debug_assert!(!system_ptr.is_null(), "System pointer is null");

    if system_ptr.is_null() {
        return;
    }

    // Retrieve ownership using `Box::from_raw`, and it will be automatically
    // released when the function exits.
    let system = unsafe { Box::from_raw(system_ptr) };

    // Notify the actix system to shut down through the `oneshot` channel.
    if let Some(shutdown_tx) = system.actix_shutdown_tx {
        eprintln!("Shutting down telemetry server...");
        let _ = shutdown_tx.send(());
    }

    if let Some(server_thread_handle) = system.actix_thread {
        eprintln!("Waiting for telemetry server to shut down...");
        let _ = server_thread_handle.join();
    }
}

fn convert_label_names(
    names_ptr: *const *const c_char,
    names_len: usize,
) -> Option<Vec<String>> {
    if names_ptr.is_null() {
        return Some(vec![]);
    }

    let mut result = Vec::with_capacity(names_len);

    for i in 0..names_len {
        let c_str_ptr = unsafe { *names_ptr.add(i) };
        if c_str_ptr.is_null() {
            return None;
        }

        let c_str = unsafe { CStr::from_ptr(c_str_ptr) };
        match c_str.to_str() {
            Ok(s) => result.push(s.to_string()),
            Err(_) => return None,
        }
    }

    Some(result)
}

fn convert_label_values(
    values_ptr: *const *const c_char,
    values_len: usize,
) -> Option<Vec<String>> {
    if values_ptr.is_null() {
        return Some(vec![]);
    }

    let mut result = Vec::with_capacity(values_len);

    for i in 0..values_len {
        let c_str_ptr = unsafe { *values_ptr.add(i) };
        if c_str_ptr.is_null() {
            return None;
        }

        let c_str = unsafe { CStr::from_ptr(c_str_ptr) };
        match c_str.to_str() {
            Ok(s) => result.push(s.to_string()),
            Err(_) => return None,
        }
    }

    Some(result)
}

fn create_metric_counter(
    system: &mut TelemetrySystem,
    name_str: &str,
    help_str: &str,
) -> Result<MetricHandle> {
    let counter_opts = Opts::new(name_str, help_str);
    match Counter::with_opts(counter_opts) {
        Ok(counter) => {
            if let Err(e) = system.registry.register(Box::new(counter.clone()))
            {
                eprintln!("Error registering counter: {:?}", e);
                return Err(anyhow::anyhow!("Error registering counter"));
            }
            Ok(MetricHandle::Counter(counter))
        }
        Err(_) => Err(anyhow::anyhow!("Error creating counter")),
    }
}

fn create_metric_counter_with_labels(
    system: &mut TelemetrySystem,
    name_str: &str,
    help_str: &str,
    label_names: &[&str],
) -> Result<MetricHandle> {
    let counter_opts = Opts::new(name_str, help_str);
    match CounterVec::new(counter_opts, label_names) {
        Ok(counter_vec) => {
            if let Err(e) =
                system.registry.register(Box::new(counter_vec.clone()))
            {
                eprintln!("Error registering counter vec: {:?}", e);
                return Err(anyhow::anyhow!("Error registering counter"));
            }
            Ok(MetricHandle::CounterVec(counter_vec))
        }
        Err(_) => Err(anyhow::anyhow!("Error creating counter")),
    }
}

fn create_metric_gauge(
    system: &mut TelemetrySystem,
    name_str: &str,
    help_str: &str,
) -> Result<MetricHandle> {
    let gauge_opts = Opts::new(name_str, help_str);
    match Gauge::with_opts(gauge_opts) {
        Ok(gauge) => {
            if let Err(e) = system.registry.register(Box::new(gauge.clone())) {
                eprintln!("Error registering gauge: {:?}", e);
                return Err(anyhow::anyhow!("Error registering gauge"));
            }
            Ok(MetricHandle::Gauge(gauge))
        }
        Err(_) => Err(anyhow::anyhow!("Error creating gauge")),
    }
}

fn create_metric_gauge_with_labels(
    system: &mut TelemetrySystem,
    name_str: &str,
    help_str: &str,
    label_names: &[&str],
) -> Result<MetricHandle> {
    let gauge_opts = Opts::new(name_str, help_str);
    match GaugeVec::new(gauge_opts, label_names) {
        Ok(gauge_vec) => {
            if let Err(e) =
                system.registry.register(Box::new(gauge_vec.clone()))
            {
                eprintln!("Error registering gauge vec: {:?}", e);
                return Err(anyhow::anyhow!("Error registering gauge"));
            }
            Ok(MetricHandle::GaugeVec(gauge_vec))
        }
        Err(_) => Err(anyhow::anyhow!("Error creating gauge")),
    }
}

fn create_metric_histogram(
    system: &mut TelemetrySystem,
    name_str: &str,
    help_str: &str,
) -> Result<MetricHandle> {
    let hist_opts = HistogramOpts::new(name_str, help_str);
    match Histogram::with_opts(hist_opts) {
        Ok(histogram) => {
            if let Err(e) =
                system.registry.register(Box::new(histogram.clone()))
            {
                eprintln!("Error registering histogram: {:?}", e);
                return Err(anyhow::anyhow!("Error registering histogram"));
            }
            Ok(MetricHandle::Histogram(histogram))
        }
        Err(_) => Err(anyhow::anyhow!("Error creating histogram")),
    }
}

fn create_metric_histogram_with_labels(
    system: &mut TelemetrySystem,
    name_str: &str,
    help_str: &str,
    label_names: &[&str],
) -> Result<MetricHandle> {
    let hist_opts = HistogramOpts::new(name_str, help_str);
    match HistogramVec::new(hist_opts, label_names) {
        Ok(histogram_vec) => {
            if let Err(e) =
                system.registry.register(Box::new(histogram_vec.clone()))
            {
                eprintln!("Error registering histogram vec: {:?}", e);
                return Err(anyhow::anyhow!("Error registering histogram"));
            }
            Ok(MetricHandle::HistogramVec(histogram_vec))
        }
        Err(_) => Err(anyhow::anyhow!("Error creating histogram")),
    }
}

/// Create a metric.
///
/// # Parameter
/// - `system_ptr`: Pointer to the previously created TelemetrySystem.
/// - `metric_type`: 0 for Counter, 1 for Gauge, 2 for Histogram.
/// - `name`, `help`: The name and description of the metric.
/// - `label_names_ptr` and `label_names_len`: If not null, a metric with labels
///   will be created; only label names are required at creation time.
///
/// # Return
/// Returns a pointer to MetricHandle on success, otherwise returns null.
#[no_mangle]
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn ten_metric_create(
    system_ptr: *mut TelemetrySystem,
    metric_type: u32, // 0=Counter, 1=Gauge, 2=Histogram
    name: *const c_char,
    help: *const c_char,
    label_names_ptr: *const *const c_char,
    label_names_len: usize,
) -> *mut MetricHandle {
    debug_assert!(!system_ptr.is_null(), "System pointer is null");
    debug_assert!(!name.is_null(), "Name is null for metric creation");
    debug_assert!(!help.is_null(), "Help is null for metric creation");

    if system_ptr.is_null() || name.is_null() || help.is_null() {
        return ptr::null_mut();
    }

    let system = unsafe { &mut *system_ptr };

    let name_str = match unsafe { CStr::from_ptr(name) }.to_str() {
        Ok(s) => s,
        Err(_) => return ptr::null_mut(),
    };
    let help_str = match unsafe { CStr::from_ptr(help) }.to_str() {
        Ok(s) => s,
        Err(_) => return ptr::null_mut(),
    };

    let label_names_owned =
        match convert_label_names(label_names_ptr, label_names_len) {
            Some(v) => v,
            None => return ptr::null_mut(),
        };
    let label_names: Vec<&str> =
        label_names_owned.iter().map(|s| s.as_str()).collect();

    let metric_handle = match metric_type {
        0 => {
            // Counter.
            if label_names.is_empty() {
                match create_metric_counter(system, name_str, help_str) {
                    Ok(metric) => metric,
                    Err(_) => return ptr::null_mut(),
                }
            } else {
                match create_metric_counter_with_labels(
                    system,
                    name_str,
                    help_str,
                    &label_names,
                ) {
                    Ok(metric) => metric,
                    Err(_) => return ptr::null_mut(),
                }
            }
        }
        1 => {
            // Gauge.
            if label_names.is_empty() {
                match create_metric_gauge(system, name_str, help_str) {
                    Ok(metric) => metric,
                    Err(_) => return ptr::null_mut(),
                }
            } else {
                match create_metric_gauge_with_labels(
                    system,
                    name_str,
                    help_str,
                    &label_names,
                ) {
                    Ok(metric) => metric,
                    Err(_) => return ptr::null_mut(),
                }
            }
        }
        2 => {
            // Histogram.
            if label_names.is_empty() {
                match create_metric_histogram(system, name_str, help_str) {
                    Ok(metric) => metric,
                    Err(_) => return ptr::null_mut(),
                }
            } else {
                match create_metric_histogram_with_labels(
                    system,
                    name_str,
                    help_str,
                    &label_names,
                ) {
                    Ok(metric) => metric,
                    Err(_) => return ptr::null_mut(),
                }
            }
        }
        _ => return ptr::null_mut(),
    };

    Box::into_raw(Box::new(metric_handle))
}

/// Release a metric handle created by `ten_metric_create`.
#[no_mangle]
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn ten_metric_destroy(metric_ptr: *mut MetricHandle) {
    debug_assert!(!metric_ptr.is_null(), "Metric pointer is null");

    if metric_ptr.is_null() {
        return;
    }

    unsafe {
        let _ = Box::from_raw(metric_ptr);
    }
}

#[no_mangle]
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn ten_metric_counter_inc(
    metric_ptr: *mut MetricHandle,
    label_values_ptr: *const *const c_char,
    label_values_len: usize,
) {
    debug_assert!(!metric_ptr.is_null(), "Metric pointer is null");

    if metric_ptr.is_null() {
        return;
    }

    let metric = unsafe { &mut *metric_ptr };
    match metric {
        MetricHandle::Counter(ref counter) => {
            counter.inc();
        }
        MetricHandle::CounterVec(ref counter_vec) => {
            let values_owned = match convert_label_values(
                label_values_ptr,
                label_values_len,
            ) {
                Some(v) => v,
                None => return,
            };
            let label_refs: Vec<&str> =
                values_owned.iter().map(|s| s.as_str()).collect();
            if let Ok(counter) =
                counter_vec.get_metric_with_label_values(&label_refs)
            {
                counter.inc();
            }
        }
        _ => {}
    }
}

#[no_mangle]
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn ten_metric_counter_add(
    metric_ptr: *mut MetricHandle,
    value: f64,
    label_values_ptr: *const *const c_char,
    label_values_len: usize,
) {
    debug_assert!(!metric_ptr.is_null(), "Metric pointer is null");

    if metric_ptr.is_null() {
        return;
    }

    let metric = unsafe { &mut *metric_ptr };
    match metric {
        MetricHandle::Counter(ref counter) => {
            counter.inc_by(value);
        }
        MetricHandle::CounterVec(ref counter_vec) => {
            let values_owned = match convert_label_values(
                label_values_ptr,
                label_values_len,
            ) {
                Some(v) => v,
                None => return,
            };
            let label_refs: Vec<&str> =
                values_owned.iter().map(|s| s.as_str()).collect();
            if let Ok(counter) =
                counter_vec.get_metric_with_label_values(&label_refs)
            {
                counter.inc_by(value);
            }
        }
        _ => {}
    }
}

#[no_mangle]
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn ten_metric_gauge_set(
    metric_ptr: *mut MetricHandle,
    value: f64,
    label_values_ptr: *const *const c_char,
    label_values_len: usize,
) {
    debug_assert!(!metric_ptr.is_null(), "Metric pointer is null");

    if metric_ptr.is_null() {
        return;
    }

    let metric = unsafe { &mut *metric_ptr };
    match metric {
        MetricHandle::Gauge(ref gauge) => {
            gauge.set(value);
        }
        MetricHandle::GaugeVec(ref gauge_vec) => {
            let values_owned = match convert_label_values(
                label_values_ptr,
                label_values_len,
            ) {
                Some(v) => v,
                None => return,
            };
            let label_refs: Vec<&str> =
                values_owned.iter().map(|s| s.as_str()).collect();
            if let Ok(gauge) =
                gauge_vec.get_metric_with_label_values(&label_refs)
            {
                gauge.set(value);
            }
        }
        _ => {}
    }
}

#[no_mangle]
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn ten_metric_gauge_inc(
    metric_ptr: *mut MetricHandle,
    label_values_ptr: *const *const c_char,
    label_values_len: usize,
) {
    debug_assert!(!metric_ptr.is_null(), "Metric pointer is null");

    if metric_ptr.is_null() {
        return;
    }

    let metric = unsafe { &mut *metric_ptr };
    match metric {
        MetricHandle::Gauge(ref gauge) => {
            gauge.inc();
        }
        MetricHandle::GaugeVec(ref gauge_vec) => {
            let values_owned = match convert_label_values(
                label_values_ptr,
                label_values_len,
            ) {
                Some(v) => v,
                None => return,
            };
            let label_refs: Vec<&str> =
                values_owned.iter().map(|s| s.as_str()).collect();
            if let Ok(gauge) =
                gauge_vec.get_metric_with_label_values(&label_refs)
            {
                gauge.inc();
            }
        }
        _ => {}
    }
}

#[no_mangle]
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn ten_metric_gauge_dec(
    metric_ptr: *mut MetricHandle,
    label_values_ptr: *const *const c_char,
    label_values_len: usize,
) {
    debug_assert!(!metric_ptr.is_null(), "Metric pointer is null");

    if metric_ptr.is_null() {
        return;
    }

    let metric = unsafe { &mut *metric_ptr };
    match metric {
        MetricHandle::Gauge(ref gauge) => {
            gauge.dec();
        }
        MetricHandle::GaugeVec(ref gauge_vec) => {
            let values_owned = match convert_label_values(
                label_values_ptr,
                label_values_len,
            ) {
                Some(v) => v,
                None => return,
            };
            let label_refs: Vec<&str> =
                values_owned.iter().map(|s| s.as_str()).collect();
            if let Ok(gauge) =
                gauge_vec.get_metric_with_label_values(&label_refs)
            {
                gauge.dec();
            }
        }
        _ => {}
    }
}

#[no_mangle]
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn ten_metric_histogram_observe(
    metric_ptr: *mut MetricHandle,
    value: f64,
    label_values_ptr: *const *const c_char,
    label_values_len: usize,
) {
    debug_assert!(!metric_ptr.is_null(), "Metric pointer is null");

    if metric_ptr.is_null() {
        return;
    }

    let metric = unsafe { &mut *metric_ptr };
    match metric {
        MetricHandle::Histogram(ref histogram) => {
            histogram.observe(value);
        }
        MetricHandle::HistogramVec(ref histogram_vec) => {
            let values_owned = match convert_label_values(
                label_values_ptr,
                label_values_len,
            ) {
                Some(v) => v,
                None => return,
            };
            let label_refs: Vec<&str> =
                values_owned.iter().map(|s| s.as_str()).collect();
            if let Ok(histogram) =
                histogram_vec.get_metric_with_label_values(&label_refs)
            {
                histogram.observe(value);
            }
        }
        _ => {}
    }
}
