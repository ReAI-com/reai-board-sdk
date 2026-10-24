//! BLE 主机绑定测试示例（固件 #13 / reai-board-sdk#16，固件 v1.75+）。
//!
//! 验证「插 USB 的那台电脑独占蓝牙连接」全链路：
//! USB 绑定 → GET 回读 → BLE 身份校验 → 已绑定其他电脑时停止重连。
//!
//! Quick start:
//! ```sh
//! # 1) USB 连接键盘：自动绑定本机（token 未指定则随机生成并打印，记住它）
//! cargo run --example host_bind --features usb -- bind
//! # 2) 查询当前绑定状态（USB）
//! cargo run --example host_bind --features usb -- status
//! # 3) 拔线走 BLE：本机是绑定主机 → 校验通过、正常可用
//! cargo run --example host_bind --features usb,ble -- monitor --token <上一步打印的 token>
//! # 4) 另一台电脑（未绑定）用同一 token 监听 → 被踢 + BoundToOtherHost + 停止重试
//! # 5) 售后兜底：解绑恢复开放
//! cargo run --example host_bind --features usb -- unbind
//! ```
//!
//! token 是 32 位 hex（16 字节）。`--token` 不传时 bind 子命令自动生成；
//! monitor 子命令不传 `--token` 时不注入 token，即旧行为（不校验）。

use std::time::Duration;

use reai_board_sdk::kernel::event::HostBindingState;
use reai_board_sdk::{BoardConfig, BoardDevice, BoardEvent};

fn parse_token(hex: &str) -> anyhow::Result<[u8; 16]> {
    let hex = hex.trim().replace('-', "");
    anyhow::ensure!(
        hex.len() == 32,
        "token 须为 32 位 hex（16 字节），收到 {} 位",
        hex.len()
    );
    let mut token = [0u8; 16];
    for (i, byte) in token.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16)?;
    }
    Ok(token)
}

fn random_token() -> [u8; 16] {
    // 随机来源：/dev/urandom（macOS/Linux）或时间熵兜底。Driver 接手后由
    // Driver 用系统安全随机生成并持久化（见 reai-board-sdk#16 driver 清单）。
    let mut token = [0u8; 16];
    #[cfg(unix)]
    {
        use std::io::Read;
        if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
            let _ = f.read_exact(&mut token);
            return token;
        }
    }
    let seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let mut state = seed;
    for byte in token.iter_mut() {
        state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        *byte = (state >> 33) as u8;
    }
    token
}

async fn open_device(token: Option<[u8; 16]>) -> anyhow::Result<BoardDevice> {
    let device = BoardDevice::open(BoardConfig {
        host_token: token,
        ..Default::default()
    })?;
    device.start().await?;
    Ok(device)
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let action = args.first().map(String::as_str).unwrap_or_else(|| {
        eprintln!("用法: host_bind <bind|status|unbind|monitor> [--token <hex32>]");
        std::process::exit(2);
    });
    let token_hex = args
        .iter()
        .position(|a| a == "--token")
        .and_then(|i| args.get(i + 1))
        .cloned();

    match action {
        // USB：自动绑定本机（SDK 连接后自动 SET + GET 回读）
        "bind" => {
            let token = match token_hex {
                Some(hex) => parse_token(&hex)?,
                None => {
                    let t = random_token();
                    println!(
                        "生成新 token（请保存，BLE 校验用）: {}",
                        t.iter().map(|b| format!("{b:02X}")).collect::<String>()
                    );
                    t
                }
            };
            let device = open_device(Some(token)).await?;
            println!("USB 已连接；绑定由 SDK 自动完成（能力位支持时），查看日志 [host-bind] 行。");
            // 等连接回调完成绑定（spawn 任务在 500ms 查询序列之后）
            tokio::time::sleep(Duration::from_secs(3)).await;
            let status = device.host_bind_get().await?;
            println!(
                "绑定状态: bound={} token_prefix={:02X?} addr_type={} addr={:02x?}",
                status.bound, status.token_prefix, status.addr_type, status.addr
            );
            if !status.bound || status.token_prefix != token[..4] {
                anyhow::bail!("绑定未生效——固件是否 v1.75+（能力位 bit4）？");
            }
            println!("✅ 本机已绑定为该键盘的独占主机");
            device.shutdown();
        }

        // USB/BLE：查询绑定状态
        "status" => {
            let device = open_device(None).await?;
            tokio::time::sleep(Duration::from_millis(800)).await;
            let status = device.host_bind_get().await?;
            println!(
                "绑定状态: bound={} token_prefix={:02X?} addr_type={} addr={:02x?}",
                status.bound, status.token_prefix, status.addr_type, status.addr
            );
            device.shutdown();
        }

        // 解绑（售后/调试兜底）
        "unbind" => {
            let device = open_device(None).await?;
            tokio::time::sleep(Duration::from_millis(800)).await;
            device.host_bind_unbind().await?;
            println!("✅ 已解绑（恢复开放广播/直连）");
            device.shutdown();
        }

        // BLE 监听：观察身份校验结果（Verified / BoundToOtherHost / 被固件踢）
        "monitor" => {
            let token = match token_hex {
                Some(hex) => Some(parse_token(&hex)?),
                None => {
                    println!("未传 --token：不注入 token，SDK 跳过身份校验（旧行为）");
                    None
                }
            };
            let device = open_device(token).await?;
            println!("已启动，监听事件 10 分钟（Ctrl-C 退出）……");
            let mut events = device.events();
            let deadline = tokio::time::Instant::now() + Duration::from_secs(600);
            while tokio::time::Instant::now() < deadline {
                match tokio::time::timeout_at(deadline, events.recv()).await {
                    Ok(Ok(Some(BoardEvent::Connection(c)))) => {
                        println!(
                            "[Connection] connected={} type={:?} reason={:?}",
                            c.connected, c.connection_type, c.reason
                        );
                        if !c.connected {
                            println!("（已断开；若 reason=device_disconnect 且伴随下方 HostBinding，即绑定拒绝）");
                        }
                    }
                    Ok(Ok(Some(BoardEvent::HostBinding(e)))) => match e.state {
                        HostBindingState::Verified => {
                            println!("✅ [HostBinding] 身份校验通过：本机是绑定主机");
                        }
                        HostBindingState::BoundToOtherHost => {
                            println!("⛔ [HostBinding] 键盘已绑定其他电脑：SDK 已停止重连；插 USB 可换绑到本机");
                        }
                    },
                    Ok(Ok(Some(evt))) => println!("[{:?}]", std::mem::discriminant(&evt)),
                    Ok(Ok(None)) | Ok(Err(_)) | Err(_) => break,
                    Err(_) => break,
                }
            }
            device.shutdown();
        }

        other => anyhow::bail!("未知子命令: {other}（支持 bind / status / unbind / monitor）"),
    }
    Ok(())
}
