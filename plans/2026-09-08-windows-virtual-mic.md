# Windows 虚拟麦克风（virtual-mic 二期）实施记录（2026-09-08）

> Issue: reai-board-sdk#11 · 分支: `codex/windows-virtual-mic`
>
> **状态：源码交付 + Rust 侧全量验证通过；驱动已于 2026-09-08 在本机
> （WDK 10.0.26100 + BuildTools 17.14 + Spectre 库）编译、测试签名并通过
> inf2cat 签名测试；真机安装/录音验收待重启进入测试签名模式后进行。**

#11 的 macOS 部分（HAL 插件 + UDP 喂入）已随 v0.3.2 落地；本期补 Windows。
Windows 没有用户态虚拟音频 API，唯一正路是内核驱动（issue 里预估的
sysvad 派生 + 签名路线），本期交付的是这条路的**开发模式形态**：
测试签名 + 本机/CI 构建，EV 证书 + Partner Center 证明签名留给独立二期。

## 交付物

1. **驱动源码工程** `virtual-mic/driver-windows/`（WDK/MSBuild，x64 + ARM64）
   - 派生自微软 **SimpleAudioSample**（MIT，与 SDK 同许可）；不做 sysvad
     全家桶 fork——示例里采集（micarray）+ 渲染（speaker）双端点，我们删
     speaker、把格式收窄为单区间 16 kHz / mono / 16-bit、把正弦发生器换成
     用户态喂 PCM。
   - **控制设备** `\\.\ReAIVibeBoardVirtualMic`：`IRP_MJ_WRITE` 收 S16LE
     PCM，写入 `pcmfeed` 环形缓冲（128 KB ≈ 4 s，自旋锁保护，欠载补零、
     满时丢新数据）；WaveRT 采集流的 notification DPC 从 ring 拉数进
     DMA 缓冲——与 macOS 侧「欠载静音、不积延迟」的行为合同一致。
   - CDO 生命周期挂 `AddDevice` / `REMOVE_DEVICE` / `DriverUnload`；
     `DriverEntry` 在 `PcInitializeAdapterDriver` 之后覆写
     `IRP_MJ_CREATE/CLEANUP/WRITE`，非 CDO 的 IRP 原样转发
     `PcDispatchIrp`，PortCls 行为零变化。`PnpHandler` 对 CDO 提前返回，
     防止把 CDO 扩展当 `PortClassDeviceContext` 解引用（会蓝屏的点）。
   - INF：根枚举 `ROOT\ReAIVB`、服务 `ReAIVibeBoardVirtualMic`、端点友好名
     **"ReAI-Vibe-Board"**（与 `virtual_mic::DEVICE_NAME` 相等）；删掉了
     示例的 DRM `SignatureAttributes`（无保护内容，免得给证明签名添堵）；
     目标下限 Windows 10 2004（ExAllocatePool2/POOL_FLAG 的要求）。
2. **Rust 侧补全** `src/virtual_mic/windows.rs`
   - 合同与占位期一致（控制设备路径、INF 名、根设备 ID 不变）；
     新增：安装后对控制设备**有界等待**（5 s 轮询，服务异步启动）；
     S16LE 编码抽出 `s16le_payload` 并配单测（clamp/LE/空输入）；
     `ensure_installed` 的失败信息写明测试签名模式与证书两项前提。
   - `tests/virtual_mic_windows_contract.rs`：交叉合同守护——Rust 常量
     与驱动 INF/头文件里的设备名、硬件 ID、包文件名、端点名任一单边
     漂移都直接挂 CI（crate 包不含驱动源码时自动跳过）。
3. **构建与 CI**
   - `scripts/build-driver-windows.ps1`：vswhere 找 MSBuild → 构建 → 收集
     `.sys/.inf/.cat` → `-TestSign` 时 New-SelfSignedCertificate + signtool
     签 .cat/.sys 并导出 .cer。
   - `.github/workflows/driver-windows.yml`：**windows-2022**（runner 镜像
     预装 WDK；windows-2025 已移除 WDK，actions/runner-images#13071），
     x64+ARM64 矩阵，测试签名并上传 artifact——`REAI_VIRTUAL_MIC_DRIVER_DIR`
     直接指向解包目录即可用。
   - `ci.yml` 的 check 矩阵加 `windows-latest`：`src/virtual_mic/windows.rs`
     首次进 CI 编译；`check-release-readiness.sh` 步骤显式 `shell: bash`。

## 关键决策

- **SimpleAudioSample 而非 sysvad 全量**：最小 PortCls/WaveRT 采集驱动就是
  虚拟麦克风的全部；sysvad 的蓝牙/表格检测/keyword detector 全是无关面积。
  端点保留 micarray 拓扑骨架（音量/静音/jack/几何处理器齐全），几何改成
  单个全向麦克风，避免为改名 KSNODETYPE_MICROPHONE 重写 800 行 topo 代码。
- **控制设备写文件，而不是学 macOS 走 UDP**：Windows 侧 CDO + buffered
  write 是最短数据路径（一次内核拷贝进 ring），不引入端口监听/防火墙/
  端口占用问题；SDK `VirtualMic` 从 UDP socket 换成 `File::write`，
  `PcmSink` 合同不变。
- **16 kHz mono 原生采样率**：音频引擎对 App 侧做重采样（同 macOS 的
  「设备原生 16k、系统转换」），SDK 与驱动都不做重采样。
- **CDO 安全描述符允许 Users 读写**（`D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;GRGW;;;WD)`）：
  喂麦克风的普通进程不该要求管理员——与 macOS「装一次要密码、之后随便喂」
  的体验对齐；只有安装（pnputil）走 UAC。
- **Feature 门控不变**：`virtual-mic` feature 依旧零新依赖；Windows 驱动
  产物不进 crate 包（Cargo.toml `exclude`），随 GitHub artifact 分发。

## 验证状态

- [x] `cargo test --workspace --all-features`（Windows 本机）：145 lib +
      集成测试全绿，含 3 个 Windows 合同测试与 3 个 S16LE 单测
- [x] `cargo clippy --workspace --all-features --all-targets -D warnings` +
      `cargo fmt --check`（Windows 本机，clippy 1.94）
- [x] 默认 feature 构建不受影响（`virtual-mic` 仍是零依赖 feature；
      Windows 代码全部在 `#[cfg(target_os = "windows")]` 之后）
- [x] `cargo test`（默认 feature）全绿
- [ ] **驱动编译**：本机无 WDK（只有 SDK），由 `driver-windows` workflow 在
      windows-2022 runner 上验证；首次推送后若报错按 CI 修正
- [ ] **真机验收**：测试签名模式重启 + 导入 .cer + `pnputil /install` +
      系统声音设置出现 "ReAI-Vibe-Board" + BLE 连板录音比对（需要一台允许
      测试签名的 Windows 真机）
- [ ] **二期**：EV 证书 + Partner Center 证明签名、安装包化（去掉
      testsigning 前提）

## 已知限制（文档里都要讲）

- 测试签名前提：`bcdedit /set TESTSIGNING ON` + 重启 + 导入测试证书——开发
  机可接受，最终用户不可接受，所以才有二期签名计划。
- Windows 驱动无法声明「不参与默认设备竞选」（macOS 侧 CanBeDefault=false
  的等价物不存在），首次安装可能抢默认麦克风，文档提示用户去声音设置确认。
- 驱动为单实例设计（全局 CDO + 全局 ring）；多板多设备场景不在本期范围。
