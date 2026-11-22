# MailCraft → OpenHarmony / HarmonyOS NEXT 交叉编译可行性验证报告

- 日期：2026-11-20
- 上游：`UCS-Care-Group/mailcraft` `main` @ `33740ca6`（workspace 版本 **0.14.0**）
- 宿主：Windows 11，rustc 1.99.1，OHOS NDK（`C:/ohos-sdk/default/openharmony/native`，clang 15.0.4）
- 目标 target：`aarch64-unknown-linux-ohos`
- 结论：**通过（PASS）**。整个移动端 Rust 核心 `mailcraft-mobile` 干净交叉编译到 OHOS，含 TLS/IMAP/SMTP/全密码学栈/日历/联系人/UniFFI 绑定生成器。命门（ring 汇编 + cryptoki/NSS）全部解除风险。

---

## 1. 上游现状（已查证）

- MailCraft 是 2026-10-12 起开发的 clean-room 邮件/日历/联系人/任务客户端，纯 Rust，README 自述 "Early alpha"，上游 `apps/` 已含 `mailcraft-android`（Gradle/Kotlin）与 `mailcraft-web`（WASM），但**无任何 OpenHarmony/HarmonyOS 目标**。
- UI 前端是 `egui`（桌面 `ui-egui`）；移动端走另一条路：`crates/mobile`（`mailcraft-mobile`）把 Rust 核心通过 **UniFFI** 导出为给 Kotlin/Swift 用的 `Core` 对象。
- 移动端架构（`crates/mobile/src/lib.rs` 头注释）：**无 UI 工具包、无 WebView**。App 从核心返回的 `blocks`（显示块）模型里画邮件；命令经 `Core::execute(id, …)` 走与 CLI/控制通道/MCP 相同的命令注册表；`Core::next_event` 是事件流。
- 集成面（`crates/mobile/src/platform.rs`）：一个 `Platform` 外挂 trait（`#[uniffi::export(with_foreign)]`）由原生层实现，是**唯一**的原生集成面。

## 2. 命门测试结果（全部 PASS）

| 步骤 | 目标 | 结果 |
|---|---|---|
| ring 0.17.14 汇编（SHA-256） | aarch64-unknown-linux-ohos | ✅ 编译通过，产物为合法 ELF64 AArch64 |
| ring + rustls 0.23.46（TLS 协议层，ring provider） | 同上 | ✅ 30.1 MB staticlib |
| `mailcraft-net`（IMAP/SMTP/TLS/OAuth/Graph/MX，含 **cryptoki/cryptoki-sys**、rustls-platform-verifier、rustls-native-certs） | 同上 | ✅ 2m36s 编译通过 |
| **`mailcraft-mobile`**（engine+host+openpgp+smime+ical+vcard+net+全密码学栈+uniffi 0.32.2） | 同上 | ✅ 2m18s 编译通过 |

产物（`target/aarch64-unknown-linux-ohos/release/`）：
- `libmailcraft_mobile.so` — **40 MB**，ELF64 AArch64 DYN（即鸿蒙 App 通过 NAPI 加载的形态）
- `libmailcraft_mobile.a` — **341 MB** 静态库

### 关键风险点已逐一解除
- **ring 汇编**：OHOS NDK 的 `clang.exe` 直接交叉编译 ring 的 aarch64 汇编通过。
- **cryptoki / NSS**：原本担心 NSS 的 `libnssckbi.so` 在 OHOS 缺失会阻塞。实测 `cryptoki-sys` 在 staticlib 下只链接 FFI 声明（`libloading` 运行时按名加载），**编译不阻塞**。运行期若需 PKCS#11 才需处理。
- **rustls-platform-verifier**：linux/OHOS 分支走 `rustls-native-certs`（纯 Rust + `openssl-probe` 找 CA 包），**编译通过**；运行期需把 CA 证书包指到鸿蒙可用路径（属运行期配置，非编译阻塞）。
- **完整密码学栈**（p256/p384/p521/ecdsa/rsa/pgp/cms/pkcs12/openpgp/smime）全部编译通过。

## 3. 集成面（鸿蒙原生层需实现）

`Platform` trait（`crates/mobile/src/platform.rs`）即唯一原生集成面，每个方法都干净映射到鸿蒙系统 API，且 `Native` 已为每个失败方法提供安全兜底：

| Platform 方法 | 鸿蒙侧实现 |
|---|---|
| `secrets()` → `SecretStore`（key = `password:<addr>`/`refresh:<addr>`，不存 access token） | `@ohos.security`（或加密 Preferences） |
| `open_sign_in_page(url)`（OAuth 页，Custom Scheme 回跳） | `@ohos.bundle.Web` 或 want 打开系统浏览器 |
| `redirect()` | `CustomScheme { scheme }` |
| `notify(NotificationRecord)` / `schedule_reminder(at, …)` | `@ohos.notification` / `@ohos.reminderAgentManager` |
| `open_file(name, bytes)`（保存附件） | `@ohos.file` |
| `open_link(url)`（浏览器/拨号） | `@ohos.bundle.openLink` / want |
| `now()` / `utc_offset_minutes(at)` / `time_zone_name()` | `@ohos.i18n.TimeZone` + 系统时钟 |
| `memory_budget()` / `network()` | `@ohos`（内存上限 / 网络状态） |
| `host` 层 `connect(host, port, timeout) -> Stream`（裸 socket，`Native` 提供"失败即类型化错误"的兜底） | 鸿蒙侧提供（参照 rustdesk 鸿蒙版 shims/tokio-native-tls 做法） |

