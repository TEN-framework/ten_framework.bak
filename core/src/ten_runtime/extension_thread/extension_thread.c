//
// Copyright © 2025 Agora
// This file is part of TEN Framework, an open source project.
// Licensed under the Apache License, Version 2.0, with certain conditions.
// Refer to the "LICENSE" file in the root directory for more information.
//
#include "include_internal/ten_runtime/extension_thread/extension_thread.h"

#include <inttypes.h>
#include <stddef.h>
#include <stdlib.h>

#include "include_internal/ten_runtime/app/app.h"
#include "include_internal/ten_runtime/app/base_dir.h"
#include "include_internal/ten_runtime/common/loc.h"
#include "include_internal/ten_runtime/engine/engine.h"
#include "include_internal/ten_runtime/engine/internal/thread.h"
#include "include_internal/ten_runtime/engine/msg_interface/common.h"
#include "include_internal/ten_runtime/engine/on_xxx.h"
#include "include_internal/ten_runtime/extension/extension.h"
#include "include_internal/ten_runtime/extension_context/extension_context.h"
#include "include_internal/ten_runtime/extension_group/extension_group.h"
#include "include_internal/ten_runtime/extension_group/on_xxx.h"
#include "include_internal/ten_runtime/extension_store/extension_store.h"
#include "include_internal/ten_runtime/extension_thread/msg_interface/common.h"
#include "include_internal/ten_runtime/msg/msg.h"
#include "include_internal/ten_runtime/ten_env/ten_env.h"
#include "include_internal/ten_utils/sanitizer/thread_check.h"
#include "ten_runtime/extension/extension.h"
#include "ten_runtime/msg/cmd/stop_graph/cmd.h"
#include "ten_runtime/msg/cmd_result/cmd_result.h"
#include "ten_runtime/ten_env/ten_env.h"
#include "ten_utils/container/list.h"
#include "ten_utils/io/runloop.h"
#include "ten_utils/lib/alloc.h"
#include "ten_utils/lib/error.h"
#include "ten_utils/lib/event.h"
#include "ten_utils/lib/mutex.h"
#include "ten_utils/lib/smart_ptr.h"
#include "ten_utils/lib/string.h"
#include "ten_utils/lib/thread.h"
#include "ten_utils/macro/check.h"
#include "ten_utils/macro/mark.h"
#include "ten_utils/sanitizer/thread_check.h"

bool ten_extension_thread_check_integrity_if_in_lock_mode(
    ten_extension_thread_t *self) {
  TEN_ASSERT(self, "Should not happen.");

  if (self->in_lock_mode) {
    return true;
  }

  return false;
}

bool ten_extension_thread_check_integrity(ten_extension_thread_t *self,
                                          bool check_thread) {
  TEN_ASSERT(self, "Should not happen.");

  if (ten_signature_get(&self->signature) !=
      (ten_signature_t)TEN_EXTENSION_THREAD_SIGNATURE) {
    TEN_ASSERT(0,
               "Failed to pass extension_thread signature checking: %" PRId64,
               self->signature);
    return false;
  }

  if (check_thread) {
    if (ten_extension_thread_check_integrity_if_in_lock_mode(self)) {
      return true;
    }

    if (!ten_sanitizer_thread_check_do_check(&self->thread_check)) {
      TEN_ASSERT(0, "Failed to pass extension_thread thread safety checking.");
      return false;
    }
  }

  return true;
}

ten_extension_thread_t *ten_extension_thread_create(void) {
  ten_extension_thread_t *self =
      (ten_extension_thread_t *)TEN_MALLOC(sizeof(ten_extension_thread_t));
  TEN_ASSERT(self, "Failed to allocate memory.");

  ten_signature_set(&self->signature,
                    (ten_signature_t)TEN_EXTENSION_THREAD_SIGNATURE);

  self->tid = 0;
  self->state = TEN_EXTENSION_THREAD_STATE_INIT;
  self->is_close_triggered = false;

  self->extension_context = NULL;
  self->extension_group = NULL;
  self->extension_store = ten_extension_store_create(
      offsetof(ten_extension_t, hh_in_extension_store));

  ten_list_init(&self->extensions);
  self->extensions_cnt_of_deleted = 0;

  ten_list_init(&self->pending_msgs_received_in_init_stage);

  self->in_lock_mode = false;
  self->lock_mode_lock = ten_mutex_create();

  ten_sanitizer_thread_check_init(&self->thread_check);

  self->runloop = NULL;
  self->runloop_is_ready_to_use = ten_event_create(0, 0);

  return self;
}

