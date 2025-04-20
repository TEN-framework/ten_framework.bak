//
// Copyright © 2025 Agora
// This file is part of TEN Framework, an open source project.
// Licensed under the Apache License, Version 2.0, with certain conditions.
// Refer to the "LICENSE" file in the root directory for more information.
//
use std::sync::{mpsc, Arc};
use std::thread;

use actix::{fut, AsyncContext};
use actix_web_actors::ws::WebsocketContext;

use ten_rust::pkg_info::manifest::support::ManifestSupport;

use super::{BuiltinFunctionOutput, WsBuiltinFunction};
use crate::output::channel::TmanOutputChannel;
use crate::output::TmanOutput;

impl WsBuiltinFunction {
    pub fn install(
        &mut self,
        base_dir: String,
        pkg_type: String,
        pkg_name: String,
        pkg_version: Option<String>,
        ctx: &mut WebsocketContext<WsBuiltinFunction>,
    ) {
        let pkg_name_and_version = if let Some(pkg_version) = pkg_version {
            format!("{}@{}", pkg_name, pkg_version)
        } else {
            pkg_name
        };

        let install_command = crate::cmd::cmd_install::InstallCommand {
            package_type: Some(pkg_type),
            package_name: Some(pkg_name_and_version),
            support: ManifestSupport {
                os: None,
                arch: None,
            },
            local_install_mode: crate::cmd::cmd_install::LocalInstallMode::Link,
            standalone: false,
            local_path: None,
            cwd: base_dir.clone(),
        };

        let addr = ctx.address();

        // Create a channel for cross-thread communication.
        let (sender, receiver) = mpsc::channel();

        let output_channel = Arc::new(Box::new(TmanOutputChannel {
            sender: sender.clone(),
        }) as Box<dyn TmanOutput>);

        // Clone the configuration required for installation.
        let tman_config = self.tman_config.clone();
        let tman_metadata = self.tman_metadata.clone();

        // Run the installation process in a new thread.
        thread::spawn(move || {
            // Create a new Tokio runtime to execute asynchronous code.
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();

            // Execute the installation in the new runtime.
            let result = rt.block_on(async {
                crate::cmd::cmd_install::execute_cmd(
                    tman_config,
                    tman_metadata,
                    install_command,
                    output_channel,
                )
                .await
            });

            // Send the completion status.
            let exit_code = if result.is_ok() { 0 } else { -1 };
            let error_message = if let Err(err) = result {
                Some(err.to_string())
            } else {
                None
            };

            let _ = sender.send(format!(
                "EXIT:{}:{}",
                exit_code,
                error_message.unwrap_or_default()
            ));
        });

        // Start a local task in the main thread to listen to the message
        // channel.
        let addr_clone = addr.clone();

        // Use actix's fut::wrap_future to convert a standard Future to an
        // ActorFuture.
        ctx.spawn(fut::wrap_future::<_, Self>(async move {
            // Use a loop to poll the receiver.
            let mut continue_running = true;

            while continue_running {
                match receiver.try_recv() {
                    Ok(msg) => {
                        if msg.starts_with("EXIT:") {
                            // Parse the exit status.
                            let parts: Vec<&str> = msg.splitn(3, ':').collect();
                            if parts.len() >= 2 {
                                let exit_code =
                                    parts[1].parse::<i32>().unwrap_or(-1);
                                let error_message = if parts.len() > 2
                                    && !parts[2].is_empty()
                                {
                                    Some(parts[2].to_string())
                                } else {
                                    None
                                };

                                // Send the exit message.
                                addr_clone.do_send(
                                    BuiltinFunctionOutput::Exit {
                                        exit_code,
                                        error_message,
                                    },
                                );
                                continue_running = false;
                            }
                        } else if msg.starts_with("normal_line:") {
                            // Parse and send normal logs.
                            let content = msg.replacen("normal_line:", "", 1);
                            addr_clone.do_send(
                                BuiltinFunctionOutput::NormalLine(content),
                            );
                        } else if msg.starts_with("normal_partial:") {
                            // Parse and send normal partial logs.
                            let content =
                                msg.replacen("normal_partial:", "", 1);
                            addr_clone.do_send(
                                BuiltinFunctionOutput::NormalPartial(content),
                            );
                        } else if msg.starts_with("error_line:") {
                            // Parse and send error line logs.
                            let content = msg.replacen("error_line:", "", 1);
                            addr_clone.do_send(
                                BuiltinFunctionOutput::ErrorLine(content),
                            );
                        } else if msg.starts_with("error_partial:") {
                            // Parse and send error partial logs.
                            let content = msg.replacen("error_partial:", "", 1);
                            addr_clone.do_send(
                                BuiltinFunctionOutput::ErrorPartial(content),
                            );
                        }
                    }
                    Err(mpsc::TryRecvError::Empty) => {
                        // No message, temporarily yield control.
                        tokio::task::yield_now().await;
                    }
                    Err(mpsc::TryRecvError::Disconnected) => {
                        // The sender has disconnected, exit the loop.
                        continue_running = false;
                    }
                }
            }
        }));
    }
}