## 4. 绑定与 UI 路径（工程阶段）

- 绑定生成：Android 用独立 `uniffi-bindgen/` 工程（Gradle 任务 `UniffiBindgen`）从 UniFFI API 生成 Kotlin 绑定；`AndroidPlatform.kt` 实现 `Platform` 外挂 trait。
- 鸿蒙路径（推荐）：
  1. Rust 核心 → `libmailcraft_mobile.so`（已 ✅）
  2. 由 UniFFI 生成 C ABI（`uniffi-bindgen` 出 C 头 + C 脚手架），写一层薄 C `libentry.so` NAPI 垫片，把 C FFI 暴露给 ArkTS（UniFFI 无官方 HarmonyOS 后端，走通用 C ABI 最稳）
  3. ArkUI 前端从 `blocks` 显示模型画三栏（文件夹 / 邮件列表 / 阅读窗格）+ 日历网格，经 NAPI 实现 `Platform` trait
- UI 重写工作量（参照 solidcraft/rustdesk 鸿蒙版）：ArkUI 原生重写 Outlook 式界面；**好消息**是核心已把 HTML 解析成 `blocks`，UI 层不必自己解析 HTML。

## 5. 运行期待办（非编译阻塞，工程阶段处理）

1. **CA 证书包**：`rustls-native-certs` 在 OHOS 上找不到系统 CA 路径 → 需内置 Mozilla CA bundle 并指向它。
2. **PKCS#11 / NSS**：仅当需要 `cryptoki` 硬件密钥时才需处理，普通 IMAP/SMTP/Graph 账户不需要。
3. **裸 socket / TLS 验证器**：参照 rustdesk 鸿蒙版的 shims 做法，由鸿蒙侧提供连接（`Native` 已有兜底）。
4. **OAuth 回跳**：Custom Scheme 需在 `module.json5` 声明并注册。

## 6. 结论与建议

- **可行性：高**。整个移动端 Rust 核心已 100% 交叉编译到 OHOS，命门全部解除，且集成面（`Platform` trait）边界清晰、1:1 映射鸿蒙系统 API。
- **工具链已配置**：`mailcraft/.cargo/config.toml` 已追加 `[target.aarch64-unknown-linux-ohos]` + NDK 环境变量段（可直接 `cargo build -p mailcraft-mobile --release --target aarch64-unknown-linux-ohos` 复现）。

## 7. 最小链路进展（已推进到 C ABI 层，含运行期证明）

采用比 UniFFI-C-生成更稳的路线：一个独立 Rust 桥接 crate 直接驱动 `mailcraft_engine` 的命令注册表（与 CLI/控制通道/MCP 同一路径），用 `#[no_mangle] extern "C"` 暴露给 NAPI。

### 7.1 桥接 crate `native/ohos-bridge`（已编译到 OHOS + 宿主逻辑验证通过）
- 依赖 `mailcraft-engine` + `mailcraft-time`；`crate-type = [cdylib, staticlib, lib]`。
- 导出三个 C ABI 入口：
  - `ohos_mailcraft_init() -> i32`：建样例邮箱 `Session`
  - `ohos_mailcraft_run(cmd, params_json) -> i32`：跑一条命令，结果存入缓冲
  - `ohos_mailcraft_result() -> *const c_char`：取结果 JSON（值或 `{"error":…}`）
- 命门式安全：命令失败返回 JSON 错误，**绝不 panic**；所有外部输入按敌意处理。

### 7.2 产物与验证
- `libmailcraft_ohos_bridge.so`（aarch64-unknown-linux-ohos）：**32 MB**，ELF64 AArch64 DYN，
  导出 `ohos_mailcraft_init/run/result` 三符号，**只依赖 OHOS `libc.so`**（无 NSS/外部依赖）。
- 宿主 smoke 二进制（`smoke.rs`，`cargo run --bin smoke`）端到端跑通：
  - `ohos_mailcraft_init()` → rc=0
  - `folder.list` → rc=0，返回样例邮箱真实文件夹树（`avery@lumen.example`、Atlas…）
  - `account.list` → rc=0，返回真实账户数据（Avery Quinn、auth=password…）
  - `view.search` → rc=1，类型化错误 `{"error":"bad parameters: query (string) is required"}`
  - 坏命令 id → rc=1，`{"error":"unknown command does.not.exist"}`（不 panic）

### 7.3 下一步
- 搭 HarmonyOS HAP 空壳（`entry` 模块 + `libentry.so` NAPI + ArkUI 单页），`libentry.so` dlopen `libmailcraft_ohos_bridge.so` 调三个 C 入口，ArkUI 渲染返回 JSON。
- 网络类命令需鸿蒙侧实现 `Platform`（socket/CA 证书/密钥库/通知/打开链接），见 §3/§5。