static void ten_extension_thread_attach_to_group(
    ten_extension_thread_t *self, ten_extension_group_t *extension_group) {
  TEN_ASSERT(self, "Invalid argument.");
  TEN_ASSERT(ten_extension_thread_check_integrity(self, false),
             "Invalid use of extension_thread %p.", self);
  TEN_ASSERT(extension_group, "Should not happen.");

  self->extension_group = extension_group;
}

void ten_extension_thread_attach_to_context_and_group(
    ten_extension_thread_t *self, ten_extension_context_t *extension_context,
    ten_extension_group_t *extension_group) {
  TEN_ASSERT(self, "Invalid argument.");
  TEN_ASSERT(ten_extension_thread_check_integrity(self, false),
             "Invalid use of extension_thread %p.", self);
  TEN_ASSERT(extension_context && extension_group, "Should not happen.");

  self->extension_context = extension_context;
  ten_extension_thread_attach_to_group(self, extension_group);
}

void ten_extension_thread_destroy(ten_extension_thread_t *self) {
  TEN_ASSERT(self, "Invalid argument.");
  TEN_ASSERT(ten_extension_thread_check_integrity(self, false),
             "Invalid use of extension_thread %p.", self);

  // All the Extensions should have been destroyed.
  TEN_ASSERT(ten_list_is_empty(&self->extensions), "Should not happen.");

  ten_signature_set(&self->signature, 0);

  ten_list_clear(&self->pending_msgs_received_in_init_stage);

  if (self->runloop) {
    ten_runloop_destroy(self->runloop);
    self->runloop = NULL;
  }

  ten_event_destroy(self->runloop_is_ready_to_use);

  ten_sanitizer_thread_check_deinit(&self->thread_check);
  ten_extension_store_destroy(self->extension_store);

  ten_mutex_destroy(self->lock_mode_lock);
  self->lock_mode_lock = NULL;

  TEN_FREE(self);
}

void ten_extension_thread_remove_from_extension_context(
    ten_extension_thread_t *self) {
  TEN_ASSERT(self, "Invalid argument.");
  TEN_ASSERT(ten_extension_thread_check_integrity(self, true),
             "Invalid use of extension_thread %p.", self);
  TEN_ASSERT(ten_engine_check_integrity(self->extension_context->engine, true),
             "Should not happen.");

  self->extension_group->extension_thread = NULL;

  ten_extension_thread_destroy(self);
}

// Notify extension context (engine) that we (extension thread) are closed, so
// that engine can join this thread to prevent memory leak.
static void ten_extension_thread_notify_engine_we_are_closed(
    ten_extension_thread_t *self) {
  TEN_ASSERT(self, "Invalid argument.");
  TEN_ASSERT(ten_extension_thread_check_integrity(self, true),
             "Invalid use of extension_thread %p.", self);

  ten_engine_t *engine = self->extension_context->engine;
  // TEN_NOLINTNEXTLINE(thread-check)
  // thread-check: In the closing flow, the closing of the engine is always
  // after the closing of the extension thread, so its thread safe to access the
  // runloop of the engine here.
  TEN_ASSERT(engine && ten_engine_check_integrity(engine, false),
             "Should not happen.");

  ten_runloop_t *engine_loop = ten_engine_get_attached_runloop(engine);
  TEN_ASSERT(engine_loop, "Should not happen.");

  ten_extension_thread_set_state(self, TEN_EXTENSION_THREAD_STATE_CLOSED);

  int rc = ten_runloop_post_task_tail(
      engine_loop, ten_engine_on_extension_thread_closed, engine, self);
  TEN_ASSERT(!rc, "Should not happen.");
}

ten_runloop_t *ten_extension_thread_get_attached_runloop(
    ten_extension_thread_t *self) {
  TEN_ASSERT(self &&
                 // TEN_NOLINTNEXTLINE(thread-check)
                 // thread-check: This function is intended to be called in
                 // threads other than the extension thread itself.
                 ten_extension_thread_check_integrity(self, false),
             "Should not happen.");

  return self->runloop;
}

