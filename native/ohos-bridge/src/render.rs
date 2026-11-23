// SPDX-FileCopyrightText: MailCraft contributors (ohos port)
// SPDX-License-Identifier: MIT OR Apache-2.0
//! egui rendering on the OpenHarmony NativeWindow via wgpu's GL backend.
//!
//! This is the OHOS twin of eframe: it drives egui through its own `egui::Context`,
//! bypassing eframe/winit. Mirrors the proven `solidcraft-hmos` render layer, minus the
//! 3D viewport's depth-stencil attachment (MailCraft's UI is pure 2D egui, so the color
//! attachment alone is enough) and minus the P3 color path (sRGB for now).
//!
//! The native window handle stays in Rust (it never enters the safe engine). wgpu 30 needs
//! a `raw_window_handle` rather than a bare pointer, so `Window` implements
//! `HasWindowHandle` / `HasDisplayHandle` with `OhosNdkWindowHandle`.

use crate::input;
use raw_window_handle::{
    DisplayHandle, HasDisplayHandle, HasWindowHandle, OhosNdkWindowHandle, WindowHandle,
};
use std::{ffi::c_void, ptr::NonNull, sync::mpsc::Receiver};
use std::time::Duration;

// P1c-1b: the real MailCraft UI. `MailApp::logic` + `MailApp::ui` are the eframe `App` phases
// inlined here; the sample mailbox is the same one the CLI/MCP smoke drives.
use mailcraft_engine::{sample, Session};
use mailcraft_ui_egui::{MailApp, Services};
// `OhosPlatform::secrets()` is a `Platform` trait method; the trait must be in scope to call it.
use mailcraft_host::Platform;

unsafe extern "C" {
    /// Release a NativeWindow reference the XComponent callback took on our behalf.
    fn OH_NativeWindow_NativeObjectUnreference(object: *mut c_void) -> i32;
}

/// Owns one NativeWindow reference (taken in `OnSurfaceCreated`, released here).
pub struct Window(pub usize);
impl Drop for Window {
    fn drop(&mut self) {
        // SAFETY: this Window owns one reference; all surfaces borrowing it are gone by drop.
        unsafe {
            OH_NativeWindow_NativeObjectUnreference(self.0 as *mut c_void);
        }
    }
}
impl HasDisplayHandle for Window {
    fn display_handle(&self) -> Result<DisplayHandle<'_>, raw_window_handle::HandleError> {
        Ok(DisplayHandle::ohos())
    }
}
impl HasWindowHandle for Window {
    fn window_handle(&self) -> Result<WindowHandle<'_>, raw_window_handle::HandleError> {
        let ptr = NonNull::new(self.0 as *mut c_void)
            .ok_or(raw_window_handle::HandleError::Unavailable)?;
        // SAFETY: the borrowed handle cannot outlive the reference this Window owns.
        Ok(unsafe { WindowHandle::borrow_raw(OhosNdkWindowHandle::new(ptr).into()) })
    }
}

/// The single-surface GL renderer.
pub struct GpuRender {
    surface: Option<wgpu::Surface<'static>>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    renderer: egui_wgpu::Renderer,
    config: wgpu::SurfaceConfiguration,
    adapter: wgpu::Adapter,
    instance: wgpu::Instance,
    window: Option<std::sync::Arc<Window>>,
}

