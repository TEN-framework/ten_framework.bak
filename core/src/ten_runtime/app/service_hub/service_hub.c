//
// Copyright © 2025 Agora
// This file is part of TEN Framework, an open source project.
// Licensed under the Apache License, Version 2.0, with certain conditions.
// Refer to the "LICENSE" file in the root directory for more information.
//
#include "include_internal/ten_runtime/app/service_hub/service_hub.h"

#if defined(TEN_ENABLE_TEN_RUST_APIS)

#include "include_internal/ten_runtime/app/app.h"
#include "include_internal/ten_runtime/app/service_hub/telemetry/telemetry.h"
#include "include_internal/ten_runtime/common/constant_str.h"
#include "include_internal/ten_rust/ten_rust.h"

static void ten_service_hub_init(ten_service_hub_t *self) {
  TEN_ASSERT(self, "Should not happen.");

  self->service_hub = NULL;
  self->metric_extension_thread_msg_queue_stay_time_us = NULL;
}

void ten_app_init_service_hub(ten_app_t *self) {
  TEN_ASSERT(self, "Should not happen.");
  TEN_ASSERT(ten_app_check_integrity(self, true), "Should not happen.");

  ten_service_hub_init(&self->service_hub);
}

#endif

bool ten_app_init_service_hub_config(ten_app_t *self, ten_value_t *value) {
#if defined(TEN_ENABLE_TEN_RUST_APIS)
  TEN_ASSERT(self, "Should not happen.");
  TEN_ASSERT(ten_app_check_integrity(self, true), "Should not happen.");

  TEN_ASSERT(value, "Should not happen.");
  TEN_ASSERT(ten_value_check_integrity(value), "Should not happen.");

  if (!ten_value_is_object(value)) {
    TEN_LOGE("Invalid value type for property: telemetry. Expected an object.");
    return false;
  }

  ten_value_t *enabled_value = ten_value_object_peek(value, TEN_STR_ENABLED);
  if (!enabled_value || !ten_value_is_bool(enabled_value) ||
      !ten_value_get_bool(enabled_value, NULL)) {
    // If the `enabled` field does not exist or is not set to `true`, the
    // telemetry system will not be activated.
    return true;
  }

  // Check if the `telemetry` object contains the `endpoint` field.
  const char *endpoint = NULL;
  ten_value_t *endpoint_value = ten_value_object_peek(value, TEN_STR_ENDPOINT);
  if (endpoint_value && ten_value_is_string(endpoint_value)) {
    endpoint = ten_value_peek_raw_str(endpoint_value, NULL);
  }

  if (!endpoint) {
    endpoint = "0.0.0.0:49484";
  }

  // TODO(Wei): The logic for starting the telemetry system should be moved out
  // of the config process.
  self->service_hub.service_hub = ten_service_hub_create(endpoint);
  if (!self->service_hub.service_hub) {
    TEN_LOGE("Failed to create service hub with default endpoint.");

    // NOLINTNEXTLINE(concurrency-mt-unsafe)
    exit(EXIT_FAILURE);
  } else {
    TEN_LOGI("Create service hub with endpoint: %s", endpoint);
  }

  ten_app_service_hub_create_metric(self);
#endif

  return true;
}

#if defined(TEN_ENABLE_TEN_RUST_APIS)

void ten_app_deinit_service_hub(ten_app_t *self) {
  if (self->service_hub.service_hub) {
    TEN_LOGD("[%s] Destroy service hub.", ten_app_get_uri(self));

    ten_app_service_hub_destroy_metric(self);

    ten_service_hub_shutdown(self->service_hub.service_hub);
  }
}

#endif