static void ten_extension_thread_inherit_thread_ownership(
    ten_extension_thread_t *self) {
  TEN_ASSERT(self, "Invalid argument.");
  // TEN_NOLINTNEXTLINE(thread-check)
  // thread-check: The correct threading ownership will be setup
  // soon, so we can _not_ check thread safety here.
  TEN_ASSERT(ten_extension_thread_check_integrity(self, false),
             "Invalid use extension thread %p.", self);

  // Move the ownership of the extension thread relevant resources to the
  // belonging extension thread.
  ten_sanitizer_thread_check_init_with_current_thread(&self->thread_check);
  ten_sanitizer_thread_check_inherit_from(&self->extension_store->thread_check,
                                          &self->thread_check);

  ten_extension_group_t *extension_group = self->extension_group;
  TEN_ASSERT(extension_group, "Invalid argument.");
  // TEN_NOLINTNEXTLINE(thread-check)
  // thread-check: The correct threading ownership will be setup
  // soon, so we can _not_ check thread safety here.
  TEN_ASSERT(ten_extension_group_check_integrity(extension_group, false),
             "Invalid use extension group %p.", extension_group);

  ten_sanitizer_thread_check_inherit_from(&extension_group->thread_check,
                                          &self->thread_check);
  ten_sanitizer_thread_check_inherit_from(
      &extension_group->ten_env->thread_check, &self->thread_check);
}

void *ten_extension_thread_main_actual(ten_extension_thread_t *self) {
  TEN_LOGD("Extension thread is started");

  TEN_ASSERT(self &&
                 // TEN_NOLINTNEXTLINE(thread-check)
                 // thread-check: The correct threading ownership will be setup
                 // soon, so we can _not_ check thread safety here.
                 ten_extension_thread_check_integrity(self, false),
             "Should not happen.");

  ten_extension_thread_inherit_thread_ownership(self);
  self->tid = ten_thread_get_id(NULL);

  // The runloop should be created in its own thread.
  self->runloop = ten_runloop_create(NULL);
  TEN_ASSERT(self->runloop, "Should not happen.");

  int rc = ten_runloop_post_task_tail(
      self->runloop, ten_extension_thread_handle_start_msg_task, self, NULL);
  TEN_ASSERT(!rc, "Should not happen.");

  // Before actually starting the extension thread's runloop, first notify the
  // engine (extension_context) that the extension thread's runloop is ready for
  // use.
  ten_event_set(self->runloop_is_ready_to_use);

  // Run the extension thread event loop.
  ten_runloop_run(self->runloop);

  ten_extension_thread_notify_engine_we_are_closed(self);

  TEN_LOGD("Extension thread is stopped.");

  return NULL;
}

// This is the extension thread.
static void *ten_extension_thread_main(void *self_) {
  ten_extension_thread_t *self = (ten_extension_thread_t *)self_;
  return ten_extension_thread_main_actual(self);
}

void ten_extension_thread_start(ten_extension_thread_t *self) {
  TEN_ASSERT(self &&
                 // TEN_NOLINTNEXTLINE(thread-check)
                 // thread-check: because the extension thread has not started
                 // yet, we can _not_ check thread safety here.
                 ten_extension_thread_check_integrity(self, false),
             "Should not happen.");

  ten_thread_create(ten_string_get_raw_str(&self->extension_group->name),
                    ten_extension_thread_main, self);

  // The runloop of the extension_thread is created within the extension thread
  // itself, which introduces a time gap. If the engine (extension_context)
  // attempts to post a task to the runloop of extension_thread before the
  // runloop has been created, it would result in a segmentation fault since the
  // runloop would still be NULL. There are two approaches to handle this
  // situation:
  //
  // 1) Protect both the extension_thread and engine access to
  //    extension_thread::runloop with a mutex. But this is too heavy.
  // 2) The approach adopted here is to have the engine thread wait briefly
  //    until the runloop is successfully created by the extension_thread before
  //    proceeding. This eliminates the need to lock every time the runloop is
  //    accessed.
  ten_event_wait(self->runloop_is_ready_to_use, -1);
}