impl GpuRender {
    pub fn new(window: std::sync::Arc<Window>, w: u32, h: u32) -> Result<Self, String> {
        // wgpu 30: build the instance without a display handle; the GL backend is OHOS's only one.
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::GL,
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        });
        let surface = instance
            .create_surface(window.clone())
            .map_err(|e| e.to_string())?;
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            compatible_surface: Some(&surface),
            ..Default::default()
        }))
        .map_err(|e| e.to_string())?;
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("MailCraft"),
            required_limits: adapter.limits(),
            ..Default::default()
        }))
        .map_err(|e| e.to_string())?;
        // Surface wgpu's unreported validation errors so a black screen is diagnosable.
        device.on_uncaptured_error(std::sync::Arc::new(|error| {
            log::error!("wgpu uncaptured: {error}");
        }));
        {
            let info = adapter.get_info();
            let caps = surface.get_capabilities(&adapter);
            log::info!(
                "mailcraft adapter name={:?} backend={:?} type={:?} formats={:?} present={:?} alpha={:?}",
                info.name,
                info.backend,
                info.device_type,
                caps.formats,
                caps.present_modes,
                caps.alpha_modes
            );
        }
        // wgpu 30: ask the surface for its default config instead of building one by hand.
        let mut config = surface
            .get_default_config(&adapter, w.max(1), h.max(1))
            .ok_or("no native surface configuration")?;
        // The OHOS GL surface's default format is an sRGB-aware Rgba8UnormSrgb. egui encodes its
        // own sRGB, so a linear framebuffer avoids a double gamma (the washed-out look). Rgba8Unorm
        // is in the adapter's format list, so a linear swap is safe.
        if config.format.is_srgb() {
            config.format = wgpu::TextureFormat::Rgba8Unorm;
        }
        config.present_mode = wgpu::PresentMode::Fifo;
        config.desired_maximum_frame_latency = 1;
        surface.configure(&device, &config);
        let renderer = egui_wgpu::Renderer::new(
            &device,
            config.format,
            egui_wgpu::RendererOptions {
                msaa_samples: 1,
                ..Default::default()
            },
        );
        Ok(Self {
            surface: Some(surface),
            device,
            queue,
            renderer,
            config,
            adapter,
            instance,
            window: Some(window),
        })
    }

    pub fn resize(&mut self, w: u32, h: u32) -> Result<(), String> {
        if w == 0 || h == 0 {
            return Ok(());
        }
        self.config.width = w;
        self.config.height = h;
        if let Some(surface) = &self.surface {
            surface.configure(&self.device, &self.config);
        }
        Ok(())
    }

    pub fn detach(&mut self) {
        self.surface = None;
        self.window = None;
    }

    /// Paint one egui frame. `shapes` come from a `ctx.run` pass in the caller.
    #[allow(clippy::too_many_arguments)]
    fn draw(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        renderer: &mut egui_wgpu::Renderer,
        ctx: &egui::Context,
        surface: &wgpu::Surface<'static>,
        config: &wgpu::SurfaceConfiguration,
        shapes: Vec<egui::epaint::ClippedShape>,
        pixels_per_point: f32,
    ) -> Result<bool, String> {
        let frame = match surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(f) | wgpu::CurrentSurfaceTexture::Suboptimal(f) => f,
            wgpu::CurrentSurfaceTexture::Outdated
            | wgpu::CurrentSurfaceTexture::Lost
            | wgpu::CurrentSurfaceTexture::Timeout
            | wgpu::CurrentSurfaceTexture::Occluded => return Ok(false),
            wgpu::CurrentSurfaceTexture::Validation => {
                return Err("native surface validation failed".into())
            }
        };
        let mut jobs = ctx.tessellate(shapes, pixels_per_point);
        let screen = egui_wgpu::ScreenDescriptor {
            size_in_pixels: [config.width, config.height],
            pixels_per_point,
        };
        let mut encoder = device.create_command_encoder(&Default::default());
        let mut commands = renderer.update_buffers(device, queue, &mut encoder, &jobs, &screen);
        let view = frame.texture.create_view(&Default::default());
        let attachments = [Some(wgpu::RenderPassColorAttachment {
            view: &view,
            depth_slice: None,
            resolve_target: None,
            ops: wgpu::Operations {
                load: wgpu::LoadOp::Clear(wgpu::Color { r: 0.12, g: 0.12, b: 0.14, a: 1.0 }),
                store: wgpu::StoreOp::Store,
            },
        })];
        let pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("MailCraft UI"),
            color_attachments: &attachments,
            depth_stencil_attachment: None,
            ..Default::default()
        });
        renderer.render(&mut pass.forget_lifetime(), &jobs, &screen);
        commands.push(encoder.finish());
        queue.submit(commands);
        queue.present(frame);
        Ok(true)
    }

    pub fn paint(&mut self, ctx: &egui::Context, mut output: egui::FullOutput) -> Result<bool, String> {
        // Consume the texture deltas (and frees) first, so a drop never trips a debug assert.
        let mut texture_updates = std::mem::take(&mut output.textures_delta.set);
        let free = std::mem::take(&mut output.textures_delta.free);
        output.textures_delta.clear();
        for (id, deltas) in &mut texture_updates {
            for delta in deltas {
                self.renderer.update_texture(&self.device, &self.queue, *id, delta);
            }
        }
        for id in free {
            self.renderer.free_texture(&id);
        }
        let Some(surface) = &self.surface else {
            return Ok(false);
        };
        Self::draw(
            &self.device,
            &self.queue,
            &mut self.renderer,
            ctx,
            surface,
            &self.config,
            output.shapes,
            output.pixels_per_point,
        )
    }
}

