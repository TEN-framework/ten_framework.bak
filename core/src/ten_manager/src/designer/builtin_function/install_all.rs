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
use crate::output::TmanOutput;

impl WsBuiltinFunction {
    pub fn install_all(
        &mut self,
        base_dir: String,
        ctx: &mut WebsocketContext<WsBuiltinFunction>,
    ) {
        let install_command = crate::cmd::cmd_install::InstallCommand {
            package_type: None,
            package_name: None,
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

        // 创建通道，用于跨线程通信
        let (sender, receiver) = mpsc::channel();

        // 创建自定义的 TmanOutput 实现，将日志发送到通道
        #[derive(Clone)]
        struct ChannelOutput {
            sender: mpsc::Sender<String>,
        }

        impl TmanOutput for ChannelOutput {
            fn normal_line(&self, text: &str) {
                let _ = self.sender.send(format!("normal_line:{}", text));
            }

            fn normal_partial(&self, text: &str) {
                let _ = self.sender.send(format!("normal_partial:{}", text));
            }

            fn error_line(&self, text: &str) {
                let _ = self.sender.send(format!("error_line:{}", text));
            }

            fn error_partial(&self, text: &str) {
                let _ = self.sender.send(format!("error_partial:{}", text));
            }

            fn is_interactive(&self) -> bool {
                false
            }
        }

        let output_channel = Arc::new(Box::new(ChannelOutput {
            sender: sender.clone(),
        }) as Box<dyn TmanOutput>);

        // Clone the config and metadata before the async block.
        let tman_config = self.tman_config.clone();
        let tman_metadata = self.tman_metadata.clone();

        // 在新线程中运行安装过程
        thread::spawn(move || {
            // 创建一个新的 Tokio 运行时来执行异步代码
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();

            // 在新的运行时中执行安装
            let result = rt.block_on(async {
                crate::cmd::cmd_install::execute_cmd(
                    tman_config,
                    tman_metadata,
                    install_command,
                    output_channel,
                )
                .await
            });

            // 发送完成状态
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

        // 在主线程中启动一个本地任务来监听消息通道
        let addr_clone = addr.clone();

        // 使用actix的fut::wrap_future将标准Future转换为ActorFuture
        ctx.spawn(fut::wrap_future::<_, Self>(async move {
            // 使用循环轮询接收器
            let mut continue_running = true;
            while continue_running {
                match receiver.try_recv() {
                    Ok(msg) => {
                        if msg.starts_with("EXIT:") {
                            // 解析退出状态
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

                                // 发送退出消息
                                addr_clone.do_send(
                                    BuiltinFunctionOutput::Exit {
                                        exit_code,
                                        error_message,
                                    },
                                );
                                continue_running = false;
                            }
                        } else if msg.starts_with("normal_line:") {
                            // 解析并发送正常日志
                            let content = msg.replacen("normal_line:", "", 1);
                            addr_clone.do_send(
                                BuiltinFunctionOutput::NormalLine(content),
                            );
                        } else if msg.starts_with("normal_partial:") {
                            let content =
                                msg.replacen("normal_partial:", "", 1);
                            addr_clone.do_send(
                                BuiltinFunctionOutput::NormalPartial(content),
                            );
                        } else if msg.starts_with("error_line:") {
                            let content = msg.replacen("error_line:", "", 1);
                            addr_clone.do_send(
                                BuiltinFunctionOutput::ErrorLine(content),
                            );
                        } else if msg.starts_with("error_partial:") {
                            let content = msg.replacen("error_partial:", "", 1);
                            addr_clone.do_send(
                                BuiltinFunctionOutput::ErrorPartial(content),
                            );
                        }
                    }
                    Err(mpsc::TryRecvError::Empty) => {
                        // 没有消息，暂时让出控制权
                        tokio::task::yield_now().await;
                    }
                    Err(mpsc::TryRecvError::Disconnected) => {
                        // 发送方已断开，退出循环
                        continue_running = false;
                    }
                }
            }
        }));
    }
}