static void ten_extension_thread_on_triggering_close(void *self_,
                                                     TEN_UNUSED void *arg) {
  ten_extension_thread_t *self = self_;
  TEN_ASSERT(self, "Invalid argument.");
  TEN_ASSERT(ten_extension_thread_check_integrity(self, true),
             "Invalid use of extension_thread %p.", self);

  // The closing flow should be executed only once.
  if (self->is_close_triggered) {
    return;
  }

  self->is_close_triggered = true;

  switch (self->state) {
    case TEN_EXTENSION_THREAD_STATE_INIT:
      // Enter the deinit flow of the extension group directly.
      ten_extension_group_on_deinit(self->extension_group);
      break;

    case TEN_EXTENSION_THREAD_STATE_CREATING_EXTENSIONS:
      // We need to wait until `on_create_extensions_done()` is called, as that
      // is the point when all the created extensions can be retrieved to begin
      // the close process. Otherwise, memory leaks caused by those extensions
      // may occur.
      break;

    case TEN_EXTENSION_THREAD_STATE_NORMAL:
      ten_extension_thread_stop_life_cycle_of_all_extensions(self);
      break;

    case TEN_EXTENSION_THREAD_STATE_PREPARE_TO_CLOSE:
    case TEN_EXTENSION_THREAD_STATE_CLOSED:
    default:
      TEN_ASSERT(0, "Should not happen.");
      break;
  }
}

void ten_extension_thread_close(ten_extension_thread_t *self) {
  TEN_ASSERT(self, "Invalid argument.");
  // TEN_NOLINTNEXTLINE(thread-check)
  // thread-check: this function is intended to be called in any threads.
  TEN_ASSERT(ten_extension_thread_check_integrity(self, false),
             "Should not happen.");

  TEN_LOGD("Try to close extension thread.");

  // Notify extension thread that it is about to close.
  int rc = ten_runloop_post_task_tail(
      self->runloop, ten_extension_thread_on_triggering_close, self, NULL);
  TEN_ASSERT(!rc, "Should not happen.");
}

bool ten_extension_thread_call_by_me(ten_extension_thread_t *self) {
  TEN_ASSERT(self &&
                 // TEN_NOLINTNEXTLINE(thread-check)
                 // thread-check: this function is intended to be called in any
                 // threads.
                 ten_extension_thread_check_integrity(self, false),
             "Should not happen.");

  return ten_thread_equal(NULL, ten_sanitizer_thread_check_get_belonging_thread(
                                    &self->thread_check));
}

bool ten_extension_thread_not_call_by_me(ten_extension_thread_t *self) {
  TEN_ASSERT(self &&
                 // TEN_NOLINTNEXTLINE(thread-check)
                 // thread-check: this function is intended to be called in any
                 // threads.
                 ten_extension_thread_check_integrity(self, false),
             "Should not happen.");

  return !ten_extension_thread_call_by_me(self);
}

TEN_EXTENSION_THREAD_STATE
ten_extension_thread_get_state(ten_extension_thread_t *self) {
  TEN_ASSERT(self, "Invalid argument.");
  TEN_ASSERT(ten_extension_thread_check_integrity(self, true),
             "Invalid use of extension_thread %p.", self);

  return self->state;
}

void ten_extension_thread_set_state(ten_extension_thread_t *self,
                                    TEN_EXTENSION_THREAD_STATE state) {
  TEN_ASSERT(self, "Invalid argument.");
  TEN_ASSERT(ten_extension_thread_check_integrity(self, true),
             "Invalid use of extension_thread %p.", self);

  self->state = state;
}

static void ten_extension_thread_add_extension(ten_extension_thread_t *self,
                                               ten_extension_t *extension) {
  TEN_ASSERT(self, "Invalid argument.");
  TEN_ASSERT(ten_extension_thread_check_integrity(self, true),
             "Invalid use of extension_thread %p.", self);

  TEN_ASSERT(extension && ten_extension_check_integrity(extension, true),
             "Should not happen.");

  extension->extension_thread = self;

  TEN_UNUSED bool rc =
      ten_extension_store_add_extension(self->extension_store, extension);
  TEN_ASSERT(rc, "Should not happen.");
}

static void ten_extension_thread_stop_life_cycle_of_all_extensions_task(
    void *self, TEN_UNUSED void *arg) {
  ten_extension_thread_t *extension_thread = self;
  TEN_ASSERT(extension_thread &&
                 ten_extension_thread_check_integrity(extension_thread, true),
             "Invalid argument.");

  ten_extension_thread_stop_life_cycle_of_all_extensions(extension_thread);
}