/// Owns the window + GPU, independent of the GPU lifetime.
pub struct Render {
    width: u32,
    height: u32,
    window: Option<std::sync::Arc<Window>>,
    gpu: Option<GpuRender>,
}

impl Render {
    pub fn new() -> Self {
        Self {
            width: 0,
            height: 0,
            window: None,
            gpu: None,
        }
    }

    pub fn bind(&mut self, window: Window, w: u32, h: u32) -> Result<(), String> {
        self.window = Some(std::sync::Arc::new(window));
        self.width = w;
        self.height = h;
        let window = self.window.clone().ok_or("native window unavailable")?;
        if let Some(gpu) = self.gpu.as_mut() {
            let surface = gpu
                .instance
                .create_surface(window.clone())
                .map_err(|e| e.to_string())?;
            gpu.surface = Some(surface);
            gpu.window = Some(window);
            gpu.resize(w.max(1), h.max(1))
        } else {
            self.gpu = Some(GpuRender::new(window, w, h)?);
            Ok(())
        }
    }

    pub fn resize(&mut self, w: u32, h: u32) -> Result<(), String> {
        self.width = w;
        self.height = h;
        if w == 0 || h == 0 {
            return Ok(());
        }
        if let Some(gpu) = self.gpu.as_mut() {
            gpu.resize(w, h)
        } else {
            Ok(())
        }
    }

    pub fn detach(&mut self) {
        if let Some(gpu) = self.gpu.as_mut() {
            gpu.detach();
        }
        self.window = None;
    }

    pub fn paint(&mut self, ctx: &egui::Context, output: egui::FullOutput) -> Result<bool, String> {
        if self.width == 0 || self.height == 0 {
            return Ok(false);
        }
        let gpu = self.gpu.as_mut().ok_or("native GPU renderer unavailable")?;
        gpu.paint(ctx, output)
    }
}

