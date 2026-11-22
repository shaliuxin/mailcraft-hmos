// SPDX-FileCopyrightText: MailCraft contributors (ohos port)
// SPDX-License-Identifier: MIT OR Apache-2.0
// P1c: the egui UI page. An XComponent SURFACE (libraryname "entry") is the native window the
// Rust render worker (native/ohos-bridge/src/render.rs) paints the MailCraft egui UI onto. The
// frame is pumped by a timer (entry.eguiFrame ~60 Hz); touch/keys reach egui through the XComponent
// callbacks in napi_init.cpp. The placeholder UI is in render.rs; P1c-1b swaps it for
// MailApp::logic(ctx).
import entry from 'libentry.so';
import { common } from '@kit.AbilityKit';
import display from '@ohos.display';
import i18n from '@ohos.i18n';

@Entry
@Component
struct Index {
  @State startupVisible: boolean = true;
  @State failure: string = '';
  @State statusText: string = '启动 MailCraft egui…';
  private frameTimer: number = -1;
  private context: common.UIAbilityContext =
    this.getUIContext().getHostContext() as common.UIAbilityContext;

  aboutToAppear(): void {
    this.startRender();
  }

  aboutToDisappear(): void {
    if (this.frameTimer >= 0) {
      clearInterval(this.frameTimer);
      this.frameTimer = -1;
    }
    try {
      entry.eguiActive(false);
    } catch (err) {
      console.warn(`MC-RENDER: active(false) failed: ${(err as Error).message}`);
    }
  }

  // Forward the system back gesture/key to egui as an Escape key.
  onBackPress(): boolean {
    try {
      entry.eguiInput(JSON.stringify({
        kind: 'key',
        code: 2070,
        action: 1,
        ctrl: false,
        shift: false,
        alt: false,
        text: '',
      }));
      entry.eguiInput(JSON.stringify({
        kind: 'key',
        code: 2070,
        action: 0,
        ctrl: false,
        shift: false,
        alt: false,
        text: '',
      }));
    } catch (err) {
      console.warn(`MC-RENDER: back key forward failed: ${(err as Error).message}`);
    }
    return true;
  }

  // Build the egui Context + start the render thread, then pump frames on a timer.
  private startRender(): void {
    console.info('MC-RENDER: starting egui render');
    // The device pixel ratio. On a PC/2in1 running at a >1 density a free window's
    // UIContext.vp2px(1) can report 1.0, which makes egui lay the UI out 1:1 and look
    // too small. Use the display's real densityDPI (the canonical 160dpi baseline) as the
    // primary source, falling back to vp2px, and clamp to a sane range so a 0 / NaN can't
    // shrink the UI to nothing.
    let scale = 1;
    try {
      const info = display.getDefaultDisplaySync();
      const dpi = info.densityDPI;
      if (dpi > 0) {
        scale = dpi / 160.0;
      }
      console.info(`MC-RENDER: display densityDPI=${dpi} -> scale=${scale}`);
    } catch (err) {
      console.warn(`MC-RENDER: display density failed, trying vp2px: ${(err as Error).message}`);
    }
    if (!(scale > 0)) {
      try {
        scale = this.getUIContext().vp2px(1);
      } catch (err) {
        console.warn(`MC-RENDER: vp2px failed, defaulting scale=1: ${(err as Error).message}`);
      }
    }
    if (!(scale > 0)) {
      scale = 1;
    }
    // 把沙箱目录交给原生侧作为密钥存储的落点：真实账户的密码/刷新令牌会写到
    // <filesDir>/mailcraft/secrets.json。必须在 eguiInit 之前——渲染线程在 eguiInit 时
    // 构建 Services，会把这份沙箱里的 FileSecretStore 接进 services.secrets。
    try {
      const sandboxDir = this.context.filesDir + '/mailcraft';
      const r = entry.setSandbox(sandboxDir);
      console.info(`MC-RENDER: setSandbox ${r}`);
    } catch (err) {
      console.warn(`MC-RENDER: setSandbox failed: ${(err as Error).message}`);
    }
    // 界面语言按系统自动选择：把系统 locale（如 "zh_CN"）交给桥接。UiState 的默认偏好是 "auto"，
    // 故中文系统得中文界面、英文系统得英文。必须在 eguiInit 之前——渲染线程在 eguiInit 时构建
    // MailApp，会把 system_locale 接进 services，首帧的 apply_language 才解析得出系统语言。
    try {
      const sysLocale = i18n.System.getSystemLocale();
      const lr = entry.setLocale(sysLocale);
      console.info(`MC-RENDER: setLocale ${sysLocale} ${lr}`);
    } catch (err) {
      console.warn(`MC-RENDER: setLocale failed: ${(err as Error).message}`);
    }
    try {
      entry.eguiInit(scale, (message: string): void => {
        console.info(`MC-NATIVE: ${message}`);
        this.statusText = message;
      });
      // The native worker draws on demand; pump a frame ~60 times a second.
      this.frameTimer = setInterval(() => {
        try {
          entry.eguiFrame();
        } catch (err) {
          console.warn(`MC-RENDER: eguiFrame failed: ${(err as Error).message}`);
        }
      }, 16);
      // On launch, open the first message so the reading pane isn't empty (the same "open the
      // top message" Outlook does). Driven through the engine's command channel — the one path
      // the CLI / control channel use — so it mutates the real UI session and repaints the pane.
      setTimeout(() => {
        try {
          entry.eguiCommand('mail.first', '{}');
          entry.eguiCommand('view.mail', '{}');
        } catch (err) {
          console.warn(`MC-RENDER: startup select failed: ${(err as Error).message}`);
        }
      }, 3000);
      this.startupVisible = false;
      this.statusText = 'egui 渲染已启动';
      console.info('MC-RENDER: egui render started, scale=' + scale);
    } catch (err) {
      const m = err instanceof Error ? err.message : String(err);
      this.failure = m;
      console.error(`MC-RENDER: start failed: ${m}`);
    }
  }

  build() {
    Stack() {
      // The native egui surface. libraryname "entry" -> libentry.so, whose Init registers the
      // XComponent callbacks (surface attach/resize/detach + touch/keys -> egui input).
      XComponent({
        id: 'mailcraft-egui',
        type: XComponentType.SURFACE,
        libraryname: 'entry',
      })
        .width('100%')
        .height('100%')

      // A boot overlay until the first frame is up; a fallback if the render thread never starts.
      if (this.startupVisible) {
        Column() {
          Text('MailCraft').fontSize(30).fontWeight(FontWeight.Medium).fontColor('#4C8DFF')
          Text(this.statusText).fontSize(14).fontColor('#B8BAC2').margin({ top: 12 })
          LoadingProgress().width(22).height(22).color('#4C8DFF').margin({ top: 18 })
        }
        .width('100%')
        .height('100%')
        .justifyContent(FlexAlign.Center)
        .backgroundColor('#1A1A1A')
        .hitTestBehavior(HitTestMode.Block)
      }

      if (this.failure.length > 0) {
        Column() {
          Text('MailCraft 原生渲染错误').fontSize(18).fontColor('#FFFFFF')
          Text(this.failure).fontSize(13).fontColor('#FFFFFF').margin({ top: 10 })
          Button('重试').margin({ top: 12 }).onClick(() => {
            this.failure = '';
            this.startRender();
          })
        }
        .padding(24)
        .backgroundColor('#DD262626')
        .borderRadius(8)
        .width('80%')
      }
    }
    .width('100%')
    .height('100%')
    .backgroundColor('#1A1A1A')
  }
}