/**
 * Begin processing all lifecycle stages of the extensions contained within the
 * extension thread. This means starting to invoke each extension's series of
 * lifecycle methods, beginning with `on_configure`.
 */
static void ten_extension_thread_start_life_cycle_of_all_extensions_task(
    void *self_, TEN_UNUSED void *arg) {
  ten_extension_thread_t *self = self_;
  TEN_ASSERT(ten_extension_thread_check_integrity(self, true),
             "Should not happen.");

  if (self->is_close_triggered) {
    return;
  }

  ten_extension_thread_set_state(self, TEN_EXTENSION_THREAD_STATE_NORMAL);

  // From here, it begins calling a series of lifecycle methods for the
  // extension, starting with `on_configure`.

  ten_list_foreach (&self->extensions, iter) {
    ten_extension_t *extension = ten_ptr_listnode_get(iter.node);
    TEN_ASSERT(extension && ten_extension_check_integrity(extension, true),
               "Should not happen.");

    ten_extension_load_metadata(extension);
  }
}

/**
 * After the initialization of all extension threads in the engine (representing
 * a graph) is completed (regardless of whether the result is success or
 * failure), the engine needs to respond to the original requester of the graph
 * creation (i.e., a `start_graph` command) with a result.
 */
static void ten_engine_on_all_extension_threads_are_ready(
    ten_engine_t *self, ten_extension_thread_t *extension_thread) {
  TEN_ASSERT(self && ten_engine_check_integrity(self, true),
             "Should not happen.");
  TEN_ASSERT(
      extension_thread &&
          // TEN_NOLINTNEXTLINE(thread-check)
          // thread-check: this function does not access this extension_thread,
          // we just check if the arg is an ten_extension_thread_t.
          ten_extension_thread_check_integrity(extension_thread, false),
      "Should not happen.");

  ten_extension_context_t *extension_context = self->extension_context;
  TEN_ASSERT(extension_context &&
                 ten_extension_context_check_integrity(extension_context, true),
             "Should not happen.");

  extension_context->extension_threads_cnt_of_ready++;
  if (extension_context->extension_threads_cnt_of_ready ==
      ten_list_size(&extension_context->extension_threads)) {
    bool error_occurred = false;

    // Check if there were any errors during the creation and/or initialization
    // of any extension thread/group. If errors are found, shut down the
    // engine/graph and return the corresponding result to the original
    // requester.

    ten_list_foreach (&extension_context->extension_groups, iter) {
      ten_extension_group_t *extension_group = ten_ptr_listnode_get(iter.node);
      TEN_ASSERT(extension_group && ten_extension_group_check_integrity(
                                        extension_group, false),
                 "Should not happen.");

      if (!ten_error_is_success(&extension_group->err_before_ready)) {
        error_occurred = true;
        break;
      }
    }

    ten_shared_ptr_t *state_requester_cmd =
        extension_context->state_requester_cmd;
    TEN_ASSERT(state_requester_cmd, "Should not happen.");

    ten_shared_ptr_t *cmd_result = NULL;
    if (error_occurred) {
      TEN_LOGE("[%s] Failed to start the graph successfully, shutting it down.",
               ten_engine_get_id(self, true));

      cmd_result = ten_cmd_result_create_from_cmd(TEN_STATUS_CODE_ERROR,
                                                  state_requester_cmd);
    } else {
      TEN_LOGD("[%s] All extension threads are initted.",
               ten_engine_get_id(self, true));

      ten_string_t *graph_id = &self->graph_id;

      const char *body_str =
          ten_string_is_empty(graph_id) ? "" : ten_string_get_raw_str(graph_id);

      cmd_result = ten_cmd_result_create_from_cmd(TEN_STATUS_CODE_OK,
                                                  state_requester_cmd);
      ten_msg_set_property(cmd_result, "detail",
                           ten_value_create_string(body_str), NULL);

      // Mark the engine that it could start to handle messages.
      self->is_ready_to_handle_msg = true;

      TEN_LOGD("[%s] Engine is ready to handle messages.",
               ten_engine_get_id(self, true));
    }

    ten_env_return_result(self->ten_env, cmd_result, state_requester_cmd, NULL,
                          NULL, NULL);
    ten_shared_ptr_destroy(cmd_result);

    ten_shared_ptr_destroy(state_requester_cmd);
    extension_context->state_requester_cmd = NULL;

    if (error_occurred) {
      ten_app_t *app = self->app;
      TEN_ASSERT(app && ten_app_check_integrity(app, false),
                 "Invalid argument.");

      // This graph/engine will not be functioning properly, so it will be shut
      // down directly.
      ten_shared_ptr_t *stop_graph_cmd = ten_cmd_stop_graph_create();
      ten_msg_clear_and_set_dest(stop_graph_cmd, ten_app_get_uri(app),
                                 ten_engine_get_id(self, false), NULL, NULL,
                                 NULL);

      ten_env_send_cmd(self->ten_env, stop_graph_cmd, NULL, NULL, NULL, NULL);

      ten_shared_ptr_destroy(stop_graph_cmd);
    } else {
      // Because the engine is just ready to handle messages, hence, we trigger
      // the engine to handle any _pending_/_cached_ external messages if any.
      ten_engine_handle_in_msgs_async(self);
    }
  }
}