/// Render-thread main loop. `rx` receives messages from the NAPI thread; `ctx` is the
/// shared egui Context.
pub fn run(rx: Receiver<crate::Message>, ctx: egui::Context) {
    log::info!("mailcraft render thread starting");
    let mut render = Render::new();

    // P1c-1b: the real MailCraft UI. Built on this thread (MailApp is `!Send`) and driven every
    // frame by `logic` (state update) then `ui` (draw) — the eframe `App::logic` + `App::ui`
    // split, inlined because OHOS has no eframe. The sample mailbox (Avery Quinn, 13 messages) is
    // the same one the CLI/MCP smoke uses.
    let now = mailcraft_time::now();
    let session = Session::new(sample::mailbox(now, 0));
    // 接真实 IMAP/SMTP：sync 闭包让 UI 的「收发(Send/Receive)」动作走 services.sync → 后台线程跑
    // mailcraft_net::sync_with（真实 IMAP/SMTP 连接），结果逐块经 channel 回传，UI 在自己的帧里
    // 轮询 sync_rx 应用。sync_cancel 是共享取消旗标。无该闭包时 UI 会报「此构建不能连接邮件服务器」。
    let sync_cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let mut services = Services::default();
    services.sync_cancel = sync_cancel.clone();
    services.sync = Some(Box::new(move |request| {
        use std::sync::atomic::Ordering;
        let (tx, rx) = std::sync::mpsc::sync_channel(8);
        let cancel = sync_cancel.clone();
        let _ = std::thread::Builder::new()
            .name("mc-sync".to_string())
            .spawn(move || {
                mailcraft_net::sync_with(&request, &cancel, &mut |piece| {
                    // 接收端被 UI 丢弃（取消/换账号）时通知后台线程停手。
                    if tx.send(piece).is_err() {
                        cancel.store(true, Ordering::Relaxed);
                    }
                });
            });
        rx
    }));
    log::info!("mailcraft sync closure wired (IMAP/SMTP live)");
    // 密钥安全存储：把沙箱里的 FileSecretStore 接进 Services，真实账户的密码/刷新令牌会
    // 持久化到 <sandbox>/secrets.json（重启后仍可读取）。沙箱路径由 ArkTS 在 eguiInit 前经
    // setSandbox 设好；没设则无存储（如同无密钥管理的系统，每次启动都问）。
    if let Some(sandbox) = crate::sandbox() {
        let platform = crate::OhosPlatform::with_sandbox(sandbox.clone());
        services.secrets = platform.secrets();
        log::info!("mailcraft credential store wired at {:?}", sandbox);
    } else {
        log::warn!("mailcraft no sandbox set: credential store unavailable");
    }
    // 界面语言按系统自动选择：ArkTS 在 eguiInit 前经 setLocale 把系统 locale（如 "zh_CN"）交给
    // 桥接。UiState 的默认偏好已是 "auto"，故把 system_locale 接上后，from_pref("auto", locale)
    // 即解析出系统语言——中文系统得中文界面，英文系统得英文。没有设则 None（退回英文）。
    services.system_locale = crate::locale().map(|s| s.to_string());
    log::info!("mailcraft interface locale: {:?}", crate::locale());
    let mut mail_app = MailApp::new(session, services);
    log::info!("mailcraft MailApp constructed (sample mailbox, sync wired)");

    // High-DPI: the OHOS surface is in device pixels, egui lays out in points. egui 0.37 derives
    // the effective pixels-per-point as `zoom_factor * native_pixels_per_point`, and reads
    // `native_pixels_per_point` from each frame's `RawInput.viewports` (NOT from a one-shot
    // `ctx.set_pixels_per_point`, which only nudges the zoom_factor and jitter-scales the first
    // screen_rect). So we feed the device ratio through the per-frame input (below) instead.
    let dpr = crate::current_scale();
    let dpr = if dpr > 0.0 { dpr } else { 1.0 };
    log::info!("mailcraft render thread dpr={dpr}");

    loop {
        match rx.try_recv() {
            Ok(crate::Message::Attach(window, width, height)) => {
                log::info!("mailcraft attach {width}x{height}");
                match render.bind(Window(window), width, height) {
                    Ok(()) => log::info!("mailcraft surface bound"),
                    Err(e) => log::error!("mailcraft bind failed: {e}"),
                }
            }
            Ok(crate::Message::Resize(width, height)) => {
                if let Err(e) = render.resize(width, height) {
                    log::error!("mailcraft resize failed: {e}");
                }
            }
            Ok(crate::Message::Detach) => {
                render.detach();
                log::info!("mailcraft detach");
            }
            Ok(crate::Message::Frame) => {
                // The screen is the physical surface in points (physical / dpr); egui's
                // pixels-per-point (set per-frame below) scales points back up to device pixels.
                let size = egui::vec2(
                    (render.width as f32 / dpr).max(1.0),
                    (render.height as f32 / dpr).max(1.0),
                );
                let viewport_id = egui::ViewportId::default();
                let mut raw = egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
                    viewport_id,
                    ..Default::default()
                };
                // egui 0.37 reads the device ratio from the input's viewport info, not from a
                // one-shot `ctx.set_pixels_per_point`. Feed it here so the UI renders at the
                // native resolution instead of looking half-sized on a >1-dpr screen.
                raw.viewports
                    .entry(viewport_id)
                    .or_default()
                    .native_pixels_per_point = Some(dpr);
                // `logic` runs on the first layout pass only; `ui` draws on every pass — the same
                // split eframe makes between `App::logic` and `App::ui`.
                let full_output = ctx.run_ui(raw, |ui| {
                    if ui.ctx().current_pass_index() == 0 {
                        mail_app.logic(ui.ctx());
                    }
                    mail_app.ui(ui);
                });
                if let Err(e) = render.paint(&ctx, full_output) {
                    log::warn!("mailcraft paint failed: {e}");
                }
            }
            Ok(crate::Message::Active(active)) => {
                log::info!("mailcraft active={active}");
            }
            Ok(crate::Message::Input(input)) => {
                log::info!("mailcraft input: {input:?}");
                input::apply(&ctx, &input, dpr);
            }
            // A programmatic command (a tap / menu press turned into an engine command): run it
            // on the UI's `MailApp` session. `execute` mutates the session (e.g. selects a
            // message); the next Frame's `ui` then shows the reading pane with that message.
            Ok(crate::Message::Command { id, params }) => {
                let p: serde_json::Value =
                    serde_json::from_str(params.trim()).unwrap_or(serde_json::Value::Null);
                match mail_app.execute(&id, p) {
                    Ok(v) => log::info!("mailcraft command {id}: {v}"),
                    Err(e) => log::warn!("mailcraft command {id} failed: {e}"),
                }
            }
            Ok(crate::Message::Wake(_)) => {}
            // No work this tick (normal before the surface is attached, and between
            // frame pumps). Keep looping so a later Frame / Attach / Input is not lost.
            // Only a truly closed channel ends the thread.
            Err(std::sync::mpsc::TryRecvError::Empty) => {
                std::thread::sleep(Duration::from_millis(8));
                continue;
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => break,
        }
        std::thread::sleep(Duration::from_millis(8));
    }
    log::info!("mailcraft render thread exiting");
}
