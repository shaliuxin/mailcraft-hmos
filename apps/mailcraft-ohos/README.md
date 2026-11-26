# MailCraft for OpenHarmony — 最小链路空壳（apps/mailcraft-ohos）

这是把 MailCraft 的 Rust 核心接到鸿蒙的移植工程，已对齐上游 **v0.15.0**：
**ArkUI → NAPI(`libentry.so`) → C ABI(`libmailcraft_ohos_bridge`) → MailCraft 引擎 + egui 渲染**。

## 当前状态（v0.15.0，2026-11）

早期文档里的「下一步」现已落地，下面这段是当前真机验证的现状。

- **egui 三栏渲染（桌面布局）**：邮件 / 日历等模块已能用上桌面三栏 + ribbon + 状态栏，HTML 邮件由核心解析成 `blocks` 显示模型，UI 不自己解析 HTML。
- **触摸 / 键输入（egui 0.37）**：把鸿蒙触摸事件翻译成 egui 事件，**正确喂入渲染帧的 `RawInput.events`**（适配 egui 0.37「只消费 `Context::run_ui(new_input,…)` 那一帧 `new.events`」的事件消费模型），真机上按钮 / 列表 / 弹窗点击均生效。
  - 通用铁律：桥接**不能用 `ctx.input_mut` 把事件塞进旧状态**（会被下一帧 `new.events` 静默丢弃），必须翻译后交给 `raw.events`。
- **手机 / 桌面双布局按设备类型启用**：`@ohos.deviceInfo.deviceType === 'phone'` 才用手机版界面（单栏 + drawer + tab bar）；平板 / 2-in-1 PC 用原版桌面界面。判定由 ArkTS 读设备类型、经 NAPI `eguiSetPhone` 透传给桥接原子量，render 线程据此设 `phone_layout`。**默认桌面**（探测失败也降级桌面）。
  - 通用铁律：布局开关**绝不能用 `phone_layout: cfg!(target_env="ohos")` 做默认**（那会让所有鸿蒙设备都进手机界面），必须「ArkTS `deviceType==='phone'` → 桥接透传 → 运行时开关」且默认桌面。