static void
ten_engine_find_extension_info_for_all_extensions_of_extension_thread(
    void *self_, void *arg) {
  ten_engine_t *self = self_;
  TEN_ASSERT(self && ten_engine_check_integrity(self, true),
             "Should not happen.");

  ten_extension_context_t *extension_context = self->extension_context;
  TEN_ASSERT(extension_context &&
                 ten_extension_context_check_integrity(extension_context, true),
             "Should not happen.");

  TEN_UNUSED ten_extension_thread_t *extension_thread = arg;
  TEN_ASSERT(extension_thread &&
                 // TEN_NOLINTNEXTLINE(thread-check)
                 // thread-check: this function does not access this
                 // extension_thread, we just check if the arg is an
                 // ten_extension_thread_t.
                 ten_extension_thread_check_integrity(extension_thread, false),
             "Should not happen.");

  ten_list_foreach (&extension_thread->extensions, iter) {
    ten_extension_t *extension = ten_ptr_listnode_get(iter.node);
    TEN_ASSERT(ten_extension_check_integrity(extension, false),
               "Should not happen.");

    // Setup 'extension_context' field, this is the most important field when
    // extension is initiating.
    extension->extension_context = extension_context;

    // Find the extension_info of the specified 'extension'.
    extension->extension_info =
        ten_extension_context_get_extension_info_by_name(
            extension_context, ten_app_get_uri(extension_context->engine->app),
            ten_engine_get_id(extension_context->engine, true),
            ten_extension_group_get_name(extension_thread->extension_group,
                                         false),
            ten_extension_get_name(extension, false));
  }

  if (extension_thread->is_close_triggered) {
    int rc = ten_runloop_post_task_tail(
        ten_extension_thread_get_attached_runloop(extension_thread),
        ten_extension_thread_stop_life_cycle_of_all_extensions_task,
        extension_thread, NULL);
    TEN_ASSERT(!rc, "Should not happen.");
  } else {
    ten_engine_on_all_extension_threads_are_ready(self, extension_thread);

    int rc = ten_runloop_post_task_tail(
        ten_extension_thread_get_attached_runloop(extension_thread),
        ten_extension_thread_start_life_cycle_of_all_extensions_task,
        extension_thread, NULL);
    TEN_ASSERT(!rc, "Should not happen.");
  }
}

