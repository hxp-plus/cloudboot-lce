/*
 * Copyright 2025 Xiping Hu <hxp@hxp.plus>
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 *    http://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
*/

use tokio::process::Command;

const SSH_PASS: &str = "abc123";

// SSH 到指定主机并运行命令，返回命令的运行结果
pub async fn run_ssh_command_on_host(ip_addr: &str, command: &str) -> Option<String> {
    let output = Command::new("sshpass")
        .arg("-p")
        .arg(SSH_PASS)
        .arg("ssh")
        .arg("-o")
        .arg("LogLevel=ERROR")
        .arg("-o")
        .arg("StrictHostKeyChecking=no")
        .arg("-o")
        .arg("UserKnownHostsFile=/dev/null")
        .arg("-o")
        .arg("ConnectTimeout=3")
        .arg(ip_addr)
        .arg(command)
        .output()
        .await;
    match output {
        Ok(output) if output.status.success() => {
            Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
        }
        _ => {
            println!(
                "[INFO] Run SSH command \"{}\" on host {} failed!",
                command, ip_addr
            );
            None
        }
    }
}

pub fn run_ssh_command_on_host_sync(ip_addr: &str, command: &str) -> Option<String> {
    let output = std::process::Command::new("sshpass")
        .arg("-p")
        .arg(SSH_PASS)
        .arg("ssh")
        .arg("-o")
        .arg("LogLevel=ERROR")
        .arg("-o")
        .arg("StrictHostKeyChecking=no")
        .arg("-o")
        .arg("UserKnownHostsFile=/dev/null")
        .arg("-o")
        .arg("ConnectTimeout=3")
        .arg(ip_addr)
        .arg(command)
        .output();
    match output {
        Ok(output) if output.status.success() => {
            Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
        }
        _ => {
            println!(
                "[INFO] Run SSH command \"{}\" on host {} failed!",
                command, ip_addr
            );
            None
        }
    }
}