- **内置 CA 证书**（`crates/net`，`target_env="ohos"`）：OHOS 无系统 CA 库，改用 **webpki-roots**（Mozilla roots）做 TLS 校验；真机实测 IMAP 到 `imap.gmail.com:993` 的 TLS 握手成功、被服务器以「凭证无效」拒绝（即通路、网络、TLS 端到端验证通过）。
- **Platform trait 实现**：`OhosPlatform` 提供文件型沙箱密钥库、`network=Online`、OAuth `CustomScheme("mailcraft")` 重定向。
- **交付**：源码在本仓库 `hmos` 分支；v0.15.0 安装包见 GitHub [Release v0.15.0](https://github.com/shaliuxin/mailcraft-hmos/releases/tag/v0.15.0)。
  注意：DevEco 自动签名的 Profile 绑定构建机设备 UDID，**第三方设备请用 unsigned 包自有证书重签**。

## 关键坑（移植记录）

## 架构

```
ArkUI (Index.ets: 按钮 → runCommand / setSandbox / hostReady)
   │  import('libentry.so')  →  NAPI 模块 entry.*
   ▼
libentry.so  (entry/src/main/cpp/napi_init.cpp，C++)
   │  静态链接 libmailcraft_ohos_bridge.a
   ▼
libmailcraft_ohos_bridge  (native/ohos-bridge，Rust)
   │  两条路：
   │   1) 命令：mailcraft_engine::Session::run（与 CLI / MCP 同一命令注册表）
   │   2) 平台：mailcraft_host::Host 由 OhosPlatform 构建（网络 / 密钥库 / OAuth 重定向），
   │            供异步同步 job（Host::run_job）使用
   ▼
返回 JSON 结果（命令值 或 {"error": …}，绝不 panic）
```

## 已验证（2026-11-21，aarch64-unknown-linux-ohos，设备 `46G0225429001073`）

- **完整 HAP 构建成功**：`entry-default-signed.hap`（DevEco 自动签名，`cn.shaliuxin.mailcraft`），
  33/33 hvigor 任务通过（含 SignHap），内含 `libs/arm64-v8a/libentry.so`（承载整个 Rust core）
  + `libc++_shared.so` + `ets/modules.abc`。
- **真机运行 + 核心命令**：`hdc install` 成功 → 全新进程，hilog `MC-CORE` 证据：
  - `account.list => [rc=0] {"accounts":[{…"displayName":"Avery Quinn","email":"avery@lumen.example"…}]}`
  - `folder.list  => [rc=0] {…Inbox messages:13 unread:4…}`
  - `view.search  => [rc=0] {"found":13,"query":"","scope":"everywhere"}`
- **CA 证书内置（Task #11）**：OHOS 无系统 CA 库，`crates/net/src/tls.rs` 在
  `#[cfg(target_env="ohos")]` 下改用 **webpki-roots 0.26（Mozilla roots）** +
  `rustls::client::WebPkiServerVerifier::builder_with_provider(store, provider)`（rustls 0.23 无
  `WebPkiVerifier`，见下方坑）。`cargo build -p mailcraft-net --target aarch64-unknown-linux-ohos`
  通过；新 `libentry.so` 上真机跑核心命令无回归。
- **Platform trait 鸿蒙实现（Task #12）**：`native/ohos-bridge/src/ohos_platform.rs` 的
  `OhosPlatform` 实现 `mailcraft_host::Platform`——沙箱 `secrets.json` 文件型密钥库、
  `network=Online`、`redirect=CustomScheme("mailcraft")`，时钟/socket 用 trait 默认。
  hilog `MC-PLATFORM` 证据：
  - `setSandbox => [rc=0] sandbox`
  - `hostReady  => [rc=0] {"host":"ready","memory_budget":268435456,"network":"online","redirect":"custom-scheme:mailcraft"}`
  即 `Host` 从 `OhosPlatform` 成功构建（内存预算 256MB = 引擎上限）。
- **真实 IMAP/SMTP 连接测试（CA 的落地）**：`ohos_mailcraft_test_account(host, port, security,
  user, pass)` 在后台线程上让 `mailcraft_net::test_account` 打开账户的 IMAP 入站服务器（走 TLS，
  用 CA 任务内置的 webpki-roots 校验器握手）并登录；`ohos_mailcraft_test_account_result()` 取结果。
  NAPI 暴露 `entry.testAccount(...)` / `entry.testAccountResult()`，smoke 页开机即向
  `imap.gmail.com:993` 发起一次 TLS 连接并轮询结果（`MC-NET`）。这是 CA 内置的端到端证明。
  hilog `MC-NET` 证据（设备 `46G0225429001073`，pid 21569）：
  ```
  MC-NET: testAccount => [rc=0] started
  MC-NET: poll#0 => [rc=0] pending
  MC-NET: poll#1 => [rc=0] pending
  MC-NET: poll#2 => [rc=0] error: sign-in failed: [AUTHENTICATIONFAILED] Invalid credentials
  ```
  **关键**：结果是 `[AUTHENTICATIONFAILED] Invalid credentials`（用了假账号 `smoke@mc.invalid`/`smoke`），
  **不是**连接失败、**不是** TLS/证书错误。这证明整条通路在真机上是活的：DNS 解析 → TCP 连上
  `imap.gmail.com:993` → **TLS 握手成功（webpki-roots 校验器接受了 Google 的证书——即 CA 任务的落地）**
  → IMAP 协议走到登录步、被服务器以「凭证无效」拒绝。即 CA 内置 + 网络 + TLS 端到端验证通过。

## 关键坑（移植记录）

- **cfg 用 `target_env` 不用 `target_os`**：目标 `aarch64-unknown-linux-ohos` 报
  `target_os="linux"` / `target_env="ohos"`，写 `#[cfg(target_os="ohos")]` 永远为假、CA 分支
  根本不编。一律 `#[cfg(target_env="ohos")]`。
- **rustls 0.23 无 `WebPkiVerifier`**（那是 0.24+）：用
  `rustls::client::WebPkiServerVerifier::builder_with_provider(store, provider).build()`
  （重导出在 `rustls::client::`，不在 crate 根）；`RootCertStore.roots` 是 pub 字段，直接
  `extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned())`。
- **HAP「重复 libentry.so」**：`entry/libs/arm64-v8a/` **只放 `libmailcraft_ohos_bridge.a`**；
  `libentry.so` 由 hvigor/CMake（`napi_init.cpp` + 该 `.a`）产出。预置同名 `.so` 会让 hvigor
  合并两个 `libentry.so` 而 FAIL。`build_entry_native.sh` 已改为只编并拷 `.a`。
- **并行 rustc 死锁**：bridge 独立 target 首次全量编译可能多 rustc 0-CPU 冻结；
  双采样 CPU 零增量 + 3min 无产物即判死锁 → 停任务、清残留进程、**增量重跑同一条 cargo build** 即愈。
- **hdc 路径**：Git Bash 下 `hdc install <绝对路径>` 会与 CWD 拼接翻倍，改用 PowerShell 原生反斜杠路径。
- **进程复用**：鸿蒙 PC 上 `aa start` 复用旧实例（`aboutToAppear` 不重跑）；`hdc uninstall`+`install`
  取新进程才触发页面初始化。

## 构建

```bash
# 交叉编译 Rust 桥接，产出 entry/libs/arm64-v8a/libmailcraft_ohos_bridge.a（libentry.so 交给 hvigor）
OHOS_SDK="C:/ohos-sdk/default/openharmony/native" ./scripts/build_entry_native.sh
```

然后用 DevEco Studio / hvigor 打 HAP：

1. **签名**：在 DevEco 里用本项目专属 `bundleName = cn.shaliuxin.mailcraft` 做一次自动签名，
   把生成的 `.cer`/`.p12`/`.p7b` 填进 `build-profile.json5` 的 `signingConfigs`（**切勿复用其他项目签名**）。
2. 在 DevEco 打开 `apps/mailcraft-ohos`，或命令行
   `devecocli build --build-mode debug --modules entry --assemble-type assembleHap`（hvigor 会
   编 `napi_init.cpp` + 链接 `.a` → `libentry.so`，再打包、签名）。
3. 真机/模拟器安装后，首页点 `account.list` / `folder.list` / `hostReady` 按钮即驱动核心/平台跑并显示 JSON。

## 下一步（工程阶段）

- 真实 IMAP/SMTP 同步：`ohos_mailcraft_test_account` 已打通「打开 TLS 连接 + 登录」这一段（CA
  内置的落地验证）。下一步接 `Host::run_job` + `sync_driver` + 账户配置，把「测试连接」扩成
  双向同步（拉取新邮件 / 推送发件箱）。CA 已内置、密钥库已就位，`OhosPlatform` 是换入系统凭证
  管理器（`@ohos.useriam`）的换入点。
- 把 smoke 页扩成 Outlook 式三栏（文件夹 / 邮件列表 / 阅读窗格）：核心已把 HTML 解析成 `blocks`
  显示模型，UI 不必自己解析 HTML。
- OAuth 回跳：`redirect` 已报 `CustomScheme("mailcraft")`，待注册自定义 scheme 的 ability 接回。