static void ten_extension_thread_log_graph_resources(
    ten_extension_thread_t *self) {
  TEN_ASSERT(self, "Invalid argument.");
  TEN_ASSERT(ten_extension_thread_check_integrity(self, true),
             "Invalid use of extension_thread %p.", self);

  // Get the required information.
  const char *app_base_dir =
      ten_app_get_base_dir(self->extension_context->engine->app);
  const char *app_uri = ten_app_get_uri(self->extension_context->engine->app);
  const char *graph_id =
      ten_engine_get_id(self->extension_context->engine, false);
  const char *graph_name =
      ten_string_get_raw_str(&self->extension_context->engine->graph_name);

  // Build extension thread entry.
  ten_string_t extension_threads_json;
  ten_string_init(&extension_threads_json);

  // Extensions by thread ID
  ten_string_t thread_entry;
  ten_string_init(&thread_entry);
  ten_string_t extensions_array;
  ten_string_init(&extensions_array);

  bool first_extension = true;

  ten_list_foreach (&self->extensions, iter) {
    ten_extension_t *extension = ten_ptr_listnode_get(iter.node);
    TEN_ASSERT(extension, "Should not happen.");
    TEN_ASSERT(ten_extension_check_integrity(extension, true),
               "Invalid use of extension %p.", extension);

    // Add comma for all but the first extension.
    if (!first_extension) {
      ten_string_append_formatted(&extensions_array, "%s", ", ");
    }
    first_extension = false;

    // Add extension name to the array
    ten_string_append_formatted(&extensions_array, "\"%s\"",
                                ten_extension_get_name(extension, true));
  }

  ten_string_append_formatted(&thread_entry, "\"%lld\": {\"extensions\": [%s]}",
                              (long long)self->tid,
                              ten_string_get_raw_str(&extensions_array));

  ten_string_append_formatted(&extension_threads_json, "%s",
                              ten_string_get_raw_str(&thread_entry));

  // Log the complete JSON in a single call.
  ten_string_t log_json;
  ten_string_init(&log_json);

  // Always add app_base_dir
  ten_string_append_formatted(&log_json, "\"app_base_dir\": \"%s\"",
                              app_base_dir);

  // Conditionally add app_uri if it exists and is not empty
  if (app_uri != NULL && app_uri[0] != '\0') {
    ten_string_append_formatted(&log_json, ", \"app_uri\": \"%s\"", app_uri);
  }

  // Conditionally add graph name if it exists and is not empty
  if (graph_name != NULL && graph_name[0] != '\0') {
    ten_string_append_formatted(&log_json, ", \"graph_name\": \"%s\"",
                                graph_name);
  }

  // Always add graph id and extension_threads.
  ten_string_append_formatted(
      &log_json, ", \"graph_id\": \"%s\", \"extension_threads\": {%s}",
      graph_id, ten_string_get_raw_str(&extension_threads_json));

  // Log the complete JSON
  TEN_LOGM("[graph resources] {%s}", ten_string_get_raw_str(&log_json));

  // Clean up.
  ten_string_deinit(&log_json);
  ten_string_deinit(&thread_entry);
  ten_string_deinit(&extensions_array);
  ten_string_deinit(&extension_threads_json);
}

void ten_extension_thread_add_all_created_extensions(
    ten_extension_thread_t *self) {
  TEN_ASSERT(self, "Invalid argument.");
  TEN_ASSERT(ten_extension_thread_check_integrity(self, true),
             "Invalid use of extension_thread %p.", self);

  ten_extension_context_t *extension_context = self->extension_context;
  TEN_ASSERT(
      extension_context &&
          // TEN_NOLINTNEXTLINE(thread-check)
          // thread-check: We are in the extension thread, and throughout the
          // entire lifecycle of the extension, the extension_context where
          // the extension resides remains unchanged. Even in the closing
          // flow, the extension_context is closed later than the extension
          // itself. Therefore, using a pointer to the extension_context
          // within the extension thread is thread-safe.
          ten_extension_context_check_integrity(extension_context, false),
      "Should not happen.");

  ten_list_foreach (&self->extensions, iter) {
    ten_extension_t *extension = ten_ptr_listnode_get(iter.node);
    TEN_ASSERT(ten_extension_check_integrity(extension, true),
               "Should not happen.");

    // Correct the belonging_thread of the contained path_table.
    ten_sanitizer_thread_check_set_belonging_thread_to_current_thread(
        &extension->path_table->thread_check);

    ten_extension_thread_add_extension(self, extension);
  }

  ten_extension_thread_log_graph_resources(self);

  // Notify the engine to handle those newly created extensions.

  ten_engine_t *engine = extension_context->engine;
  // TEN_NOLINTNEXTLINE(thread-check)
  // thread-check: The runloop of the engine will not be changed during the
  // whole lifetime of the extension thread, so it's thread safe to access it
  // here.
  TEN_ASSERT(engine && ten_engine_check_integrity(engine, false),
             "Should not happen.");

  int rc = ten_runloop_post_task_tail(
      ten_engine_get_attached_runloop(engine),
      ten_engine_find_extension_info_for_all_extensions_of_extension_thread,
      engine, self);
  TEN_ASSERT(!rc, "Should not happen.");
}
